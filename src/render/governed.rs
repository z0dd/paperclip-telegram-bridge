//! Governed cards: `toolAction` and `secretProposal` (stage 6a).

use time::OffsetDateTime;

use super::cards::{NoticeReason, RenderConfig, Rendered, Tail, compose, header, status_lines};
use crate::domain::*;
use crate::store::Delivery;

pub fn tool_action(
    d: &Delivery,
    _t: &ToolAction,
    _draft: Option<&Draft>,
    cfg: &RenderConfig,
    _now: OffsetDateTime,
    _open: bool,
    _notice: Option<NoticeReason>,
) -> Rendered {
    let mut tail = Tail::default();
    tail.line(&format!("Решите в Paperclip: {}", d.issue_ref));
    status_lines(d, &mut tail);
    Rendered {
        messages: compose(
            &header("🛠", "Действие от", d, cfg),
            "",
            &tail.into_html(),
            &d.issue_ref,
            1,
        ),
        keyboard: None,
    }
}

pub fn secret_proposal(
    d: &Delivery,
    _s: &SecretProposal,
    _draft: Option<&Draft>,
    cfg: &RenderConfig,
    _now: OffsetDateTime,
    _open: bool,
    _notice: Option<NoticeReason>,
) -> Rendered {
    let mut tail = Tail::default();
    tail.line(&format!("Решите в Paperclip: {}", d.issue_ref));
    status_lines(d, &mut tail);
    Rendered {
        messages: compose(
            &header("🔑", "Секрет от", d, cfg),
            "",
            &tail.into_html(),
            &d.issue_ref,
            1,
        ),
        keyboard: None,
    }
}
