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

impl Mailer {
    pub fn from_config(smtp: Option<&Smtp>, from: &str) -> Result<Self> {
        type Transport = AsyncSmtpTransport<Tokio1Executor>;
        let transport = match smtp {
            None => return Ok(Self::Log),
            Some(Smtp::Url(url)) => Transport::from_url(url)?.build(),
            Some(Smtp::Server { host, port, username, password }) => {
                let builder = if *port == 465 { Transport::relay(host)? } else { Transport::starttls_relay(host)? };
                builder.port(*port).credentials(Credentials::new(username.clone(), password.clone())).build()
            }
        };
        Ok(Self::Smtp { transport, from: from.to_owned() })
    }

    /// Whether e-mails actually go out (or, in tests, are collected).
    pub fn delivers(&self) -> bool {
        !matches!(self, Self::Log)
    }

    pub fn send(&self, mail: Mail) {
        match self {
            Self::Smtp { transport, from } => {
                let (transport, from) = (transport.clone(), from.clone());
                tokio::spawn(async move {
                    let to = mail.to.clone();
                    if let Err(e) = send_smtp(&transport, &from, mail).await {
                        tracing::error!(error = %e, %to, "sending e-mail failed");
                    }
                });
            }
            Self::Log => {
                tracing::warn!(to = %mail.to, subject = %mail.subject, "SMTP is not configured; e-mail not sent");
                tracing::debug!(body = %mail.body, "unsent e-mail");
            }
            Self::Memory(sent) => sent.lock().unwrap().push(mail),
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
