//! Bridge logic (ADR-0010 §2.2): discovery, outbound, sender and topics,
//! inbound, pc_outbox, start checks. Depends on `domain`, `store`, `render`
//! and the API traits only.

pub mod allowlist;
pub mod checks;
pub mod discovery;
pub mod inbound;
pub mod outbound;
pub mod outbox;
pub mod poller;
pub mod sender;
pub mod topics;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sha2::{Digest, Sha256};
use secrecy::ExposeSecret;
use tokio::sync::{Notify, mpsc};

use crate::config::Config;
use crate::health::HealthTracker;
use crate::paperclip::{PaperclipApi, PaperclipError};
use crate::render::RenderConfig;
use crate::store::{Store, StoreError};
use crate::telegram::{TelegramApi, TelegramError};
use crate::timeutil;

use allowlist::Allowlist;

/// Retry interval after `401` from Paperclip and `403` from Telegram.
pub const AUTH_RETRY_INTERVAL: Duration = Duration::from_secs(300);

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("telegram: {0}")]
    Telegram(#[from] TelegramError),
    #[error("paperclip: {0}")]
    Paperclip(#[from] PaperclipError),
    /// Injected crash (feature `fault-injection`).
    #[error("injected fault")]
    Fault,
}

pub type Result<T> = std::result::Result<T, CoreError>;

/// A bridge message for the group: to a topic or to General (`None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub thread_id: Option<i64>,
    pub html: String,
}

/// `<index>:<first 8 hex of sha256(key)>` (ADR-0010 §1.1). The key itself is
/// never stored.
pub fn key_ref(index: usize, token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let hex: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!("{index}:{hex}")
}

#[derive(Debug, Default)]
pub struct KeyState {
    pub rejected: bool,
    pub notified: bool,
}

#[derive(Default)]
pub struct Shared {
    /// company → agent id → name, refreshed by discovery.
    pub agents: Mutex<HashMap<String, HashMap<String, String>>>,
    /// Per configured key.
    pub keys: Mutex<Vec<KeyState>>,
    /// `404 Agent Chat is disabled` was told to General.
    pub agent_chat_notified: Mutex<bool>,
    /// Paperclip user that owns the conversations (`conversationUserId`) and
    /// the key that saw it first.
    pub owner: Mutex<Option<(String, usize)>>,
}

pub struct Bridge<P, T> {
    pub cfg: Arc<Config>,
    /// One client per board key, in configuration order.
    pub keys: Vec<Arc<P>>,
    pub key_refs: Vec<String>,
    pub telegram: Arc<T>,
    pub store: Store,
    pub health: Arc<HealthTracker>,
    notices: mpsc::UnboundedSender<Notice>,
    /// Wakes the sender: new topic, new pieces, a notice.
    pub sender_notify: Arc<Notify>,
    /// Wakes the pc_outbox.
    pub outbox_notify: Arc<Notify>,
    pub allowlist: Allowlist,
    pub render: RenderConfig,
    pub shared: Shared,
}

pub type Ctx<P, T> = Arc<Bridge<P, T>>;

impl<P: PaperclipApi, T: TelegramApi> Bridge<P, T> {
    /// Returns the context and the notice queue for [`sender::Sender`].
    pub fn new(
        cfg: Arc<Config>,
        keys: Vec<Arc<P>>,
        telegram: Arc<T>,
        store: Store,
        health: Arc<HealthTracker>,
    ) -> (Ctx<P, T>, mpsc::UnboundedReceiver<Notice>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let key_refs = cfg
            .paperclip
            .tokens
            .iter()
            .enumerate()
            .map(|(i, t)| key_ref(i, t.expose_secret()))
            .collect();
        let render = RenderConfig {
            max_chunks: cfg.max_message_chunks,
            ui_url: cfg.paperclip.ui_url.clone(),
        };
        let shared = Shared {
            keys: Mutex::new((0..keys.len()).map(|_| KeyState::default()).collect()),
            ..Shared::default()
        };
        let ctx = Arc::new(Self {
            allowlist: Allowlist::new(cfg.telegram.chat_id, cfg.telegram.allowed_user_ids.clone()),
            cfg,
            keys,
            key_refs,
            telegram,
            store,
            health,
            notices: tx,
            sender_notify: Arc::new(Notify::new()),
            outbox_notify: Arc::new(Notify::new()),
            render,
            shared,
        });
        (ctx, rx)
    }

    /// Queues a message for the group (best effort, not persisted).
    pub fn notify(&self, thread_id: Option<i64>, html: impl Into<String>) {
        let _ = self.notices.send(Notice {
            thread_id,
            html: html.into(),
        });
        self.sender_notify.notify_one();
    }

    /// Index of the key a conversation or company was found with: by the
    /// key hash, then by position; the first key otherwise.
    pub fn key_index(&self, key_ref: &str) -> usize {
        if let Some(i) = self.key_refs.iter().position(|r| r == key_ref) {
            return i;
        }
        let hash = key_ref.split(':').nth(1).unwrap_or_default();
        if let Some(i) = self
            .key_refs
            .iter()
            .position(|r| r.split(':').nth(1) == Some(hash))
        {
            return i;
        }
        key_ref
            .split(':')
            .next()
            .and_then(|i| i.parse::<usize>().ok())
            .filter(|i| *i < self.keys.len())
            .unwrap_or(0)
    }

    pub fn key(&self, index: usize) -> &Arc<P> {
        &self.keys[index.min(self.keys.len() - 1)]
    }

    /// A Paperclip call with key `index` succeeded.
    pub fn paperclip_ok(&self, index: usize) {
        self.health.paperclip_ok();
        let all_ok = match self.shared.keys.lock() {
            Ok(mut keys) => {
                if let Some(k) = keys.get_mut(index) {
                    if k.rejected {
                        tracing::info!(key = index, "paperclip key accepted again");
                    }
                    *k = KeyState::default();
                }
                keys.iter().all(|k| !k.rejected)
            }
            Err(_) => true,
        };
        self.health.set_paperclip_auth_ok(all_ok);
    }

    /// Key `index` got `401`: unhealthy, one message to General.
    pub fn key_rejected(&self, index: usize) {
        self.health.set_paperclip_auth_ok(false);
        let first = match self.shared.keys.lock() {
            Ok(mut keys) => match keys.get_mut(index) {
                Some(k) => {
                    k.rejected = true;
                    !std::mem::replace(&mut k.notified, true)
                }
                None => false,
            },
            Err(_) => false,
        };
        if first {
            tracing::error!(key = index, "paperclip rejected the board key (401)");
            self.notify(None, crate::render::notices::KEY_REJECTED);
        }
    }

    pub fn is_key_rejected(&self, index: usize) -> bool {
        self.shared
            .keys
            .lock()
            .ok()
            .and_then(|k| k.get(index).map(|s| s.rejected))
            .unwrap_or(false)
    }

    /// `hot_until` for activity now.
    pub fn hot_until(&self) -> String {
        timeutil::format(timeutil::add(timeutil::now(), self.cfg.hot_window))
    }

    pub fn agent_name(&self, company_id: &str, agent_id: &str) -> Option<String> {
        self.shared
            .agents
            .lock()
            .ok()?
            .get(company_id)?
            .get(agent_id)
            .cloned()
    }
}

/// Time until an RFC 3339 instant (zero when past or unparsable).
pub fn until(at: &str) -> Duration {
    timeutil::parse(at)
        .map(|t| t - timeutil::now())
        .and_then(|d| Duration::try_from(d).ok())
        .unwrap_or(Duration::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_ref_has_no_key_material() {
        let r = key_ref(1, "pcp_board_secret_value");
        assert!(r.starts_with("1:"));
        assert_eq!(r.len(), 2 + 8);
        assert!(!r.contains("pcp"));
        assert_eq!(r, key_ref(1, "pcp_board_secret_value"));
        assert_ne!(r, key_ref(1, "pcp_other"));
    }
}
