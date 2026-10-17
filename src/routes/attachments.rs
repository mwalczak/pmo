use axum::{
    body::Body,
    extract::{Multipart, Request, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use tower::ServiceExt;
use tower_http::services::ServeFile;
use utoipa::ToSchema;
use uuid::Uuid;

use super::issues::{self, IssueRow};
use crate::{
    AppState,
    auth::Principal,
    error::{ApiError, ApiResult, ErrorBody},
    events::Event,
    extract::{Json, Path},
};

const MAX_PER_ISSUE: i64 = 20;

/// A screenshot or recording attached to an issue.
#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct Attachment {
    pub id: Uuid,
    /// Key of the issue.
    pub issue: String,
    pub filename: Option<String>,
    /// Detected from the file's contents, e.g. `image/png`, `video/mp4`.
    pub content_type: String,
    /// `image` or `video`.
    pub kind: String,
    /// Bytes.
    pub size: i64,
    /// Where to download it, with the same key (GET; supports `Range` for videos).
    pub url: String,
    /// Name of the user or app that uploaded it.
    pub uploaded_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct AttachmentRow {
    id: Uuid,
    issue_id: Uuid,
    issue: String,
    filename: Option<String>,
    content_type: String,
    size: i64,
    sha256: String,
    uploaded_by: Option<String>,
    uploaded_by_user_id: Option<Uuid>,
    uploaded_by_app_id: Option<Uuid>,
    created_at: DateTime<Utc>,
}

impl AttachmentRow {
    fn into_attachment(self, public_url: Option<&str>) -> Attachment {
        Attachment {
            url: format!("{}/v1/attachments/{}", public_url.unwrap_or(""), self.id),
            kind: self.content_type.split('/').next().unwrap_or("").to_owned(),
            id: self.id,
            issue: self.issue,
            filename: self.filename,
            content_type: self.content_type,
            size: self.size,
            uploaded_by: self.uploaded_by,
            created_at: self.created_at,
        }
    }
}

const SELECT: &str = "SELECT t.id, t.issue_id, p.key || '-' || i.number AS issue, t.filename, t.content_type, t.size,
    t.sha256, coalesce(u.name, u.email, a.name) AS uploaded_by, t.uploaded_by_user_id, t.uploaded_by_app_id,
    t.created_at
    FROM attachments t
    JOIN issues i ON i.id = t.issue_id
    JOIN projects p ON p.id = i.project_id
    LEFT JOIN users u ON u.id = t.uploaded_by_user_id
    LEFT JOIN apps a ON a.id = t.uploaded_by_app_id";

async fn fetch(db: impl sqlx::PgExecutor<'_>, id: Uuid) -> sqlx::Result<Option<AttachmentRow>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE t.id = $1"))).bind(id).fetch_optional(db).await
}

/// Loads an attachment whose issue the caller can see.
async fn load(state: &AppState, principal: &Principal, id: Uuid) -> ApiResult<(AttachmentRow, IssueRow)> {
    let row = fetch(&state.db, id).await?.ok_or_else(|| ApiError::not_found("attachment"))?;
    let issue = issues::load_by_id(state, principal, row.issue_id).await.map_err(|e| match e {
        ApiError::NotFound(_) => ApiError::not_found("attachment"),
        e => e,
    })?;
    Ok((row, issue))
}

/// Admins, the issue's reporter, and the app it was reported through can attach files.
fn may_attach(principal: &Principal, issue: &IssueRow) -> bool {
    principal.is_admin()
        || match principal {
            Principal::User(u) => u.id == issue.reporter_id,
            Principal::App(a) => issue.source_app_id == Some(a.id),
        }
}

/// Removes an issue's attachments, returning their file hashes for `Storage::remove_unused` after commit.
pub async fn delete_for_issue(db: &mut PgConnection, issue_id: Uuid) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar("DELETE FROM attachments WHERE issue_id = $1 RETURNING sha256")
        .bind(issue_id)
        .fetch_all(db)
        .await
}

/// List an issue's attachments.
#[utoipa::path(
    get, operation_id = "listAttachments", path = "/v1/issues/{key}/attachments", tag = "attachments",
    params(("key" = String, Path, example = "DM-12")),
    responses((status = 200, body = Vec<Attachment>), (status = 404, body = ErrorBody))
)]
pub async fn list(
    State(state): State<AppState>,
    principal: Principal,
    Path(key): Path<String>,
) -> ApiResult<Json<Vec<Attachment>>> {
    let issue = issues::load(&state, &principal, &key).await?;
    let rows: Vec<AttachmentRow> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE t.issue_id = $1 ORDER BY t.created_at, t.id")))
            .bind(issue.id)
            .fetch_all(&state.db)
            .await?;
    Ok(Json(rows.into_iter().map(|r| r.into_attachment(state.public_url.as_deref())).collect()))
}

/// Attach a screenshot or recording.
///
/// Send `multipart/form-data` with the file in a `file` field, one file per request. Images (PNG, JPEG, GIF, WebP,
/// HEIC, AVIF) and videos (MP4, MOV, WebM) are accepted, up to `MAX_UPLOAD_MB` (100 MB by default) and 20 per
/// issue; the type is detected from the contents. Admins, the issue's reporter and the app it was reported through
/// can attach files.
#[utoipa::path(
    post, operation_id = "addAttachment", path = "/v1/issues/{key}/attachments", tag = "attachments",
    params(("key" = String, Path, example = "DM-12")),
    request_body(content_type = "multipart/form-data", description = "A `file` field with the image or video"),
    responses(
        (status = 201, body = Attachment),
        (status = 403, body = ErrorBody),
        (status = 413, body = ErrorBody, description = "Too large"),
        (status = 415, body = ErrorBody, description = "Not an image or video"),
    )
)]
pub async fn add(
    State(state): State<AppState>,
    principal: Principal,
    Path(key): Path<String>,
    mut multipart: Multipart,
) -> ApiResult<(StatusCode, Json<Attachment>)> {
    let issue = issues::load(&state, &principal, &key).await?;
    if !may_attach(&principal, &issue) {
        return Err(ApiError::forbidden("only admins, the reporter and the app it came through can attach files"));
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM attachments WHERE issue_id = $1")
        .bind(issue.id)
        .fetch_one(&state.db)
        .await?;
    if count >= MAX_PER_ISSUE {
        return Err(ApiError::conflict(format!("an issue can have at most {MAX_PER_ISSUE} attachments")));
    }

    let mut field = loop {
        match multipart.next_field().await.map_err(|e| ApiError::bad_request(e.body_text()))? {
            Some(field) if field.name() == Some("file") => break field,
            Some(_) => continue,
            None => return Err(ApiError::bad_request("send the file in a multipart field named `file`")),
        }
    };
    let filename = field.file_name().map(clean_filename).filter(|f| !f.is_empty());
    let stored = state.storage.store(&mut field).await?;

    let id = Uuid::now_v7();
    let (user_id, app_id) = match &principal {
        Principal::User(u) => (Some(u.id), None),
        Principal::App(a) => (None, Some(a.id)),
    };
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "INSERT INTO attachments (id, issue_id, sha256, content_type, size, filename, uploaded_by_user_id,
                                  uploaded_by_app_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(id)
    .bind(issue.id)
    .bind(&stored.sha256)
    .bind(stored.content_type)
    .bind(stored.size as i64)
    .bind(&filename)
    .bind(user_id)
    .bind(app_id)
    .execute(&mut *tx)
    .await?;
    Event::new("attachment.added", "attachment", id)
        .project(issue.project_id)
        .issue(issue.id, issue.key())
        .changes(serde_json::json!({
            "filename": filename, "content_type": stored.content_type, "size": stored.size,
        }))
        .record(&mut *tx, (&principal).into())
        .await?;
    let row = fetch(&mut *tx, id).await?.expect("just inserted");
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(row.into_attachment(state.public_url.as_deref()))))
}

/// Keeps a client's file name readable and safe in a header: no paths, quotes or control characters.
fn clean_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    base.chars().filter(|c| !c.is_control() && !matches!(c, '"' | ';')).take(200).collect::<String>().trim().to_owned()
}

/// Download an attachment.
///
/// Anyone who can see the issue can download its attachments. Supports `Range` requests, so videos can be scrubbed.
#[utoipa::path(
    get, operation_id = "getAttachment", path = "/v1/attachments/{id}", tag = "attachments", params(("id" = Uuid, Path)),
    responses(
        (status = 200, description = "The file", content_type = "application/octet-stream"),
        (status = 206, description = "Part of the file (Range)"),
        (status = 404, body = ErrorBody),
    )
)]
pub async fn download(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
    request: Request,
) -> ApiResult<Response> {
    let (row, _) = load(&state, &principal, id).await?;
    let path = state.storage.path(&row.sha256);
    let mut response = ServeFile::new(path)
        .oneshot(request)
        .await
        .map_err(|e| anyhow::anyhow!("serving an attachment: {e}"))?
        .map(Body::new);
    if response.status() == StatusCode::NOT_FOUND {
        return Err(anyhow::anyhow!("attachment {id} has no file").into());
    }
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_str(&row.content_type).map_err(anyhow::Error::from)?);
    let disposition = match &row.filename {
        Some(name) => format!("inline; filename=\"{}\"", name.replace(|c: char| !c.is_ascii(), "_")),
        None => "inline".to_owned(),
    };
    if let Ok(value) = HeaderValue::from_str(&disposition) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=86400"));
    headers.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    Ok(response.into_response())
}

/// Delete an attachment.
///
/// Admins, and whoever uploaded it.
#[utoipa::path(
    delete, operation_id = "deleteAttachment", path = "/v1/attachments/{id}", tag = "attachments",
    params(("id" = Uuid, Path)),
    responses((status = 204), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody))
)]
pub async fn delete(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let (row, issue) = load(&state, &principal, id).await?;
    let uploader = match &principal {
        Principal::User(u) => row.uploaded_by_user_id == Some(u.id),
        Principal::App(a) => row.uploaded_by_app_id == Some(a.id),
    };
    if !principal.is_admin() && !uploader {
        return Err(ApiError::forbidden("only admins and the uploader can delete an attachment"));
    }
    let mut tx = state.db.begin().await?;
    sqlx::query("DELETE FROM attachments WHERE id = $1").bind(id).execute(&mut *tx).await?;
    Event::new("attachment.deleted", "attachment", id)
        .project(issue.project_id)
        .issue(issue.id, issue.key())
        .changes(serde_json::json!({ "filename": row.filename, "content_type": row.content_type, "size": row.size }))
        .record(&mut *tx, (&principal).into())
        .await?;
    tx.commit().await?;
    remove_files(&state, &state.db, vec![row.sha256]).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_files(state: &AppState, db: &PgPool, hashes: Vec<String>) {
    state.storage.remove_unused(db, hashes).await;
}
