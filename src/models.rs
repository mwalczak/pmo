//! Enums shared by the database and the API.
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "user_role", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Manages everything: users, apps, projects, milestones and issue triage.
    Admin,
    /// Reports issues and edits the title and description of their own.
    Reporter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "issue_type", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum IssueType {
    Bug,
    Idea,
    /// Abuse reports are private: only admins and the reporter see them.
    Abuse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "issue_priority")]
pub enum Priority {
    P1,
    P2,
    P3,
    P4,
    P5,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "issue_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum IssueStatus {
    Reported,
    Accepted,
    InProgress,
    Done,
    WontFix,
    /// Set together with `duplicate_of`.
    Duplicate,
}

impl IssueStatus {
    pub const ALL: [Self; 6] =
        [Self::Reported, Self::Accepted, Self::InProgress, Self::Done, Self::WontFix, Self::Duplicate];
    pub const OPEN: [Self; 3] = [Self::Reported, Self::Accepted, Self::InProgress];
    pub const CLOSED: [Self; 3] = [Self::Done, Self::WontFix, Self::Duplicate];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reported => "reported",
            Self::Accepted => "accepted",
            Self::InProgress => "in_progress",
            Self::Done => "done",
            Self::WontFix => "wont_fix",
            Self::Duplicate => "duplicate",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "milestone_state", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum MilestoneState {
    Open,
    Closed,
}

/// Distinguishes "field absent" (`None`) from "field set to null" (`Some(None)`) in PATCH bodies.
pub fn double_option<'de, T, D>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

/// Trims a string, turning blank into `None`.
pub fn non_blank(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}
