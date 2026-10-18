use axum::{extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use utoipa::ToSchema;
use uuid::Uuid;

use super::issues::{self, IssueRow};
use crate::{
    AppState,
    auth::Principal,
    error::{ApiError, ApiResult, ErrorBody},
    events::{Diff, Event},
    extract::{Json, Path},
    models::non_blank,
    notify::Settings,
};

const MAX_BODY: usize = 10_000;

/// A comment on an issue, e.g. why it won't be fixed.
#[derive(Serialize, ToSchema)]
pub struct Comment {
    pub id: Uuid,
    /// Key of the issue.
    pub issue: String,
    pub body: String,
    /// Internal comments are visible to admins only.
    pub internal: bool,
    pub author: CommentAuthor,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
pub struct CommentAuthor {
    /// `user` or `app`; `deleted` when the account no longer exists.
    pub kind: &'static str,
    pub id: Option<Uuid>,
    pub name: Option<String>,
}

#[derive(sqlx::FromRow)]
struct CommentRow {
    id: Uuid,
    issue_id: Uuid,
    issue: String,
    body: String,
    internal: bool,
    author_user_id: Option<Uuid>,
    author_app_id: Option<Uuid>,
    author_name: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl CommentRow {
    fn into_comment(self) -> Comment {
        let kind = match (self.author_user_id, self.author_app_id) {
            (Some(_), _) => "user",
            (None, Some(_)) => "app",
            (None, None) => "deleted",
        };
        Comment {
            id: self.id,
            issue: self.issue,
            body: self.body,
            internal: self.internal,
            author: CommentAuthor { kind, id: self.author_user_id.or(self.author_app_id), name: self.author_name },
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }

    fn is_author(&self, principal: &Principal) -> bool {
        match principal {
            Principal::User(u) => self.author_user_id == Some(u.id),
            Principal::App(a) => self.author_app_id == Some(a.id),
        }
    }
}

const SELECT: &str = "SELECT c.id, c.issue_id, p.key || '-' || i.number AS issue, c.body, c.internal, c.author_user_id,
    c.author_app_id, coalesce(u.name, a.name) AS author_name, c.created_at, c.updated_at
    FROM issue_comments c
    JOIN issues i ON i.id = c.issue_id
    JOIN projects p ON p.id = i.project_id
    LEFT JOIN users u ON u.id = c.author_user_id
    LEFT JOIN apps a ON a.id = c.author_app_id";

fn valid_body(body: &str) -> ApiResult<String> {
    let body = non_blank(Some(body.to_owned())).ok_or_else(|| ApiError::bad_request("the comment is empty"))?;
    if body.chars().count() > MAX_BODY {
        return Err(ApiError::bad_request(format!("comments can be at most {MAX_BODY} characters")));
    }
    Ok(body)
}

/// Adds a comment in the caller's transaction and logs it; also used when an update carries a comment.
pub(super) async fn add(
    db: &mut PgConnection,
    principal: &Principal,
    issue: &IssueRow,
    body: &str,
    internal: bool,
) -> ApiResult<Uuid> {
    let body = valid_body(body)?;
    if internal && !principal.is_admin() {
        return Err(ApiError::forbidden("only admins can write internal comments"));
    }
    let (user_id, app_id) = match principal {
        Principal::User(u) => (Some(u.id), None),
        Principal::App(a) => (None, Some(a.id)),
    };
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO issue_comments (id, issue_id, author_user_id, author_app_id, body, internal)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id)
    .bind(issue.id)
    .bind(user_id)
    .bind(app_id)
    .bind(&body)
    .bind(internal)
    .execute(&mut *db)
    .await?;
    Event::new("comment.added", "comment", id)
        .project(issue.project_id)
        .issue(issue.id, issue.key())
        .changes(serde_json::json!({ "body": body, "internal": internal }))
        .record(&mut *db, principal.into())
        .await?;
    Ok(id)
}

/// Loads a comment the caller may see: its issue must be visible, and internal comments are for admins.
async fn load(state: &AppState, principal: &Principal, id: Uuid) -> ApiResult<(CommentRow, IssueRow)> {
    let row: CommentRow = sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE c.id = $1")))
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .filter(|c: &CommentRow| !c.internal || principal.is_admin())
        .ok_or_else(|| ApiError::not_found("comment"))?;
    let issue = issues::load_by_id(state, principal, row.issue_id).await.map_err(|e| match e {
        ApiError::NotFound(_) => ApiError::not_found("comment"),
        e => e,
    })?;
    Ok((row, issue))
}

/// List an issue's comments.
///
/// Oldest first. Internal comments are included for admins only.
#[utoipa::path(
    get, operation_id = "listComments", path = "/v1/issues/{key}/comments", tag = "comments",
    params(("key" = String, Path, example = "DM-12")),
    responses((status = 200, body = Vec<Comment>), (status = 404, body = ErrorBody))
)]
pub async fn list(
    State(state): State<AppState>,
    principal: Principal,
    Path(key): Path<String>,
) -> ApiResult<Json<Vec<Comment>>> {
    let issue = issues::load(&state, &principal, &key).await?;
    let rows: Vec<CommentRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT} WHERE c.issue_id = $1 AND ($2 OR NOT c.internal) ORDER BY c.created_at, c.id"
    )))
    .bind(issue.id)
    .bind(principal.is_admin())
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows.into_iter().map(CommentRow::into_comment).collect()))
}

#[derive(Deserialize, ToSchema)]
pub struct CreateComment {
    #[schema(example = "We won't change this: the map needs location access to show places near you.")]
    pub body: String,
    /// Admins only: visible to admins only. Default `false`.
    pub internal: Option<bool>,
}

/// Comment on an issue.
///
/// Anyone who can see the issue can comment. The reporter is e-mailed about public comments by others.
#[utoipa::path(
    post, operation_id = "addComment", path = "/v1/issues/{key}/comments", tag = "comments",
    params(("key" = String, Path, example = "DM-12")), request_body = CreateComment,
    responses((status = 201, body = Comment), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody))
)]
pub async fn create(
    State(state): State<AppState>,
    principal: Principal,
    Path(key): Path<String>,
    Json(body): Json<CreateComment>,
) -> ApiResult<(StatusCode, Json<Comment>)> {
    let issue = issues::load(&state, &principal, &key).await?;
    let internal = body.internal.unwrap_or(false);
    let mut tx = state.db.begin().await?;
    let id = add(&mut tx, &principal, &issue, &body.body, internal).await?;
    let row: CommentRow =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE c.id = $1"))).bind(id).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    let by_reporter = principal.user().is_some_and(|u| u.id == issue.reporter_id);
    if !internal && !by_reporter {
        let settings = Settings::load(&state.db, issue.project_id).await?;
        if settings.notify_comments {
            state.mailer.send(settings.comment_mail(&issue.mail_info(), &row.body, row.author_name.as_deref()));
        }
    }
    Ok((StatusCode::CREATED, Json(row.into_comment())))
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateComment {
    pub body: String,
}

/// Edit a comment.
///
/// Its author, or an admin.
#[utoipa::path(
    patch, operation_id = "updateComment", path = "/v1/comments/{id}", tag = "comments",
    params(("id" = Uuid, Path)), request_body = UpdateComment,
    responses((status = 200, body = Comment), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody))
)]
pub async fn update(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateComment>,
) -> ApiResult<Json<Comment>> {
    let (current, issue) = load(&state, &principal, id).await?;
    if !current.is_author(&principal) && !principal.is_admin() {
        return Err(ApiError::forbidden("only the author and admins can edit a comment"));
    }
    let text = valid_body(&body.body)?;
    let mut tx = state.db.begin().await?;
    sqlx::query("UPDATE issue_comments SET body = $2, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(&text)
        .execute(&mut *tx)
        .await?;
    let diff = Diff::default().field("body", &current.body, &text);
    if !diff.is_empty() {
        // `internal` rides along so the issue's history can hide internal comments from non-admins.
        let mut changes = diff.into_value();
        changes["internal"] = current.internal.into();
        Event::new("comment.updated", "comment", id)
            .project(issue.project_id)
            .issue(issue.id, issue.key())
            .changes(changes)
            .record(&mut *tx, (&principal).into())
            .await?;
    }
    let row: CommentRow =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE c.id = $1"))).bind(id).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(row.into_comment()))
}

/// Delete a comment.
///
/// Its author, or an admin.
#[utoipa::path(
    delete, operation_id = "deleteComment", path = "/v1/comments/{id}", tag = "comments", params(("id" = Uuid, Path)),
    responses((status = 204), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody))
)]
pub async fn delete(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let (current, issue) = load(&state, &principal, id).await?;
    if !current.is_author(&principal) && !principal.is_admin() {
        return Err(ApiError::forbidden("only the author and admins can delete a comment"));
    }
    let mut tx = state.db.begin().await?;
    sqlx::query("DELETE FROM issue_comments WHERE id = $1").bind(id).execute(&mut *tx).await?;
    Event::new("comment.deleted", "comment", id)
        .project(issue.project_id)
        .issue(issue.id, issue.key())
        .changes(serde_json::json!({ "body": current.body, "internal": current.internal }))
        .record(&mut *tx, (&principal).into())
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
