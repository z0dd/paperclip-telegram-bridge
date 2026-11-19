//! Integration harness: real HTTP Paperclip clients against the stateful
//! mock, an in-memory forum supergroup, a real SQLite store in a temp dir.
//! Steps are driven explicitly: `cycle` (discovery + outbound), `drain`
//! (sender), `outbox` (pc_outbox), `update` (inbound).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bridge::config::{Config, Env};
use bridge::core::discovery::{self, DiscoveryState};
use bridge::core::sender::Sender;
use bridge::core::{Bridge, Ctx, inbound, outbox};
use bridge::health::HealthTracker;
use bridge::paperclip::HttpPaperclipClient;
use bridge::store::Store;
use bridge::telegram::*;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::paperclip::{KEY, PaperclipMock};

/// The forum supergroup and the allowed user.
pub const GROUP: i64 = -1001234567890;
pub const USER: i64 = 111;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    pub id: i64,
    pub thread: Option<i64>,
    pub text: String,
    pub html: bool,
}

#[derive(Debug)]
pub struct TgState {
    pub sent: Vec<Sent>,
    /// (thread id, name) in creation order.
    pub topics: Vec<(i64, String)>,
    pub deleted: HashSet<i64>,
    pub closed: HashSet<i64>,
    pub reopened: Vec<i64>,
    /// Errors returned by the next `sendMessage` calls.
    pub send_errors: VecDeque<TelegramError>,
    /// Errors returned by the next `createForumTopic` calls.
    pub topic_errors: VecDeque<TelegramError>,
    /// Every HTML message fails with `can't parse entities`.
    pub reject_html: bool,
    pub chat: ChatInfo,
    pub member: MemberInfo,
    next_thread: i64,
    next_message: i64,
}

impl Default for TgState {
    fn default() -> Self {
        Self {
            sent: Vec::new(),
            topics: Vec::new(),
            deleted: HashSet::new(),
            closed: HashSet::new(),
            reopened: Vec::new(),
            send_errors: VecDeque::new(),
            topic_errors: VecDeque::new(),
            reject_html: false,
            chat: ChatInfo {
                kind: "supergroup".into(),
                is_forum: true,
            },
            member: MemberInfo {
                status: "administrator".into(),
                can_manage_topics: true,
            },
            next_thread: 100,
            next_message: 1000,
        }
    }
}

#[derive(Default)]
pub struct FakeTelegram {
    pub state: Mutex<TgState>,
}

impl FakeTelegram {
    pub fn st(&self) -> std::sync::MutexGuard<'_, TgState> {
        self.state.lock().unwrap()
    }

    pub fn sent(&self) -> Vec<Sent> {
        self.st().sent.clone()
    }

    /// Texts sent to a topic.
    pub fn in_topic(&self, thread: i64) -> Vec<String> {
        self.sent()
            .into_iter()
            .filter(|s| s.thread == Some(thread))
            .map(|s| s.text)
            .collect()
    }

    pub fn general(&self) -> Vec<String> {
        self.sent()
            .into_iter()
            .filter(|s| s.thread.is_none())
            .map(|s| s.text)
            .collect()
    }

    pub fn topics(&self) -> Vec<(i64, String)> {
        self.st().topics.clone()
    }

    /// Thread id of the topic with this name (the newest one).
    pub fn topic(&self, name: &str) -> Option<i64> {
        self.topics()
            .into_iter()
            .rev()
            .find(|(_, n)| n == name)
            .map(|(t, _)| t)
    }
}

impl TelegramApi for FakeTelegram {
    async fn get_updates(&self, _offset: i64, timeout: Duration) -> Result<Vec<Update>> {
        tokio::time::sleep(timeout).await;
        Ok(Vec::new())
    }

    async fn send_message(
        &self,
        _chat: i64,
        thread: Option<i64>,
        text: &str,
        html: bool,
    ) -> Result<i64> {
        let mut st = self.st();
        if let Some(e) = st.send_errors.pop_front() {
            return Err(e);
        }
        if let Some(t) = thread {
            if st.deleted.contains(&t) {
                return Err(TelegramError::ThreadNotFound);
            }
            if st.closed.contains(&t) {
                return Err(TelegramError::TopicClosed);
            }
        }
        if html && st.reject_html {
            return Err(TelegramError::CantParseEntities);
        }
        st.next_message += 1;
        let id = st.next_message;
        st.sent.push(Sent {
            id,
            thread,
            text: text.to_string(),
            html,
        });
        Ok(id)
    }

    async fn create_forum_topic(&self, _chat: i64, name: &str) -> Result<i64> {
        let mut st = self.st();
        if let Some(e) = st.topic_errors.pop_front() {
            return Err(e);
        }
        st.next_thread += 1;
        let t = st.next_thread;
        st.topics.push((t, name.to_string()));
        Ok(t)
    }

    async fn reopen_forum_topic(&self, _chat: i64, thread: i64) -> Result<()> {
        let mut st = self.st();
        if st.deleted.contains(&thread) {
            return Err(TelegramError::ThreadNotFound);
        }
        st.closed.remove(&thread);
        st.reopened.push(thread);
        Ok(())
    }

    async fn get_me(&self) -> Result<i64> {
        Ok(42)
    }

    async fn get_chat(&self, _chat: i64) -> Result<ChatInfo> {
        Ok(self.st().chat.clone())
    }

    async fn get_chat_member(&self, _chat: i64, _user: i64) -> Result<MemberInfo> {
        Ok(self.st().member.clone())
    }
}

pub type TestCtx = Ctx<HttpPaperclipClient, FakeTelegram>;

pub struct Harness {
    pub ctx: TestCtx,
    pub tg: Arc<FakeTelegram>,
    pub dir: Arc<tempfile::TempDir>,
    pub env: HashMap<String, String>,
    sender: tokio::sync::Mutex<Sender<HttpPaperclipClient, FakeTelegram>>,
    pub discovery: tokio::sync::Mutex<DiscoveryState>,
    next_update: AtomicI64,
}

impl Harness {
    pub async fn start(pc: &PaperclipMock, overrides: &[(&str, &str)]) -> Self {
        Self::start_keys(pc, &[KEY], overrides).await
    }

    /// One key → `PAPERCLIP_TOKEN_FILE`, several → `PAPERCLIP_TOKEN_FILES`.
    pub async fn start_keys(pc: &PaperclipMock, keys: &[&str], overrides: &[(&str, &str)]) -> Self {
        let dir = Arc::new(tempfile::tempdir().unwrap());
        let key_files: Vec<String> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| {
                let p = dir.path().join(format!("pc{i}"));
                std::fs::write(&p, format!("{k}\n")).unwrap();
                p.to_string_lossy().into_owned()
            })
            .collect();
        std::fs::write(dir.path().join("tg"), "123456:placeholder\n").unwrap();
        let mut env: HashMap<String, String> = [
            ("PAPERCLIP_URL", pc.uri()),
            ("TELEGRAM_CHAT_ID", GROUP.to_string()),
            ("TELEGRAM_ALLOWED_USER_IDS", USER.to_string()),
            ("TELEGRAM_PROXY_URL", "socks5h://127.0.0.1:9".into()),
            ("RETRY_BASE_DELAY", "1ms".into()),
            ("RETRY_MAX_DELAY", "5ms".into()),
            ("RETRY_NOTIFY_AFTER", "2".into()),
            ("RETRY_MAX_ATTEMPTS", "3".into()),
            ("PAPERCLIP_REQUEST_TIMEOUT", "3s".into()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        if key_files.len() == 1 {
            env.insert("PAPERCLIP_TOKEN_FILE".into(), key_files[0].clone());
        } else {
            env.insert("PAPERCLIP_TOKEN_FILES".into(), key_files.join(","));
        }
        for (k, v) in [
            ("TELEGRAM_BOT_TOKEN_FILE", "tg"),
            ("STATE_PATH", "bridge.db"),
            ("HEALTH_FILE", "health.json"),
        ] {
            env.insert(k.into(), dir.path().join(v).to_string_lossy().into());
        }
        for (k, v) in overrides {
            env.insert(k.to_string(), v.to_string());
        }
        Self::build(dir, env, Arc::new(FakeTelegram::default()), 1).await
    }

    async fn build(
        dir: Arc<tempfile::TempDir>,
        env: HashMap<String, String>,
        tg: Arc<FakeTelegram>,
        first_update: i64,
    ) -> Self {
        let cfg = Config::from_env(&Env::new(&env)).unwrap();
        let store = Store::open(&cfg.state_path).unwrap();
        store.recover().await.unwrap();
        let keys = HttpPaperclipClient::from_config(&cfg.paperclip)
            .unwrap()
            .into_iter()
            .map(Arc::new)
            .collect();
        let health = Arc::new(HealthTracker::new());
        let (ctx, notices) = Bridge::new(Arc::new(cfg), keys, tg.clone(), store, health);
        let sender = Sender::new(ctx.clone(), notices);
        Self {
            ctx,
            tg,
            dir,
            env,
            sender: tokio::sync::Mutex::new(sender),
            discovery: tokio::sync::Mutex::new(DiscoveryState::default()),
            next_update: AtomicI64::new(first_update),
        }
    }

    /// Simulates a process restart on the same state and the same group.
    pub async fn restart(self) -> Self {
        let dir = self.dir.clone();
        let env = self.env.clone();
        let tg = self.tg.clone();
        let next = self.next_update.load(Ordering::SeqCst);
        drop(self);
        tokio::time::sleep(Duration::from_millis(20)).await;
        Self::build(dir, env, tg, next).await
    }

    /// One discovery + outbound pass (companies are rediscovered every time
    /// and per-company backoff is cleared).
    pub async fn cycle(&self) {
        self.try_cycle().await.unwrap();
    }

    pub async fn try_cycle(&self) -> bridge::core::Result<()> {
        let mut d = self.discovery.lock().await;
        d.force_discovery();
        d.reset_backoff();
        discovery::cycle(&self.ctx, &mut d).await
    }

    /// Runs the sender until it has nothing to do now.
    pub async fn drain(&self) {
        self.try_drain().await.unwrap();
    }

    pub async fn try_drain(&self) -> bridge::core::Result<()> {
        let mut s = self.sender.lock().await;
        s.resume();
        s.drain().await
    }

    /// `cycle` then `drain`.
    pub async fn sync(&self) {
        self.cycle().await;
        self.drain().await;
    }

    pub async fn outbox(&self) {
        self.try_outbox().await.unwrap();
    }

    pub async fn try_outbox(&self) -> bridge::core::Result<()> {
        outbox::process_due(&self.ctx, &CancellationToken::new()).await
    }

    pub fn next_update_id(&self) -> i64 {
        self.next_update.fetch_add(1, Ordering::SeqCst)
    }

    pub async fn update(&self, update: Value) -> Update {
        let u: Update = serde_json::from_value(update).unwrap();
        inbound::handle_update(&self.ctx, &u).await.unwrap();
        u
    }

    /// A message update; `thread = None` is General.
    pub fn message(&self, thread: Option<i64>, from: i64, chat: i64, fields: Value) -> Value {
        let id = self.next_update_id();
        let mut m = json!({"message_id": 5000 + id, "from": {"id": from, "is_bot": false, "first_name": "u"},
            "chat": {"id": chat, "type": "supergroup", "is_forum": true}, "date": 0});
        if let Some(t) = thread {
            m["message_thread_id"] = json!(t);
            m["is_topic_message"] = json!(true);
        }
        if let (Some(obj), Some(extra)) = (m.as_object_mut(), fields.as_object()) {
            for (k, v) in extra {
                obj.insert(k.clone(), v.clone());
            }
        }
        json!({"update_id": id, "message": m})
    }

    /// The allowed user writes `text` in a topic.
    pub async fn say(&self, thread: i64, text: &str) -> Update {
        let u = self.message(Some(thread), USER, GROUP, json!({"text": text}));
        self.update(u).await
    }
}
