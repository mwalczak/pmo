pub mod auth;
pub mod config;
pub mod error;
pub mod events;
pub mod extract;
pub mod mail;
pub mod models;
pub mod notify;
pub mod ratelimit;
pub mod routes;
pub mod storage;

use std::sync::Arc;

use sqlx::PgPool;

pub use routes::{app, openapi};

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub mailer: Arc<mail::Mailer>,
    /// Public base URL of the API, for links in e-mails.
    pub public_url: Option<String>,
    /// Attachment files.
    pub storage: Arc<storage::Storage>,
    /// Requests per key per minute.
    pub rate_limiter: Arc<ratelimit::RateLimiter>,
}

/// Makes the configured e-mails admins, creating their users if needed. An admin who never had a key gets one
/// by e-mail; without SMTP, use `pmo create-key <email> --admin` instead.
pub async fn ensure_admins(state: &AppState, emails: &[String]) -> anyhow::Result<()> {
    for email in emails {
        let before: Option<models::Role> = sqlx::query_scalar("SELECT role FROM users WHERE lower(email) = lower($1)")
            .bind(email)
            .fetch_optional(&state.db)
            .await?;
        let id: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO users (id, email, role) VALUES ($1, $2, 'admin')
             ON CONFLICT (lower(email)) DO UPDATE SET role = 'admin',
               updated_at = CASE WHEN users.role = 'admin' THEN users.updated_at ELSE now() END
             RETURNING id",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(email)
        .fetch_one(&state.db)
        .await?;
        let event =
            match before {
                None => Some(
                    events::Event::new("user.created", "user", id)
                        .changes(serde_json::json!({ "email": email, "role": "admin", "via": "ADMIN_EMAILS" })),
                ),
                Some(models::Role::Reporter) => Some(events::Event::new("user.updated", "user", id).changes(
                    serde_json::json!({ "role": { "from": "reporter", "to": "admin" }, "via": "ADMIN_EMAILS" }),
                )),
                Some(models::Role::Admin) => None,
            };
        if let Some(event) = event {
            event.record(&state.db, events::Actor::System).await?;
        }
        let had_key: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM api_keys WHERE user_id = $1)")
            .bind(id)
            .fetch_one(&state.db)
            .await?;
        if had_key {
            continue;
        }
        if !state.mailer.delivers() {
            tracing::warn!(%email, "admin has no key and SMTP is not configured; run `pmo create-key <email> --admin`");
            continue;
        }
        let key = auth::create_key(
            &mut *state.db.acquire().await?,
            auth::KeyOwner::User(id),
            Some("welcome e-mail"),
            events::Actor::System,
        )
        .await?;
        state.mailer.send(mail::welcome_mail(state.public_url.as_deref(), email, None, &key.key, None));
        tracing::info!(%email, "e-mailed a key to a new admin");
    }
    Ok(())
}
