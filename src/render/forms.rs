//! Questions (single / multi / text / own answer, multi-step forms with a
//! summary) and item verdicts (architecture §8.3 a, b, d, f).

use super::cards::{Tail, cb};
use super::text::truncate_chars;
use super::{Op, button};
use crate::domain::*;
use crate::store::Delivery;
use crate::telegram::{Button, InlineKeyboard};

pub fn answer_at(draft: Option<&Draft>, q: usize) -> Option<&FormAnswer> {
    draft
        .and_then(|d| d.answers.form.get(q))
        .and_then(Option::as_ref)
        .filter(|a| !a.is_empty())
}

/// Human-readable answer: option labels and/or own text.
pub fn answer_text(q: &Question, a: Option<&FormAnswer>) -> String {
    let Some(a) = a else {
        return "—".into();
    };
    let mut parts: Vec<String> = a
        .option_ids
        .iter()
        .filter_map(|id| q.options.iter().find(|o| &o.id == id))
        .map(|o| truncate_chars(&o.label, 60))
        .collect();
    if let Some(t) = a.other_text.as_deref().filter(|t| !t.is_empty()) {
        parts.push(format!("«{}»", truncate_chars(t, 100)));
    }
    if parts.is_empty() {
        "—".into()
    } else {
        parts.join(", ")
    }
}

fn custom_caption(q: &Question) -> String {
    match q.custom_answer.as_ref().and_then(|c| c.label.as_deref()) {
        Some(l) => format!("✍️ {l}"),
        None => "✍️ Свой ответ".into(),
    }
}

pub fn text_hint(q: &Question) -> Option<String> {
    let tv = q.text_validation.as_ref()?;
    let mut hints = Vec::new();
    match tv.input_type {
        InputType::Integer => hints.push("целое число".to_string()),
        InputType::Number => hints.push("число".to_string()),
        InputType::Text => {}
    }
    match (tv.min_length, tv.max_length) {
        (Some(a), Some(b)) => hints.push(format!("от {a} до {b} символов")),
        (Some(a), None) => hints.push(format!("не меньше {a} символов")),
        (None, Some(b)) => hints.push(format!("не больше {b} символов")),
        _ => {}
    }
    match (tv.minimum, tv.maximum) {
        (Some(a), Some(b)) => hints.push(format!("от {a} до {b}")),
        (Some(a), None) => hints.push(format!("не меньше {a}")),
        (None, Some(b)) => hints.push(format!("не больше {b}")),
        _ => {}
    }
    (!hints.is_empty()).then(|| format!("Формат: {}", hints.join(", ")))
}

fn question_lines(tail: &mut Tail, f: &QuestionForm, q_idx: usize) {
    let q = &f.questions[q_idx];
    let n = f.questions.len();
    let mut head = String::new();
    if n > 1 {
        head.push_str(&format!("Вопрос {}/{}", q_idx + 1, n));
        if let Some(h) = q.header.as_deref() {
            head.push_str(&format!(" · {}", truncate_chars(h, 80)));
        }
        if !q.required {
            head.push_str(" (необязательный)");
        }
        tail.line_html(&format!("<b>{}</b>", super::escape(&head)));
    }
    tail.line(&truncate_chars(&q.prompt, 1000));
    if let Some(h) = q.help_text.as_deref() {
        tail.line_html(&format!(
            "<i>{}</i>",
            super::escape(&truncate_chars(h, 500))
        ));
    }
    if !q.options.is_empty() {
        tail.blank();
        for (i, o) in q.options.iter().enumerate() {
            match &o.description {
                Some(desc) => tail.line(&format!(
                    "{}. {} — {}",
                    i + 1,
                    truncate_chars(&o.label, 200),
                    truncate_chars(desc, 200)
                )),
                None => tail.line(&format!("{}. {}", i + 1, truncate_chars(&o.label, 200))),
            }
        }
    }
    if q.mode == AnswerMode::Text
        && let Some(h) = text_hint(q)
    {
        tail.line(&h);
    }
}

pub fn questions_tail(
    d: &Delivery,
    f: &QuestionForm,
    draft: Option<&Draft>,
) -> (Tail, Option<InlineKeyboard>) {
    let mut tail = Tail::default();
    let mut kb = InlineKeyboard::default();
    let n = f.questions.len();
    if n == 0 {
        // A form of optional questions only: nothing to ask, submit empty.
        tail.line("В форме нет вопросов.");
        kb.row(vec![button("Отправить", cb(d, Op::Submit))]);
        return (tail, Some(kb));
    }
    if let Some(Awaiting::Text { question }) = draft.and_then(|x| x.awaiting)
        && let Some(q) = f.questions.get(question)
    {
        question_lines(&mut tail, f, question);
        tail.blank();
        tail.line("✍️ Напишите ответ одним сообщением. /cancel — отмена");
        if q.mode == AnswerMode::Text
            && let Some(h) = text_hint(q)
        {
            tail.line(&h);
        }
        kb.row(vec![button("Отмена", cb(d, Op::Cancel))]);
        return (tail, Some(kb));
    }
    let step = draft.map_or(0, |x| x.step).min(n);
    if step == n {
        summary(&mut tail, &mut kb, d, f, draft);
        return (tail, Some(kb));
    }
    let q = &f.questions[step];
    question_lines(&mut tail, f, step);
    let current = answer_at(draft, step);
    if current.is_some() && n > 1 {
        tail.blank();
        tail.line(&format!("Ваш ответ: {}", answer_text(q, current)));
    }
    match q.mode {
        AnswerMode::Single => {
            for (i, o) in q.options.iter().enumerate() {
                let chosen = current.is_some_and(|a| a.option_ids.contains(&o.id));
                let mark = if chosen { "✓ " } else { "" };
                kb.row(vec![button(
                    &format!("{mark}{}. {}", i + 1, o.label),
                    cb(d, Op::Option).with_ab(step, i),
                )]);
            }
        }
        AnswerMode::Multi => {
            for (i, o) in q.options.iter().enumerate() {
                let chosen = current.is_some_and(|a| a.option_ids.contains(&o.id));
                let mark = if chosen { "☑" } else { "☐" };
                kb.row(vec![button(
                    &format!("{mark} {}. {}", i + 1, o.label),
                    cb(d, Op::Toggle).with_ab(step, i),
                )]);
            }
        }
        AnswerMode::Text => kb.row(vec![button(
            "✍️ Ответить текстом",
            cb(d, Op::Text).with_a(step),
        )]),
    }
    if q.custom_answer.is_some() && q.mode != AnswerMode::Text {
        kb.row(vec![button(
            &custom_caption(q),
            cb(d, Op::Text).with_a(step),
        )]);
    }
    // Single-select in a one-question form submits on press; everything else
    // navigates.
    let immediate = n == 1 && q.mode == AnswerMode::Single;
    if !immediate {
        let mut nav: Vec<Button> = Vec::new();
        if step > 0 {
            nav.push(button("◀ Назад", cb(d, Op::Back)));
        }
        if !q.required || current.is_some() {
            nav.push(button(
                if step + 1 == n {
                    "Готово ▶"
                } else {
                    "Далее ▶"
                },
                cb(d, Op::Next),
            ));
        }
        kb.row(nav);
    }
    (tail, Some(kb))
}

fn summary(
    tail: &mut Tail,
    kb: &mut InlineKeyboard,
    d: &Delivery,
    f: &QuestionForm,
    draft: Option<&Draft>,
) {
    let n = f.questions.len();
    if n == 1 {
        let q = &f.questions[0];
        tail.line(&truncate_chars(&q.prompt, 500));
        tail.blank();
        tail.line(&format!(
            "Отправить этот ответ? {}",
            answer_text(q, answer_at(draft, 0))
        ));
        kb.row(vec![
            button("Отправить", cb(d, Op::Submit)),
            button("Изменить", cb(d, Op::Edit).with_a(0)),
            button("Отмена", cb(d, Op::Cancel)),
        ]);
        return;
    }
    tail.line_html("<b>Проверьте ответы:</b>");
    for (i, q) in f.questions.iter().enumerate() {
        tail.line(&format!(
            "{}) {} → {}",
            i + 1,
            truncate_chars(&q.prompt, 80),
            answer_text(q, answer_at(draft, i))
        ));
    }
    let missing: Vec<usize> = f
        .questions
        .iter()
        .enumerate()
        .filter(|(i, q)| q.required && answer_at(draft, *i).is_none())
        .map(|(i, _)| i + 1)
        .collect();
    if !missing.is_empty() {
        tail.blank();
        tail.line(&format!(
            "Нет ответа на обязательные вопросы: {}",
            missing
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    } else {
        kb.row(vec![button(
            f.submit_label.as_deref().unwrap_or("Отправить"),
            cb(d, Op::Submit),
        )]);
    }
    let edits: Vec<Button> = (0..n)
        .map(|i| button(&format!("Изменить {}", i + 1), cb(d, Op::Edit).with_a(i)))
        .collect();
    for chunk in edits.chunks(4) {
        kb.row(chunk.to_vec());
    }
    kb.row(vec![button("Отменить", cb(d, Op::Cancel))]);
}

pub fn verdict_icon(v: Verdict) -> &'static str {
    match v {
        Verdict::Approve => "✅",
        Verdict::Reject => "❌",
        Verdict::Defer => "⏸",
    }
}

pub fn verdict_word(v: Verdict) -> &'static str {
    match v {
        Verdict::Approve => "одобрено",
        Verdict::Reject => "отклонено",
        Verdict::Defer => "отложено",
    }
}

pub fn verdict_op(v: Verdict) -> Op {
    match v {
        Verdict::Approve => Op::VerdictApprove,
        Verdict::Reject => Op::VerdictReject,
        Verdict::Defer => Op::VerdictDefer,
    }
}

pub fn chosen_verdict(draft: Option<&Draft>, item: usize) -> Option<&VerdictChoice> {
    draft
        .and_then(|d| d.answers.verdicts.get(item))
        .and_then(Option::as_ref)
}

pub fn verdicts_tail(
    d: &Delivery,
    v: &ItemVerdicts,
    draft: Option<&Draft>,
) -> (Tail, Option<InlineKeyboard>) {
    let mut tail = Tail::default();
    let mut kb = InlineKeyboard::default();
    let decided = |id: &str| v.decided.iter().find(|(i, _)| i == id).map(|(_, vd)| *vd);
    for (n, item) in v.items.iter().enumerate() {
        let state = match (decided(&item.id), chosen_verdict(draft, n)) {
            (Some(vd), _) => format!("{} {} (отправлено)", verdict_icon(vd), verdict_word(vd)),
            (None, Some(c)) => format!("{} {}", verdict_icon(c.verdict), verdict_word(c.verdict)),
            (None, None) => "—".into(),
        };
        let desc = item
            .description
            .as_deref()
            .map(|d| format!(" ({})", truncate_chars(d, 150)))
            .unwrap_or_default();
        tail.line(&format!(
            "{}. {}{desc}: {state}",
            n + 1,
            truncate_chars(&item.label, 150)
        ));
    }
    if let Some(Awaiting::VerdictReason { item, verdict }) = draft.and_then(|x| x.awaiting) {
        let label = v.items.get(item).map(|i| i.label.as_str()).unwrap_or("");
        tail.blank();
        tail.line(&format!(
            "✍️ Причина для «{}» ({}): напишите одним сообщением. /cancel — отмена",
            truncate_chars(label, 80),
            verdict_word(verdict)
        ));
        kb.row(vec![button("Отмена", cb(d, Op::Cancel))]);
        return (tail, Some(kb));
    }
    let mut open = 0;
    for (n, item) in v.items.iter().enumerate() {
        if decided(&item.id).is_some() {
            continue;
        }
        open += 1;
        let chosen = chosen_verdict(draft, n).map(|c| c.verdict);
        let row = v
            .verdicts
            .iter()
            .map(|vd| {
                let mark = if chosen == Some(*vd) { "•" } else { "" };
                button(
                    &format!("{mark}{} {}", verdict_icon(*vd), n + 1),
                    cb(d, verdict_op(*vd)).with_a(n),
                )
            })
            .collect();
        kb.row(row);
    }
    let picked = (0..v.items.len())
        .filter(|n| decided(&v.items[*n].id).is_none() && chosen_verdict(draft, *n).is_some())
        .count();
    let mut last = Vec::new();
    if open > 0 && v.allow_bulk_approve && v.verdicts.contains(&Verdict::Approve) {
        last.push(button("Одобрить все", cb(d, Op::ApproveAll)));
    }
    if picked > 0 {
        last.push(button(&format!("Отправить ({picked})"), cb(d, Op::Submit)));
    }
    kb.row(last);
    (tail, Some(kb))
}
