//! `ask_user_questions` and `request_item_verdicts` dialogs (stage 6).

use super::dialog::Effects;
use super::{Ctx, Result};
use crate::domain::*;
use crate::paperclip::PaperclipApi;
use crate::render::Callback;
use crate::store::{Delivery, OutboxEntry};
use crate::telegram::TelegramApi;

pub fn questions_callback(
    d: &Delivery,
    _f: &QuestionForm,
    _draft: Option<Draft>,
    _cb: Callback,
) -> Effects {
    Effects::stale(d)
}

pub fn verdicts_callback(
    d: &Delivery,
    _v: &ItemVerdicts,
    _draft: Option<Draft>,
    _cb: Callback,
) -> Effects {
    Effects::stale(d)
}

pub fn questions_text(
    _d: &Delivery,
    _f: &QuestionForm,
    _draft: Option<Draft>,
    _text: &str,
    _via_reply: bool,
) -> Effects {
    Effects::message(super::dialog::NOT_WAITING)
}

pub fn verdicts_text(
    _d: &Delivery,
    _v: &ItemVerdicts,
    _draft: Option<Draft>,
    _text: &str,
) -> Effects {
    Effects::message(super::dialog::NOT_WAITING)
}

pub fn respond_summary(_f: &QuestionForm, _answers: &[QuestionAnswer]) -> String {
    "✅ Ответ отправлен".into()
}

pub async fn verdicts_partial_done<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    d: &Delivery,
    e: &OutboxEntry,
    _record: &Interaction,
) -> Result<()> {
    ctx.store
        .outbox_done(e.id, d.id, crate::store::DeliveryStatus::Answered, None)
        .await?;
    Ok(())
}

/// After a restart: one message per unfinished form, then the card again.
pub async fn announce_restart<P: PaperclipApi, T: TelegramApi>(_ctx: &Ctx<P, T>) -> Result<()> {
    Ok(())
}
