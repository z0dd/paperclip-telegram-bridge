//! Telegram → Paperclip, first half (ADR-0010 §2.5): filters an update and
//! queues a text from a conversation topic in `pc_outbox` together with the
//! offset. Everything else only advances the offset (with a hint where the
//! user may expect a reaction).

use uuid::Uuid;

use super::{Ctx, Result};
use crate::paperclip::PaperclipApi;
use crate::render::notices;
use crate::store::{CommitOutcome, NewPcEntry};
use crate::telegram::{Message, TelegramApi, Update};

/// Namespace of `clientRequestId`: UUIDv5 of the project URL, so the ids stay
/// the same across versions and restarts.
pub fn namespace() -> Uuid {
    Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        b"https://github.com/z0dd/paperclip-telegram-bridge",
    )
}

/// `UUIDv5(NS_BRIDGE, "tg:<chat_id>:<message_id>")`: a retried message gets the
/// same id, and Paperclip returns the existing comment (ADR-0010 §1.3).
pub fn client_request_id(chat_id: i64, message_id: i64) -> String {
    Uuid::new_v5(
        &namespace(),
        format!("tg:{chat_id}:{message_id}").as_bytes(),
    )
    .to_string()
}

/// What happens with an admitted message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// Service message about topics: ignored.
    Service,
    /// General: hint to write in a topic.
    General,
    /// A topic without a live conversation: hint.
    Unlinked { thread_id: i64 },
    /// Not a text: hint.
    NotText { thread_id: i64 },
    /// Queue for the conversation.
    Forward {
        issue_id: String,
        thread_id: i64,
        text: String,
    },
}

pub async fn route<P: PaperclipApi, T: TelegramApi>(ctx: &Ctx<P, T>, m: &Message) -> Result<Route> {
    if m.is_topic_service() {
        return Ok(Route::Service);
    }
    let Some(thread_id) = m.topic_thread_id() else {
        return Ok(Route::General);
    };
    let conv = ctx.store.conversation_by_thread(thread_id).await?;
    let Some(conv) = conv.filter(|c| c.removed_at.is_none()) else {
        return Ok(Route::Unlinked { thread_id });
    };
    match m.text.as_deref().map(str::trim) {
        Some(t) if !t.is_empty() => Ok(Route::Forward {
            issue_id: conv.issue_id,
            thread_id,
            text: m.text.clone().unwrap_or_default(),
        }),
        _ => Ok(Route::NotText { thread_id }),
    }
}

/// Processes one update. An error leaves the offset where it was: the
/// update comes again.
pub async fn handle_update<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    u: &Update,
) -> Result<()> {
    let m = match ctx.allowlist.check(u) {
        Ok(m) => m,
        Err(reason) => {
            ctx.allowlist.log_rejection(u, reason);
            ctx.store.advance_offset(u.update_id).await?;
            return Ok(());
        }
    };
    let hint = |thread: Option<i64>, text: &str| {
        ctx.notify(thread, text);
    };
    match route(ctx, m).await? {
        Route::Service => {}
        Route::General => hint(None, notices::WRITE_IN_TOPIC),
        Route::Unlinked { thread_id } => hint(Some(thread_id), notices::TOPIC_NOT_LINKED),
        Route::NotText { thread_id } => hint(Some(thread_id), notices::TEXT_ONLY),
        Route::Forward {
            issue_id,
            thread_id,
            text,
        } => {
            let outcome = ctx
                .store
                .enqueue_message(
                    u.update_id,
                    NewPcEntry {
                        chat_id: m.chat.id,
                        message_id: m.message_id,
                        issue_id: issue_id.clone(),
                        client_request_id: client_request_id(m.chat.id, m.message_id),
                        body: text,
                    },
                )
                .await?;
            if outcome == CommitOutcome::Applied {
                tracing::info!(
                    update_id = u.update_id,
                    issue_id = %issue_id,
                    thread_id,
                    "message queued for paperclip"
                );
                ctx.outbox_notify.notify_one();
            }
            return Ok(());
        }
    }
    ctx.store.advance_offset(u.update_id).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_request_id_is_a_stable_uuid_v5() {
        let a = client_request_id(-1001234567890, 42);
        assert_eq!(a, client_request_id(-1001234567890, 42));
        assert_ne!(a, client_request_id(-1001234567890, 43));
        assert_ne!(a, client_request_id(-1009999999999, 42));
        let parsed = Uuid::parse_str(&a).unwrap();
        assert_eq!(parsed.get_version_num(), 5);
        // Pinned: a change would post a retried message twice across an upgrade.
        assert_eq!(a, "3374c92a-c501-5638-9ac8-eaa1500d5e8c");
    }
}
