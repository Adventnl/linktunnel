//! Attended-consent prompts for a SYSTEM agent: a Yes/No box on the console session.
//!
//! A SYSTEM agent's IPC socket is admin-only, so the usual "UI client answers" route does not
//! exist for the person sitting at the PC. `WTSSendMessage` reaches their desktop directly.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use nexus_core::i18n::{Locale, tr, tr_args};

use crate::consent::ConsentPrompter;

/// Asks on the active console session.
#[derive(Debug, Clone, Copy)]
pub struct WtsPrompter {
    locale: Locale,
}

impl WtsPrompter {
    /// A prompter that speaks `locale`.
    #[must_use]
    pub fn new(locale: Locale) -> Self {
        Self { locale }
    }
}

impl ConsentPrompter for WtsPrompter {
    fn ask(
        &self,
        peer_name: String,
        fingerprint: String,
        grants: Vec<String>,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = Option<bool>> + Send>> {
        let locale = self.locale;
        Box::pin(async move {
            let secs = timeout.as_secs().max(1);
            let title = tr(locale, "agent.consent.title");
            let body = tr_args(
                locale,
                "agent.consent.body",
                &[
                    ("name", &peer_name),
                    ("fingerprint", &fingerprint),
                    ("grants", &grants.join(", ")),
                    ("secs", &secs.to_string()),
                ],
            );
            tokio::task::spawn_blocking(move || {
                let session = nexus_winservice::launcher::active_console_session()?;
                nexus_winservice::prompt::ask(
                    session,
                    &title,
                    &body,
                    u32::try_from(secs).unwrap_or(30),
                )
            })
            .await
            .ok()
            .flatten()
        })
    }
}
