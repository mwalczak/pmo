//! E-mails to reporters, per project: the language, which changes are worth an e-mail, and who sends them.
use serde::{Deserialize, Serialize};
use sqlx::PgExecutor;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    config::Smtp,
    mail::{Mail, Sender},
    models::IssueStatus,
};

/// A project's notification settings (`project_notifications`, or the defaults).
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct Settings {
    pub key: String,
    pub name: String,
    pub language: String,
    pub notify_statuses: Vec<String>,
    pub notify_comments: bool,
    pub mail_from: Option<String>,
    pub smtp_host: Option<String>,
    pub smtp_port: Option<i32>,
    pub smtp_username: Option<String>,
}

impl Settings {
    pub async fn load(db: impl PgExecutor<'_>, project_id: Uuid) -> sqlx::Result<Self> {
        sqlx::query_as(
            "SELECT p.key, p.name, coalesce(n.language, 'en') AS language,
                    coalesce(n.notify_statuses, $2) AS notify_statuses,
                    coalesce(n.notify_comments, true) AS notify_comments,
                    n.mail_from, n.smtp_host, n.smtp_port, n.smtp_username
             FROM projects p LEFT JOIN project_notifications n ON n.project_id = p.id
             WHERE p.id = $1",
        )
        .bind(project_id)
        .bind(IssueStatus::ALL.map(|s| s.as_str().to_owned()).to_vec())
        .fetch_one(db)
        .await
    }

    /// The environment variable holding this project's SMTP password.
    pub fn password_variable(&self) -> String {
        format!("SMTP_PASSWORD_{}", self.key)
    }

    /// The project's own sender, when its address, server, user and password are all set.
    pub fn sender(&self) -> Option<Sender> {
        let password = std::env::var(self.password_variable()).ok().filter(|p| !p.trim().is_empty())?;
        Some(Sender {
            from: self.mail_from.clone()?,
            smtp: Smtp::Server {
                host: self.smtp_host.clone()?,
                port: self.smtp_port.unwrap_or(465) as u16,
                username: self.smtp_username.clone()?,
                password,
            },
        })
    }

    pub fn notifies_status(&self, status: IssueStatus) -> bool {
        self.notify_statuses.iter().any(|s| s == status.as_str())
    }

    fn polish(&self) -> bool {
        self.language == "pl"
    }

    fn mail(&self, to: &str, subject: String, body: String) -> Mail {
        Mail { to: to.to_owned(), subject, body, sender: self.sender() }
    }

    /// The reporter's issue changed status; `comment` is the public comment that came with the change.
    pub fn status_mail(
        &self,
        issue: &IssueMail,
        status: IssueStatus,
        duplicate_of: Option<&str>,
        comment: Option<&str>,
    ) -> Mail {
        let comment = comment.map_or_else(String::new, |c| format!("\n{c}\n"));
        if self.polish() {
            let duplicate = duplicate_of.map_or_else(String::new, |d| format!(" Duplikuje zgłoszenie {d}."));
            let label = polish_status(status);
            self.mail(
                &issue.reporter_email,
                format!("[{}] {}: {label}", issue.key, issue.title),
                format!(
                    "{}\n\nTwoje zgłoszenie {} „{}” ma nowy status: {label}.{duplicate}\n{comment}\nDziękujemy!\n{}\n",
                    greeting_pl(issue.reporter_name.as_deref()),
                    issue.key,
                    issue.title,
                    self.name
                ),
            )
        } else {
            let duplicate = duplicate_of.map_or_else(String::new, |d| format!(" It duplicates {d}."));
            let label = status.as_str().replace('_', " ");
            self.mail(
                &issue.reporter_email,
                format!("[{}] {}: {label}", issue.key, issue.title),
                format!("Your report {} \"{}\" is now: {label}.{duplicate}\n{comment}", issue.key, issue.title),
            )
        }
    }

    /// Someone else commented publicly on the reporter's issue.
    pub fn comment_mail(&self, issue: &IssueMail, body: &str, author: Option<&str>) -> Mail {
        if self.polish() {
            self.mail(
                &issue.reporter_email,
                format!("[{}] {}: nowy komentarz", issue.key, issue.title),
                format!(
                    "{}\n\n{} skomentował(a) Twoje zgłoszenie {} „{}”:\n\n{body}\n\n{}\n",
                    greeting_pl(issue.reporter_name.as_deref()),
                    author.unwrap_or("Zespół"),
                    issue.key,
                    issue.title,
                    self.name
                ),
            )
        } else {
            self.mail(
                &issue.reporter_email,
                format!("[{}] {}: new comment", issue.key, issue.title),
                format!(
                    "{} commented on your report {} \"{}\":\n\n{body}\n",
                    author.unwrap_or("Someone"),
                    issue.key,
                    issue.title
                ),
            )
        }
    }

    /// A first report created the reporter's account: their personal key. `via` is the app they reported through.
    pub fn welcome_mail(
        &self,
        public_url: Option<&str>,
        email: &str,
        name: Option<&str>,
        key: &str,
        via: Option<&str>,
    ) -> Mail {
        if !self.polish() {
            let mut mail = crate::mail::welcome_mail(public_url, email, name, key, via);
            mail.sender = self.sender();
            return mail;
        }
        let docs = public_url.map_or_else(String::new, |url| format!("\nDokumentacja API: {url}/docs\n"));
        let via = via.map_or_else(String::new, |app| format!(" w {app}"));
        self.mail(
            email,
            format!("Twój klucz do zgłoszeń – {}", self.name),
            format!(
                "{}\n\nDziękujemy za zgłoszenie{via}! Zgłoszenia trafiają do naszego trackera (PMO), więc założyliśmy \
                 Ci w nim konto. Napiszemy, gdy zgłoszenie zostanie załatwione.\n\nTwój osobisty klucz API:\n\n    {key}\n\n\
                 Dzięki niemu możesz śledzić swoje zgłoszenia i dodawać nowe, także przez swoich asystentów AI \
                 (nagłówek `Authorization: Bearer <klucz>`).\n{docs}\nNie udostępniaj go nikomu.\n\n{}\n",
                greeting_pl(name),
                self.name
            ),
        )
    }
}

/// What a notification needs to know about the issue.
pub struct IssueMail {
    pub key: String,
    pub title: String,
    pub reporter_email: String,
    pub reporter_name: Option<String>,
}

fn greeting_pl(name: Option<&str>) -> String {
    name.map_or_else(|| "Cześć!".to_owned(), |n| format!("Cześć {n}!"))
}

fn polish_status(status: IssueStatus) -> &'static str {
    match status {
        IssueStatus::Reported => "zgłoszone",
        IssueStatus::Accepted => "przyjęte",
        IssueStatus::InProgress => "w toku",
        IssueStatus::InReview => "w przeglądzie",
        IssueStatus::WaitingForRelease => "czeka na wydanie",
        IssueStatus::Done => "zrobione",
        IssueStatus::WontFix => "nie zrobimy",
        IssueStatus::Duplicate => "duplikat",
    }
}

/// Notification settings as the API shows and takes them.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct NotificationSettings {
    /// Language of the e-mails to reporters: `en` or `pl`.
    #[schema(example = "pl")]
    pub language: String,
    /// Status changes the reporter is e-mailed about (a comment given with the change is included).
    #[schema(example = json!(["done", "wont_fix"]))]
    pub statuses: Vec<IssueStatus>,
    /// Whether reporters are e-mailed about public comments by others.
    pub comments: bool,
    /// The project's own sender; `null` sends from the server's address.
    pub mail: Option<MailSettings>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct MailSettings {
    #[schema(example = "Dobre Mleko <kontakt@dobre-mleko.app>")]
    pub from: String,
    #[schema(example = "ssl0.ovh.net")]
    pub smtp_host: String,
    /// 465 (TLS, the default) or 587 (STARTTLS).
    pub smtp_port: Option<u16>,
    #[schema(example = "kontakt@dobre-mleko.app")]
    pub smtp_username: String,
    /// Read-only: the environment variable (a deploy secret) holding the password.
    #[serde(default)]
    pub password_variable: String,
    /// Read-only: whether that variable is set. Until it is, the server's sender is used.
    #[serde(default)]
    pub password_set: bool,
}

impl From<&Settings> for NotificationSettings {
    fn from(s: &Settings) -> Self {
        let mail = s.mail_from.clone().zip(s.smtp_host.clone()).zip(s.smtp_username.clone()).map(
            |((from, smtp_host), smtp_username)| MailSettings {
                from,
                smtp_host,
                smtp_port: s.smtp_port.map(|p| p as u16),
                smtp_username,
                password_variable: s.password_variable(),
                password_set: std::env::var(s.password_variable()).is_ok_and(|p| !p.trim().is_empty()),
            },
        );
        Self {
            language: s.language.clone(),
            statuses: IssueStatus::ALL.into_iter().filter(|st| s.notifies_status(*st)).collect(),
            comments: s.notify_comments,
            mail,
        }
    }
}
