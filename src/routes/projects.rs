use axum::{extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::types::Json as Jsonb;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    AppState,
    auth::Principal,
    error::{ApiError, ApiResult, ErrorBody},
    events::{Diff, Event},
    extract::{Json, Path},
    models::{IssueType, non_blank, platform_name},
    notify::{NotificationSettings, Settings},
};

/// Something a project offers its reporters (a platform or an issue type), with the label apps show for it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Choice {
    #[schema(example = "android")]
    pub id: String,
    #[schema(example = "Android")]
    pub label: String,
}

/// A platform or issue type when configuring a project; the label defaults to the id.
#[derive(Deserialize, ToSchema)]
pub struct ChoiceInput {
    pub id: String,
    pub label: Option<String>,
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct Project {
    pub id: Uuid,
    /// Short uppercase key, the prefix of issue keys (`DM` in `DM-12`).
    pub key: String,
    pub name: String,
    pub description: Option<String>,
    /// Platforms the project ships on; an issue may name one of them. Apps offer these in their report forms.
    #[schema(value_type = Vec<Choice>)]
    pub platforms: Jsonb<Vec<Choice>>,
    /// Issue types reporters can choose in this project, with their labels, in the order to show them.
    /// Admins can also create issues of other types (e.g. `todo`).
    #[schema(value_type = Vec<Choice>)]
    pub issue_types: Jsonb<Vec<Choice>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Project {
    pub fn platform_ids(&self) -> Vec<&str> {
        self.platforms.0.iter().map(|p| p.id.as_str()).collect()
    }

    pub fn offers_type(&self, issue_type: IssueType) -> bool {
        self.issue_types.0.iter().any(|t| t.id == issue_type.as_str())
    }
}

const COLUMNS: &str = "id, key, name, description, platforms, issue_types, created_at, updated_at";
const SELECT: &str =
    "SELECT id, key, name, description, platforms, issue_types, created_at, updated_at FROM projects p";
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

/// Validates platforms: ids of lowercase letters, digits and dashes, no repeats.
fn platform_list(items: Vec<ChoiceInput>) -> ApiResult<Vec<Choice>> {
    let mut out: Vec<Choice> = Vec::new();
    for item in items {
        let id = platform_name(&item.id).ok_or_else(|| {
            ApiError::bad_request(format!("invalid platform id {:?}: use lowercase letters, digits, -", item.id))
        })?;
        if out.iter().any(|c| c.id == id) {
            return Err(ApiError::bad_request(format!("platform {id:?} is listed twice")));
        }
        out.push(Choice { label: non_blank(item.label).unwrap_or_else(|| id.clone()), id });
    }
    Ok(out)
}

/// Validates issue types: known types (`bug`, `idea`, `abuse`, `todo`), no repeats.
fn issue_type_list(items: Vec<ChoiceInput>) -> ApiResult<Vec<Choice>> {
    let mut out: Vec<Choice> = Vec::new();
    for item in items {
        let id = item.id.trim().to_lowercase();
        if !IssueType::ALL.iter().any(|t| t.as_str() == id) {
            return Err(ApiError::bad_request(format!("unknown issue type {id:?}; types: bug, idea, abuse, todo")));
        }
        if out.iter().any(|c| c.id == id) {
            return Err(ApiError::bad_request(format!("issue type {id:?} is listed twice")));
        }
        out.push(Choice { label: non_blank(item.label).unwrap_or_else(|| id.clone()), id });
    }
    if out.is_empty() {
        return Err(ApiError::bad_request("a project needs at least one issue type"));
    }
    Ok(out)
}

/// The issue types a new project offers.
fn default_issue_types() -> Vec<Choice> {
    [("bug", "Bug"), ("idea", "Idea"), ("abuse", "Abuse"), ("todo", "To do")]
        .map(|(id, label)| Choice { id: id.to_owned(), label: label.to_owned() })
        .to_vec()
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
    /// Platforms the project ships on. Default none.
    #[schema(example = json!([{"id": "web", "label": "Web"}, {"id": "android", "label": "Android"}, {"id": "ios", "label": "iOS"}]))]
    pub platforms: Option<Vec<ChoiceInput>>,
    /// Issue types reporters can choose, with labels in the project's language. Default: bug, idea, abuse, todo.
    #[schema(example = json!([{"id": "bug", "label": "Błąd"}, {"id": "idea", "label": "Pomysł"}, {"id": "abuse", "label": "Nadużycie"}]))]
    pub issue_types: Option<Vec<ChoiceInput>>,
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
    let platforms = platform_list(body.platforms.unwrap_or_default())?;
    let issue_types = match body.issue_types {
        Some(types) => issue_type_list(types)?,
        None => default_issue_types(),
    };
    let mut tx = state.db.begin().await?;
    let project: Project = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "INSERT INTO projects (id, key, name, description, platforms, issue_types) VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING {COLUMNS}"
    )))
    .bind(Uuid::now_v7())
    .bind(key)
    .bind(name)
    .bind(non_blank(body.description))
    .bind(Jsonb(platforms))
    .bind(Jsonb(issue_types))
    .fetch_one(&mut *tx)
    .await?;
    Event::new("project.created", "project", project.id)
        .project(project.id)
        .changes(serde_json::json!({
            "key": project.key,
            "name": project.name,
            "description": project.description,
            "platforms": project.platforms,
            "issue_types": project.issue_types,
        }))
        .record(&mut *tx, (&principal).into())
        .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(project)))
}

/// Get a project.
///
/// Includes the platforms and issue types apps should offer in their report forms.
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
    /// Replaces the platforms. Issues keep a platform that is removed.
    pub platforms: Option<Vec<ChoiceInput>>,
    /// Replaces the issue types reporters can choose. Existing issues keep their type.
    pub issue_types: Option<Vec<ChoiceInput>>,
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
        None => current.name.clone(),
    };
    let description = match body.description {
        Some(d) => non_blank(Some(d)),
        None => current.description.clone(),
    };
    let platforms = match body.platforms {
        Some(p) => platform_list(p)?,
        None => current.platforms.0.clone(),
    };
    let issue_types = match body.issue_types {
        Some(t) => issue_type_list(t)?,
        None => current.issue_types.0.clone(),
    };
    let mut tx = state.db.begin().await?;
    let project: Project = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "UPDATE projects SET name = $2, description = $3, platforms = $4, issue_types = $5, updated_at = now()
         WHERE id = $1 RETURNING {COLUMNS}"
    )))
    .bind(current.id)
    .bind(&name)
    .bind(&description)
    .bind(Jsonb(platforms))
    .bind(Jsonb(issue_types))
    .fetch_one(&mut *tx)
    .await?;
    let diff = Diff::default()
        .field("name", &current.name, &project.name)
        .field("description", &current.description, &project.description)
        .field("platforms", &current.platforms.0, &project.platforms.0)
        .field("issue_types", &current.issue_types.0, &project.issue_types.0);
    if !diff.is_empty() {
        Event::new("project.updated", "project", project.id)
            .project(project.id)
            .changes(diff.into_value())
            .record(&mut *tx, (&principal).into())
            .await?;
    }
    tx.commit().await?;
    Ok(Json(project))
}

/// Get a project's notification settings.
///
/// How reporters are e-mailed: the language, which status changes and whether comments, and the sender. Admin only.
#[utoipa::path(
    get, operation_id = "getProjectNotifications", path = "/v1/projects/{key}/notifications", tag = "projects",
    params(("key" = String, Path, example = "DM")),
    responses((status = 200, body = NotificationSettings), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody))
)]
pub async fn get_notifications(
    State(state): State<AppState>,
    principal: Principal,
    Path(key): Path<String>,
) -> ApiResult<Json<NotificationSettings>> {
    principal.require_admin()?;
    let project = load(&state, &principal, &key).await?;
    Ok(Json(NotificationSettings::from(&Settings::load(&state.db, project.id, state.secrets.as_deref()).await?)))
}

/// Set a project's notification settings.
///
/// Replaces them all. With `mail`, e-mails go through the project's own SMTP server from its own address. Its
/// `smtp_password` is stored encrypted (MAIL_ENCRYPTION_KEY) and never returned; leave it out to keep the stored
/// one, and `mail: null` removes it. Until a password is stored, the server's sender is used. Admin only.
#[utoipa::path(
    put, operation_id = "setProjectNotifications", path = "/v1/projects/{key}/notifications", tag = "projects",
    params(("key" = String, Path, example = "DM")), request_body = NotificationSettings,
    responses((status = 200, body = NotificationSettings), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody))
)]
pub async fn set_notifications(
    State(state): State<AppState>,
    principal: Principal,
    Path(key): Path<String>,
    Json(body): Json<NotificationSettings>,
) -> ApiResult<Json<NotificationSettings>> {
    principal.require_admin()?;
    let project = load(&state, &principal, &key).await?;
    if !matches!(body.language.as_str(), "en" | "pl") {
        return Err(ApiError::bad_request("language must be en or pl"));
    }
    if let Some(mail) = &body.mail {
        mail.from
            .parse::<lettre::message::Mailbox>()
            .map_err(|_| ApiError::bad_request(format!("invalid from address {:?}", mail.from)))?;
        if mail.smtp_host.trim().is_empty() || mail.smtp_username.trim().is_empty() {
            return Err(ApiError::bad_request("mail needs smtp_host and smtp_username"));
        }
    }
    let before = NotificationSettings::from(&Settings::load(&state.db, project.id, state.secrets.as_deref()).await?);
    let statuses: Vec<&str> = body.statuses.iter().map(|s| s.as_str()).collect();
    let mail = body.mail.as_ref();
    let new_password = mail.and_then(|m| m.smtp_password.as_deref()).filter(|p| !p.is_empty());
    let sealed = match (new_password, state.secrets.as_deref()) {
        (Some(password), Some(secrets)) => Some(secrets.seal(password, project.id.as_bytes())),
        (Some(_), None) => {
            return Err(ApiError::bad_request(
                "MAIL_ENCRYPTION_KEY is not configured on the server, so no password can be stored",
            ));
        }
        (None, _) => None,
    };
    // A new password replaces the stored one; without `mail` there is none; otherwise the stored one stays.
    let replace_password = mail.is_none() || sealed.is_some();
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "INSERT INTO project_notifications
           (project_id, language, notify_statuses, notify_comments, mail_from, smtp_host, smtp_port, smtp_username,
            smtp_password)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (project_id) DO UPDATE SET language = $2, notify_statuses = $3, notify_comments = $4,
           mail_from = $5, smtp_host = $6, smtp_port = $7, smtp_username = $8,
           smtp_password = CASE WHEN $10 THEN $9 ELSE project_notifications.smtp_password END, updated_at = now()",
    )
    .bind(project.id)
    .bind(&body.language)
    .bind(&statuses)
    .bind(body.comments)
    .bind(mail.map(|m| m.from.trim()))
    .bind(mail.map(|m| m.smtp_host.trim()))
    .bind(mail.and_then(|m| m.smtp_port).map(i32::from))
    .bind(mail.map(|m| m.smtp_username.trim()))
    .bind(&sealed)
    .bind(replace_password)
    .execute(&mut *tx)
    .await?;
    let after = NotificationSettings::from(&Settings::load(&mut *tx, project.id, state.secrets.as_deref()).await?);
    let as_json = |s: &NotificationSettings| serde_json::to_value(s).expect("serializes");
    let (old, new) = (as_json(&before), as_json(&after));
    let mut diff = Diff::default();
    for field in ["language", "statuses", "comments", "mail"] {
        diff = diff.field(field, &old[field], &new[field]);
    }
    if !diff.is_empty() {
        Event::new("project.notifications_updated", "project", project.id)
            .project(project.id)
            .changes(diff.into_value())
            .record(&mut *tx, (&principal).into())
            .await?;
    }
    tx.commit().await?;
    Ok(Json(after))
}
