//! Sending and editing card messages.

use super::{CoreError, Ctx, Result};
use crate::fault;
use crate::paperclip::PaperclipApi;
use crate::render::{self, cards::notice_reason};
use crate::store::DeliveryStatus;
use crate::telegram::{ReplyMarkup, TelegramApi, TelegramError};
use crate::timeutil;

/// Delivers a `new` / `sending` card: all chunks, keyboard on the last one.
/// A crash between `sendMessage` and the commit re-sends the card after a
/// restart (accepted, architecture §4.2 invariant 4).
pub async fn deliver<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    delivery_id: i64,
) -> Result<()> {
    let Some(d) = ctx.store.delivery(delivery_id).await? else {
        return Ok(());
    };
    if !matches!(d.status, DeliveryStatus::New | DeliveryStatus::Sending) {
        return Ok(());
    }
    let final_status = if notice_reason(&d.snapshot.interaction, &ctx.render.governed).is_some() {
        DeliveryStatus::Unsupported
    } else {
        DeliveryStatus::Sent
    };
    ctx.store
        .set_status(delivery_id, DeliveryStatus::Sending, None)
        .await?;
    let draft = ctx.store.draft(delivery_id).await?;
    let r = render::card(&d, draft.as_ref(), &ctx.render, timeutil::now());
    let last = r.messages.len().saturating_sub(1);
    let mut ids = Vec::with_capacity(r.messages.len());
    for (n, html) in r.messages.into_iter().enumerate() {
        let markup = if n == last {
            r.keyboard.clone().map(ReplyMarkup::Inline)
        } else {
            None
        };
        ids.push(ctx.tg.send(html, markup).await?);
    }
    if fault::hit(fault::AFTER_SEND_BEFORE_COMMIT) {
        return Err(CoreError::Fault);
    }
    ctx.store.mark_sent(delivery_id, final_status, ids).await?;
    tracing::info!(
        delivery_id,
        interaction_id = %d.interaction_id,
        kind = %d.kind,
        status = final_status.as_str(),
        "card delivered"
    );
    Ok(())
}

/// Re-renders the card in place (last message). A deleted message is sent
/// again as a new one.
pub async fn refresh<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    delivery_id: i64,
) -> Result<()> {
    let Some(d) = ctx.store.delivery(delivery_id).await? else {
        return Ok(());
    };
    let Some(message_id) = d.last_message_id() else {
        return Ok(());
    };
    let draft = ctx.store.draft(delivery_id).await?;
    let r = render::card(&d, draft.as_ref(), &ctx.render, timeutil::now());
    let html = r.messages.last().cloned().unwrap_or_default();
    match ctx
        .tg
        .edit(message_id, html.clone(), r.keyboard.clone())
        .await
    {
        Ok(()) | Err(TelegramError::NotModified) => Ok(()),
        Err(TelegramError::MessageNotFound) => {
            let id = ctx
                .tg
                .send(html, r.keyboard.map(ReplyMarkup::Inline))
                .await?;
            ctx.store.replace_last_message(delivery_id, id).await?;
            Ok(())
        }
        Err(e) => {
            tracing::warn!(delivery_id, class = e.class(), "cannot update card message");
            Ok(())
        }
    }
}

/// Plain informational message to the chat; failures are logged only.
pub async fn notify<P: PaperclipApi, T: TelegramApi>(ctx: &Ctx<P, T>, html: String) {
    if let Err(e) = ctx.tg.send(html, None).await {
        tracing::warn!(class = e.class(), "cannot send notification");
    }
}
