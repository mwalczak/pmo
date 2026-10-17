//! API keys and the authenticated caller.
//!
//! Keys are random 24-byte tokens, `pmo_u_…` for users and `pmo_a_…` for apps. Only their SHA-256 is stored.
use axum::{extract::FromRequestParts, http::request::Parts};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::PgExecutor;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    AppState,
    error::{ApiError, ApiResult},
    models::Role,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyOwner {
    User(Uuid),
    App(Uuid),
}

/// An API key as listed; the key itself is shown only once, when created.
#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct ApiKey {
    pub id: Uuid,
    pub name: Option<String>,
    /// The first characters of the key, to tell keys apart.
    pub prefix: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// A newly created key. Store `key` now: it cannot be retrieved later.
#[derive(Serialize, ToSchema)]
pub struct NewApiKey {
    /// Send as `Authorization: Bearer <key>`.
    pub key: String,
    #[serde(flatten)]
    pub info: ApiKey,
}

pub fn hash_key(key: &str) -> Vec<u8> {
    Sha256::digest(key.as_bytes()).to_vec()
}

pub async fn create_key(db: impl PgExecutor<'_>, owner: KeyOwner, name: Option<&str>) -> sqlx::Result<NewApiKey> {
    let (kind, user_id, app_id) = match owner {
        KeyOwner::User(id) => ("u", Some(id), None),
        KeyOwner::App(id) => ("a", None, Some(id)),
    };
    let key = format!("pmo_{kind}_{}", hex::encode(rand::random::<[u8; 24]>()));
    let info = sqlx::query_as::<_, ApiKey>(
        "INSERT INTO api_keys (id, user_id, app_id, name, prefix, hash) VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING id, name, prefix, created_at, last_used_at, revoked_at",
    )
    .bind(Uuid::now_v7())
    .bind(user_id)
    .bind(app_id)
    .bind(name)
    .bind(&key[..12])
    .bind(hash_key(&key))
    .fetch_one(db)
    .await?;
    Ok(NewApiKey { key, info })
}

pub async fn list_keys(db: impl PgExecutor<'_>, owner: KeyOwner) -> sqlx::Result<Vec<ApiKey>> {
    let (user_id, app_id) = match owner {
        KeyOwner::User(id) => (Some(id), None),
        KeyOwner::App(id) => (None, Some(id)),
    };
    sqlx::query_as(
        "SELECT id, name, prefix, created_at, last_used_at, revoked_at FROM api_keys
         WHERE user_id IS NOT DISTINCT FROM $1 AND app_id IS NOT DISTINCT FROM $2 ORDER BY created_at",
    )
    .bind(user_id)
    .bind(app_id)
    .fetch_all(db)
    .await
}

#[derive(Clone, Debug)]
pub struct CurrentUser {
    pub id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub role: Role,
}

#[derive(Clone, Debug)]
pub struct CurrentApp {
    pub id: Uuid,
    pub name: String,
}

/// Who is calling: a user (with a personal key) or an app (server-to-server, scoped to its projects).
#[derive(Clone, Debug)]
pub enum Principal {
    User(CurrentUser),
    App(CurrentApp),
}

impl Principal {
    pub fn is_admin(&self) -> bool {
        matches!(self, Self::User(u) if u.role == Role::Admin)
    }

    pub fn require_admin(&self) -> ApiResult<&CurrentUser> {
        match self {
            Self::User(u) if u.role == Role::Admin => Ok(u),
            _ => Err(ApiError::forbidden("admin only")),
        }
    }

    pub fn user(&self) -> Option<&CurrentUser> {
        match self {
            Self::User(u) => Some(u),
            Self::App(_) => None,
        }
    }

    pub fn app_id(&self) -> Option<Uuid> {
        match self {
            Self::App(a) => Some(a.id),
            Self::User(_) => None,
        }
    }

    pub fn key_owner(&self) -> KeyOwner {
        match self {
            Self::User(u) => KeyOwner::User(u.id),
            Self::App(a) => KeyOwner::App(a.id),
        }
    }
}

#[derive(sqlx::FromRow)]
struct KeyLookup {
    id: Uuid,
    last_used_at: Option<DateTime<Utc>>,
    user_id: Option<Uuid>,
    email: Option<String>,
    user_name: Option<String>,
    role: Option<Role>,
    app_id: Option<Uuid>,
    app_name: Option<String>,
}

impl FromRequestParts<AppState> for Principal {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let key = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim)
            .ok_or(ApiError::Unauthorized)?;
        let found = sqlx::query_as::<_, KeyLookup>(
            "SELECT k.id, k.last_used_at, u.id AS user_id, u.email, u.name AS user_name, u.role,
                    a.id AS app_id, a.name AS app_name
             FROM api_keys k LEFT JOIN users u ON u.id = k.user_id LEFT JOIN apps a ON a.id = k.app_id
             WHERE k.hash = $1 AND k.revoked_at IS NULL",
        )
        .bind(hash_key(key))
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::Unauthorized)?;

        // Record use at most once a minute, without delaying the request.
        if found.last_used_at.is_none_or(|t| Utc::now() - t > chrono::TimeDelta::minutes(1)) {
            let db = state.db.clone();
            tokio::spawn(async move {
                let _ = sqlx::query("UPDATE api_keys SET last_used_at = now() WHERE id = $1")
                    .bind(found.id)
                    .execute(&db)
                    .await;
            });
        }

        match found {
            KeyLookup { user_id: Some(id), email: Some(email), role: Some(role), user_name, .. } => {
                Ok(Self::User(CurrentUser { id, email, name: user_name, role }))
            }
            KeyLookup { app_id: Some(id), app_name: Some(name), .. } => Ok(Self::App(CurrentApp { id, name })),
            _ => Err(ApiError::Unauthorized),
        }
    }
}
