use axum::{extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use utoipa::ToSchema;
use uuid::Uuid;

use super::me::CreateKey;
use crate::{
    AppState,
    auth::{self, ApiKey, KeyOwner, NewApiKey, Principal},
    error::{ApiError, ApiResult, ErrorBody},
    extract::{Json, Path},
    models::non_blank,
};

/// A client app calling the API server-to-server with its own keys.
#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct App {
    pub id: Uuid,
    #[schema(example = "dobre-mleko.app")]
    pub name: String,
    /// Keys of the projects the app can read and report issues to.
    pub projects: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const SELECT: &str = "SELECT a.id, a.name, a.created_at, a.updated_at,
    coalesce(array_agg(p.key ORDER BY p.key) FILTER (WHERE p.key IS NOT NULL), '{}') AS projects
    FROM apps a LEFT JOIN app_projects ap ON ap.app_id = a.id LEFT JOIN projects p ON p.id = ap.project_id";

async fn load(db: &mut PgConnection, id: Uuid) -> ApiResult<App> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE a.id = $1 GROUP BY a.id")))
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| ApiError::not_found("app"))
}

/// Replaces the app's projects.
async fn set_projects(db: &mut PgConnection, app_id: Uuid, keys: &[String]) -> ApiResult<()> {
    let keys: Vec<String> = keys.iter().map(|k| k.trim().to_uppercase()).collect();
    let ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM projects WHERE key = ANY($1)").bind(&keys).fetch_all(&mut *db).await?;
    let mut unique = keys.clone();
    unique.sort();
    unique.dedup();
    if ids.len() != unique.len() {
        return Err(ApiError::bad_request("unknown project key"));
    }
    sqlx::query("DELETE FROM app_projects WHERE app_id = $1").bind(app_id).execute(&mut *db).await?;
    sqlx::query("INSERT INTO app_projects (app_id, project_id) SELECT $1, unnest($2::uuid[])")
        .bind(app_id)
        .bind(&ids)
        .execute(&mut *db)
        .await?;
    Ok(())
}

/// List apps.
#[utoipa::path(get, operation_id = "listApps", path = "/v1/apps", tag = "apps", responses((status = 200, body = Vec<App>)))]
pub async fn list(State(state): State<AppState>, principal: Principal) -> ApiResult<Json<Vec<App>>> {
    principal.require_admin()?;
    let apps = sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} GROUP BY a.id ORDER BY a.name")))
        .fetch_all(&state.db)
        .await?;
    Ok(Json(apps))
}

#[derive(Deserialize, ToSchema)]
pub struct CreateApp {
    #[schema(example = "dobre-mleko.app")]
    pub name: String,
    /// Project keys the app may use.
    #[schema(example = json!(["DM"]))]
    pub projects: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub struct CreatedApp {
    pub app: App,
    /// The app's first key. Store it in the app's server-side configuration: it cannot be retrieved later.
    pub key: NewApiKey,
}

/// Create an app.
#[utoipa::path(
    post, operation_id = "createApp", path = "/v1/apps", tag = "apps", request_body = CreateApp,
    responses((status = 201, body = CreatedApp), (status = 409, body = ErrorBody, description = "Name taken"))
)]
pub async fn create(
    State(state): State<AppState>,
    principal: Principal,
    Json(body): Json<CreateApp>,
) -> ApiResult<(StatusCode, Json<CreatedApp>)> {
    principal.require_admin()?;
    let name = non_blank(Some(body.name)).ok_or_else(|| ApiError::bad_request("name is required"))?;
    let id = Uuid::now_v7();
    let mut tx = state.db.begin().await?;
    sqlx::query("INSERT INTO apps (id, name) VALUES ($1, $2)").bind(id).bind(name).execute(&mut *tx).await?;
    set_projects(&mut tx, id, &body.projects).await?;
    let key = auth::create_key(&mut *tx, KeyOwner::App(id), None).await?;
    let app = load(&mut tx, id).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(CreatedApp { app, key })))
}

/// Get an app.
#[utoipa::path(
    get, operation_id = "getApp", path = "/v1/apps/{id}", tag = "apps", params(("id" = Uuid, Path)),
    responses((status = 200, body = App), (status = 404, body = ErrorBody))
)]
pub async fn get(State(state): State<AppState>, principal: Principal, Path(id): Path<Uuid>) -> ApiResult<Json<App>> {
    principal.require_admin()?;
    Ok(Json(load(&mut *state.db.acquire().await?, id).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateApp {
    pub name: Option<String>,
    /// Replaces the app's project keys.
    pub projects: Option<Vec<String>>,
}

/// Update an app.
#[utoipa::path(
    patch, operation_id = "updateApp", path = "/v1/apps/{id}", tag = "apps", params(("id" = Uuid, Path)), request_body = UpdateApp,
    responses((status = 200, body = App), (status = 404, body = ErrorBody))
)]
pub async fn update(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateApp>,
) -> ApiResult<Json<App>> {
    principal.require_admin()?;
    let mut tx = state.db.begin().await?;
    load(&mut tx, id).await?;
    if let Some(name) = body.name {
        let name = non_blank(Some(name)).ok_or_else(|| ApiError::bad_request("name cannot be blank"))?;
        sqlx::query("UPDATE apps SET name = $2 WHERE id = $1").bind(id).bind(name).execute(&mut *tx).await?;
    }
    if let Some(projects) = body.projects {
        set_projects(&mut tx, id, &projects).await?;
    }
    sqlx::query("UPDATE apps SET updated_at = now() WHERE id = $1").bind(id).execute(&mut *tx).await?;
    let app = load(&mut tx, id).await?;
    tx.commit().await?;
    Ok(Json(app))
}

/// Delete an app.
///
/// Its keys go with it; its issues stay.
#[utoipa::path(
    delete, operation_id = "deleteApp", path = "/v1/apps/{id}", tag = "apps", params(("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody))
)]
pub async fn delete(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    principal.require_admin()?;
    let deleted = sqlx::query("DELETE FROM apps WHERE id = $1").bind(id).execute(&state.db).await?.rows_affected();
    if deleted == 0 {
        return Err(ApiError::not_found("app"));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// List an app's keys.
#[utoipa::path(
    get, operation_id = "listAppKeys", path = "/v1/apps/{id}/keys", tag = "apps", params(("id" = Uuid, Path)),
    responses((status = 200, body = Vec<ApiKey>), (status = 404, body = ErrorBody))
)]
pub async fn list_keys(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<ApiKey>>> {
    principal.require_admin()?;
    load(&mut *state.db.acquire().await?, id).await?;
    Ok(Json(auth::list_keys(&state.db, KeyOwner::App(id)).await?))
}

/// Create a key for an app.
///
/// To rotate keys: create one, deploy it to the app, then revoke the old one.
#[utoipa::path(
    post, operation_id = "createAppKey", path = "/v1/apps/{id}/keys", tag = "apps", params(("id" = Uuid, Path)), request_body = CreateKey,
    responses((status = 201, body = NewApiKey), (status = 404, body = ErrorBody))
)]
pub async fn create_key(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateKey>,
) -> ApiResult<(StatusCode, Json<NewApiKey>)> {
    principal.require_admin()?;
    load(&mut *state.db.acquire().await?, id).await?;
    let key = auth::create_key(&state.db, KeyOwner::App(id), non_blank(body.name).as_deref()).await?;
    Ok((StatusCode::CREATED, Json(key)))
}
