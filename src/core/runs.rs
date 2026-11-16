//! Agent run failures and completed tasks (stage 7).

use tokio_util::sync::CancellationToken;

use super::Ctx;
use crate::paperclip::PaperclipApi;
use crate::telegram::TelegramApi;

pub async fn run<P: PaperclipApi, T: TelegramApi>(_ctx: Ctx<P, T>, cancel: CancellationToken) {
    cancel.cancelled().await;
}
