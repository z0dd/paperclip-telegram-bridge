//! In-progress answers of the user (bridge state, not Paperclip data).

use serde::{Deserialize, Serialize};

use super::Verdict;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FormAnswer {
    pub option_ids: Vec<String>,
    pub other_text: Option<String>,
}

impl FormAnswer {
    pub fn is_empty(&self) -> bool {
        self.option_ids.is_empty() && self.other_text.as_deref().is_none_or(str::is_empty)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerdictChoice {
    pub verdict: Verdict,
    pub reason: Option<String>,
}

/// Answers collected so far. Only the part matching the card kind is used.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DraftAnswers {
    /// One slot per question (`ask_user_questions`).
    #[serde(default)]
    pub form: Vec<Option<FormAnswer>>,
    /// Selected option ids (checkbox confirmation).
    #[serde(default)]
    pub selected: Vec<String>,
    /// Whether `selected` was initialised from the defaults.
    #[serde(default)]
    pub selected_init: bool,
    /// One slot per item (`request_item_verdicts`).
    #[serde(default)]
    pub verdicts: Vec<Option<VerdictChoice>>,
}

/// What the bridge is waiting for in this draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Awaiting {
    /// Free text for question `question` (text mode or own answer).
    Text { question: usize },
    /// Rejection reason; `optional` adds a "without reason" button.
    RejectReason { optional: bool },
    /// Reason for verdict on item `item`.
    VerdictReason { item: usize, verdict: Verdict },
    /// Second confirmation step of a destructive action (expires in 2 min).
    GovernedConfirm,
}

impl Awaiting {
    pub fn expects_text(self) -> bool {
        !matches!(self, Self::GovernedConfirm)
    }

    pub fn to_db(self) -> String {
        match self {
            Self::Text { question } => format!("text:{question}"),
            Self::RejectReason { optional: false } => "reject_reason".into(),
            Self::RejectReason { optional: true } => "reject_reason_optional".into(),
            Self::VerdictReason { item, verdict } => {
                format!("verdict_reason:{item}:{}", verdict.as_str())
            }
            Self::GovernedConfirm => "governed_confirm".into(),
        }
    }

    pub fn from_db(s: &str) -> Option<Self> {
        let mut parts = s.split(':');
        Some(match parts.next()? {
            "text" => Self::Text {
                question: parts.next()?.parse().ok()?,
            },
            "reject_reason" => Self::RejectReason { optional: false },
            "reject_reason_optional" => Self::RejectReason { optional: true },
            "verdict_reason" => Self::VerdictReason {
                item: parts.next()?.parse().ok()?,
                verdict: Verdict::parse(parts.next()?)?,
            },
            "governed_confirm" => Self::GovernedConfirm,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub delivery_id: i64,
    /// Current question index; `== questions.len()` means the summary screen.
    pub step: usize,
    pub answers: DraftAnswers,
    pub awaiting: Option<Awaiting>,
    pub prompt_message_id: Option<i64>,
    pub updated_at: String,
}

impl Draft {
    pub fn new(delivery_id: i64) -> Self {
        Self {
            delivery_id,
            step: 0,
            answers: DraftAnswers::default(),
            awaiting: None,
            prompt_message_id: None,
            updated_at: crate::timeutil::now_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn awaiting_roundtrip() {
        for a in [
            Awaiting::Text { question: 3 },
            Awaiting::RejectReason { optional: false },
            Awaiting::RejectReason { optional: true },
            Awaiting::VerdictReason {
                item: 2,
                verdict: Verdict::Reject,
            },
            Awaiting::GovernedConfirm,
        ] {
            assert_eq!(Awaiting::from_db(&a.to_db()), Some(a));
        }
        assert_eq!(Awaiting::from_db("nonsense"), None);
        assert!(!Awaiting::GovernedConfirm.expects_text());
    }
}
