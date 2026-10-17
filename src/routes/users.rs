use axum::{extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::{Page, Paging, me::CreateKey};
use crate::{
    AppState,
    auth::{self, ApiKey, KeyOwner, NewApiKey, Principal},
    error::{ApiError, ApiResult, ErrorBody},
    events::{Actor, Diff, Event},
    extract::{Json, Path, Query},
    mail::welcome_mail,
    models::{Role, non_blank},
};

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub role: Role,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const SELECT: &str = "SELECT id, email, name, role, created_at, updated_at FROM users";

pub fn valid_email(email: &str) -> ApiResult<String> {
    let email = email.trim();
    email.parse::<lettre::Address>().map_err(|_| ApiError::bad_request(format!("invalid e-mail: {email}")))?;
    Ok(email.to_owned())
}

/// Finds the user with this e-mail or creates a reporter. A new user also gets a key, to be e-mailed after commit.
pub async fn find_or_create(
    db: &mut PgConnection,
    email: &str,
    name: Option<String>,
    actor: Actor,
) -> ApiResult<(Uuid, Option<NewApiKey>)> {
    let email = valid_email(email)?;
    let name = non_blank(name);
    let created: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO users (id, email, name) VALUES ($1, $2, $3) ON CONFLICT (lower(email)) DO NOTHING RETURNING id",
    )
    .bind(Uuid::now_v7())
    .bind(&email)
    .bind(&name)
    .fetch_optional(&mut *db)
    .await?;
    if let Some(id) = created {
        Event::new("user.created", "user", id)
            .changes(serde_json::json!({ "email": email, "name": name, "role": Role::Reporter }))
            .record(&mut *db, actor)
            .await?;
        let key = auth::create_key(&mut *db, KeyOwner::User(id), Some("welcome e-mail"), actor).await?;
        return Ok((id, Some(key)));
    }
    let id =
        sqlx::query_scalar("UPDATE users SET name = coalesce(name, $2) WHERE lower(email) = lower($1) RETURNING id")
            .bind(&email)
            .bind(&name)
            .fetch_one(&mut *db)
            .await?;
    Ok((id, None))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListUsers {
    /// Exact e-mail, case-insensitive.
    pub email: Option<String>,
    pub role: Option<Role>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// List users.
#[utoipa::path(get, operation_id = "listUsers", path = "/v1/users", tag = "users", params(ListUsers), responses((status = 200, body = Page<User>)))]
pub async fn list(
    State(state): State<AppState>,
    principal: Principal,
    Query(query): Query<ListUsers>,
) -> ApiResult<Json<Page<User>>> {
    principal.require_admin()?;
    let (limit, offset) = Paging { limit: query.limit, offset: query.offset }.resolve();
    let users = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT} WHERE ($1::text IS NULL OR lower(email) = lower($1)) AND ($2::user_role IS NULL OR role = $2)
         ORDER BY created_at, id LIMIT $3 OFFSET $4"
    )))
    .bind(query.email)
    .bind(query.role)
    .bind(limit + 1)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(Page::from_rows(users, limit, offset)))
}

#[derive(Deserialize, ToSchema)]
pub struct CreateUser {
    pub email: String,
    pub name: Option<String>,
    /// Default `reporter`.
    pub role: Option<Role>,
    /// E-mail the new user a key. Default `true`.
    pub send_key: Option<bool>,
}

/// Create a user.
#[utoipa::path(
    post, operation_id = "createUser", path = "/v1/users", tag = "users", request_body = CreateUser,
    responses((status = 201, body = User), (status = 409, body = ErrorBody, description = "E-mail taken"))
)]
pub async fn create(
    State(state): State<AppState>,
    principal: Principal,
    Json(body): Json<CreateUser>,
) -> ApiResult<(StatusCode, Json<User>)> {
    principal.require_admin()?;
    let email = valid_email(&body.email)?;
    let mut tx = state.db.begin().await?;
    let user: User = sqlx::query_as(
        "INSERT INTO users (id, email, name, role) VALUES ($1, $2, $3, $4)
         RETURNING id, email, name, role, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(&email)
    .bind(non_blank(body.name))
    .bind(body.role.unwrap_or(Role::Reporter))
    .fetch_one(&mut *tx)
    .await?;
    Event::new("user.created", "user", user.id)
        .changes(serde_json::json!({ "email": user.email, "name": user.name, "role": user.role }))
        .record(&mut *tx, (&principal).into())
        .await?;
    let key = match body.send_key.unwrap_or(true) {
        true => {
            Some(auth::create_key(&mut tx, KeyOwner::User(user.id), Some("welcome e-mail"), (&principal).into()).await?)
        }
        false => None,
    };
    tx.commit().await?;
    if let Some(key) = key {
        state.mailer.send(welcome_mail(state.public_url.as_deref(), &user.email, user.name.as_deref(), &key.key, None));
    }
    Ok((StatusCode::CREATED, Json(user)))
}

async fn load(state: &AppState, id: Uuid) -> ApiResult<User> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE id = $1")))
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("user"))
}

/// Get a user.
#[utoipa::path(
    get, operation_id = "getUser", path = "/v1/users/{id}", tag = "users", params(("id" = Uuid, Path)),
    responses((status = 200, body = User), (status = 404, body = ErrorBody))
)]
pub async fn get(State(state): State<AppState>, principal: Principal, Path(id): Path<Uuid>) -> ApiResult<Json<User>> {
    principal.require_admin()?;
    Ok(Json(load(&state, id).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateUser {
    /// An empty string clears it.
    pub name: Option<String>,
    pub role: Option<Role>,
}

/// Update a user.
#[utoipa::path(
    patch, operation_id = "updateUser", path = "/v1/users/{id}", tag = "users", params(("id" = Uuid, Path)), request_body = UpdateUser,
    responses((status = 200, body = User), (status = 404, body = ErrorBody))
)]
pub async fn update(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateUser>,
) -> ApiResult<Json<User>> {
    let admin = principal.require_admin()?;
    if admin.id == id && body.role.is_some_and(|r| r != Role::Admin) {
        return Err(ApiError::bad_request("you cannot remove your own admin role"));
    }
    let current = load(&state, id).await?;
    let mut tx = state.db.begin().await?;
    let user: User = sqlx::query_as(
        "UPDATE users SET name = $2, role = $3, updated_at = now() WHERE id = $1
         RETURNING id, email, name, role, created_at, updated_at",
    )
    .bind(id)
    .bind(body.name.map_or_else(|| current.name.clone(), |n| non_blank(Some(n))))
    .bind(body.role.unwrap_or(current.role))
    .fetch_one(&mut *tx)
    .await?;
    let diff = Diff::default().field("name", &current.name, &user.name).field("role", &current.role, &user.role);
    if !diff.is_empty() {
        Event::new("user.updated", "user", id).changes(diff.into_value()).record(&mut *tx, (&principal).into()).await?;
    }
    tx.commit().await?;
    Ok(Json(user))
}

/// Delete a user.
///
/// Removes the account and its keys; the action log keeps what they did. Users who reported issues cannot be
/// deleted (their issues name them); admins cannot delete themselves.
#[utoipa::path(
    delete, operation_id = "deleteUser", path = "/v1/users/{id}", tag = "users", params(("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody, description = "Has issues"))
)]
pub async fn delete(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let admin = principal.require_admin()?;
    if admin.id == id {
        return Err(ApiError::bad_request("you cannot delete yourself"));
    }
    let user = load(&state, id).await?;
    let issues: i64 =
        sqlx::query_scalar("SELECT count(*) FROM issues WHERE reporter_id = $1").bind(id).fetch_one(&state.db).await?;
    if issues > 0 {
        return Err(ApiError::conflict(format!(
            "{} reported {issues} issue(s); revoke their keys instead",
            user.email
        )));
    }
    let mut tx = state.db.begin().await?;
    sqlx::query("DELETE FROM users WHERE id = $1").bind(id).execute(&mut *tx).await?;
    Event::new("user.deleted", "user", id)
        .changes(serde_json::json!({ "email": user.email, "name": user.name, "role": user.role }))
        .record(&mut *tx, (&principal).into())
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// List a user's keys.
#[utoipa::path(
    get, operation_id = "listUserKeys", path = "/v1/users/{id}/keys", tag = "users", params(("id" = Uuid, Path)),
    responses((status = 200, body = Vec<ApiKey>), (status = 404, body = ErrorBody))
)]
pub async fn list_keys(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<ApiKey>>> {
    principal.require_admin()?;
    load(&state, id).await?;
    Ok(Json(auth::list_keys(&state.db, KeyOwner::User(id)).await?))
}

/// Create a key for a user.
///
/// The key is returned, not e-mailed.
#[utoipa::path(
    post, operation_id = "createUserKey", path = "/v1/users/{id}/keys", tag = "users", params(("id" = Uuid, Path)), request_body = CreateKey,
    responses((status = 201, body = NewApiKey), (status = 404, body = ErrorBody))
)]
pub async fn create_key(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateKey>,
) -> ApiResult<(StatusCode, Json<NewApiKey>)> {
    principal.require_admin()?;
    load(&state, id).await?;
    let key = auth::create_key(
        &mut *state.db.acquire().await?,
        KeyOwner::User(id),
        non_blank(body.name).as_deref(),
        (&principal).into(),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(key)))
}
