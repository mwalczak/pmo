use std::sync::{Arc, Mutex};

use anyhow::Result;
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor, message::header::ContentType,
    transport::smtp::authentication::Credentials,
};

use crate::config::Smtp;

#[derive(Clone, Debug)]
pub struct Mail {
    pub to: String,
    pub subject: String,
    pub body: String,
    /// A project's own sender; `None` sends from the server's address.
    pub sender: Option<Sender>,
}

/// Who sends an e-mail: the From address and the SMTP server that may send for it.
#[derive(Clone, Debug)]
pub struct Sender {
    pub from: String,
    pub smtp: Smtp,
}

/// Sends e-mails in the background, so a slow SMTP server never delays a response.
#[derive(Clone)]
pub enum Mailer {
    Smtp {
        transport: AsyncSmtpTransport<Tokio1Executor>,
        from: String,
    },
    /// No SMTP configured: e-mails are logged (bodies only at debug level, as they can contain keys).
    Log,
    /// Collects e-mails, for tests.
    Memory(Arc<Mutex<Vec<Mail>>>),
}

type Transport = AsyncSmtpTransport<Tokio1Executor>;

fn transport(smtp: &Smtp) -> Result<Transport> {
    Ok(match smtp {
        Smtp::Url(url) => Transport::from_url(url)?.build(),
        Smtp::Server { host, port, username, password } => {
            let builder = if *port == 465 { Transport::relay(host)? } else { Transport::starttls_relay(host)? };
            builder.port(*port).credentials(Credentials::new(username.clone(), password.clone())).build()
        }
    })
}

/// Sends one e-mail in the background, logging failures.
fn spawn_send(transport: Transport, from: String, mail: Mail) {
    tokio::spawn(async move {
        let to = mail.to.clone();
        if let Err(e) = send_smtp(&transport, &from, mail).await {
            tracing::error!(error = %e, %to, %from, "sending e-mail failed");
        }
    });
}

impl Mailer {
    pub fn from_config(smtp: Option<&Smtp>, from: &str) -> Result<Self> {
        Ok(match smtp {
            None => Self::Log,
            Some(smtp) => Self::Smtp { transport: transport(smtp)?, from: from.to_owned() },
        })
    }

    /// Whether e-mails actually go out (or, in tests, are collected).
    pub fn delivers(&self) -> bool {
        !matches!(self, Self::Log)
    }

    pub fn send(&self, mail: Mail) {
        if let Self::Memory(sent) = self {
            sent.lock().unwrap().push(mail);
            return;
        }
        // A project's own sender goes through its server, whatever the server's default is.
        if let Some(sender) = mail.sender.clone() {
            match transport(&sender.smtp) {
                Ok(transport) => spawn_send(transport, sender.from, mail),
                Err(e) => tracing::error!(error = %e, from = %sender.from, "project SMTP settings are invalid"),
            }
            return;
        }
        match self {
            Self::Smtp { transport, from } => spawn_send(transport.clone(), from.clone(), mail),
            Self::Log => {
                tracing::warn!(to = %mail.to, subject = %mail.subject, "SMTP is not configured; e-mail not sent");
                tracing::debug!(body = %mail.body, "unsent e-mail");
            }
            Self::Memory(_) => unreachable!("handled above"),
        }
    }
}

async fn send_smtp(transport: &AsyncSmtpTransport<Tokio1Executor>, from: &str, mail: Mail) -> Result<()> {
    let message = Message::builder()
        .from(from.parse()?)
        .to(mail.to.parse()?)
        .subject(mail.subject)
        .header(ContentType::TEXT_PLAIN)
        .body(mail.body)?;
    transport.send(message).await?;
    Ok(())
}

/// The e-mail that hands a new user their key. `via` is the app they reported through.
pub fn welcome_mail(public_url: Option<&str>, email: &str, name: Option<&str>, key: &str, via: Option<&str>) -> Mail {
    let greeting = name.map_or_else(|| "Hi,".to_owned(), |n| format!("Hi {n},"));
    let reason = match via {
        Some(app) => {
            format!("You reported an issue through {app}. It is tracked in PMO, so an account was created for you.")
        }
        None => "An account was created for you in PMO.".to_owned(),
    };
    let docs = public_url.map_or_else(String::new, |url| format!("\nAPI documentation: {url}/docs\n"));
    Mail {
        sender: None,
        to: email.to_owned(),
        subject: "Your PMO API key".to_owned(),
        body: format!(
            "{greeting}\n\n{reason}\n\nYour personal API key:\n\n    {key}\n\n\
             Use it to follow your reports and file new ones from other apps or your AI agents, by sending\n\
             the header `Authorization: Bearer <key>` with each request.\n{docs}\n\
             Keep this key secret. You can create more keys and revoke this one through the API (/v1/me/keys).\n"
        ),
    }
}
