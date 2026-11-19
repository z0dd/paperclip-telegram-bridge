//! Paperclip → Telegram (ADR-0010 §2.3, §2.6): reads new comments of a
//! conversation after its cursor, keeps the agent's replies (receipts
//! included) and commits the Telegram pieces, the seen ids and the new cursor
//! in one transaction.

use super::{CoreError, Ctx, Result};
use crate::domain::{Comment, should_forward};
use crate::fault;
use crate::paperclip::PaperclipApi;
use crate::render;
use crate::store::{CommentBatch, Conversation, NewChunk};
use crate::telegram::TelegramApi;
use crate::timeutil;

/// Page size of `GET /issues/{id}/comments`.
pub const PAGE: u32 = 100;
/// Upper bound of pages per pass, so that one huge thread cannot stall the loop.
const MAX_PAGES: usize = 20;

/// Reads everything new in `conv`. `updated` — the conversation `updatedAt`
/// changed since the last poll (an empty first page then means the anchor
/// is gone, ADR-0010 §2.3).
pub async fn read<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    key: usize,
    conv: &Conversation,
    updated: bool,
) -> Result<()> {
    let api = ctx.key(key);
    let mut cursor = conv.cursor_comment_id.clone();
    for page_no in 0..MAX_PAGES {
        let page = api
            .comments_after(&conv.issue_id, cursor.as_deref(), PAGE)
            .await?;
        ctx.paperclip_ok(key);
        let full = page.len() as u32 >= PAGE;
        if page.is_empty() {
            if page_no == 0 && updated && cursor.is_some() {
                return fallback(ctx, key, conv).await;
            }
            break;
        }
        let last = page.last().map(|c| c.id.clone());
        let fresh = unseen(ctx, conv, page).await?;
        commit(ctx, conv, fresh, last.clone()).await?;
        if !full {
            break;
        }
        cursor = last;
    }
    Ok(())
}

/// Drops comments already in `seen_comments` (the anchor comes back).
async fn unseen<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    conv: &Conversation,
    comments: Vec<Comment>,
) -> Result<Vec<Comment>> {
    let ids = comments.iter().map(|c| c.id.clone()).collect();
    let seen = ctx.store.seen_of(conv.issue_id.clone(), ids).await?;
    Ok(comments
        .into_iter()
        .filter(|c| !seen.contains(&c.id))
        .collect())
}

/// The cursor anchor vanished (deleted comment): take the newest page, keep
/// what is newer than the last seen comment, move the cursor to the newest.
async fn fallback<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    key: usize,
    conv: &Conversation,
) -> Result<()> {
    let latest = ctx.key(key).comments_latest(&conv.issue_id, PAGE).await?;
    let Some(newest) = latest.first().map(|c| c.id.clone()) else {
        return commit(ctx, conv, Vec::new(), None).await;
    };
    let ids = latest.iter().map(|c| c.id.clone()).collect();
    let seen = ctx.store.seen_of(conv.issue_id.clone(), ids).await?;
    // Newest first: everything before the first seen comment is new.
    let mut fresh: Vec<Comment> = latest
        .into_iter()
        .take_while(|c| !seen.contains(&c.id))
        .collect();
    fresh.reverse();
    if fresh.is_empty() {
        tracing::debug!(issue_id = %conv.issue_id, "empty page after update, nothing new");
    } else {
        tracing::warn!(
            issue_id = %conv.issue_id,
            new = fresh.len(),
            "comment cursor anchor is gone, recovered from the newest page"
        );
    }
    commit(ctx, conv, fresh, Some(newest)).await
}

async fn commit<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    conv: &Conversation,
    fresh: Vec<Comment>,
    cursor: Option<String>,
) -> Result<()> {
    let mut batch = CommentBatch {
        issue_id: conv.issue_id.clone(),
        cursor,
        swept_at: timeutil::now_string(),
        ..CommentBatch::default()
    };
    let mut forwarded = 0;
    for c in &fresh {
        batch.seen.push(c.id.clone());
        if !forwards(c, conv) {
            continue;
        }
        let pieces = render::comment_chunks(&c.body, conv.identifier.as_deref(), &ctx.render);
        let total = pieces.len() as u32;
        for (i, p) in pieces.into_iter().enumerate() {
            batch.chunks.push(NewChunk {
                comment_id: c.id.clone(),
                chunk: i as u32,
                chunks: total,
                html: p.html,
                plain: p.plain,
            });
        }
        forwarded += 1;
    }
    if forwarded > 0 {
        batch.hot_until = Some(ctx.hot_until());
    }
    if fault::hit(fault::AFTER_READ_BEFORE_CURSOR_COMMIT) {
        return Err(CoreError::Fault);
    }
    let pieces = batch.chunks.len();
    ctx.store.commit_comments(batch).await?;
    if forwarded > 0 {
        tracing::info!(
            issue_id = %conv.issue_id,
            comments = forwarded,
            pieces,
            "agent replies queued for telegram"
        );
        ctx.sender_notify.notify_one();
    }
    Ok(())
}

/// The forwarding rule (ADR-0010 §1.6, open question 6.3 → `should_forward`).
fn forwards(c: &Comment, conv: &Conversation) -> bool {
    should_forward(c, &conv.agent_id)
}
