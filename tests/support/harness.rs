//! Integration harness: real HTTP Paperclip client against wiremock, an
//! in-memory Telegram fake, a real SQLite store in a temp dir.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bridge::config::{Config, Env};
use bridge::core::sender::{self, SenderSettings, TgSender};
use bridge::core::{Bridge, Ctx, dialog, outbox, sync};
use bridge::health::HealthTracker;
use bridge::paperclip::HttpPaperclipClient;
use bridge::retry::Backoff;
use bridge::store::{Delivery, Store};
use bridge::telegram::*;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{CHAT, USER};

#[derive(Debug, Clone, PartialEq)]
pub enum TgCall {
    Send {
        id: i64,
        html: String,
        keyboard: Option<InlineKeyboard>,
        force_reply: bool,
    },
    Edit {
        id: i64,
        html: String,
        keyboard: Option<InlineKeyboard>,
    },
    Answer {
        text: Option<String>,
    },
    Commands,
}

#[derive(Default)]
pub struct FakeTelegram {
    pub calls: Mutex<Vec<TgCall>>,
    next_id: AtomicI64,
    pub fail_sends: AtomicBool,
}

impl FakeTelegram {
    pub fn calls(&self) -> Vec<TgCall> {
        self.calls.lock().unwrap().clone()
    }

    /// Sent messages: (id, html, keyboard).
    pub fn sent(&self) -> Vec<(i64, String, Option<InlineKeyboard>)> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                TgCall::Send {
                    id, html, keyboard, ..
                } => Some((id, html, keyboard)),
                _ => None,
            })
            .collect()
    }

    pub fn prompts(&self) -> Vec<(i64, String)> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                TgCall::Send {
                    id,
                    html,
                    force_reply: true,
                    ..
                } => Some((id, html)),
                _ => None,
            })
            .collect()
    }

    /// Current text and keyboard of a message (after edits).
    pub fn message(&self, id: i64) -> Option<(String, Option<InlineKeyboard>)> {
        let mut cur = None;
        for c in self.calls() {
            match c {
                TgCall::Send {
                    id: i,
                    html,
                    keyboard,
                    ..
                } if i == id => cur = Some((html, keyboard)),
                TgCall::Edit {
                    id: i,
                    html,
                    keyboard,
                } if i == id => cur = Some((html, keyboard)),
                _ => {}
            }
        }
        cur
    }

    pub fn answers(&self) -> Vec<Option<String>> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                TgCall::Answer { text } => Some(text),
                _ => None,
            })
            .collect()
    }

    pub fn texts(&self) -> Vec<String> {
        self.sent().into_iter().map(|(_, h, _)| h).collect()
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
        html: &str,
        markup: Option<&ReplyMarkup>,
    ) -> Result<i64> {
        if self.fail_sends.load(Ordering::SeqCst) {
            return Err(TelegramError::Transient { reason: "connect" });
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1000;
        let (keyboard, force_reply) = match markup {
            Some(ReplyMarkup::Inline(k)) => (Some(k.clone()), false),
            Some(ReplyMarkup::ForceReply) => (None, true),
            None => (None, false),
        };
        self.calls.lock().unwrap().push(TgCall::Send {
            id,
            html: html.to_string(),
            keyboard,
            force_reply,
        });
        Ok(id)
    }

    async fn edit_message_text(
        &self,
        _chat: i64,
        id: i64,
        html: &str,
        keyboard: Option<&InlineKeyboard>,
    ) -> Result<()> {
        self.calls.lock().unwrap().push(TgCall::Edit {
            id,
            html: html.to_string(),
            keyboard: keyboard.cloned(),
        });
        Ok(())
    }

    async fn edit_reply_markup(
        &self,
        _chat: i64,
        _id: i64,
        _keyboard: Option<&InlineKeyboard>,
    ) -> Result<()> {
        Ok(())
    }

    async fn answer_callback_query(&self, _id: &str, text: Option<&str>) -> Result<()> {
        self.calls.lock().unwrap().push(TgCall::Answer {
            text: text.map(str::to_string),
        });
        Ok(())
    }

    async fn set_my_commands(&self, _chat: i64, _c: &[BotCommand]) -> Result<()> {
        self.calls.lock().unwrap().push(TgCall::Commands);
        Ok(())
    }

    async fn get_me(&self) -> Result<()> {
        Ok(())
    }
}

pub struct Harness {
    pub ctx: Ctx<HttpPaperclipClient, FakeTelegram>,
    pub tg: Arc<FakeTelegram>,
    pub dir: Arc<tempfile::TempDir>,
    pub env: HashMap<String, String>,
    sender_cancel: CancellationToken,
    next_update: AtomicI64,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.sender_cancel.cancel();
    }
}

impl Harness {
    pub async fn start(paperclip_uri: &str, overrides: &[(&str, &str)]) -> Self {
        let dir = Arc::new(tempfile::tempdir().unwrap());
        std::fs::write(dir.path().join("pc"), "pcp_board_placeholder\n").unwrap();
        std::fs::write(dir.path().join("tg"), "123456:placeholder\n").unwrap();
        let mut env: HashMap<String, String> = [
            ("PAPERCLIP_URL", paperclip_uri.to_string()),
            ("PAPERCLIP_COMPANY_IDS", "company-1".into()),
            ("TELEGRAM_CHAT_ID", CHAT.to_string()),
            ("TELEGRAM_USER_ID", USER.to_string()),
            ("TELEGRAM_PROXY_URL", "socks5h://127.0.0.1:9".into()),
            ("RETRY_BASE_DELAY", "1ms".into()),
            ("RETRY_MAX_DELAY", "100ms".into()),
            ("RETRY_NOTIFY_AFTER", "2".into()),
            ("PAPERCLIP_REQUEST_TIMEOUT", "3s".into()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        env.insert(
            "PAPERCLIP_TOKEN_FILE".into(),
            dir.path().join("pc").to_string_lossy().into(),
        );
        env.insert(
            "TELEGRAM_BOT_TOKEN_FILE".into(),
            dir.path().join("tg").to_string_lossy().into(),
        );
        env.insert(
            "STATE_PATH".into(),
            dir.path().join("bridge.db").to_string_lossy().into(),
        );
        env.insert(
            "HEALTH_FILE".into(),
            dir.path().join("health.json").to_string_lossy().into(),
        );
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
        let paperclip = Arc::new(HttpPaperclipClient::from_config(&cfg.paperclip).unwrap());
        let (sender_handle, rx) = TgSender::channel();
        let health = Arc::new(HealthTracker::new());
        let sender_cancel = CancellationToken::new();
        tokio::spawn(sender::run(
            tg.clone(),
            rx,
            SenderSettings {
                chat_id: CHAT,
                min_interval: Duration::ZERO,
                backoff: Backoff::new(Duration::from_millis(1), Duration::from_millis(5)),
                max_attempts: 2,
            },
            sender_cancel.clone(),
            health.clone(),
        ));
        let ctx = Bridge::new(
            Arc::new(cfg),
            paperclip,
            tg.clone(),
            store,
            sender_handle,
            health,
        );
        ctx.load_persisted().await.unwrap();
        Self {
            ctx,
            tg,
            dir,
            env,
            sender_cancel,
            next_update: AtomicI64::new(first_update),
        }
    }

    /// Simulates a process restart on the same state and chat.
    pub async fn restart(self) -> Self {
        let dir = self.dir.clone();
        let env = self.env.clone();
        let tg = self.tg.clone();
        let next = self.next_update.load(Ordering::SeqCst);
        drop(self);
        tokio::time::sleep(Duration::from_millis(20)).await;
        Self::build(dir, env, tg, next).await
    }

    pub async fn sync(&self) {
        sync::cycle(&self.ctx).await.unwrap();
    }

    pub async fn try_sync(&self) -> bridge::core::Result<()> {
        sync::cycle(&self.ctx).await
    }

    pub async fn outbox(&self) {
        outbox::process_due(&self.ctx, &CancellationToken::new())
            .await
            .unwrap();
    }

    pub async fn try_outbox(&self) -> bridge::core::Result<()> {
        outbox::process_due(&self.ctx, &CancellationToken::new()).await
    }

    pub async fn delivery(&self, interaction_id: &str) -> Delivery {
        self.ctx
            .store
            .delivery_by_interaction(interaction_id.into())
            .await
            .unwrap()
            .expect("delivery exists")
    }

    fn update_id(&self) -> i64 {
        self.next_update.fetch_add(1, Ordering::SeqCst)
    }

    pub async fn send_update(&self, update: Value) -> Update {
        let u: Update = serde_json::from_value(update).unwrap();
        dialog::handle_update(&self.ctx, &u).await.unwrap();
        u
    }

    pub fn callback_update(&self, data: &str, message_id: i64) -> Value {
        json!({"update_id": self.update_id(), "callback_query": {
            "id": format!("cq-{}", self.next_update.load(Ordering::SeqCst)),
            "from": {"id": USER, "is_bot": false, "first_name": "u"},
            "message": {"message_id": message_id, "chat": {"id": CHAT, "type": "private"}, "date": 0},
            "chat_instance": "x", "data": data}})
    }

    /// Presses a button: `data` is the full callback_data.
    pub async fn press(&self, data: &str) -> Update {
        let u = self.callback_update(data, 1);
        self.send_update(u).await
    }

    /// Presses the button whose caption starts with `caption` on the card.
    pub async fn press_button(&self, interaction_id: &str, caption: &str) -> Update {
        let d = self.delivery(interaction_id).await;
        let (_, kb) = self
            .tg
            .message(d.last_message_id().unwrap())
            .expect("card message");
        let kb = kb.expect("keyboard");
        let data = kb
            .inline_keyboard
            .iter()
            .flatten()
            .find(|b| b.text.starts_with(caption))
            .unwrap_or_else(|| panic!("no button {caption}: {kb:?}"))
            .callback_data
            .clone();
        self.press(&data).await
    }

    pub async fn text(&self, text: &str, reply_to: Option<i64>) -> Update {
        let mut msg = json!({"message_id": 9000 + self.next_update.load(Ordering::SeqCst),
            "from": {"id": USER, "is_bot": false, "first_name": "u"},
            "chat": {"id": CHAT, "type": "private"}, "date": 0, "text": text});
        if let Some(r) = reply_to {
            msg["reply_to_message"] =
                json!({"message_id": r, "chat": {"id": CHAT, "type": "private"}, "date": 0});
        }
        let u = json!({"update_id": self.update_id(), "message": msg});
        self.send_update(u).await
    }

    /// Current text of the card (last message) and its keyboard.
    pub async fn card(&self, interaction_id: &str) -> (String, Option<InlineKeyboard>) {
        let d = self.delivery(interaction_id).await;
        self.tg.message(d.last_message_id().unwrap()).unwrap()
    }

    pub fn buttons(kb: &Option<InlineKeyboard>) -> Vec<String> {
        kb.iter()
            .flat_map(|k| k.inline_keyboard.iter().flatten().map(|b| b.text.clone()))
            .collect()
    }
}
