use axum::{extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    AppState,
    auth::Principal,
    error::{ApiError, ApiResult, ErrorBody},
    extract::{Json, Path},
    models::non_blank,
};

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct Project {
    pub id: Uuid,
    /// Short uppercase key, the prefix of issue keys (`DM` in `DM-12`).
    pub key: String,
    pub name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const SELECT: &str = "SELECT id, key, name, description, created_at, updated_at FROM projects p";
/// Restricts to the projects an app can reach; `$1` is the app's id, or null for users (who see all projects).
pub const SCOPE: &str =
    "($1::uuid IS NULL OR EXISTS (SELECT 1 FROM app_projects ap WHERE ap.project_id = p.id AND ap.app_id = $1))";

/// Loads a project by key, as the caller may see it: apps only reach their own projects.
pub async fn load(state: &AppState, principal: &Principal, key: &str) -> ApiResult<Project> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE {SCOPE} AND p.key = $2")))
        .bind(principal.app_id())
        .bind(key.to_uppercase())
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("project"))
}

/// List projects.
#[utoipa::path(get, operation_id = "listProjects", path = "/v1/projects", tag = "projects", responses((status = 200, body = Vec<Project>)))]
pub async fn list(State(state): State<AppState>, principal: Principal) -> ApiResult<Json<Vec<Project>>> {
    let projects = sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE {SCOPE} ORDER BY p.key")))
        .bind(principal.app_id())
        .fetch_all(&state.db)
        .await?;
    Ok(Json(projects))
}

#[derive(Deserialize, ToSchema)]
pub struct CreateProject {
    /// 2-10 characters, uppercase letters and digits, starting with a letter. Cannot be changed later.
    #[schema(example = "DM")]
    pub key: String,
    #[schema(example = "Dobre Mleko")]
    pub name: String,
    pub description: Option<String>,
}

/// Create a project.
#[utoipa::path(
    post, operation_id = "createProject", path = "/v1/projects", tag = "projects", request_body = CreateProject,
    responses((status = 201, body = Project), (status = 409, body = ErrorBody, description = "Key taken"))
)]
pub async fn create(
    State(state): State<AppState>,
    principal: Principal,
    Json(body): Json<CreateProject>,
) -> ApiResult<(StatusCode, Json<Project>)> {
    principal.require_admin()?;
    let key = body.key.trim().to_uppercase();
    let valid_key = (2..=10).contains(&key.len())
        && key.starts_with(|c: char| c.is_ascii_uppercase())
        && key.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    if !valid_key {
        return Err(ApiError::bad_request("key must be 2-10 uppercase letters or digits, starting with a letter"));
    }
    let name = non_blank(Some(body.name)).ok_or_else(|| ApiError::bad_request("name is required"))?;
    let project = sqlx::query_as(
        "INSERT INTO projects (id, key, name, description) VALUES ($1, $2, $3, $4)
         RETURNING id, key, name, description, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(key)
    .bind(name)
    .bind(non_blank(body.description))
    .fetch_one(&state.db)
    .await?;
    Ok((StatusCode::CREATED, Json(project)))
}

/// Get a project.
#[utoipa::path(
    get, operation_id = "getProject", path = "/v1/projects/{key}", tag = "projects", params(("key" = String, Path, example = "DM")),
    responses((status = 200, body = Project), (status = 404, body = ErrorBody))
)]
pub async fn get(
    State(state): State<AppState>,
    principal: Principal,
    Path(key): Path<String>,
) -> ApiResult<Json<Project>> {
    Ok(Json(load(&state, &principal, &key).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateProject {
    pub name: Option<String>,
    /// An empty string clears it.
    pub description: Option<String>,
}

/// Update a project.
#[utoipa::path(
    patch, operation_id = "updateProject", path = "/v1/projects/{key}", tag = "projects", params(("key" = String, Path)), request_body = UpdateProject,
    responses((status = 200, body = Project), (status = 404, body = ErrorBody))
)]
pub async fn update(
    State(state): State<AppState>,
    principal: Principal,
    Path(key): Path<String>,
    Json(body): Json<UpdateProject>,
) -> ApiResult<Json<Project>> {
    principal.require_admin()?;
    let current = load(&state, &principal, &key).await?;
    let name = match body.name {
        Some(n) => non_blank(Some(n)).ok_or_else(|| ApiError::bad_request("name cannot be blank"))?,
        None => current.name,
    };
    let description = match body.description {
        Some(d) => non_blank(Some(d)),
        None => current.description,
    };
    let project = sqlx::query_as(
        "UPDATE projects SET name = $2, description = $3, updated_at = now() WHERE id = $1
         RETURNING id, key, name, description, created_at, updated_at",
    )
    .bind(current.id)
    .bind(name)
    .bind(description)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(project))
}
