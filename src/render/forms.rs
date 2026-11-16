//! Questions and item verdicts (stage 6).

use super::cards::Tail;
use crate::domain::*;
use crate::store::Delivery;
use crate::telegram::InlineKeyboard;

pub fn questions_tail(
    d: &Delivery,
    _f: &QuestionForm,
    _draft: Option<&Draft>,
) -> (Tail, Option<InlineKeyboard>) {
    let mut t = Tail::default();
    t.line(&format!("Ответьте в Paperclip: {}", d.issue_ref));
    (t, None)
}

pub fn verdicts_tail(
    d: &Delivery,
    _v: &ItemVerdicts,
    _draft: Option<&Draft>,
) -> (Tail, Option<InlineKeyboard>) {
    let mut t = Tail::default();
    t.line(&format!("Ответьте в Paperclip: {}", d.issue_ref));
    (t, None)
}
