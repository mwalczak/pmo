use axum::{extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    AppState,
    auth::{self, ApiKey, KeyOwner, NewApiKey, Principal},
    error::{ApiError, ApiResult, ErrorBody},
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
    let key = auth::create_key(&state.db, principal.key_owner(), non_blank(body.name).as_deref()).await?;
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
    let revoked = sqlx::query(
        "UPDATE api_keys SET revoked_at = coalesce(revoked_at, now())
         WHERE id = $1 AND ($2 OR user_id IS NOT DISTINCT FROM $3 AND app_id IS NOT DISTINCT FROM $4)",
    )
    .bind(id)
    .bind(principal.is_admin())
    .bind(user_id)
    .bind(app_id)
    .execute(&state.db)
    .await?
    .rows_affected();
    if revoked == 0 {
        return Err(ApiError::not_found("key"));
    }
    Ok(StatusCode::NO_CONTENT)
}
