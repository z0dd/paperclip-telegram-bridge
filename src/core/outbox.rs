//! Telegram → Paperclip, second half (ADR-0010 §2.5): posts queued user
//! messages as conversation comments with a deterministic `clientRequestId`.
//! Transient failures are retried forever (a message is never dropped); a
//! repeated POST after a crash returns the existing comment, so delivery is
//! exactly once without reconciling.

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::{AUTH_RETRY_INTERVAL, CoreError, Ctx, Result};
use crate::fault;
use crate::paperclip::{PaperclipApi, PaperclipError};
use crate::render::notices;
use crate::retry::Backoff;
use crate::store::PcEntry;
use crate::telegram::TelegramApi;
use crate::timeutil;

const IDLE_WAIT: Duration = Duration::from_secs(60);

pub async fn run<P: PaperclipApi, T: TelegramApi>(ctx: Ctx<P, T>, cancel: CancellationToken) {
    loop {
        if let Err(e) = process_due(&ctx, &cancel).await {
            tracing::warn!(error = %e, "pc_outbox pass failed");
        }
        let wait = match ctx.store.next_message_due().await {
            Ok(Some(at)) => super::until(&at).max(Duration::from_millis(10)).min(IDLE_WAIT),
            _ => IDLE_WAIT,
        };
        let notified = ctx.outbox_notify.notified();
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = notified => {}
            _ = tokio::time::sleep(wait) => {}
        }
    }
}

/// Processes all due entries. Stops taking new ones once shutdown starts;
/// the entry in flight is finished (bounded by `SHUTDOWN_GRACE`).
pub async fn process_due<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    cancel: &CancellationToken,
) -> Result<()> {
    for entry in ctx.store.due_messages(timeutil::now_string()).await? {
        if cancel.is_cancelled() {
            break;
        }
        process(ctx, entry).await?;
    }
    Ok(())
}

/// What to do with a Paperclip answer (ADR-0010 §2.5, table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Final refusal; `notice` goes to the topic.
    Rejected { notice: Option<String> },
    /// `401`: keep pending, retry every 5 minutes.
    KeyRejected,
    /// `5xx`, `429`, timeout, network, unreadable answer: the same
    /// `clientRequestId` makes the repeat safe.
    Retry,
}

pub fn classify(e: &PaperclipError) -> Outcome {
    match e {
        PaperclipError::Auth => Outcome::KeyRejected,
        PaperclipError::Decode => Outcome::Retry,
        e if e.is_transient() => Outcome::Retry,
        PaperclipError::Conflict { .. } if e.message_contains("paused") => Outcome::Rejected {
            notice: Some(notices::PAUSED.into()),
        },
        PaperclipError::Conflict { .. } if e.message_contains("already used for different content") => {
            Outcome::Rejected { notice: None }
        }
        PaperclipError::Forbidden { .. } => Outcome::Rejected {
            notice: Some(notices::NOT_OWNER.into()),
        },
        PaperclipError::NotFound { .. } => Outcome::Rejected {
            notice: Some(notices::NOT_FOUND.into()),
        },
        e => Outcome::Rejected {
            notice: Some(notices::rejected(e.message())),
        },
    }
}

async fn process<P: PaperclipApi, T: TelegramApi>(ctx: &Ctx<P, T>, e: PcEntry) -> Result<()> {
    let conv = ctx.store.conversation(e.issue_id.clone()).await?;
    let (Some(body), Some(conv)) = (e.body.clone(), conv) else {
        ctx.store
            .message_rejected(e.id, "lost".into())
            .await?;
        return Ok(());
    };
    let key = ctx.key_index(&conv.key_ref);
    ctx.store.message_in_flight(e.id).await?;
    let result = ctx
        .key(key)
        .post_comment(&e.issue_id, &body, &e.client_request_id)
        .await;
    if fault::hit(fault::AFTER_HTTP_BEFORE_OUTBOX_COMMIT) {
        return Err(CoreError::Fault);
    }
    let err = match result {
        Ok(comment_id) => {
            ctx.paperclip_ok(key);
            ctx.store.message_done(e.id, comment_id).await?;
            ctx.store
                .extend_hot(e.issue_id.clone(), ctx.hot_until())
                .await?;
            tracing::info!(
                pc_outbox_id = e.id,
                issue_id = %e.issue_id,
                outcome = "sent",
                "message delivered to paperclip"
            );
            return Ok(());
        }
        Err(err) => err,
    };
    let thread = conv.topic_thread_id;
    match classify(&err) {
        Outcome::Rejected { notice } => {
            if matches!(err, PaperclipError::Forbidden { .. } | PaperclipError::Conflict { .. })
                && notice.as_deref() != Some(notices::PAUSED)
            {
                tracing::error!(
                    pc_outbox_id = e.id,
                    issue_id = %e.issue_id,
                    class = %err.class(),
                    "message rejected by paperclip"
                );
            } else {
                tracing::warn!(
                    pc_outbox_id = e.id,
                    issue_id = %e.issue_id,
                    class = %err.class(),
                    "message rejected by paperclip"
                );
            }
            ctx.store.message_rejected(e.id, err.class()).await?;
            if let Some(n) = notice {
                ctx.notify(thread, n);
            }
            Ok(())
        }
        Outcome::KeyRejected => {
            ctx.key_rejected(key);
            retry(ctx, &e, &err, thread, AUTH_RETRY_INTERVAL).await
        }
        Outcome::Retry => {
            let attempts = e.attempts + 1;
            let delay = match err.retry_after() {
                Some(ra) => ra + crate::retry::rate_limit_jitter(),
                None => Backoff::from_config(&ctx.cfg.retry).delay(attempts),
            };
            retry(ctx, &e, &err, thread, delay).await
        }
    }
}

/// Keeps the message, backs off, tells the user once that it is queued.
async fn retry<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    e: &PcEntry,
    err: &PaperclipError,
    thread: Option<i64>,
    delay: Duration,
) -> Result<()> {
    let attempts = e.attempts + 1;
    let notify = !e.notified && attempts >= ctx.cfg.retry.notify_after;
    let next = timeutil::format(timeutil::add(timeutil::now(), delay));
    ctx.store
        .message_retry(e.id, attempts, next, err.class(), e.notified || notify)
        .await?;
    tracing::warn!(
        pc_outbox_id = e.id,
        class = %err.class(),
        attempts,
        "message not delivered yet, will retry"
    );
    if notify {
        ctx.notify(thread, notices::QUEUED);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conflict(m: &str) -> PaperclipError {
        PaperclipError::Conflict { message: m.into() }
    }

    #[test]
    fn answers_are_classified_as_in_the_adr_table() {
        assert_eq!(
            classify(&conflict("Task is paused. Resume it before sending a message.")),
            Outcome::Rejected {
                notice: Some(notices::PAUSED.into())
            }
        );
        assert_eq!(
            classify(&conflict(
                "Message request ID was already used for different content"
            )),
            Outcome::Rejected { notice: None }
        );
        assert_eq!(
            classify(&PaperclipError::Forbidden {
                message: "Only the conversation owner can send messages".into()
            }),
            Outcome::Rejected {
                notice: Some(notices::NOT_OWNER.into())
            }
        );
        assert_eq!(
            classify(&PaperclipError::NotFound {
                message: "Agent Chat is disabled".into()
            }),
            Outcome::Rejected {
                notice: Some(notices::NOT_FOUND.into())
            }
        );
        assert_eq!(
            classify(&PaperclipError::Invalid {
                status: 422,
                message: "body <too> long".into()
            }),
            Outcome::Rejected {
                notice: Some("❌ Paperclip не принял сообщение: body &lt;too&gt; long".into())
            }
        );
        assert_eq!(classify(&PaperclipError::Auth), Outcome::KeyRejected);
        for e in [
            PaperclipError::Server { status: 502 },
            PaperclipError::Transient {
                reason: "rate_limited",
                retry_after: Some(Duration::from_secs(3)),
            },
            PaperclipError::Transient {
                reason: "timeout",
                retry_after: None,
            },
            PaperclipError::Decode,
        ] {
            assert_eq!(classify(&e), Outcome::Retry, "{e:?}");
        }
    }
}
