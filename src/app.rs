//! Wiring of the running service: state, clients, tasks, graceful shutdown.

use std::sync::Arc;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::core::sender::{QUEUE_WARN_DEPTH, Sender};
use crate::core::{Bridge, Ctx, checks, discovery, outbox, poller};
use crate::health::{self, HealthTracker};
use crate::paperclip::{HttpPaperclipClient, PaperclipApi, version};
use crate::store::Store;
use crate::telegram::{HttpTelegramClient, TelegramApi};

pub async fn run(cfg: Config, token: CancellationToken) -> Result<(), String> {
    let cfg = Arc::new(cfg);
    for var in &cfg.ignored_vars {
        tracing::warn!(
            variable = var,
            "variable of v1 is no longer used, remove it"
        );
    }
    let store = Store::open(&cfg.state_path).map_err(|e| format!("cannot open state: {e}"))?;
    let rec = store
        .recover()
        .await
        .map_err(|e| format!("state recovery failed: {e}"))?;
    tracing::info!(pc_outbox_reset = rec.pc_reset, "state opened");
    let keys: Vec<Arc<HttpPaperclipClient>> = HttpPaperclipClient::from_config(&cfg.paperclip)?
        .into_iter()
        .map(Arc::new)
        .collect();
    let telegram = Arc::new(HttpTelegramClient::new(&cfg.telegram)?);
    match keys[0].health().await {
        Ok(h) => version::log_check(h.version.as_deref()),
        Err(e) => tracing::warn!(class = %e.class(), "paperclip health check failed, continuing"),
    }
    let tracker = Arc::new(HealthTracker::new());
    let (ctx, notices) = Bridge::new(cfg.clone(), keys, telegram, store.clone(), tracker.clone());

    // The sender outlives the other tasks so that their last messages go out.
    let sender_token = CancellationToken::new();
    let sender_task = tokio::spawn(Sender::new(ctx.clone(), notices).run(sender_token.clone()));

    let tasks = spawn_tasks(&ctx, &token);

    token.cancelled().await;
    tracing::info!(grace_s = cfg.shutdown_grace.as_secs(), "stopping");
    if tokio::time::timeout(cfg.shutdown_grace, wait_all(tasks.0))
        .await
        .is_err()
    {
        tracing::warn!("tasks did not stop within the grace period");
    }
    // In-flight message: finish within the grace period, otherwise it stays
    // in_flight and returns to pending on the next start.
    let outbox_task = tasks.1;
    if tokio::time::timeout(cfg.shutdown_grace, async {
        let _ = outbox_task.await;
    })
    .await
    .is_err()
    {
        tracing::warn!("pc_outbox did not finish within the grace period");
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
        tokio::spawn(checks::run(ctx.clone(), token.clone())),
        tokio::spawn(poller::run(ctx.clone(), token.clone())),
        tokio::spawn(discovery::run(ctx.clone(), token.clone())),
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
    let mut warned = false;
    loop {
        if let Ok(depth) = ctx.store.queue_depth().await {
            ctx.health.set_queue_depth(depth);
            if depth > QUEUE_WARN_DEPTH && !warned {
                tracing::warn!(
                    queue_depth = depth,
                    "telegram queue is long: replies are delayed by the 20 messages a minute limit"
                );
            }
            warned = depth > QUEUE_WARN_DEPTH;
        }
        if let Err(e) = health::write_file(&ctx.cfg.health.file, &ctx.health.snapshot()) {
            tracing::warn!(error_kind = ?e.kind(), "cannot write health file");
        }
        tokio::select! {
            _ = token.cancelled() => break,
            _ = tokio::time::sleep(health::WRITE_INTERVAL) => {}
        }
    }
}
