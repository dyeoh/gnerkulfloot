//! The `smtp` mail adapter. Works with any SMTP server; for Google Workspace
//! use `smtp.gmail.com:587` with an app password, or Workspace's SMTP relay.

use async_trait::async_trait;
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{Mailbox, MultiPart},
    transport::smtp::authentication::Credentials,
};

use super::{Email, MailAdapter, MailError};
use crate::config::{MailTransportConfig, SmtpSecurity};

pub struct Smtp {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
    reply_to: Option<Mailbox>,
}

fn mailbox(address: &str) -> Result<Mailbox, MailError> {
    address
        .parse()
        .map_err(|_| MailError::InvalidAddress(address.to_owned()))
}

impl Smtp {
    /// # Panics
    /// If called with a non-SMTP config; [`super::from_config`] never does.
    pub fn new(cfg: &MailTransportConfig) -> Result<Self, MailError> {
        let MailTransportConfig::Smtp {
            host,
            port,
            username,
            password,
            security,
            from,
            reply_to,
        } = cfg
        else {
            panic!("Smtp::new needs an smtp config");
        };
        let transport_err = |e: lettre::transport::smtp::Error| MailError::Transport(e.to_string());
        let mut builder = match security {
            SmtpSecurity::Starttls => {
                AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host).map_err(transport_err)?
            }
            SmtpSecurity::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(host).map_err(transport_err)?,
            SmtpSecurity::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host),
        }
        .port(*port);
        if let (Some(user), Some(pass)) = (username, password) {
            builder = builder.credentials(Credentials::new(user.clone(), pass.clone()));
        }
        Ok(Self {
            transport: builder.build(),
            from: mailbox(from)?,
            reply_to: reply_to.as_deref().map(mailbox).transpose()?,
        })
    }
}

#[async_trait]
impl MailAdapter for Smtp {
    fn id(&self) -> &'static str {
        "smtp"
    }

    async fn send(&self, email: &Email) -> Result<(), MailError> {
        let mut builder = Message::builder()
            .from(self.from.clone())
            .to(mailbox(&email.to)?)
            .subject(&email.subject);
        if let Some(reply_to) = &self.reply_to {
            builder = builder.reply_to(reply_to.clone());
        }
        let message = builder
            .multipart(MultiPart::alternative_plain_html(
                email.text.clone(),
                email.html.clone(),
            ))
            .map_err(|e| MailError::Build(e.to_string()))?;
        self.transport
            .send(message)
            .await
            .map_err(|e| MailError::Transport(e.to_string()))?;
        Ok(())
    }
}
