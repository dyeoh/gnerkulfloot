//! The `log` mail adapter: logs emails instead of sending them, and keeps
//! them in memory so tests can read them back.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::{Email, MailAdapter, MailError};

#[derive(Clone)]
pub struct Log {
    from: String,
    sent: Arc<Mutex<Vec<Email>>>,
}

impl Log {
    pub fn new(from: String) -> Self {
        Self {
            from,
            sent: Arc::default(),
        }
    }

    /// Everything "sent" so far, oldest first.
    pub fn outbox(&self) -> Vec<Email> {
        self.sent.lock().expect("outbox lock").clone()
    }
}

#[async_trait]
impl MailAdapter for Log {
    fn id(&self) -> &'static str {
        "log"
    }

    async fn send(&self, email: &Email) -> Result<(), MailError> {
        tracing::info!(from = %self.from, to = %email.to, subject = %email.subject, "email (log adapter, not sent)\n{}", email.text);
        self.sent.lock().expect("outbox lock").push(email.clone());
        Ok(())
    }
}
