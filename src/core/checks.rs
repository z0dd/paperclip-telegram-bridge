//! Start checks of the supergroup (ADR-0010 §2.7): `getMe`, `getChat`
//! (supergroup with topics), `getChatMember` (bot is an admin with
//! `can_manage_topics`). A violation is an ERROR and unhealthy, checked again
//! every 5 minutes; the process keeps running (no restart loop).

use tokio_util::sync::CancellationToken;

use super::{AUTH_RETRY_INTERVAL, Ctx};
use crate::paperclip::PaperclipApi;
use crate::retry::Backoff;
use crate::telegram::{TelegramApi, TelegramError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckError {
    /// Telegram answered and the setup is wrong. The text is for the log.
    Setup(String),
    /// No answer (network, proxy, rate limit): try again soon.
    Unavailable(TelegramError),
}

impl From<TelegramError> for CheckError {
    fn from(e: TelegramError) -> Self {
        match e {
            TelegramError::Auth { .. } => {
                CheckError::Setup("telegram rejected the bot token".into())
            }
            TelegramError::Forbidden => {
                CheckError::Setup("the bot is not a member of TELEGRAM_CHAT_ID".into())
            }
            TelegramError::BadRequest { .. } => CheckError::Setup(
                "TELEGRAM_CHAT_ID is not a chat the bot can see (check the id and that the bot is added)"
                    .into(),
            ),
            e => CheckError::Unavailable(e),
        }
    }
}

/// One round of the checks.
pub async fn check<T: TelegramApi>(api: &T, chat_id: i64) -> Result<(), CheckError> {
    let bot_id = api.get_me().await?;
    let chat = api.get_chat(chat_id).await?;
    if chat.kind != "supergroup" {
        return Err(CheckError::Setup(format!(
            "TELEGRAM_CHAT_ID is a {}, not a supergroup",
            chat.kind
        )));
    }
    if !chat.is_forum {
        return Err(CheckError::Setup(
            "topics are off in the supergroup: turn on Topics in the group settings".into(),
        ));
    }
    let member = api.get_chat_member(chat_id, bot_id).await?;
    if member.status != "administrator" {
        return Err(CheckError::Setup(format!(
            "the bot is {} in the supergroup, it must be an administrator",
            member.status
        )));
    }
    if !member.can_manage_topics {
        return Err(CheckError::Setup(
            "the bot has no \"Manage topics\" right (can_manage_topics)".into(),
        ));
    }
    Ok(())
}

/// Runs the checks until they pass.
pub async fn run<P: PaperclipApi, T: TelegramApi>(ctx: Ctx<P, T>, cancel: CancellationToken) {
    let backoff = Backoff::from_config(&ctx.cfg.retry);
    let mut failures = 0u32;
    loop {
        let delay = match check(ctx.telegram.as_ref(), ctx.cfg.telegram.chat_id).await {
            Ok(()) => {
                ctx.health.telegram_ok();
                ctx.health.set_telegram_group_ok(true);
                tracing::info!("telegram supergroup checks passed");
                return;
            }
            Err(CheckError::Setup(reason)) => {
                ctx.health.telegram_ok();
                ctx.health.set_telegram_group_ok(false);
                tracing::error!(reason = %reason, "telegram supergroup check failed, retry in 5 minutes");
                AUTH_RETRY_INTERVAL
            }
            Err(CheckError::Unavailable(e)) => {
                failures += 1;
                tracing::warn!(
                    class = e.class(),
                    failures,
                    "telegram supergroup check postponed"
                );
                match e {
                    TelegramError::RateLimited { retry_after } => retry_after,
                    _ => backoff.delay(failures),
                }
            }
        };
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(delay) => {}
        }
    }
}
