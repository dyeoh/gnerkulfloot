//! Outgoing email: the `MailAdapter` trait and the emails the shop sends.
//!
//! Emails are never sent from inside a request. Code that wants an email sent
//! queues a job (see [`crate::jobs`]) in the same transaction as the change
//! that causes it, and a worker renders and sends it.

mod log;
mod smtp;
mod templates;

use std::sync::Arc;

use async_trait::async_trait;

pub use log::Log;
pub use smtp::Smtp;
pub use templates::{OrderEmail, render_order_email};

use crate::config::MailTransportConfig;

/// One email, ready to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Email {
    pub to: String,
    pub subject: String,
    pub text: String,
    pub html: String,
}

#[derive(Debug, thiserror::Error)]
pub enum MailError {
    #[error("invalid email address {0:?}")]
    InvalidAddress(String),
    #[error("couldn't build the email: {0}")]
    Build(String),
    #[error("mail server refused or unreachable: {0}")]
    Transport(String),
}

#[async_trait]
pub trait MailAdapter: Send + Sync {
    fn id(&self) -> &'static str;

    /// Sends one email. An error means it wasn't accepted and may be retried.
    async fn send(&self, email: &Email) -> Result<(), MailError>;
}

/// Builds the transport selected in config.
///
/// # Errors
/// Fails on an unparseable `from`/`reply_to` address or bad SMTP settings, so a
/// misconfigured shop stops at startup instead of silently not emailing.
pub fn from_config(cfg: &MailTransportConfig) -> Result<Arc<dyn MailAdapter>, MailError> {
    Ok(match cfg {
        MailTransportConfig::Log { from } => Arc::new(Log::new(from.clone())),
        MailTransportConfig::Smtp { .. } => Arc::new(Smtp::new(cfg)?),
    })
}
