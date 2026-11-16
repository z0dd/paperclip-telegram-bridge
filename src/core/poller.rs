//! `getUpdates` long polling → dialog, one update at a time, in order.

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::{Ctx, dialog};
use crate::paperclip::PaperclipApi;
use crate::retry::{Backoff, rate_limit_jitter};
use crate::telegram::{TelegramApi, TelegramError};

/// Back-off ceiling for `409 Conflict` (another consumer or a webhook).
pub const CONFLICT_MAX_DELAY: Duration = Duration::from_secs(300);

pub async fn run<P: PaperclipApi, T: TelegramApi>(ctx: Ctx<P, T>, cancel: CancellationToken) {
    let backoff = Backoff::from_config(&ctx.cfg.retry);
    let conflict_backoff = Backoff::new(ctx.cfg.retry.base_delay, CONFLICT_MAX_DELAY);
    let mut failures = 0u32;
    loop {
        let offset = match ctx.store.telegram_offset().await {
            Ok(o) => o,
            Err(e) => {
                tracing::error!(error = %e, "cannot read telegram offset");
                return;
            }
        };
        // Cancelling here drops the long poll: the offset was not advanced, so
        // the same updates come again after a restart.
        let result = tokio::select! {
            _ = cancel.cancelled() => break,
            r = ctx.telegram.get_updates(offset, ctx.cfg.telegram.long_poll_timeout) => r,
        };
        let delay = match result {
            Ok(mut updates) => {
                failures = 0;
                ctx.health.telegram_ok();
                updates.sort_by_key(|u| u.update_id);
                let mut failed = false;
                for u in updates {
                    if cancel.is_cancelled() {
                        break;
                    }
                    if let Err(e) = dialog::handle_update(&ctx, &u).await {
                        // Not committed: stop here and fetch again from the offset.
                        tracing::warn!(update_id = u.update_id, error = %e, "update processing failed");
                        failed = true;
                        break;
                    }
                }
                if failed {
                    failures += 1;
                    backoff.delay(failures)
                } else {
                    continue;
                }
            }
            Err(TelegramError::RateLimited { retry_after }) => retry_after + rate_limit_jitter(),
            Err(TelegramError::Conflict) => {
                failures += 1;
                tracing::error!(
                    "getUpdates conflict: another bridge instance is running or a webhook is set for this bot; the webhook is not removed automatically"
                );
                conflict_backoff.delay(failures)
            }
            Err(TelegramError::Auth { status }) => {
                failures += 1;
                tracing::error!(status, "telegram rejected the bot token");
                CONFLICT_MAX_DELAY
            }
            Err(e) => {
                failures += 1;
                tracing::warn!(class = e.class(), failures, "getUpdates failed");
                backoff.delay(failures)
            }
        };
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(delay) => {}
        }
    }
}
