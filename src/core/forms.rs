//! `ask_user_questions` and `request_item_verdicts` dialogs. Forms are
//! collected in the draft and sent as one `respond`: the server accepts no
//! partial forms (API §6.1). Verdicts may be sent partially (API §6.4).

use super::dialog::{Effects, cancel_waiting};
use super::{Ctx, Result, cards};
use crate::domain::*;
use crate::paperclip::PaperclipApi;
use crate::render::forms::{answer_at, chosen_verdict};
use crate::render::{Callback, Op, escape};
use crate::store::{Delivery, DeliveryStatus, OutboxEntry, StoreOp};
use crate::telegram::TelegramApi;
use crate::timeutil;

fn draft_or_new(d: &Delivery, draft: Option<Draft>, slots: usize) -> Draft {
    let mut dr = draft.unwrap_or_else(|| Draft::new(d.id));
    if dr.answers.form.len() < slots {
        dr.answers.form.resize(slots, None);
    }
    dr.updated_at = timeutil::now_string();
    dr
}

/// Answers for `respond`, in question order; empty answers are omitted.
pub fn build_answers(f: &QuestionForm, draft: Option<&Draft>) -> Vec<QuestionAnswer> {
    f.questions
        .iter()
        .enumerate()
        .filter_map(|(i, q)| {
            let a = answer_at(draft, i)?;
            let mut option_ids = a.option_ids.clone();
            let other_text = a.other_text.clone().filter(|t| !t.is_empty());
            // Legacy forms carry own text in a dedicated free-text option.
            if other_text.is_some()
                && let Some(id) = q.custom_answer.as_ref().and_then(|c| c.option_id.clone())
                && !option_ids.contains(&id)
            {
                option_ids.push(id);
            }
            Some(QuestionAnswer {
                question_id: q.id.clone(),
                option_ids,
                other_text,
            })
        })
        .collect()
}

pub fn missing_required(f: &QuestionForm, draft: Option<&Draft>) -> Vec<usize> {
    f.questions
        .iter()
        .enumerate()
        .filter(|(i, q)| q.required && answer_at(draft, *i).is_none())
        .map(|(i, _)| i)
        .collect()
}

fn reset_draft(d: &Delivery) -> Effects {
    Effects {
        ops: vec![StoreOp::DeleteDraft(d.id), StoreOp::BumpGeneration(d.id)],
        answer: Some("Черновик сброшен".into()),
        refresh: vec![d.id],
        ..Default::default()
    }
}

pub fn questions_callback(
    d: &Delivery,
    f: &QuestionForm,
    draft: Option<Draft>,
    cb: Callback,
) -> Effects {
    let n = f.questions.len();
    let step = draft.as_ref().map_or(0, |x| x.step).min(n);
    let q_idx = cb.a.map(|a| a as usize);
    match cb.op {
        Op::Option => {
            let (Some(qi), Some(oi)) = (q_idx, cb.b) else {
                return Effects::stale(d);
            };
            let Some(q) = f
                .questions
                .get(qi)
                .filter(|q| q.mode == AnswerMode::Single && qi == step)
            else {
                return Effects::stale(d);
            };
            let Some(opt) = q.options.get(oi as usize) else {
                return Effects::stale(d);
            };
            let mut dr = draft_or_new(d, draft, n);
            dr.answers.form[qi] = Some(FormAnswer {
                option_ids: vec![opt.id.clone()],
                other_text: None,
            });
            if n == 1 {
                let answers = build_answers(f, Some(&dr));
                let mut e = Effects::enqueue(d, Resolution::Respond { answers }, "Отправляю…");
                e.ops.insert(0, StoreOp::PutDraft(dr));
                return e;
            }
            dr.step = qi + 1;
            Effects::redraw(d, dr, None)
        }
        Op::Toggle => {
            let (Some(qi), Some(oi)) = (q_idx, cb.b) else {
                return Effects::stale(d);
            };
            let Some(q) = f
                .questions
                .get(qi)
                .filter(|q| q.mode == AnswerMode::Multi && qi == step)
            else {
                return Effects::stale(d);
            };
            let Some(opt) = q.options.get(oi as usize) else {
                return Effects::stale(d);
            };
            let mut dr = draft_or_new(d, draft, n);
            let a = dr.answers.form[qi].get_or_insert_with(FormAnswer::default);
            if let Some(pos) = a.option_ids.iter().position(|x| *x == opt.id) {
                a.option_ids.remove(pos);
            } else {
                a.option_ids.push(opt.id.clone());
                // Keep the order of options.
                let order: Vec<String> = q.options.iter().map(|o| o.id.clone()).collect();
                a.option_ids
                    .sort_by_key(|id| order.iter().position(|o| o == id).unwrap_or(usize::MAX));
            }
            Effects::redraw(d, dr, None)
        }
        Op::Next => {
            if step >= n {
                return Effects::stale(d);
            }
            if f.questions[step].required && answer_at(draft.as_ref(), step).is_none() {
                return Effects::answer("Сначала ответьте на этот вопрос");
            }
            let mut dr = draft_or_new(d, draft, n);
            dr.step = step + 1;
            Effects::redraw(d, dr, None)
        }
        Op::Back => {
            if step == 0 {
                return Effects::stale(d);
            }
            let mut dr = draft_or_new(d, draft, n);
            dr.step = step - 1;
            Effects::redraw(d, dr, None)
        }
        Op::Edit => {
            let Some(qi) = q_idx.filter(|q| *q < n) else {
                return Effects::stale(d);
            };
            let mut dr = draft_or_new(d, draft, n);
            dr.step = qi;
            Effects::redraw(d, dr, None)
        }
        Op::Text => {
            let Some(qi) = q_idx.filter(|q| *q < n) else {
                return Effects::stale(d);
            };
            let q = &f.questions[qi];
            if q.mode != AnswerMode::Text && q.custom_answer.is_none() {
                return Effects::stale(d);
            }
            let mut prompt = format!(
                "Напишите ответ на вопрос {} ({}) одним сообщением. /cancel — отмена",
                qi + 1,
                escape(&d.issue_ref)
            );
            if q.mode == AnswerMode::Text
                && let Some(h) = crate::render::forms::text_hint(q)
            {
                prompt.push('\n');
                prompt.push_str(&escape(&h));
            }
            Effects::await_text(
                d,
                draft_or_new(d, draft, n),
                Awaiting::Text { question: qi },
                prompt,
            )
        }
        Op::Submit => {
            if step < n && n > 0 {
                return Effects::stale(d);
            }
            if let Some(first) = missing_required(f, draft.as_ref()).first() {
                return Effects::answer(&format!(
                    "Нет ответа на обязательный вопрос {}",
                    first + 1
                ));
            }
            let answers = build_answers(f, draft.as_ref());
            Effects::enqueue(d, Resolution::Respond { answers }, "Отправляю…")
        }
        Op::Cancel => {
            if draft.as_ref().and_then(|x| x.awaiting).is_some() {
                return cancel_waiting(d, draft);
            }
            reset_draft(d)
        }
        _ => Effects::stale(d),
    }
}

/// Bridge-side `textValidation` (API §6.1). `pattern` is left to the server:
/// a `422` returns the form to the user with the server message.
pub fn validate_text(q: &Question, text: &str) -> std::result::Result<(), String> {
    if text.is_empty() {
        return Err("Ответ не может быть пустым.".into());
    }
    let Some(tv) = q
        .text_validation
        .as_ref()
        .filter(|_| q.mode == AnswerMode::Text)
    else {
        return Ok(());
    };
    let len = text.chars().count() as u32;
    if let Some(min) = tv.min_length.filter(|m| len < *m) {
        return Err(format!("Слишком коротко: нужно не меньше {min} символов."));
    }
    if let Some(max) = tv.max_length.filter(|m| len > *m) {
        return Err(format!("Слишком длинно: не больше {max} символов."));
    }
    let number = match tv.input_type {
        InputType::Text => None,
        InputType::Integer => Some(
            text.parse::<i64>()
                .map(|v| v as f64)
                .map_err(|_| "Нужно целое число.".to_string())?,
        ),
        InputType::Number => Some(
            text.parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
                .ok_or_else(|| "Нужно число, например 3.5".to_string())?,
        ),
    };
    if let Some(v) = number {
        if let Some(min) = tv.minimum.filter(|m| v < *m) {
            return Err(format!("Число должно быть не меньше {min}."));
        }
        if let Some(max) = tv.maximum.filter(|m| v > *m) {
            return Err(format!("Число должно быть не больше {max}."));
        }
    }
    Ok(())
}

pub fn questions_text(
    d: &Delivery,
    f: &QuestionForm,
    draft: Option<Draft>,
    text: &str,
    via_reply: bool,
) -> Effects {
    let n = f.questions.len();
    let qi = match draft.as_ref().and_then(|x| x.awaiting) {
        Some(Awaiting::Text { question }) => question,
        _ if via_reply => {
            // A reply to the card answers the current question if it takes text.
            let step = draft.as_ref().map_or(0, |x| x.step);
            match f.questions.get(step) {
                Some(q) if q.mode == AnswerMode::Text || q.custom_answer.is_some() => step,
                _ => return Effects::message(super::dialog::NOT_WAITING),
            }
        }
        _ => return Effects::message(super::dialog::NOT_WAITING),
    };
    let Some(q) = f.questions.get(qi) else {
        return Effects::message(super::dialog::NOT_WAITING);
    };
    if let Err(hint) = validate_text(q, text) {
        return Effects::message(format!("{} Попробуйте ещё раз или /cancel.", escape(&hint)));
    }
    let mut dr = draft_or_new(d, draft, n);
    let keep_options = if q.mode == AnswerMode::Multi {
        dr.answers.form[qi]
            .as_ref()
            .map(|a| a.option_ids.clone())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    dr.answers.form[qi] = Some(FormAnswer {
        option_ids: keep_options,
        other_text: Some(text.to_string()),
    });
    dr.awaiting = None;
    // A one-question form goes to the confirmation; multi-select with own text
    // stays on the question; otherwise move on.
    dr.step = if n == 1 {
        1
    } else if q.mode == AnswerMode::Multi {
        qi
    } else {
        qi + 1
    };
    Effects::redraw(d, dr, None)
}

pub fn verdicts_callback(
    d: &Delivery,
    v: &ItemVerdicts,
    draft: Option<Draft>,
    cb: Callback,
) -> Effects {
    let decided = |id: &str| v.decided.iter().any(|(i, _)| i == id);
    let slots = v.items.len();
    let mut dr = draft.clone().unwrap_or_else(|| Draft::new(d.id));
    if dr.answers.verdicts.len() < slots {
        dr.answers.verdicts.resize(slots, None);
    }
    dr.updated_at = timeutil::now_string();
    let verdict = match cb.op {
        Op::VerdictApprove => Some(Verdict::Approve),
        Op::VerdictReject => Some(Verdict::Reject),
        Op::VerdictDefer => Some(Verdict::Defer),
        _ => None,
    };
    if let Some(verdict) = verdict {
        let Some(item) = cb.a.map(|a| a as usize).filter(|i| *i < slots) else {
            return Effects::stale(d);
        };
        if !v.verdicts.contains(&verdict) || decided(&v.items[item].id) {
            return Effects::stale(d);
        }
        if chosen_verdict(Some(&dr), item).is_some_and(|c| c.verdict == verdict) {
            dr.answers.verdicts[item] = None;
            return Effects::redraw(d, dr, None);
        }
        if v.require_reason_on.contains(&verdict) {
            return Effects::await_text(
                d,
                dr,
                Awaiting::VerdictReason { item, verdict },
                format!(
                    "Причина для пункта {} ({}): напишите одним сообщением. /cancel — отмена",
                    item + 1,
                    crate::render::forms::verdict_word(verdict)
                ),
            );
        }
        dr.answers.verdicts[item] = Some(VerdictChoice {
            verdict,
            reason: None,
        });
        return Effects::redraw(d, dr, None);
    }
    match cb.op {
        Op::ApproveAll => {
            if !v.allow_bulk_approve || !v.verdicts.contains(&Verdict::Approve) {
                return Effects::stale(d);
            }
            if v.require_reason_on.contains(&Verdict::Approve) {
                return Effects::answer("Для одобрения нужна причина — выберите по пунктам");
            }
            for (n, item) in v.items.iter().enumerate() {
                if !decided(&item.id) {
                    dr.answers.verdicts[n] = Some(VerdictChoice {
                        verdict: Verdict::Approve,
                        reason: None,
                    });
                }
            }
            Effects::redraw(d, dr, None)
        }
        Op::Submit => {
            let verdicts: Vec<ItemVerdictAnswer> = v
                .items
                .iter()
                .enumerate()
                .filter(|(_, item)| !decided(&item.id))
                .filter_map(|(n, item)| {
                    chosen_verdict(Some(&dr), n).map(|c| ItemVerdictAnswer {
                        id: item.id.clone(),
                        verdict: c.verdict,
                        reason: c.reason.clone(),
                    })
                })
                .collect();
            if verdicts.is_empty() {
                return Effects::answer("Выберите хотя бы один вердикт");
            }
            Effects::enqueue(d, Resolution::Verdicts { verdicts }, "Отправляю…")
        }
        Op::Cancel => {
            if draft.as_ref().and_then(|x| x.awaiting).is_some() {
                return cancel_waiting(d, draft);
            }
            reset_draft(d)
        }
        _ => Effects::stale(d),
    }
}

pub fn verdicts_text(d: &Delivery, v: &ItemVerdicts, draft: Option<Draft>, text: &str) -> Effects {
    let Some(mut dr) = draft else {
        return Effects::message(super::dialog::NOT_WAITING);
    };
    let Some(Awaiting::VerdictReason { item, verdict }) = dr.awaiting else {
        return Effects::message(super::dialog::NOT_WAITING);
    };
    if text.is_empty() {
        return Effects::message("Причина не может быть пустой. /cancel — отмена");
    }
    if dr.answers.verdicts.len() < v.items.len() {
        dr.answers.verdicts.resize(v.items.len(), None);
    }
    if item >= dr.answers.verdicts.len() {
        return Effects::message(super::dialog::NOT_WAITING);
    }
    dr.answers.verdicts[item] = Some(VerdictChoice {
        verdict,
        reason: Some(text.to_string()),
    });
    dr.awaiting = None;
    dr.updated_at = timeutil::now_string();
    Effects::redraw(d, dr, None)
}

pub fn respond_summary(f: &QuestionForm, answers: &[QuestionAnswer]) -> String {
    if let ([q], [a]) = (f.questions.as_slice(), answers)
        && a.other_text.is_none()
        && let Some(label) = a
            .option_ids
            .first()
            .and_then(|id| q.options.iter().find(|o| &o.id == id))
            .map(|o| crate::render::truncate_chars(&o.label, 100))
    {
        return format!("✅ Ответ отправлен: {label}");
    }
    "✅ Ответ отправлен".into()
}

/// A partial verdict submission succeeded: the card stays open with the
/// remaining items.
pub async fn verdicts_partial_done<P: PaperclipApi, T: TelegramApi>(
    ctx: &Ctx<P, T>,
    d: &Delivery,
    e: &OutboxEntry,
    record: &Interaction,
) -> Result<()> {
    ctx.store
        .outbox_remove(e.id, d.id, DeliveryStatus::Sent)
        .await?;
    let mut snapshot = d.snapshot.clone();
    if let (InteractionBody::Verdicts(new), InteractionBody::Verdicts(old)) =
        (&record.body, &mut snapshot.interaction.body)
    {
        old.decided = new.decided.clone();
    }
    ctx.store.update_snapshot(d.id, snapshot).await?;
    ctx.store
        .apply(vec![
            StoreOp::DeleteDraft(d.id),
            StoreOp::BumpGeneration(d.id),
        ])
        .await?;
    tracing::info!(
        delivery_id = d.id,
        interaction_id = %d.interaction_id,
        outcome = "partial",
        "verdicts delivered to Paperclip"
    );
    cards::notify(
        ctx,
        format!(
            "✅ Вердикты по {} отправлены, остальные пункты ждут решения.",
            escape(&d.issue_ref)
        ),
    )
    .await;
    cards::refresh(ctx, d.id).await
}

/// After a restart: one message per unfinished draft, then the card again
/// with a fresh keyboard (architecture §4.3 п. 4).
pub async fn announce_restart<P: PaperclipApi, T: TelegramApi>(ctx: &Ctx<P, T>) -> Result<()> {
    for dr in ctx.store.drafts().await? {
        let Some(d) = ctx.store.delivery(dr.delivery_id).await? else {
            continue;
        };
        if d.status != DeliveryStatus::Sent {
            continue;
        }
        let progress = match &d.snapshot.interaction.body {
            InteractionBody::Questions(f) if f.questions.len() > 1 => {
                if dr.step >= f.questions.len() {
                    "сводка ответов".to_string()
                } else {
                    format!("вопрос {}/{}", dr.step + 1, f.questions.len())
                }
            }
            _ => "черновик ответа".to_string(),
        };
        let waiting = if dr.awaiting.is_some_and(Awaiting::expects_text) {
            ", жду текстовый ответ"
        } else {
            ""
        };
        cards::notify(
            ctx,
            format!(
                "🔄 Bridge перезапущен, продолжаем {}: {progress}{waiting}",
                escape(&d.issue_ref)
            ),
        )
        .await;
        ctx.store.apply(vec![StoreOp::BumpGeneration(d.id)]).await?;
        cards::refresh(ctx, d.id).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(mode: AnswerMode, tv: Option<TextValidation>) -> Question {
        Question {
            id: "q".into(),
            header: None,
            prompt: "?".into(),
            help_text: None,
            required: true,
            mode,
            options: vec![],
            custom_answer: None,
            text_validation: tv,
        }
    }

    fn tv(
        input_type: InputType,
        min: Option<u32>,
        max: Option<u32>,
        lo: Option<f64>,
        hi: Option<f64>,
    ) -> TextValidation {
        TextValidation {
            min_length: min,
            max_length: max,
            pattern: Some("^x$".into()),
            input_type,
            minimum: lo,
            maximum: hi,
        }
    }

    #[test]
    fn text_validation_rules() {
        let int = q(
            AnswerMode::Text,
            Some(tv(InputType::Integer, None, Some(3), Some(1.0), Some(99.0))),
        );
        assert!(validate_text(&int, "7").is_ok());
        assert!(validate_text(&int, "7.5").unwrap_err().contains("целое"));
        assert!(validate_text(&int, "100").unwrap_err().contains("больше"));
        assert!(validate_text(&int, "1000").unwrap_err().contains("длинно"));
        assert!(validate_text(&int, "0").unwrap_err().contains("меньше"));
        let num = q(
            AnswerMode::Text,
            Some(tv(InputType::Number, None, None, None, None)),
        );
        assert!(validate_text(&num, "3.5").is_ok());
        assert!(validate_text(&num, "abc").is_err());
        assert!(validate_text(&num, "NaN").is_err());
        let txt = q(
            AnswerMode::Text,
            Some(tv(InputType::Text, Some(2), None, None, None)),
        );
        assert!(validate_text(&txt, "a").unwrap_err().contains("коротко"));
        assert!(
            validate_text(&txt, "ab").is_ok(),
            "pattern is checked by the server"
        );
        assert!(validate_text(&q(AnswerMode::Text, None), "").is_err());
    }
}
