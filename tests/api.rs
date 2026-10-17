use std::sync::{Arc, Mutex};

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode},
};
use http_body_util::BodyExt;
use pmo::{
    AppState,
    auth::{self, KeyOwner},
    mail::{Mail, Mailer},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

struct TestApp {
    router: Router,
    state: AppState,
    mails: Arc<Mutex<Vec<Mail>>>,
}

impl TestApp {
    fn new(db: PgPool) -> Self {
        let mails = Arc::new(Mutex::new(Vec::new()));
        let state = AppState {
            db,
            mailer: Arc::new(Mailer::Memory(mails.clone())),
            public_url: Some("https://pmo.test".to_owned()),
        };
        Self { router: pmo::app(state.clone()), state, mails }
    }

    async fn user_key(&self, email: &str, role: &str) -> String {
        let id: Uuid =
            sqlx::query_scalar("INSERT INTO users (id, email, role) VALUES ($1, $2, $3::user_role) RETURNING id")
                .bind(Uuid::now_v7())
                .bind(email)
                .bind(role)
                .fetch_one(&self.state.db)
                .await
                .unwrap();
        auth::create_key(&self.state.db, KeyOwner::User(id), None).await.unwrap().key
    }

    async fn call(&self, method: Method, path: &str, key: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
        let mut req = Request::builder().method(method).uri(path);
        if let Some(key) = key {
            req = req.header("authorization", format!("Bearer {key}"));
        }
        let req = match body {
            Some(b) => req.header("content-type", "application/json").body(Body::from(b.to_string())),
            None => req.body(Body::empty()),
        }
        .unwrap();
        let res = self.router.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let value =
            serde_json::from_slice(&bytes).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()));
        (status, value)
    }

    async fn get(&self, path: &str, key: &str) -> (StatusCode, Value) {
        self.call(Method::GET, path, Some(key), None).await
    }
    async fn post(&self, path: &str, key: &str, body: Value) -> (StatusCode, Value) {
        self.call(Method::POST, path, Some(key), Some(body)).await
    }
    async fn patch(&self, path: &str, key: &str, body: Value) -> (StatusCode, Value) {
        self.call(Method::PATCH, path, Some(key), Some(body)).await
    }

    fn take_mails(&self) -> Vec<Mail> {
        std::mem::take(&mut *self.mails.lock().unwrap())
    }

    /// An admin key, project DM and an app for it; returns (admin key, app key).
    async fn setup(&self) -> (String, String) {
        let admin = self.user_key("admin@example.com", "admin").await;
        let (s, _) = self.post("/v1/projects", &admin, json!({ "key": "DM", "name": "Dobre Mleko" })).await;
        assert_eq!(s, StatusCode::CREATED);
        let (s, app) = self.post("/v1/apps", &admin, json!({ "name": "dobre-mleko.app", "projects": ["DM"] })).await;
        assert_eq!(s, StatusCode::CREATED, "{app}");
        (admin, app["key"]["key"].as_str().unwrap().to_owned())
    }
}

fn keys(page: &Value) -> Vec<&str> {
    page["items"].as_array().unwrap().iter().map(|i| i["key"].as_str().unwrap()).collect()
}

/// The key in a welcome e-mail.
fn mailed_key(mail: &Mail) -> String {
    mail.body.split_whitespace().find(|w| w.starts_with("pmo_u_")).unwrap().to_owned()
}

#[sqlx::test]
async fn public_routes_and_auth(db: PgPool) {
    let app = TestApp::new(db);
    let (s, body) = app.call(Method::GET, "/healthz", None, None).await;
    assert_eq!((s, body), (StatusCode::OK, json!("ok")));
    let (s, doc) = app.call(Method::GET, "/openapi.json", None, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(doc["servers"][0]["url"], "https://pmo.test");

    let (s, body) = app.call(Method::GET, "/v1/me", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "unauthorized");
    let (s, _) = app.get("/v1/me", "pmo_u_nope").await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn app_reports_issue_and_reporter_gets_a_key(db: PgPool) {
    let app = TestApp::new(db);
    let (_, app_key) = app.setup().await;

    let (s, me) = app.get("/v1/me", &app_key).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(me, json!({ "kind": "app", "id": me["id"], "name": "dobre-mleko.app", "projects": ["DM"] }));

    let report = json!({
        "type": "bug", "title": "Map does not load", "description": "Blank screen",
        "reporter": { "email": "Ola@Example.com", "name": "Ola" }
    });
    let (s, issue) = app.post("/v1/projects/DM/issues", &app_key, report).await;
    assert_eq!(s, StatusCode::CREATED, "{issue}");
    assert_eq!(issue["key"], "DM-1");
    assert_eq!(issue["status"], "reported");
    assert_eq!(issue["priority"], "P3");
    assert_eq!(issue["private"], false);
    assert_eq!(issue["source_app"], "dobre-mleko.app");
    assert_eq!(issue["reporter"]["name"], "Ola");
    assert!(issue["reporter"].get("email").is_none(), "apps do not see reporter e-mails");

    let mails = app.take_mails();
    assert_eq!(mails.len(), 1);
    assert_eq!(mails[0].to, "Ola@Example.com");
    assert!(mails[0].body.contains("dobre-mleko.app") && mails[0].body.contains("https://pmo.test/docs"));
    let ola = mailed_key(&mails[0]);

    let (s, me) = app.get("/v1/me", &ola).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(me["kind"], "user");
    assert_eq!(me["role"], "reporter");
    let (_, issue) = app.get("/v1/issues/DM-1", &ola).await;
    assert_eq!(issue["reporter"]["email"], "Ola@Example.com", "reporters see their own e-mail");

    // Same e-mail, any case: same user, no second e-mail.
    let (s, issue) = app
        .post(
            "/v1/projects/DM/issues",
            &app_key,
            json!({ "type": "idea", "title": "Dark mode", "reporter": { "email": "ola@example.com" } }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(issue["key"], "DM-2");
    assert_eq!(issue["reporter"]["id"], me["id"]);
    assert!(app.take_mails().is_empty());

    // Apps must say who reports.
    let (s, _) = app.post("/v1/projects/DM/issues", &app_key, json!({ "type": "bug", "title": "x" })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = app
        .post(
            "/v1/projects/DM/issues",
            &app_key,
            json!({ "type": "bug", "title": "x", "reporter": { "email": "nope" } }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn abuse_reports_are_private(db: PgPool) {
    let app = TestApp::new(db);
    let (admin, app_key) = app.setup().await;
    let report = json!({ "type": "abuse", "title": "Spam account", "reporter": { "email": "ola@example.com" } });
    let (_, issue) = app.post("/v1/projects/DM/issues", &app_key, report).await;
    assert_eq!(issue["private"], true);
    let ola = mailed_key(&app.take_mails()[0]);
    let other = app.user_key("kuba@example.com", "reporter").await;

    let (_, page) = app.get("/v1/projects/DM/issues", &app_key).await;
    assert!(keys(&page).is_empty(), "apps do not list private issues");
    let (_, page) = app.get("/v1/projects/DM/issues", &other).await;
    assert!(keys(&page).is_empty());
    let (s, _) = app.get("/v1/issues/DM-1", &other).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let (_, page) = app.get("/v1/projects/DM/issues", &ola).await;
    assert_eq!(keys(&page), ["DM-1"]);
    let (_, page) = app.get("/v1/projects/DM/issues?type=abuse", &admin).await;
    assert_eq!(keys(&page), ["DM-1"]);

    // Admins can still publish it.
    let (s, issue) = app.patch("/v1/issues/DM-1", &admin, json!({ "private": false })).await;
    assert_eq!((s, &issue["private"]), (StatusCode::OK, &json!(false)));
    let (_, page) = app.get("/v1/projects/DM/issues", &other).await;
    assert_eq!(keys(&page), ["DM-1"]);
}

#[sqlx::test]
async fn apps_only_reach_their_projects(db: PgPool) {
    let app = TestApp::new(db);
    let (admin, app_key) = app.setup().await;
    app.post("/v1/projects", &admin, json!({ "key": "XX", "name": "Other" })).await;

    let (_, projects) = app.get("/v1/projects", &app_key).await;
    assert_eq!(projects.as_array().unwrap().len(), 1);
    let (_, projects) = app.get("/v1/projects", &admin).await;
    assert_eq!(projects.as_array().unwrap().len(), 2);
    let (s, _) = app.get("/v1/projects/XX", &app_key).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let report = json!({ "type": "bug", "title": "x", "reporter": { "email": "a@example.com" } });
    let (s, _) = app.post("/v1/projects/XX/issues", &app_key, report).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Apps cannot administer.
    let (s, _) = app.post("/v1/projects", &app_key, json!({ "key": "YY", "name": "Y" })).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = app.get("/v1/users", &app_key).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn reporters_edit_only_their_own_text(db: PgPool) {
    let app = TestApp::new(db);
    let (admin, _) = app.setup().await;
    let ola = app.user_key("ola@example.com", "reporter").await;
    let kuba = app.user_key("kuba@example.com", "reporter").await;

    let (s, _) =
        app.post("/v1/projects/DM/issues", &ola, json!({ "type": "bug", "title": "x", "priority": "P1" })).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "reporters cannot set priority");
    let (s, _) = app
        .post(
            "/v1/projects/DM/issues",
            &ola,
            json!({ "type": "bug", "title": "x", "reporter": { "email": "kuba@example.com" } }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "reporters cannot report for others");
    let (s, issue) = app.post("/v1/projects/DM/issues", &ola, json!({ "type": "bug", "title": "Typo" })).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(issue["source_app"], Value::Null);

    let (s, issue) = app.patch("/v1/issues/DM-1", &ola, json!({ "title": "Typo on home page" })).await;
    assert_eq!((s, &issue["title"]), (StatusCode::OK, &json!("Typo on home page")));
    let (s, _) = app.patch("/v1/issues/DM-1", &ola, json!({ "status": "done" })).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = app.patch("/v1/issues/DM-1", &kuba, json!({ "title": "mine now" })).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = app.call(Method::DELETE, "/v1/issues/DM-1", Some(&ola), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let (s, issue) = app.patch("/v1/issues/DM-1", &admin, json!({ "status": "in_progress", "priority": "P2" })).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!((&issue["status"], &issue["priority"]), (&json!("in_progress"), &json!("P2")));
    let mails = app.take_mails();
    assert_eq!(mails.len(), 1);
    assert_eq!(mails[0].to, "ola@example.com");
    assert_eq!(mails[0].subject, "[DM-1] Typo on home page: in progress");
}

#[sqlx::test]
async fn duplicates(db: PgPool) {
    let app = TestApp::new(db);
    let (admin, _) = app.setup().await;
    for title in ["Milk price is wrong", "Price of milk wrong"] {
        app.post("/v1/projects/DM/issues", &admin, json!({ "type": "bug", "title": title })).await;
    }

    let (s, _) = app.patch("/v1/issues/DM-2", &admin, json!({ "status": "duplicate" })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = app.patch("/v1/issues/DM-2", &admin, json!({ "duplicate_of": "DM-2" })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, issue) = app.patch("/v1/issues/dm-2", &admin, json!({ "duplicate_of": "DM-1" })).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!((&issue["status"], &issue["duplicate_of"]), (&json!("duplicate"), &json!("DM-1")));

    let (_, page) = app.get("/v1/projects/DM/issues?status=open", &admin).await;
    assert_eq!(keys(&page), ["DM-1"]);
    let (_, page) = app.get("/v1/projects/DM/issues?status=closed", &admin).await;
    assert_eq!(keys(&page), ["DM-2"]);
    let (s, _) = app.get("/v1/projects/DM/issues?status=nope", &admin).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    let (_, issue) = app.patch("/v1/issues/DM-2", &admin, json!({ "status": "accepted" })).await;
    assert_eq!((&issue["status"], &issue["duplicate_of"]), (&json!("accepted"), &Value::Null));

    let (_, issue) = app.patch("/v1/issues/DM-2", &admin, json!({ "duplicate_of": "DM-1" })).await;
    assert_eq!(issue["status"], "duplicate");
    let (s, _) = app.call(Method::DELETE, "/v1/issues/DM-1", Some(&admin), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, issue) = app.get("/v1/issues/DM-2", &admin).await;
    assert_eq!((&issue["status"], &issue["duplicate_of"]), (&json!("reported"), &Value::Null));
}

#[sqlx::test]
async fn search_and_sort(db: PgPool) {
    let app = TestApp::new(db);
    let (admin, _) = app.setup().await;
    let issues = [
        ("bug", "App crashes on login", "P2"),
        ("bug", "Milk price is wrong", "P1"),
        ("idea", "Add dark mode", "P4"),
        ("bug", "Wrong opening hours", "P3"),
    ];
    for (t, title, priority) in issues {
        let body = json!({ "type": t, "title": title, "priority": priority, "description": "Seen on the map" });
        app.post("/v1/projects/DM/issues", &admin, body).await;
    }

    let (_, page) = app.get("/v1/projects/DM/issues?q=milk%20prise", &admin).await;
    assert_eq!(keys(&page), ["DM-2"], "typos still match");
    let (_, page) = app.get("/v1/projects/DM/issues?q=LOGIN", &admin).await;
    assert_eq!(keys(&page), ["DM-1"]);
    let (_, page) = app.get("/v1/projects/DM/issues?q=on%20the%20map&sort=oldest", &admin).await;
    assert_eq!(keys(&page), ["DM-1", "DM-2", "DM-3", "DM-4"], "descriptions match too");

    let (_, page) = app.get("/v1/projects/DM/issues?sort=priority", &admin).await;
    assert_eq!(keys(&page), ["DM-2", "DM-1", "DM-4", "DM-3"]);
    let (_, page) = app.get("/v1/projects/DM/issues?priority=P1,p2&type=bug", &admin).await;
    assert_eq!(keys(&page), ["DM-2", "DM-1"]);

    let (_, page) = app.get("/v1/projects/DM/issues?limit=3", &admin).await;
    assert_eq!((keys(&page), &page["next_offset"]), (vec!["DM-4", "DM-3", "DM-2"], &json!(3)));
    let (_, page) = app.get("/v1/projects/DM/issues?limit=3&offset=3", &admin).await;
    assert_eq!((keys(&page), &page["next_offset"]), (vec!["DM-1"], &Value::Null));
}

#[sqlx::test]
async fn milestones(db: PgPool) {
    let app = TestApp::new(db);
    let (admin, app_key) = app.setup().await;
    let (s, m) =
        app.post("/v1/projects/DM/milestones", &admin, json!({ "name": "v1.0", "due_on": "2026-12-01" })).await;
    assert_eq!(s, StatusCode::CREATED, "{m}");
    assert_eq!((&m["project"], &m["state"]), (&json!("DM"), &json!("open")));
    let id = m["id"].as_str().unwrap();

    app.post("/v1/projects/DM/issues", &admin, json!({ "type": "idea", "title": "A", "milestone_id": id })).await;
    app.post("/v1/projects/DM/issues", &admin, json!({ "type": "idea", "title": "B" })).await;
    let (_, issue) = app.get("/v1/issues/DM-1", &admin).await;
    assert_eq!(issue["milestone"], json!({ "id": id, "name": "v1.0" }));
    let (_, page) = app.get(&format!("/v1/projects/DM/issues?milestone_id={id}"), &app_key).await;
    assert_eq!(keys(&page), ["DM-1"]);

    let (_, m) = app.patch(&format!("/v1/milestones/{id}"), &admin, json!({ "state": "closed", "due_on": null })).await;
    assert_eq!((&m["state"], &m["due_on"]), (&json!("closed"), &Value::Null));
    let (_, list) = app.get("/v1/projects/DM/milestones?state=open", &app_key).await;
    assert_eq!(list, json!([]));

    let (s, _) = app.call(Method::DELETE, &format!("/v1/milestones/{id}"), Some(&admin), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, issue) = app.get("/v1/issues/DM-1", &admin).await;
    assert_eq!(issue["milestone"], Value::Null);
}

#[sqlx::test]
async fn keys_can_be_rotated_and_revoked(db: PgPool) {
    let app = TestApp::new(db);
    let (admin, app_key) = app.setup().await;

    let (s, new) = app.post("/v1/me/keys", &app_key, json!({ "name": "rotated" })).await;
    assert_eq!(s, StatusCode::CREATED);
    let new_key = new["key"].as_str().unwrap();
    assert!(new_key.starts_with("pmo_a_"));
    let (_, list) = app.get("/v1/me/keys", new_key).await;
    assert_eq!(list.as_array().unwrap().len(), 2);
    let old_id = list[0]["id"].as_str().unwrap();

    let (s, _) = app.call(Method::DELETE, &format!("/v1/keys/{old_id}"), Some(new_key), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = app.get("/v1/me", &app_key).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // Others' keys are not found; admins can revoke any key.
    let ola = app.user_key("ola@example.com", "reporter").await;
    let new_id = new["id"].as_str().unwrap();
    let (s, _) = app.call(Method::DELETE, &format!("/v1/keys/{new_id}"), Some(&ola), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = app.call(Method::DELETE, &format!("/v1/keys/{new_id}"), Some(&admin), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = app.get("/v1/me", new_key).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn admin_manages_users_and_apps(db: PgPool) {
    let app = TestApp::new(db);
    let (admin, _) = app.setup().await;

    let (s, user) = app.post("/v1/users", &admin, json!({ "email": "agent@example.com", "name": "Agent" })).await;
    assert_eq!((s, &user["role"]), (StatusCode::CREATED, &json!("reporter")));
    assert_eq!(app.take_mails().len(), 1);
    let (s, _) = app.post("/v1/users", &admin, json!({ "email": "AGENT@example.com" })).await;
    assert_eq!(s, StatusCode::CONFLICT);

    let id = user["id"].as_str().unwrap();
    let (_, user) = app.patch(&format!("/v1/users/{id}"), &admin, json!({ "role": "admin" })).await;
    assert_eq!(user["role"], "admin");
    let (_, page) = app.get("/v1/users?role=admin", &admin).await;
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    let (s, key) = app.post(&format!("/v1/users/{id}/keys"), &admin, json!({})).await;
    assert_eq!(s, StatusCode::CREATED);
    let (_, me) = app.get("/v1/me", key["key"].as_str().unwrap()).await;
    assert_eq!(me["email"], "agent@example.com");

    let (_, apps) = app.get("/v1/apps", &admin).await;
    let app_id = apps[0]["id"].as_str().unwrap();
    let (s, _) = app.patch(&format!("/v1/apps/{app_id}"), &admin, json!({ "projects": ["NOPE"] })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, updated) = app.patch(&format!("/v1/apps/{app_id}"), &admin, json!({ "projects": [] })).await;
    assert_eq!((s, &updated["projects"]), (StatusCode::OK, &json!([])));
}

#[sqlx::test]
async fn configured_admins_get_a_key_once(db: PgPool) {
    let app = TestApp::new(db);
    let admins = ["boss@example.com".to_owned()];
    pmo::ensure_admins(&app.state, &admins).await.unwrap();
    let mails = app.take_mails();
    assert_eq!(mails.len(), 1);
    let (_, me) = app.get("/v1/me", &mailed_key(&mails[0])).await;
    assert_eq!(me["role"], "admin");

    pmo::ensure_admins(&app.state, &admins).await.unwrap();
    assert!(app.take_mails().is_empty());
}
