//! Single outgoing queue to Telegram (architecture §7.3): FIFO, at most one
//! message per `min_interval`, `retry_after` pauses the whole queue.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::health::HealthTracker;
use crate::retry::{Backoff, rate_limit_jitter};
use crate::telegram::{BotCommand, InlineKeyboard, ReplyMarkup, TelegramApi, TelegramError};

#[derive(Debug)]
enum TgOp {
    Send {
        html: String,
        markup: Option<ReplyMarkup>,
    },
    Edit {
        message_id: i64,
        html: String,
        keyboard: Option<InlineKeyboard>,
    },
    Answer {
        id: String,
        text: Option<String>,
    },
    Commands(Vec<BotCommand>),
}

impl TgOp {
    /// Counts against the per-chat message rate.
    fn is_chat_message(&self) -> bool {
        matches!(self, TgOp::Send { .. } | TgOp::Edit { .. })
    }
}

pub struct Job {
    op: TgOp,
    reply: oneshot::Sender<Result<i64, TelegramError>>,
}

#[derive(Clone)]
pub struct TgSender {
    tx: mpsc::UnboundedSender<Job>,
}

const SHUTDOWN: TelegramError = TelegramError::Transient { reason: "shutdown" };

impl TgSender {
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<Job>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx }, rx)
    }

    async fn run(&self, op: TgOp) -> Result<i64, TelegramError> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(Job { op, reply }).map_err(|_| SHUTDOWN)?;
        rx.await.map_err(|_| SHUTDOWN)?
    }

    /// Returns the new `message_id`.
    pub async fn send(
        &self,
        html: String,
        markup: Option<ReplyMarkup>,
    ) -> Result<i64, TelegramError> {
        self.run(TgOp::Send { html, markup }).await
    }

    pub async fn edit(
        &self,
        message_id: i64,
        html: String,
        keyboard: Option<InlineKeyboard>,
    ) -> Result<(), TelegramError> {
        self.run(TgOp::Edit {
            message_id,
            html,
            keyboard,
        })
        .await
        .map(|_| ())
    }

    pub async fn answer(&self, id: String, text: Option<String>) -> Result<(), TelegramError> {
        self.run(TgOp::Answer { id, text }).await.map(|_| ())
    }

    pub async fn set_commands(&self, commands: Vec<BotCommand>) -> Result<(), TelegramError> {
        self.run(TgOp::Commands(commands)).await.map(|_| ())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SenderSettings {
    pub chat_id: i64,
    pub min_interval: Duration,
    pub backoff: Backoff,
    pub max_attempts: u32,
}

async fn exec<T: TelegramApi>(api: &T, chat_id: i64, op: &TgOp) -> Result<i64, TelegramError> {
    match op {
        TgOp::Send { html, markup } => api.send_message(chat_id, html, markup.as_ref()).await,
        TgOp::Edit {
            message_id,
            html,
            keyboard,
        } => api
            .edit_message_text(chat_id, *message_id, html, keyboard.as_ref())
            .await
            .map(|_| *message_id),
        TgOp::Answer { id, text } => api
            .answer_callback_query(id, text.as_deref())
            .await
            .map(|_| 0),
        TgOp::Commands(c) => api.set_my_commands(chat_id, c).await.map(|_| 0),
    }
}

pub async fn run<T: TelegramApi>(
    api: Arc<T>,
    mut rx: mpsc::UnboundedReceiver<Job>,
    s: SenderSettings,
    cancel: CancellationToken,
    health: Arc<HealthTracker>,
) {
    let mut last_message: Option<Instant> = None;
    loop {
        let job = tokio::select! {
            _ = cancel.cancelled() => break,
            j = rx.recv() => match j { Some(j) => j, None => break },
        };
        if job.op.is_chat_message()
            && let Some(at) = last_message
        {
            let ready = at + s.min_interval;
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep_until(ready) => {}
            }
        }
        let mut attempts = 0u32;
        let result = loop {
            let r = exec(api.as_ref(), s.chat_id, &job.op).await;
            match r {
                Err(TelegramError::RateLimited { retry_after }) => {
                    tracing::warn!(
                        retry_after_s = retry_after.as_secs(),
                        "telegram rate limit, pausing queue"
                    );
                    tokio::select! {
                        _ = cancel.cancelled() => break Err(SHUTDOWN),
                        _ = tokio::time::sleep(retry_after + rate_limit_jitter()) => continue,
                    }
                }
                Err(e) if e.is_transient() && attempts + 1 < s.max_attempts => {
                    attempts += 1;
                    tracing::debug!(
                        class = e.class(),
                        attempts,
                        "telegram call failed, retrying"
                    );
                    tokio::select! {
                        _ = cancel.cancelled() => break Err(SHUTDOWN),
                        _ = tokio::time::sleep(s.backoff.delay(attempts)) => continue,
                    }
                }
                r => break r,
            }
        };
        if job.op.is_chat_message() {
            last_message = Some(Instant::now());
        }
        match &result {
            Ok(_) => health.telegram_ok(),
            Err(e) => tracing::warn!(class = e.class(), "telegram call failed"),
        }
        let _ = job.reply.send(result);
    }
}
