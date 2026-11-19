//! Forum topics (ADR-0010 §2.6): one topic per conversation, created right
//! after discovery, reopened when closed, created again when deleted. Called
//! only from the sender, so topic calls share its rate limit.

use super::{Ctx, Result};
use crate::paperclip::PaperclipApi;
use crate::store::Conversation;
use crate::telegram::{TelegramApi, TelegramError};

/// Creates the topic of `conv` and makes it active. Telegram errors are
/// returned to the sender for its pause and retry logic.
pub async fn create<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    conv: &Conversation,
) -> Result<std::result::Result<i64, TelegramError>> {
    let chat = ctx.cfg.telegram.chat_id;
    match ctx
        .telegram
        .create_forum_topic(chat, &conv.topic_name)
        .await
    {
        Ok(thread_id) => {
            ctx.store
                .set_topic(conv.issue_id.clone(), thread_id)
                .await?;
            tracing::info!(
                issue_id = %conv.issue_id,
                thread_id,
                recreated = conv.topic_state == crate::store::TopicState::Recreating,
                "forum topic created"
            );
            Ok(Ok(thread_id))
        }
        Err(e) => {
            tracing::warn!(issue_id = %conv.issue_id, class = e.class(), "createForumTopic failed");
            Ok(Err(e))
        }
    }
}

/// Reopens a closed topic. A topic that turns out to be deleted is queued
/// for re-creation.
pub async fn reopen<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    issue_id: &str,
    thread_id: i64,
) -> Result<std::result::Result<(), TelegramError>> {
    let chat = ctx.cfg.telegram.chat_id;
    match ctx.telegram.reopen_forum_topic(chat, thread_id).await {
        Ok(()) => {
            tracing::info!(issue_id, thread_id, "forum topic reopened");
            Ok(Ok(()))
        }
        Err(TelegramError::ThreadNotFound) => {
            lost(ctx, issue_id, thread_id).await?;
            Ok(Ok(()))
        }
        Err(e) => {
            tracing::warn!(
                issue_id,
                thread_id,
                class = e.class(),
                "reopenForumTopic failed"
            );
            Ok(Err(e))
        }
    }
}

/// The topic was deleted in Telegram: the next sender step creates it again
/// and the queued pieces follow to the new topic.
pub async fn lost<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    issue_id: &str,
    thread_id: i64,
) -> Result<()> {
    tracing::warn!(issue_id, thread_id, "forum topic not found, recreating");
    ctx.store
        .topic_lost(issue_id.to_string(), thread_id)
        .await?;
    Ok(())
}
