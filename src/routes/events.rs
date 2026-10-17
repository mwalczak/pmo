use axum::extract::State;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, QueryBuilder};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::{Page, Paging, issues, projects};
use crate::{
    AppState,
    auth::Principal,
    error::{ApiResult, ErrorBody},
    extract::{Json, Path, Query},
};

/// One entry of the action log: who did what, when, and what changed.
#[derive(Serialize, ToSchema)]
pub struct EventEntry {
    pub id: Uuid,
    pub at: DateTime<Utc>,
    /// What happened, e.g. `issue.created`, `issue.updated`, `attachment.added`, `project.updated`, `key.revoked`.
    #[schema(example = "issue.updated")]
    pub action: String,
    /// `issue`, `attachment`, `project`, `milestone`, `user`, `app` or `key`.
    pub entity: String,
    pub entity_id: Uuid,
    /// Project key.
    pub project: Option<String>,
    /// Issue key, for events about an issue or its attachments.
    pub issue: Option<String>,
    pub actor: EventActor,
    /// For updates `{"field": {"from": old, "to": new}}`; for creations the initial values.
    #[schema(value_type = Object, example = json!({"status": {"from": "reported", "to": "accepted"}}))]
    pub changes: serde_json::Value,
}

#[derive(Serialize, ToSchema)]
pub struct EventActor {
    /// `user`, `app` or `system` (startup and command-line tasks).
    pub kind: &'static str,
    pub id: Option<Uuid>,
    pub name: Option<String>,
    /// Shown to admins only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

#[derive(sqlx::FromRow)]
struct EventRow {
    id: Uuid,
    at: DateTime<Utc>,
    action: String,
    entity: String,
    entity_id: Uuid,
    project: Option<String>,
    issue_key: Option<String>,
    actor_user_id: Option<Uuid>,
    actor_app_id: Option<Uuid>,
    user_name: Option<String>,
    user_email: Option<String>,
    app_name: Option<String>,
    changes: serde_json::Value,
}

impl EventRow {
    fn into_entry(self, admin: bool) -> EventEntry {
        let actor = match (self.actor_user_id, self.actor_app_id) {
            (Some(id), _) => EventActor {
                kind: "user",
                id: Some(id),
                name: self.user_name,
                email: self.user_email.filter(|_| admin),
            },
            (None, Some(id)) => EventActor { kind: "app", id: Some(id), name: self.app_name, email: None },
            (None, None) => EventActor { kind: "system", id: None, name: None, email: None },
        };
        EventEntry {
            id: self.id,
            at: self.at,
            action: self.action,
            entity: self.entity,
            entity_id: self.entity_id,
            project: self.project,
            issue: self.issue_key,
            actor,
            changes: self.changes,
        }
    }
}

const SELECT: &str = "SELECT e.id, e.at, e.action, e.entity, e.entity_id, p.key AS project, e.issue_key,
    e.actor_user_id, e.actor_app_id, u.name AS user_name, u.email AS user_email, a.name AS app_name, e.changes
    FROM events e
    LEFT JOIN projects p ON p.id = e.project_id
    LEFT JOIN users u ON u.id = e.actor_user_id
    LEFT JOIN apps a ON a.id = e.actor_app_id";

/// An issue's history.
///
/// Everything that happened to the issue and its attachments, oldest first. Anyone who can see the issue can read it.
#[utoipa::path(
    get, operation_id = "listIssueEvents", path = "/v1/issues/{key}/events", tag = "events",
    params(("key" = String, Path, example = "DM-12")),
    responses((status = 200, body = Vec<EventEntry>), (status = 404, body = ErrorBody))
)]
pub async fn for_issue(
    State(state): State<AppState>,
    principal: Principal,
    Path(key): Path<String>,
) -> ApiResult<Json<Vec<EventEntry>>> {
    let issue = issues::load(&state, &principal, &key).await?;
    let rows: Vec<EventRow> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE e.issue_id = $1 ORDER BY e.at, e.id")))
            .bind(issue.id)
            .fetch_all(&state.db)
            .await?;
    let admin = principal.is_admin();
    Ok(Json(rows.into_iter().map(|r| r.into_entry(admin)).collect()))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListEvents {
    /// Project key.
    pub project: Option<String>,
    /// Issue key, e.g. `DM-12`.
    pub issue: Option<String>,
    /// Exact action, e.g. `issue.updated`.
    pub action: Option<String>,
    pub entity: Option<String>,
    pub entity_id: Option<Uuid>,
    /// Events by this user.
    pub user_id: Option<Uuid>,
    /// Events by this app.
    pub app_id: Option<Uuid>,
    /// From this time on (inclusive).
    pub since: Option<DateTime<Utc>>,
    /// Before this time.
    pub until: Option<DateTime<Utc>>,
    /// 1 to 200, default 50.
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// The action log.
///
/// Who did what and when, across everything, newest first. Admin only.
#[utoipa::path(
    get, operation_id = "listEvents", path = "/v1/events", tag = "events", params(ListEvents),
    responses((status = 200, body = Page<EventEntry>), (status = 403, body = ErrorBody))
)]
pub async fn list(
    State(state): State<AppState>,
    principal: Principal,
    Query(query): Query<ListEvents>,
) -> ApiResult<Json<Page<EventEntry>>> {
    principal.require_admin()?;
    let (limit, offset) = Paging { limit: query.limit, offset: query.offset }.resolve();
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(SELECT);
    qb.push(" WHERE true");
    if let Some(key) = &query.project {
        let project = projects::load(&state, &principal, key).await?;
        qb.push(" AND e.project_id = ").push_bind(project.id);
    }
    if let Some(issue) = &query.issue {
        qb.push(" AND e.issue_key = ").push_bind(issue.trim().to_uppercase());
    }
    if let Some(action) = query.action {
        qb.push(" AND e.action = ").push_bind(action);
    }
    if let Some(entity) = query.entity {
        qb.push(" AND e.entity = ").push_bind(entity);
    }
    if let Some(id) = query.entity_id {
        qb.push(" AND e.entity_id = ").push_bind(id);
    }
    if let Some(id) = query.user_id {
        qb.push(" AND e.actor_user_id = ").push_bind(id);
    }
    if let Some(id) = query.app_id {
        qb.push(" AND e.actor_app_id = ").push_bind(id);
    }
    if let Some(since) = query.since {
        qb.push(" AND e.at >= ").push_bind(since);
    }
    if let Some(until) = query.until {
        qb.push(" AND e.at < ").push_bind(until);
    }
    qb.push(" ORDER BY e.at DESC, e.id DESC LIMIT ").push_bind(limit + 1).push(" OFFSET ").push_bind(offset);
    let rows: Vec<EventRow> = qb.build_query_as().fetch_all(&state.db).await?;
    let entries = rows.into_iter().map(|r| r.into_entry(true)).collect();
    Ok(Json(Page::from_rows(entries, limit, offset)))
}
