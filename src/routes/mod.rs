mod apps;
mod attachments;
mod events;
mod issues;
mod me;
mod milestones;
mod projects;
mod users;

use axum::{
    Router,
    http::{StatusCode, header},
    response::{Html, IntoResponse},
    routing::get,
};
use serde::{Deserialize, Serialize};
use tower_http::trace::TraceLayer;
use utoipa::{
    IntoParams, Modify, OpenApi, ToSchema,
    openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme},
    openapi::{Content, Ref, RefOr, ResponseBuilder},
};
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::AppState;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "PMO API",
        description = "Headless project management: projects, milestones and issues, for client apps and AI agents.\n\n\
Authenticate every `/v1` request with `Authorization: Bearer <key>`. User keys (`pmo_u_…`) act as that user; \
app keys (`pmo_a_…`) are for server-to-server calls and only reach the app's projects.\n\n\
When an app reports an issue for an e-mail that has no account yet, PMO creates a reporter account and e-mails \
them a personal key.\n\n\
Building an app's report page? Agents: read `/skills/pmo-thin-client/SKILL.md`.",
    ),
    modifiers(&BearerAuth),
    security(("api_key" = [])),
    tags(
        (name = "me", description = "The caller and their keys"),
        (name = "projects", description = "Projects group milestones and issues; their key prefixes issue keys"),
        (name = "milestones", description = "Planned releases or goals within a project"),
        (name = "issues", description = "Bugs, ideas, abuse reports and things to do"),
        (name = "attachments", description = "Screenshots and recordings on issues"),
        (name = "events", description = "The action log: who did what, when, and what changed"),
        (name = "users", description = "Admin only"),
        (name = "apps", description = "Client apps and their keys. Admin only"),
    )
)]
struct ApiDoc;

struct BearerAuth;

impl Modify for BearerAuth {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        openapi.components.get_or_insert_default().add_security_scheme(
            "api_key",
            SecurityScheme::Http(HttpBuilder::new().scheme(HttpAuthScheme::Bearer).build()),
        );
    }
}

fn api_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(me::me))
        .routes(routes!(me::list_keys, me::create_key))
        .routes(routes!(me::revoke_key))
        .routes(routes!(projects::list, projects::create))
        .routes(routes!(projects::get, projects::update))
        .routes(routes!(milestones::list, milestones::create))
        .routes(routes!(milestones::get, milestones::update, milestones::delete))
        .routes(routes!(issues::list, issues::create))
        .routes(routes!(issues::get, issues::update, issues::delete))
        .routes(routes!(events::for_issue))
        .routes(routes!(events::list))
        // Uploads stream to disk and Storage enforces MAX_UPLOAD_MB; other bodies keep axum's 2 MB limit.
        .merge(
            OpenApiRouter::new()
                .routes(routes!(attachments::list, attachments::add))
                .layer(axum::extract::DefaultBodyLimit::disable()),
        )
        .routes(routes!(attachments::download, attachments::delete))
        .routes(routes!(users::list, users::create))
        .routes(routes!(users::get, users::update))
        .routes(routes!(users::list_keys, users::create_key))
        .routes(routes!(apps::list, apps::create))
        .routes(routes!(apps::get, apps::update, apps::delete))
        .routes(routes!(apps::list_keys, apps::create_key))
}

/// The OpenAPI document, with `public_url` as its server when given.
pub fn openapi(public_url: Option<&str>) -> utoipa::openapi::OpenApi {
    let (_, mut doc) = api_router().split_for_parts();
    doc.servers = Some(vec![utoipa::openapi::Server::new(public_url.unwrap_or("/"))]);
    // Every operation needs a key. Responses declared without a description get the standard one for their status.
    let unauthorized = ResponseBuilder::new()
        .description("Missing or invalid API key")
        .content("application/json", Content::new(Some(Ref::from_schema_name("ErrorBody"))))
        .build();
    for item in doc.paths.paths.values_mut() {
        let operations = [&mut item.get, &mut item.post, &mut item.patch, &mut item.delete];
        for operation in operations.into_iter().flatten() {
            operation.responses.responses.entry("401".to_owned()).or_insert_with(|| unauthorized.clone().into());
            for (status, response) in operation.responses.responses.iter_mut() {
                if let RefOr::T(response) = response
                    && response.description.is_empty()
                {
                    let reason = status.parse().ok().and_then(|s| StatusCode::from_u16(s).ok());
                    response.description = reason.and_then(|s| s.canonical_reason()).unwrap_or("").to_owned();
                }
            }
        }
    }
    doc
}

pub fn app(state: AppState) -> Router {
    let doc = openapi(state.public_url.as_deref()).to_json().expect("OpenAPI document serializes");
    let (router, _) = api_router().split_for_parts();
    router
        .route("/healthz", get(|| async { "ok" }))
        .route("/openapi.json", get(move || async move { ([(header::CONTENT_TYPE, "application/json")], doc) }))
        .route("/", get(|| async { axum::response::Redirect::to("/docs") }))
        .route("/docs", get(docs))
        // How to build a thin client (an app's report page and backend proxy), for AI agents.
        .route(
            "/skills/pmo-thin-client/SKILL.md",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
                    include_str!("../../.claude/skills/pmo-thin-client/SKILL.md"),
                )
            }),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn docs() -> impl IntoResponse {
    Html(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>PMO API</title></head>
<body><redoc spec-url="/openapi.json"></redoc>
<script src="https://cdn.jsdelivr.net/npm/redoc@2.5.0/bundles/redoc.standalone.js"></script></body></html>"#,
    )
}

/// A page of results. `next_offset` is set when there may be more.
#[derive(Serialize, ToSchema)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_offset: Option<i64>,
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct Paging {
    /// 1 to 200, default 50.
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

impl Paging {
    /// The limit to apply and the offset. Queries fetch `limit + 1` rows to know whether there is a next page.
    pub fn resolve(&self) -> (i64, i64) {
        (self.limit.unwrap_or(50).clamp(1, 200), self.offset.unwrap_or(0).max(0))
    }
}

impl<T> Page<T> {
    /// Builds a page from up to `limit + 1` rows.
    pub fn from_rows(mut items: Vec<T>, limit: i64, offset: i64) -> Self {
        let more = items.len() as i64 > limit;
        items.truncate(limit as usize);
        Self { next_offset: more.then_some(offset + limit), items }
    }
}
