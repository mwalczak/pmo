use std::sync::Arc;

use anyhow::{Context, Result, bail};
use pmo::{AppState, auth, config::Config, events::Actor, mail::Mailer, storage::Storage};
use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::EnvFilter;

const USAGE: &str = "usage: pmo [serve | migrate | create-key <email> [--admin] | openapi]";

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] | ["serve"] => serve().await,
        ["migrate"] => {
            let db = connect(&Config::from_env()?).await?;
            pmo::MIGRATOR.run(&db).await?;
            println!("migrations applied");
            Ok(())
        }
        ["create-key", email, flags @ ..] => create_key(email, flags.contains(&"--admin")).await,
        ["openapi"] => {
            println!("{}", pmo::openapi(std::env::var("PUBLIC_URL").ok().as_deref()).to_pretty_json()?);
            Ok(())
        }
        ["-h" | "--help" | "help"] => {
            println!("{USAGE}");
            Ok(())
        }
        _ => bail!(USAGE),
    }
}

async fn connect(config: &Config) -> Result<sqlx::PgPool> {
    PgPoolOptions::new().max_connections(10).connect(&config.database_url).await.context("connecting to the database")
}

async fn serve() -> Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,tower_http=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let config = Config::from_env()?;
    let db = connect(&config).await?;
    pmo::MIGRATOR.run(&db).await?;
    let storage = Storage::new(&config.storage_dir, config.max_upload_bytes);
    storage.init().await.with_context(|| format!("STORAGE_DIR {} is not writable", config.storage_dir))?;

    let state = AppState {
        db,
        mailer: Arc::new(Mailer::from_config(config.smtp.as_ref(), &config.mail_from)?),
        public_url: config.public_url.clone(),
        storage: Arc::new(storage),
    };
    pmo::ensure_admins(&state, &config.admin_emails).await?;
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", config.port)).await?;
    tracing::info!(port = config.port, "listening");
    axum::serve(listener, pmo::app(state)).with_graceful_shutdown(shutdown()).await?;
    Ok(())
}

/// Prints a new key for a user, creating the user if needed. For bootstrapping the first admin:
/// `kubectl -n pmo exec deploy/api -- /pmo create-key you@example.com --admin`
async fn create_key(email: &str, admin: bool) -> Result<()> {
    let config = Config::from_env()?;
    let db = connect(&config).await?;
    pmo::MIGRATOR.run(&db).await?;
    let role = if admin { "admin" } else { "reporter" };
    let user_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO users (id, email, role) VALUES ($1, $2, $3::user_role)
         ON CONFLICT (lower(email)) DO UPDATE
           SET role = CASE WHEN $3 = 'admin' THEN 'admin'::user_role ELSE users.role END, updated_at = now()
         RETURNING id",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(email)
    .bind(role)
    .fetch_one(&db)
    .await?;
    let key = auth::create_key(
        &mut *db.acquire().await?,
        auth::KeyOwner::User(user_id),
        Some("created with pmo create-key"),
        Actor::System,
    )
    .await?;
    println!("{}", key.key);
    Ok(())
}

async fn shutdown() {
    let ctrl_c = async { tokio::signal::ctrl_c().await.ok() };
    #[cfg(unix)]
    let term = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal handler").recv().await
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
}
