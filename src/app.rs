//! Wiring of the running service: state, clients, tasks, graceful shutdown.

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::core::sender::{self, SenderSettings, TgSender};
use crate::core::{Bridge, Ctx, commands, forms, outbox, poller, runs, sync};
use crate::health::{self, HealthTracker};
use crate::paperclip::{HttpPaperclipClient, PaperclipApi, version};
use crate::retry::Backoff;
use crate::store::{ResetDraft, Store};
use crate::telegram::{HttpTelegramClient, TelegramApi};

/// Telegram allows about one message per second in a private chat.
pub const MESSAGE_INTERVAL: Duration = Duration::from_secs(1);

pub async fn run(cfg: Config, token: CancellationToken) -> Result<(), String> {
    let cfg = Arc::new(cfg);
    let store = Store::open(&cfg.state_path).map_err(|e| format!("cannot open state: {e}"))?;
    let rec = store
        .recover()
        .await
        .map_err(|e| format!("state recovery failed: {e}"))?;
    tracing::info!(
        outbox_reset = rec.outbox_reset,
        redeliver = rec.sending.len(),
        "state opened"
    );
    // ADR-0009 §2: before anything reads drafts (restart notice included).
    let reset = store
        .reset_chat_text_drafts()
        .await
        .map_err(|e| format!("draft migration failed: {e}"))?;
    if !reset.is_empty() {
        tracing::info!(cards = reset.len(), "chat text drafts reset");
    }
    let paperclip = Arc::new(HttpPaperclipClient::from_config(&cfg.paperclip)?);
    let telegram = Arc::new(HttpTelegramClient::new(&cfg.telegram)?);
    match paperclip.health().await {
        Ok(h) => version::log_check(h.version.as_deref()),
        Err(e) => tracing::warn!(class = %e.class(), "paperclip health check failed, continuing"),
    }
    let (tg, rx) = TgSender::channel();
    let tracker = Arc::new(HealthTracker::new());
    let ctx = Bridge::new(
        cfg.clone(),
        paperclip,
        telegram.clone(),
        store.clone(),
        tg,
        tracker.clone(),
    );
    ctx.load_persisted()
        .await
        .map_err(|e| format!("cannot load state: {e}"))?;

    // The sender outlives the other tasks so that their last messages go out.
    let sender_token = CancellationToken::new();
    let sender_task = tokio::spawn(sender::run(
        telegram,
        rx,
        SenderSettings {
            chat_id: cfg.telegram.chat_id,
            min_interval: MESSAGE_INTERVAL,
            backoff: Backoff::from_config(&cfg.retry),
            max_attempts: cfg.retry.max_attempts,
        },
        sender_token.clone(),
        tracker.clone(),
    ));

    let tasks = spawn_tasks(&ctx, &token);
    tokio::spawn(startup_messages(ctx.clone(), reset));

    token.cancelled().await;
    tracing::info!(grace_s = cfg.shutdown_grace.as_secs(), "stopping");
    if tokio::time::timeout(cfg.shutdown_grace, wait_all(tasks.0))
        .await
        .is_err()
    {
        tracing::warn!("tasks did not stop within the grace period");
    }
    // In-flight outbox entry: finish within the grace period, otherwise it
    // stays in_flight and returns to pending on the next start.
    let outbox_task = tasks.1;
    if tokio::time::timeout(cfg.shutdown_grace, async {
        let _ = outbox_task.await;
    })
    .await
    .is_err()
    {
        tracing::warn!("outbox did not finish within the grace period");
    }
    sender_token.cancel();
    let _ = sender_task.await;
    if let Err(e) = store.checkpoint().await {
        tracing::warn!(error = %e, "WAL checkpoint failed");
    }
    let _ = health::write_file(&cfg.health.file, &tracker.snapshot());
    tracing::info!("bridge stopped");
    Ok(())
}

fn spawn_tasks<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    token: &CancellationToken,
) -> (Vec<JoinHandle<()>>, JoinHandle<()>) {
    let others = vec![
        tokio::spawn(poller::run(ctx.clone(), token.clone())),
        tokio::spawn(sync::run(ctx.clone(), token.clone())),
        tokio::spawn(runs::run(ctx.clone(), token.clone())),
        tokio::spawn(health_writer(ctx.clone(), token.clone())),
    ];
    let outbox = tokio::spawn(outbox::run(ctx.clone(), token.clone()));
    (others, outbox)
}

async fn wait_all(tasks: Vec<JoinHandle<()>>) {
    for t in tasks {
        let _ = t.await;
    }
}

async fn health_writer<P: PaperclipApi, T: TelegramApi>(ctx: Ctx<P, T>, token: CancellationToken) {
    loop {
        if let Err(e) = health::write_file(&ctx.cfg.health.file, &ctx.health.snapshot()) {
            tracing::warn!(error_kind = ?e.kind(), "cannot write health file");
        }
        tokio::select! {
            _ = token.cancelled() => break,
            _ = tokio::time::sleep(health::WRITE_INTERVAL) => {}
        }
    }
}

async fn startup_messages<P: PaperclipApi, T: TelegramApi>(ctx: Ctx<P, T>, reset: Vec<ResetDraft>) {
    if let Err(e) = ctx.tg.set_commands(commands::bot_commands()).await {
        tracing::warn!(class = e.class(), "setMyCommands failed");
    }
    forms::announce_chat_text_reset(&ctx, &reset).await;
    if let Err(e) = forms::announce_restart(&ctx).await {
        tracing::warn!(error = %e, "cannot announce unfinished forms");
    }
}
