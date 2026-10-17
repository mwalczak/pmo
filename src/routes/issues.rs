use axum::{extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgExecutor, Postgres, QueryBuilder};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::{Page, Paging, milestones, projects, users};
use crate::{
    AppState,
    auth::Principal,
    error::{ApiError, ApiResult, ErrorBody},
    extract::{Json, Path, Query},
    mail::{Mail, welcome_mail},
    models::{IssueStatus, IssueType, Priority, double_option, non_blank},
};

const MAX_TITLE: usize = 200;
const MAX_DESCRIPTION: usize = 20_000;

#[derive(Serialize, ToSchema)]
pub struct Issue {
    pub id: Uuid,
    /// Project key and number; use it in `/v1/issues/{key}`.
    #[schema(example = "DM-12")]
    pub key: String,
    /// Project key.
    pub project: String,
    pub number: i32,
    #[serde(rename = "type")]
    pub issue_type: IssueType,
    pub priority: Priority,
    pub status: IssueStatus,
    pub title: String,
    pub description: Option<String>,
    /// Private issues are visible only to admins and their reporter.
    pub private: bool,
    pub reporter: Reporter,
    pub milestone: Option<MilestoneRef>,
    /// Key of the issue this one duplicates, when `status` is `duplicate`.
    pub duplicate_of: Option<String>,
    /// Name of the app the issue was reported through.
    pub source_app: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
pub struct Reporter {
    pub id: Uuid,
    pub name: Option<String>,
    /// Shown only to admins and the reporter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct MilestoneRef {
    pub id: Uuid,
    pub name: String,
}

#[derive(sqlx::FromRow)]
struct IssueRow {
    id: Uuid,
    project_id: Uuid,
    project: String,
    number: i32,
    issue_type: IssueType,
    priority: Priority,
    status: IssueStatus,
    title: String,
    description: Option<String>,
    private: bool,
    reporter_id: Uuid,
    reporter_name: Option<String>,
    reporter_email: String,
    milestone_id: Option<Uuid>,
    milestone_name: Option<String>,
    duplicate_of_id: Option<Uuid>,
    duplicate_of: Option<String>,
    source_app: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl IssueRow {
    fn into_issue(self, principal: &Principal) -> Issue {
        let sees_email = principal.is_admin() || principal.user().is_some_and(|u| u.id == self.reporter_id);
        Issue {
            id: self.id,
            key: format!("{}-{}", self.project, self.number),
            project: self.project,
            number: self.number,
            issue_type: self.issue_type,
            priority: self.priority,
            status: self.status,
            title: self.title,
            description: self.description,
            private: self.private,
            reporter: Reporter {
                id: self.reporter_id,
                name: self.reporter_name,
                email: sees_email.then_some(self.reporter_email),
            },
            milestone: self.milestone_id.zip(self.milestone_name).map(|(id, name)| MilestoneRef { id, name }),
            duplicate_of: self.duplicate_of,
            source_app: self.source_app,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

const SELECT: &str = "SELECT i.id, i.project_id, p.key AS project, i.number, i.type AS issue_type, i.priority,
    i.status, i.title, i.description, i.private, i.reporter_id, u.name AS reporter_name, u.email AS reporter_email,
    i.milestone_id, m.name AS milestone_name, i.duplicate_of_id, dp.key || '-' || d.number AS duplicate_of,
    a.name AS source_app, i.created_at, i.updated_at
    FROM issues i
    JOIN projects p ON p.id = i.project_id
    JOIN users u ON u.id = i.reporter_id
    LEFT JOIN milestones m ON m.id = i.milestone_id
    LEFT JOIN issues d ON d.id = i.duplicate_of_id
    LEFT JOIN projects dp ON dp.id = d.project_id
    LEFT JOIN apps a ON a.id = i.source_app_id";

/// Hides private issues from everyone but admins and their reporter.
fn push_visibility(qb: &mut QueryBuilder<Postgres>, principal: &Principal) {
    match principal {
        _ if principal.is_admin() => {}
        Principal::User(u) => {
            qb.push(" AND (NOT i.private OR i.reporter_id = ").push_bind(u.id).push(")");
        }
        Principal::App(_) => {
            qb.push(" AND NOT i.private");
        }
    }
}

/// Splits `DM-12` into `("DM", 12)`.
fn parse_key(key: &str) -> ApiResult<(&str, i32)> {
    key.rsplit_once('-')
        .and_then(|(project, number)| Some((project, number.parse().ok()?)))
        .ok_or_else(|| ApiError::bad_request(format!("invalid issue key {key:?}, expected e.g. DM-12")))
}

async fn fetch_by_id(db: impl PgExecutor<'_>, id: Uuid) -> sqlx::Result<IssueRow> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE i.id = $1"))).bind(id).fetch_one(db).await
}

/// Loads an issue by key, as the caller may see it.
async fn load(state: &AppState, principal: &Principal, key: &str) -> ApiResult<IssueRow> {
    let (project_key, number) = parse_key(key)?;
    let project = projects::load(state, principal, project_key).await?;
    let mut qb = QueryBuilder::new(SELECT);
    qb.push(" WHERE i.project_id = ").push_bind(project.id).push(" AND i.number = ").push_bind(number);
    push_visibility(&mut qb, principal);
    qb.build_query_as().fetch_optional(&state.db).await?.ok_or_else(|| ApiError::not_found("issue"))
}

#[derive(Deserialize, ToSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum IssueSort {
    Newest,
    Oldest,
    /// P1 first, then newest.
    Priority,
    /// Most recently updated first.
    Updated,
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListIssues {
    /// Comma-separated statuses, or `open` (reported, accepted, in_progress) or `closed` (done, wont_fix, duplicate).
    #[param(example = "open")]
    pub status: Option<String>,
    #[serde(rename = "type")]
    #[param(rename = "type")]
    pub issue_type: Option<IssueType>,
    /// Comma-separated priorities, e.g. `P1,P2`.
    pub priority: Option<String>,
    pub milestone_id: Option<Uuid>,
    pub reporter_id: Option<Uuid>,
    /// Search in titles and descriptions, tolerant of typos. Results are sorted by relevance unless `sort` is given.
    /// Use it to look for an existing report before filing a new one.
    pub q: Option<String>,
    /// Default `newest`.
    #[param(inline)]
    pub sort: Option<IssueSort>,
    /// 1 to 200, default 50.
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

fn parse_statuses(s: &str) -> ApiResult<Vec<&'static str>> {
    let mut out = Vec::new();
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let statuses: &[IssueStatus] = match part {
            "open" => &IssueStatus::OPEN,
            "closed" => &IssueStatus::CLOSED,
            _ => {
                let status = IssueStatus::ALL
                    .iter()
                    .find(|st| st.as_str() == part)
                    .ok_or_else(|| ApiError::bad_request(format!("unknown status {part:?}")))?;
                std::slice::from_ref(status)
            }
        };
        out.extend(statuses.iter().map(|st| st.as_str()));
    }
    Ok(out)
}

fn parse_priorities(s: &str) -> ApiResult<Vec<String>> {
    s.split(',')
        .map(|p| p.trim().to_uppercase())
        .filter(|p| !p.is_empty())
        .map(|p| match p.as_str() {
            "P1" | "P2" | "P3" | "P4" | "P5" => Ok(p),
            _ => Err(ApiError::bad_request(format!("unknown priority {p:?}"))),
        })
        .collect()
}

/// List or search a project's issues.
///
/// Private issues are included only for admins and their reporter.
#[utoipa::path(
    get, operation_id = "listIssues", path = "/v1/projects/{key}/issues", tag = "issues", params(("key" = String, Path), ListIssues),
    responses((status = 200, body = Page<Issue>), (status = 404, body = ErrorBody))
)]
pub async fn list(
    State(state): State<AppState>,
    principal: Principal,
    Path(key): Path<String>,
    Query(query): Query<ListIssues>,
) -> ApiResult<Json<Page<Issue>>> {
    let project = projects::load(&state, &principal, &key).await?;
    let (limit, offset) = Paging { limit: query.limit, offset: query.offset }.resolve();

    let mut qb = QueryBuilder::new(SELECT);
    qb.push(" WHERE i.project_id = ").push_bind(project.id);
    push_visibility(&mut qb, &principal);
    if let Some(status) = &query.status {
        qb.push(" AND i.status::text = ANY(").push_bind(parse_statuses(status)?).push(")");
    }
    if let Some(issue_type) = query.issue_type {
        qb.push(" AND i.type = ").push_bind(issue_type);
    }
    if let Some(priority) = &query.priority {
        qb.push(" AND i.priority::text = ANY(").push_bind(parse_priorities(priority)?).push(")");
    }
    if let Some(milestone_id) = query.milestone_id {
        qb.push(" AND i.milestone_id = ").push_bind(milestone_id);
    }
    if let Some(reporter_id) = query.reporter_id {
        qb.push(" AND i.reporter_id = ").push_bind(reporter_id);
    }
    let q = non_blank(query.q);
    if let Some(q) = &q {
        qb.push(" AND (strpos(lower(i.title), lower(").push_bind(q.clone());
        qb.push(")) > 0 OR strpos(lower(coalesce(i.description, '')), lower(").push_bind(q.clone());
        qb.push(")) > 0 OR word_similarity(").push_bind(q.clone()).push(", i.title) >= 0.3)");
    }
    match (query.sort, &q) {
        (None, Some(q)) => {
            qb.push(" ORDER BY word_similarity(").push_bind(q.clone()).push(", i.title) DESC, i.created_at DESC");
        }
        (None | Some(IssueSort::Newest), _) => {
            qb.push(" ORDER BY i.created_at DESC, i.id DESC");
        }
        (Some(IssueSort::Oldest), _) => {
            qb.push(" ORDER BY i.created_at, i.id");
        }
        (Some(IssueSort::Priority), _) => {
            qb.push(" ORDER BY i.priority, i.created_at DESC, i.id DESC");
        }
        (Some(IssueSort::Updated), _) => {
            qb.push(" ORDER BY i.updated_at DESC, i.id DESC");
        }
    }
    qb.push(" LIMIT ").push_bind(limit + 1).push(" OFFSET ").push_bind(offset);

    let rows: Vec<IssueRow> = qb.build_query_as().fetch_all(&state.db).await?;
    let issues = rows.into_iter().map(|r| r.into_issue(&principal)).collect();
    Ok(Json(Page::from_rows(issues, limit, offset)))
}

#[derive(Deserialize, ToSchema)]
pub struct NewReporter {
    pub email: String,
    pub name: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateIssue {
    #[serde(rename = "type")]
    pub issue_type: IssueType,
    pub title: String,
    pub description: Option<String>,
    /// Admins only. Default `P3`.
    pub priority: Option<Priority>,
    /// Admins only.
    pub milestone_id: Option<Uuid>,
    /// Default `false`; abuse reports are always created private.
    pub private: Option<bool>,
    /// Who reported the issue. Required for apps, optional for admins; users report as themselves.
    /// An e-mail without an account gets one, and a key is e-mailed to it.
    pub reporter: Option<NewReporter>,
}

fn valid_text(title: String, description: Option<String>) -> ApiResult<(String, Option<String>)> {
    let title = non_blank(Some(title)).ok_or_else(|| ApiError::bad_request("title is required"))?;
    if title.chars().count() > MAX_TITLE {
        return Err(ApiError::bad_request(format!("title is longer than {MAX_TITLE} characters")));
    }
    let description = non_blank(description);
    if description.as_ref().is_some_and(|d| d.chars().count() > MAX_DESCRIPTION) {
        return Err(ApiError::bad_request(format!("description is longer than {MAX_DESCRIPTION} characters")));
    }
    Ok((title, description))
}

/// Report an issue.
///
/// Apps report on behalf of their users by passing `reporter`. If that e-mail has no account yet, a reporter
/// account is created and a personal API key is e-mailed to it.
#[utoipa::path(
    post, operation_id = "createIssue", path = "/v1/projects/{key}/issues", tag = "issues", params(("key" = String, Path)), request_body = CreateIssue,
    responses((status = 201, body = Issue), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody))
)]
pub async fn create(
    State(state): State<AppState>,
    principal: Principal,
    Path(key): Path<String>,
    Json(body): Json<CreateIssue>,
) -> ApiResult<(StatusCode, Json<Issue>)> {
    let project = projects::load(&state, &principal, &key).await?;
    let (title, description) = valid_text(body.title, body.description)?;
    if !principal.is_admin() && (body.priority.is_some() || body.milestone_id.is_some()) {
        return Err(ApiError::forbidden("only admins can set priority and milestone"));
    }
    if let Some(milestone_id) = body.milestone_id {
        let milestone = milestones::load(&state, &principal, milestone_id).await?;
        if milestone.project_id != project.id {
            return Err(ApiError::bad_request("milestone belongs to another project"));
        }
    }

    let mut tx = state.db.begin().await?;
    let (reporter_id, new_key) = match (&principal, body.reporter) {
        (Principal::App(_), None) => return Err(ApiError::bad_request("apps must pass reporter.email")),
        (Principal::User(u), None) => (u.id, None),
        (Principal::User(u), Some(r)) if !principal.is_admin() => {
            if !r.email.trim().eq_ignore_ascii_case(&u.email) {
                return Err(ApiError::forbidden("users can only report as themselves"));
            }
            (u.id, None)
        }
        (_, Some(r)) => {
            let (id, key) = users::find_or_create(&mut tx, &r.email, r.name.clone()).await?;
            (id, key.map(|k| (k.key, r.email, non_blank(r.name))))
        }
    };
    let number: i32 =
        sqlx::query_scalar("UPDATE projects SET issue_seq = issue_seq + 1 WHERE id = $1 RETURNING issue_seq")
            .bind(project.id)
            .fetch_one(&mut *tx)
            .await?;
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO issues (id, project_id, number, type, priority, title, description, private, reporter_id,
                             milestone_id, source_app_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(id)
    .bind(project.id)
    .bind(number)
    .bind(body.issue_type)
    .bind(body.priority.unwrap_or(Priority::P3))
    .bind(title)
    .bind(description)
    .bind(body.issue_type == IssueType::Abuse || body.private.unwrap_or(false))
    .bind(reporter_id)
    .bind(body.milestone_id)
    .bind(principal.app_id())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    if let Some((key, email, name)) = new_key {
        let via = match &principal {
            Principal::App(a) => Some(a.name.as_str()),
            Principal::User(_) => None,
        };
        state.mailer.send(welcome_mail(state.public_url.as_deref(), &email, name.as_deref(), &key, via));
    }
    let issue = fetch_by_id(&state.db, id).await?.into_issue(&principal);
    Ok((StatusCode::CREATED, Json(issue)))
}

/// Get an issue.
#[utoipa::path(
    get, operation_id = "getIssue", path = "/v1/issues/{key}", tag = "issues", params(("key" = String, Path, example = "DM-12")),
    responses((status = 200, body = Issue), (status = 404, body = ErrorBody))
)]
pub async fn get(
    State(state): State<AppState>,
    principal: Principal,
    Path(key): Path<String>,
) -> ApiResult<Json<Issue>> {
    Ok(Json(load(&state, &principal, &key).await?.into_issue(&principal)))
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateIssue {
    pub title: Option<String>,
    /// An empty string clears it.
    pub description: Option<String>,
    #[serde(rename = "type")]
    pub issue_type: Option<IssueType>,
    pub priority: Option<Priority>,
    /// Setting `duplicate` requires `duplicate_of`; any other status clears it.
    pub status: Option<IssueStatus>,
    pub private: Option<bool>,
    /// `null` removes the issue from its milestone.
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<Uuid>)]
    pub milestone_id: Option<Option<Uuid>>,
    /// Key of the original issue in the same project. Implies `status: duplicate`.
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<String>, example = "DM-3")]
    pub duplicate_of: Option<Option<String>>,
}

/// Update or triage an issue.
///
/// Admins can change everything; reporters only the title and description of their own issues.
/// The reporter is e-mailed when the status changes.
#[utoipa::path(
    patch, operation_id = "updateIssue", path = "/v1/issues/{key}", tag = "issues", params(("key" = String, Path)), request_body = UpdateIssue,
    responses((status = 200, body = Issue), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody))
)]
pub async fn update(
    State(state): State<AppState>,
    principal: Principal,
    Path(key): Path<String>,
    Json(body): Json<UpdateIssue>,
) -> ApiResult<Json<Issue>> {
    let current = load(&state, &principal, &key).await?;
    let is_reporter = principal.user().is_some_and(|u| u.id == current.reporter_id);
    let triage = body.issue_type.is_some()
        || body.priority.is_some()
        || body.status.is_some()
        || body.private.is_some()
        || body.milestone_id.is_some()
        || body.duplicate_of.is_some();
    if !principal.is_admin() && (!is_reporter || triage) {
        return Err(ApiError::forbidden(
            "only admins can triage issues; reporters can edit the title and description of their own",
        ));
    }

    let (title, description) =
        valid_text(body.title.unwrap_or(current.title), body.description.map_or(current.description, Some))?;
    if let Some(Some(milestone_id)) = body.milestone_id {
        let milestone = milestones::load(&state, &principal, milestone_id).await?;
        if milestone.project_id != current.project_id {
            return Err(ApiError::bad_request("milestone belongs to another project"));
        }
    }
    let duplicate_of_id = match &body.duplicate_of {
        Some(Some(original)) => {
            let original = load(&state, &principal, original).await?;
            if original.project_id != current.project_id || original.id == current.id {
                return Err(ApiError::bad_request("duplicate_of must be another issue of the same project"));
            }
            Some(original.id)
        }
        Some(None) => None,
        None => current.duplicate_of_id,
    };
    let status = match (body.status, &body.duplicate_of) {
        (Some(status), Some(Some(_))) if status != IssueStatus::Duplicate => {
            return Err(ApiError::bad_request("duplicate_of requires status duplicate"));
        }
        (Some(status), _) => status,
        (None, Some(Some(_))) => IssueStatus::Duplicate,
        (None, _) => current.status,
    };
    let duplicate_of_id = match status {
        IssueStatus::Duplicate => {
            Some(duplicate_of_id.ok_or_else(|| ApiError::bad_request("status duplicate requires duplicate_of"))?)
        }
        _ => None,
    };
    let issue_type = body.issue_type.unwrap_or(current.issue_type);
    let private = body
        .private
        .unwrap_or(current.private || (issue_type == IssueType::Abuse && current.issue_type != IssueType::Abuse));

    sqlx::query(
        "UPDATE issues SET title = $2, description = $3, type = $4, priority = $5, status = $6, private = $7,
                milestone_id = $8, duplicate_of_id = $9, updated_at = now()
         WHERE id = $1",
    )
    .bind(current.id)
    .bind(title)
    .bind(description)
    .bind(issue_type)
    .bind(body.priority.unwrap_or(current.priority))
    .bind(status)
    .bind(private)
    .bind(body.milestone_id.unwrap_or(current.milestone_id))
    .bind(duplicate_of_id)
    .execute(&state.db)
    .await?;

    let updated = fetch_by_id(&state.db, current.id).await?;
    if status != current.status && !is_reporter {
        state.mailer.send(status_mail(&updated));
    }
    Ok(Json(updated.into_issue(&principal)))
}

fn status_mail(issue: &IssueRow) -> Mail {
    let key = format!("{}-{}", issue.project, issue.number);
    let status = issue.status.as_str().replace('_', " ");
    let duplicate = issue.duplicate_of.as_ref().map_or_else(String::new, |d| format!(" It duplicates {d}."));
    Mail {
        to: issue.reporter_email.clone(),
        subject: format!("[{key}] {}: {status}", issue.title),
        body: format!("Your report {key} \"{}\" is now: {status}.{duplicate}\n", issue.title),
    }
}

/// Delete an issue.
///
/// For spam and the like. Admin only.
#[utoipa::path(
    delete, operation_id = "deleteIssue", path = "/v1/issues/{key}", tag = "issues", params(("key" = String, Path)),
    responses((status = 204), (status = 404, body = ErrorBody))
)]
pub async fn delete(
    State(state): State<AppState>,
    principal: Principal,
    Path(key): Path<String>,
) -> ApiResult<StatusCode> {
    principal.require_admin()?;
    let issue = load(&state, &principal, &key).await?;
    let mut tx = state.db.begin().await?;
    // Issues marked as duplicates of this one go back to triage.
    sqlx::query(
        "UPDATE issues SET status = 'reported', duplicate_of_id = NULL, updated_at = now() WHERE duplicate_of_id = $1",
    )
    .bind(issue.id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM issues WHERE id = $1").bind(issue.id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
