//! Governed confirmations: `toolAction` and `secretProposal` (stage 6a).

use super::dialog::Effects;
use super::{Ctx, Result};
use crate::domain::*;
use crate::paperclip::PaperclipApi;
use crate::render::Callback;
use crate::store::{Delivery, DeliveryStatus, OutboxEntry};
use crate::telegram::TelegramApi;

pub async fn callback<P: PaperclipApi, T: TelegramApi>(
    _ctx: &Ctx<P, T>,
    d: &Delivery,
    _draft: Option<Draft>,
    _cb: Callback,
) -> Effects {
    Effects::stale(d)
}

pub async fn text<P: PaperclipApi, T: TelegramApi>(
    _ctx: &Ctx<P, T>,
    _d: &Delivery,
    _draft: Option<Draft>,
    _text: &str,
) -> Effects {
    Effects::message(super::dialog::NOT_WAITING)
}

pub async fn process<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    d: &Delivery,
    e: &OutboxEntry,
    _r: &Resolution,
) -> Result<()> {
    ctx.store
        .outbox_abandon(e.id, d.id, DeliveryStatus::Closed, None)
        .await?;
    Ok(())
}

pub async fn follow<P: PaperclipApi, T: TelegramApi>(
    _ctx: &Ctx<P, T>,
    _d: &Delivery,
    _i: &Interaction,
) -> Result<()> {
    Ok(())
}
