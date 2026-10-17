use axum::{extract::State, http::StatusCode};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::projects;
use crate::{
    AppState,
    auth::Principal,
    error::{ApiError, ApiResult, ErrorBody},
    extract::{Json, Path, Query},
    models::{MilestoneState, double_option, non_blank},
};

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct Milestone {
    pub id: Uuid,
    #[serde(skip)]
    pub project_id: Uuid,
    /// Project key.
    pub project: String,
    pub name: String,
    pub description: Option<String>,
    pub due_on: Option<NaiveDate>,
    pub state: MilestoneState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const SELECT: &str = "SELECT m.id, m.project_id, p.key AS project, m.name, m.description, m.due_on, m.state,
    m.created_at, m.updated_at FROM milestones m JOIN projects p ON p.id = m.project_id";

/// Loads a milestone the caller can reach.
pub async fn load(state: &AppState, principal: &Principal, id: Uuid) -> ApiResult<Milestone> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE {} AND m.id = $2", projects::SCOPE)))
        .bind(principal.app_id())
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("milestone"))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListMilestones {
    pub state: Option<MilestoneState>,
}

/// List a project's milestones.
#[utoipa::path(
    get, operation_id = "listMilestones", path = "/v1/projects/{key}/milestones", tag = "milestones",
    params(("key" = String, Path), ListMilestones),
    responses((status = 200, body = Vec<Milestone>), (status = 404, body = ErrorBody))
)]
pub async fn list(
    State(state): State<AppState>,
    principal: Principal,
    Path(key): Path<String>,
    Query(query): Query<ListMilestones>,
) -> ApiResult<Json<Vec<Milestone>>> {
    let project = projects::load(&state, &principal, &key).await?;
    let milestones = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT} WHERE m.project_id = $1 AND ($2::milestone_state IS NULL OR m.state = $2)
         ORDER BY m.due_on NULLS LAST, m.name"
    )))
    .bind(project.id)
    .bind(query.state)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(milestones))
}

#[derive(Deserialize, ToSchema)]
pub struct CreateMilestone {
    #[schema(example = "v1.0")]
    pub name: String,
    pub description: Option<String>,
    pub due_on: Option<NaiveDate>,
}

/// Create a milestone.
#[utoipa::path(
    post, operation_id = "createMilestone", path = "/v1/projects/{key}/milestones", tag = "milestones",
    params(("key" = String, Path)), request_body = CreateMilestone,
    responses((status = 201, body = Milestone), (status = 409, body = ErrorBody, description = "Name taken in this project"))
)]
pub async fn create(
    State(state): State<AppState>,
    principal: Principal,
    Path(key): Path<String>,
    Json(body): Json<CreateMilestone>,
) -> ApiResult<(StatusCode, Json<Milestone>)> {
    principal.require_admin()?;
    let project = projects::load(&state, &principal, &key).await?;
    let name = non_blank(Some(body.name)).ok_or_else(|| ApiError::bad_request("name is required"))?;
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO milestones (id, project_id, name, description, due_on) VALUES ($1, $2, $3, $4, $5)")
        .bind(id)
        .bind(project.id)
        .bind(name)
        .bind(non_blank(body.description))
        .bind(body.due_on)
        .execute(&state.db)
        .await?;
    Ok((StatusCode::CREATED, Json(load(&state, &principal, id).await?)))
}

/// Get a milestone.
#[utoipa::path(
    get, operation_id = "getMilestone", path = "/v1/milestones/{id}", tag = "milestones", params(("id" = Uuid, Path)),
    responses((status = 200, body = Milestone), (status = 404, body = ErrorBody))
)]
pub async fn get(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Milestone>> {
    Ok(Json(load(&state, &principal, id).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateMilestone {
    pub name: Option<String>,
    /// An empty string clears it.
    pub description: Option<String>,
    /// `null` clears it.
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<NaiveDate>)]
    pub due_on: Option<Option<NaiveDate>>,
    pub state: Option<MilestoneState>,
}

/// Update a milestone.
#[utoipa::path(
    patch, operation_id = "updateMilestone", path = "/v1/milestones/{id}", tag = "milestones", params(("id" = Uuid, Path)), request_body = UpdateMilestone,
    responses((status = 200, body = Milestone), (status = 404, body = ErrorBody))
)]
pub async fn update(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateMilestone>,
) -> ApiResult<Json<Milestone>> {
    principal.require_admin()?;
    let current = load(&state, &principal, id).await?;
    let name = match body.name {
        Some(n) => non_blank(Some(n)).ok_or_else(|| ApiError::bad_request("name cannot be blank"))?,
        None => current.name,
    };
    sqlx::query(
        "UPDATE milestones SET name = $2, description = $3, due_on = $4, state = $5, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(name)
    .bind(body.description.map_or(current.description, |d| non_blank(Some(d))))
    .bind(body.due_on.unwrap_or(current.due_on))
    .bind(body.state.unwrap_or(current.state))
    .execute(&state.db)
    .await?;
    Ok(Json(load(&state, &principal, id).await?))
}

/// Delete a milestone.
///
/// Its issues stay, without a milestone.
#[utoipa::path(
    delete, operation_id = "deleteMilestone", path = "/v1/milestones/{id}", tag = "milestones", params(("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody))
)]
pub async fn delete(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    principal.require_admin()?;
    let deleted =
        sqlx::query("DELETE FROM milestones WHERE id = $1").bind(id).execute(&state.db).await?.rows_affected();
    if deleted == 0 {
        return Err(ApiError::not_found("milestone"));
    }
    Ok(StatusCode::NO_CONTENT)
}
