//! Bridge logic: sync, dialog, outbox, governed, runs, commands.
//! Depends on `domain`, `store`, `render` and the API traits only.

pub mod allowlist;
pub mod cards;
pub mod commands;
pub mod dialog;
pub mod forms;
pub mod governed;
pub mod outbox;
pub mod poller;
pub mod runs;
pub mod sender;
pub mod sync;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use time::OffsetDateTime;
use tokio::sync::Notify;

use crate::config::Config;
use crate::domain::RunFailure;
use crate::health::HealthTracker;
use crate::paperclip::{PaperclipApi, PaperclipError};
use crate::render::RenderConfig;
use crate::store::{Store, StoreError};
use crate::telegram::{TelegramApi, TelegramError};

use allowlist::Allowlist;
use sender::TgSender;

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

/// Paperclip availability as seen by the bridge (architecture §7.4).
#[derive(Debug, Default)]
pub struct PaperclipState {
    pub consecutive_failures: u32,
    pub down_since: Option<OffsetDateTime>,
    pub down_notified: bool,
    pub auth_failed: bool,
    pub auth_notified: bool,
}

/// (fetched at, agent id → name)
pub type AgentNames = (Instant, HashMap<String, String>);

#[derive(Default)]
pub struct Shared {
    /// company → (fetched at, agent id → name)
    pub agents: Mutex<HashMap<String, AgentNames>>,
    /// issue → last seen activity signature in the attention feed
    pub issue_activity: Mutex<HashMap<String, String>>,
    /// issue id → (identifier, title)
    pub issues: Mutex<HashMap<String, (String, Option<String>)>>,
    /// failed_run items of the attention feed, consumed by `runs`
    pub failed_runs: Mutex<Vec<RunFailure>>,
    pub paperclip: Mutex<PaperclipState>,
    /// user id of the board key owner, learned from our own answers
    pub self_user_id: Mutex<Option<String>>,
}

pub struct Bridge<P, T> {
    pub cfg: Arc<Config>,
    pub paperclip: Arc<P>,
    pub telegram: Arc<T>,
    pub store: Store,
    pub tg: TgSender,
    pub health: Arc<HealthTracker>,
    pub outbox_notify: Arc<Notify>,
    pub allowlist: Allowlist,
    pub render: RenderConfig,
    pub shared: Shared,
}

pub type Ctx<P, T> = Arc<Bridge<P, T>>;

impl<P: PaperclipApi, T: TelegramApi> Bridge<P, T> {
    pub fn new(
        cfg: Arc<Config>,
        paperclip: Arc<P>,
        telegram: Arc<T>,
        store: Store,
        tg: TgSender,
        health: Arc<HealthTracker>,
    ) -> Ctx<P, T> {
        let render = RenderConfig {
            max_chunks: cfg.max_message_chunks,
            ui_url: cfg.paperclip.ui_url.clone(),
            governed: cfg.governed,
        };
        Arc::new(Self {
            allowlist: Allowlist::new(cfg.telegram.chat_id, cfg.telegram.user_id),
            cfg,
            paperclip,
            telegram,
            store,
            tg,
            health,
            outbox_notify: Arc::new(Notify::new()),
            render,
            shared: Shared::default(),
        })
    }

    pub fn self_user_id(&self) -> Option<String> {
        self.shared.self_user_id.lock().ok()?.clone()
    }

    pub async fn learn_self_user_id(&self, id: &str) {
        let known = self
            .shared
            .self_user_id
            .lock()
            .map(|g| g.as_deref() == Some(id))
            .unwrap_or(true);
        if known {
            return;
        }
        if let Ok(mut g) = self.shared.self_user_id.lock() {
            *g = Some(id.to_string());
        }
        let _ = self
            .store
            .set_meta(crate::store::meta::SELF_USER_ID, id.to_string())
            .await;
    }

    pub async fn load_persisted(&self) -> Result<()> {
        if let Some(id) = self
            .store
            .get_meta(crate::store::meta::SELF_USER_ID)
            .await?
            && let Ok(mut g) = self.shared.self_user_id.lock()
        {
            *g = Some(id);
        }
        Ok(())
    }

    pub fn paperclip_available(&self) -> bool {
        self.shared
            .paperclip
            .lock()
            .map(|s| !s.down_notified && !s.auth_failed)
            .unwrap_or(true)
    }

    /// Agent name from the 10-minute cache (refreshed lazily).
    pub async fn agent_name(&self, company_id: &str, agent_id: Option<&str>) -> Option<String> {
        let agent_id = agent_id?;
        let fresh = |m: &HashMap<String, AgentNames>| {
            m.get(company_id)
                .filter(|(at, _)| at.elapsed() < AGENT_CACHE_TTL)
                .map(|(_, names)| names.get(agent_id).cloned())
        };
        if let Some(hit) = self.shared.agents.lock().ok().and_then(|m| fresh(&m)) {
            return hit;
        }
        match self.paperclip.agents(company_id).await {
            Ok(list) => {
                let names: HashMap<String, String> =
                    list.into_iter().map(|a| (a.id, a.name)).collect();
                let name = names.get(agent_id).cloned();
                if let Ok(mut m) = self.shared.agents.lock() {
                    m.insert(company_id.to_string(), (Instant::now(), names));
                }
                name
            }
            Err(e) => {
                tracing::debug!(error = %e, "agent list unavailable");
                self.shared.agents.lock().ok().and_then(|m| {
                    m.get(company_id)
                        .and_then(|(_, n)| n.get(agent_id).cloned())
                })
            }
        }
    }

    pub fn remember_issue(&self, issue_id: &str, identifier: Option<&str>, title: Option<&str>) {
        if let (Some(ident), Ok(mut m)) = (identifier, self.shared.issues.lock()) {
            m.insert(
                issue_id.to_string(),
                (ident.to_string(), title.map(str::to_string)),
            );
        }
    }

    pub fn issue_ref(&self, issue_id: &str) -> Option<String> {
        self.shared
            .issues
            .lock()
            .ok()?
            .get(issue_id)
            .map(|(r, _)| r.clone())
    }
}

pub const AGENT_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(600);
