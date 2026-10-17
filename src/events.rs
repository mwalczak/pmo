//! The action log: who (a user, an app or the system) did what to which record, when, and what changed.
use serde_json::{Map, Value, json};
use sqlx::PgExecutor;
use uuid::Uuid;

use crate::auth::Principal;

#[derive(Clone, Copy, Debug)]
pub enum Actor {
    User(Uuid),
    App(Uuid),
    /// Startup and command-line tasks.
    System,
}

impl From<&Principal> for Actor {
    fn from(p: &Principal) -> Self {
        match p {
            Principal::User(u) => Self::User(u.id),
            Principal::App(a) => Self::App(a.id),
        }
    }
}

/// An entry for the log, e.g. `Event::new("issue.updated", "issue", id).issue(id, "DM-12").changes(diff)`.
pub struct Event {
    action: &'static str,
    entity: &'static str,
    entity_id: Uuid,
    project_id: Option<Uuid>,
    issue: Option<(Uuid, String)>,
    changes: Value,
}

impl Event {
    pub fn new(action: &'static str, entity: &'static str, entity_id: Uuid) -> Self {
        Self { action, entity, entity_id, project_id: None, issue: None, changes: json!({}) }
    }

    pub fn project(mut self, project_id: Uuid) -> Self {
        self.project_id = Some(project_id);
        self
    }

    /// The issue the event belongs to, so it shows in that issue's history.
    pub fn issue(mut self, issue_id: Uuid, key: impl Into<String>) -> Self {
        self.issue = Some((issue_id, key.into()));
        self
    }

    pub fn changes(mut self, changes: Value) -> Self {
        self.changes = changes;
        self
    }

    pub async fn record(self, db: impl PgExecutor<'_>, actor: Actor) -> sqlx::Result<()> {
        let (user_id, app_id) = match actor {
            Actor::User(id) => (Some(id), None),
            Actor::App(id) => (None, Some(id)),
            Actor::System => (None, None),
        };
        let (issue_id, issue_key) = self.issue.unzip();
        sqlx::query(
            "INSERT INTO events (id, actor_user_id, actor_app_id, action, entity, entity_id, project_id, issue_id,
                                 issue_key, changes)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(Uuid::now_v7())
        .bind(user_id)
        .bind(app_id)
        .bind(self.action)
        .bind(self.entity)
        .bind(self.entity_id)
        .bind(self.project_id)
        .bind(issue_id)
        .bind(issue_key)
        .bind(self.changes)
        .execute(db)
        .await?;
        Ok(())
    }
}

/// Collects field changes as `{"field": {"from": old, "to": new}}`, leaving out unchanged fields.
#[derive(Default)]
pub struct Diff(Map<String, Value>);

impl Diff {
    pub fn field<T: serde::Serialize + PartialEq>(mut self, name: &str, from: &T, to: &T) -> Self {
        if from != to {
            self.0.insert(name.to_owned(), json!({ "from": from, "to": to }));
        }
        self
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn into_value(self) -> Value {
        Value::Object(self.0)
    }
}
