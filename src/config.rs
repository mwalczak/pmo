use anyhow::{Context, Result};

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub port: u16,
    /// Public base URL of the API, e.g. `https://pmo.example.com`, used in e-mails and the OpenAPI document.
    pub public_url: Option<String>,
    /// Users made admins at startup.
    pub admin_emails: Vec<String>,
    pub smtp: Option<Smtp>,
    pub mail_from: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            database_url: std::env::var("DATABASE_URL").context("DATABASE_URL is not set")?,
            port: var("PORT").map(|p| p.parse()).transpose().context("PORT")?.unwrap_or(8080),
            public_url: var("PUBLIC_URL").map(|u| u.trim_end_matches('/').to_owned()),
            admin_emails: var("ADMIN_EMAILS")
                .map(|v| v.split(',').map(|e| e.trim().to_owned()).filter(|e| !e.is_empty()).collect())
                .unwrap_or_default(),
            smtp: Smtp::from_env()?,
            mail_from: var("MAIL_FROM")
                .or_else(|| var("SMTP_USERNAME").map(|u| format!("PMO <{u}>")))
                .unwrap_or_else(|| "PMO <pmo@localhost>".to_owned()),
        })
    }
}

/// Where e-mails go. Without either setting, they are only logged.
#[derive(Clone, Debug)]
pub enum Smtp {
    /// `SMTP_URL`, e.g. `smtp://localhost:1025` (Mailpit) or `smtps://user:pass@host:465`.
    Url(String),
    /// `SMTP_HOST`, `SMTP_PORT` (465: implicit TLS, otherwise STARTTLS), `SMTP_USERNAME`, `SMTP_PASSWORD`.
    Server { host: String, port: u16, username: String, password: String },
}

impl Smtp {
    fn from_env() -> Result<Option<Self>> {
        if let Some(url) = var("SMTP_URL") {
            return Ok(Some(Self::Url(url)));
        }
        let Some(host) = var("SMTP_HOST") else { return Ok(None) };
        Ok(Some(Self::Server {
            host,
            port: var("SMTP_PORT").map(|p| p.parse()).transpose().context("SMTP_PORT")?.unwrap_or(465),
            username: var("SMTP_USERNAME").context("SMTP_HOST is set but SMTP_USERNAME is not")?,
            password: var("SMTP_PASSWORD").context("SMTP_HOST is set but SMTP_PASSWORD is not")?,
        }))
    }
}

/// An environment variable, treating empty values as unset.
fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}
