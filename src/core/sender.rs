//! The single queue of chat-changing Telegram calls (ADR-0010 §2.2, §2.6):
//! bridge notices, `createForumTopic` / `reopenForumTopic` and the pieces of
//! agent replies from `tg_outbox`. At most one call per
//! `TELEGRAM_GROUP_MIN_INTERVAL` (20 a minute per supergroup); `retry_after`
//! pauses the whole queue.

use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{AUTH_RETRY_INTERVAL, CoreError, Ctx, Notice, Result, topics};
use crate::fault;
use crate::paperclip::PaperclipApi;
use crate::retry::{Backoff, rate_limit_jitter};
use crate::store::TgChunk;
use crate::telegram::{TelegramApi, TelegramError};
use crate::timeutil;

/// The sender wakes up at least this often to look at the queue.
const IDLE_WAIT: Duration = Duration::from_secs(60);
/// `queue_depth` above this logs a WARN (ADR-0010 §5).
pub const QUEUE_WARN_DEPTH: u64 = 60;

/// Outcome of one [`Sender::step`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// A Telegram call was made.
    Called,
    /// Nothing to do; next due work in this long, if any.
    Idle(Option<Duration>),
    /// The queue is paused this long (rate limit, missing rights, backoff).
    Paused(Duration),
}

pub struct Sender<P, T> {
    ctx: Ctx<P, T>,
    notices: mpsc::UnboundedReceiver<Notice>,
    /// A notice that hit the rate limit: sent first after the pause.
    held_notice: Option<Notice>,
    pause_until: Option<Instant>,
    /// Consecutive failures of topic creation (backoff for the queue).
    topic_failures: u32,
    backoff: Backoff,
}

impl<P: PaperclipApi, T: TelegramApi> Sender<P, T> {
    pub fn new(ctx: Ctx<P, T>, notices: mpsc::UnboundedReceiver<Notice>) -> Self {
        let backoff = Backoff::from_config(&ctx.cfg.retry);
        Self {
            ctx,
            notices,
            held_notice: None,
            pause_until: None,
            topic_failures: 0,
            backoff,
        }
    }

    fn pause(&mut self, d: Duration) -> Step {
        self.pause_until = Some(Instant::now() + d);
        Step::Paused(d)
    }

    /// One unit of work: a notice first, then a topic, then a piece.
    pub async fn step(&mut self) -> Result<Step> {
        if let Some(until) = self.pause_until {
            let now = Instant::now();
            if until > now {
                return Ok(Step::Paused(until - now));
            }
            self.pause_until = None;
        }
        if let Some(n) = self
            .held_notice
            .take()
            .or_else(|| self.notices.try_recv().ok())
        {
            self.send_notice(n).await;
            return Ok(Step::Called);
        }
        if let Some(conv) = self.ctx.store.next_topic_job().await? {
            return match topics::create(&self.ctx, &conv).await? {
                Ok(_) => {
                    self.topic_failures = 0;
                    self.ctx.health.telegram_ok();
                    Ok(Step::Called)
                }
                Err(e) => Ok(self.topic_failed(e)),
            };
        }
        if let Some(chunk) = self.ctx.store.next_chunk(timeutil::now_string()).await? {
            return self.send_chunk(chunk).await;
        }
        let next = self
            .ctx
            .store
            .next_chunk_due()
            .await?
            .map(|at| super::until(&at));
        Ok(Step::Idle(next))
    }

    async fn send_notice(&mut self, n: Notice) {
        let chat = self.ctx.cfg.telegram.chat_id;
        let mut thread = n.thread_id;
        for attempt in 0..2 {
            let html = &n.html;
            match self
                .ctx
                .telegram
                .send_message(chat, thread, html, true)
                .await
            {
                Ok(_) => {
                    self.ctx.health.telegram_ok();
                    return;
                }
                // The topic is gone: say it in General instead.
                Err(TelegramError::ThreadNotFound | TelegramError::TopicClosed)
                    if attempt == 0 && thread.is_some() =>
                {
                    thread = None;
                }
                Err(TelegramError::RateLimited { retry_after }) => {
                    self.pause(retry_after + rate_limit_jitter());
                    self.held_notice = Some(Notice {
                        thread_id: thread,
                        html: n.html,
                    });
                    return;
                }
                Err(e) => {
                    tracing::warn!(class = e.class(), "bridge notice not delivered");
                    return;
                }
            }
        }
    }

    fn topic_failed(&mut self, e: TelegramError) -> Step {
        match e {
            TelegramError::RateLimited { retry_after } => {
                self.pause(retry_after + rate_limit_jitter())
            }
            TelegramError::Forbidden | TelegramError::Auth { .. } => {
                tracing::error!(
                    class = e.class(),
                    "cannot create forum topics: check that the bot is an admin with can_manage_topics"
                );
                self.ctx.health.set_telegram_send_ok(false);
                self.pause(AUTH_RETRY_INTERVAL)
            }
            e => {
                self.topic_failures += 1;
                if !e.is_transient() {
                    tracing::error!(
                        class = e.class(),
                        "createForumTopic rejected: check the supergroup (topics on, bot rights)"
                    );
                }
                let d = self.backoff.delay(self.topic_failures);
                self.pause(d)
            }
        }
    }

    async fn send_chunk(&mut self, c: TgChunk) -> Result<Step> {
        let ctx = self.ctx.clone();
        let chat = ctx.cfg.telegram.chat_id;
        let Some(thread) = c.thread_id else {
            return Ok(Step::Idle(None));
        };
        let (text, html) = if c.use_plain {
            (c.plain.clone(), false)
        } else {
            (c.html.clone(), true)
        };
        let Some(text) = text.filter(|t| !t.is_empty()) else {
            ctx.store.chunk_abandon(c.id).await?;
            return Ok(Step::Idle(None));
        };
        match ctx
            .telegram
            .send_message(chat, Some(thread), &text, html)
            .await
        {
            Ok(message_id) => {
                if !ctx.health.is_telegram_send_ok() {
                    tracing::info!("telegram accepts messages again");
                }
                ctx.health.telegram_ok();
                ctx.health.set_telegram_send_ok(true);
                if fault::hit(fault::AFTER_SEND_BEFORE_COMMIT) {
                    return Err(CoreError::Fault);
                }
                ctx.store.chunk_sent(c.id, message_id).await?;
                tracing::info!(
                    issue_id = %c.issue_id,
                    comment_id = %c.comment_id,
                    chunk = c.chunk,
                    chunks = c.chunks,
                    "reply delivered to telegram"
                );
                Ok(Step::Called)
            }
            Err(TelegramError::ThreadNotFound) => {
                topics::lost(&ctx, &c.issue_id, thread).await?;
                Ok(Step::Called)
            }
            Err(TelegramError::TopicClosed) => {
                match topics::reopen(&ctx, &c.issue_id, thread).await? {
                    Ok(()) => Ok(Step::Called),
                    Err(e) => self.chunk_failed(&c, e).await,
                }
            }
            Err(TelegramError::CantParseEntities) if !c.use_plain => {
                tracing::warn!(
                    comment_id = %c.comment_id,
                    chunk = c.chunk,
                    "telegram cannot parse the HTML, sending as plain text"
                );
                ctx.store.chunk_use_plain(c.id).await?;
                Ok(Step::Called)
            }
            Err(e) => self.chunk_failed(&c, e).await,
        }
    }

    async fn chunk_failed(&mut self, c: &TgChunk, e: TelegramError) -> Result<Step> {
        let ctx = self.ctx.clone();
        match e {
            TelegramError::RateLimited { retry_after } => {
                tracing::warn!(
                    retry_after_s = retry_after.as_secs(),
                    "telegram rate limit, pausing queue"
                );
                Ok(self.pause(retry_after + rate_limit_jitter()))
            }
            TelegramError::Forbidden | TelegramError::Auth { .. } => {
                if ctx.health.is_telegram_send_ok() {
                    tracing::error!(
                        class = e.class(),
                        "telegram refuses messages to the supergroup: is the bot still a member and admin?"
                    );
                }
                ctx.health.set_telegram_send_ok(false);
                Ok(self.pause(AUTH_RETRY_INTERVAL))
            }
            e => {
                let attempts = c.attempts + 1;
                if !e.is_transient() && attempts >= ctx.cfg.retry.max_attempts {
                    tracing::error!(
                        comment_id = %c.comment_id,
                        chunk = c.chunk,
                        class = e.class(),
                        attempts,
                        "reply piece abandoned"
                    );
                    ctx.store.chunk_abandon(c.id).await?;
                    return Ok(Step::Called);
                }
                tracing::warn!(
                    comment_id = %c.comment_id,
                    class = e.class(),
                    attempts,
                    "reply piece not delivered yet, will retry"
                );
                let delay = self.backoff.delay(attempts);
                let next = timeutil::format(timeutil::add(timeutil::now(), delay));
                ctx.store.chunk_retry(c.id, attempts, next).await?;
                Ok(Step::Called)
            }
        }
    }

    /// Runs steps until there is nothing to do right now (tests).
    pub async fn drain(&mut self) -> Result<()> {
        for _ in 0..10_000 {
            match self.step().await? {
                Step::Called => {}
                Step::Idle(Some(d)) if d.is_zero() => {}
                _ => return Ok(()),
            }
        }
        Ok(())
    }

    /// Clears a pause (tests).
    pub fn resume(&mut self) {
        self.pause_until = None;
    }

    /// The task: one call per `min_interval`, sleeping when idle.
    pub async fn run(mut self, cancel: CancellationToken) {
        let min_interval = self.ctx.cfg.telegram.group_min_interval;
        let notify = self.ctx.sender_notify.clone();
        let mut last_call: Option<Instant> = None;
        loop {
            if let Some(at) = last_call {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep_until(at + min_interval) => {}
                }
            }
            let notified = notify.notified();
            let wait = match self.step().await {
                Ok(Step::Called) => {
                    last_call = Some(Instant::now());
                    continue;
                }
                Ok(Step::Idle(next)) => next.unwrap_or(IDLE_WAIT).min(IDLE_WAIT),
                Ok(Step::Paused(d)) => d,
                Err(e) => {
                    tracing::warn!(error = %e, "sender step failed");
                    last_call = Some(Instant::now());
                    Duration::from_secs(1)
                }
            };
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = notified => {}
                _ = tokio::time::sleep(wait) => {}
            }
        }
    }
}
