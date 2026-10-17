use axum::{extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    AppState,
    auth::{self, ApiKey, KeyOwner, NewApiKey, Principal},
    error::{ApiError, ApiResult, ErrorBody},
    events::Event,
    extract::{Json, Path},
    models::{Role, non_blank},
};

/// The caller: a user or an app.
#[derive(Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Me {
    User {
        id: Uuid,
        email: String,
        name: Option<String>,
        role: Role,
    },
    App {
        id: Uuid,
        name: String,
        /// Keys of the projects this app can reach.
        projects: Vec<String>,
    },
}

/// Who am I.
#[utoipa::path(get, operation_id = "getMe", path = "/v1/me", tag = "me", responses((status = 200, body = Me)))]
pub async fn me(State(state): State<AppState>, principal: Principal) -> ApiResult<Json<Me>> {
    Ok(Json(match principal {
        Principal::User(u) => Me::User { id: u.id, email: u.email, name: u.name, role: u.role },
        Principal::App(a) => {
            let projects = sqlx::query_scalar(
                "SELECT p.key FROM app_projects ap JOIN projects p ON p.id = ap.project_id
                 WHERE ap.app_id = $1 ORDER BY p.key",
            )
            .bind(a.id)
            .fetch_all(&state.db)
            .await?;
            Me::App { id: a.id, name: a.name, projects }
        }
    }))
}

/// List my keys.
///
/// The caller's keys, including revoked ones.
#[utoipa::path(get, operation_id = "listMyKeys", path = "/v1/me/keys", tag = "me", responses((status = 200, body = Vec<ApiKey>)))]
pub async fn list_keys(State(state): State<AppState>, principal: Principal) -> ApiResult<Json<Vec<ApiKey>>> {
    Ok(Json(auth::list_keys(&state.db, principal.key_owner()).await?))
}

#[derive(Deserialize, ToSchema, Default)]
pub struct CreateKey {
    /// What the key is for, e.g. "claude agent".
    pub name: Option<String>,
}

/// Create a key for myself.
///
/// For example one per agent, or to rotate a key before revoking the old one.
#[utoipa::path(post, operation_id = "createMyKey", path = "/v1/me/keys", tag = "me", request_body = CreateKey, responses((status = 201, body = NewApiKey)))]
pub async fn create_key(
    State(state): State<AppState>,
    principal: Principal,
    Json(body): Json<CreateKey>,
) -> ApiResult<(StatusCode, Json<NewApiKey>)> {
    let key = auth::create_key(
        &mut *state.db.acquire().await?,
        principal.key_owner(),
        non_blank(body.name).as_deref(),
        (&principal).into(),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(key)))
}

/// Revoke a key.
///
/// Users and apps can revoke their own keys; admins any key.
#[utoipa::path(
    delete, operation_id = "revokeKey", path = "/v1/keys/{id}", tag = "me",
    params(("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody))
)]
pub async fn revoke_key(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let (user_id, app_id) = match principal.key_owner() {
        KeyOwner::User(id) => (Some(id), None),
        KeyOwner::App(id) => (None, Some(id)),
    };
    let mut tx = state.db.begin().await?;
    // `was_active` tells a fresh revocation from repeating one, which is not logged again.
    let revoked: Option<(String, Option<Uuid>, Option<Uuid>, bool)> = sqlx::query_as(
        "UPDATE api_keys k SET revoked_at = coalesce(k.revoked_at, now())
         FROM (SELECT id, revoked_at IS NULL AS was_active FROM api_keys WHERE id = $1) old
         WHERE k.id = old.id AND ($2 OR k.user_id IS NOT DISTINCT FROM $3 AND k.app_id IS NOT DISTINCT FROM $4)
         RETURNING k.prefix, k.user_id, k.app_id, old.was_active",
    )
    .bind(id)
    .bind(principal.is_admin())
    .bind(user_id)
    .bind(app_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((prefix, owner_user, owner_app, was_active)) = revoked else {
        return Err(ApiError::not_found("key"));
    };
    if was_active {
        Event::new("key.revoked", "key", id)
            .changes(serde_json::json!({ "prefix": prefix, "user_id": owner_user, "app_id": owner_app }))
            .record(&mut *tx, (&principal).into())
            .await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
