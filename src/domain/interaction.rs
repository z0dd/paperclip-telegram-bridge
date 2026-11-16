//! Interactions ("cards") as the bridge understands them. Independent of the
//! Paperclip JSON format and of Telegram.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionKind {
    AskUserQuestions,
    RequestConfirmation,
    RequestCheckboxConfirmation,
    RequestItemVerdicts,
    SuggestTasks,
    ConnectionIntent,
    Other(String),
}

impl InteractionKind {
    pub fn parse(s: &str) -> Self {
        match s {
            "ask_user_questions" => Self::AskUserQuestions,
            "request_confirmation" => Self::RequestConfirmation,
            "request_checkbox_confirmation" => Self::RequestCheckboxConfirmation,
            "request_item_verdicts" => Self::RequestItemVerdicts,
            "suggest_tasks" => Self::SuggestTasks,
            "connection_intent" => Self::ConnectionIntent,
            other => Self::Other(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::AskUserQuestions => "ask_user_questions",
            Self::RequestConfirmation => "request_confirmation",
            Self::RequestCheckboxConfirmation => "request_checkbox_confirmation",
            Self::RequestItemVerdicts => "request_item_verdicts",
            Self::SuggestTasks => "suggest_tasks",
            Self::ConnectionIntent => "connection_intent",
            Self::Other(s) => s,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionStatus {
    Pending,
    Accepted,
    Rejected,
    Answered,
    Cancelled,
    Expired,
    Failed,
    Other(String),
}

impl InteractionStatus {
    pub fn parse(s: &str) -> Self {
        match s {
            "pending" => Self::Pending,
            "accepted" => Self::Accepted,
            "rejected" => Self::Rejected,
            "answered" => Self::Answered,
            "cancelled" => Self::Cancelled,
            "expired" => Self::Expired,
            "failed" => Self::Failed,
            other => Self::Other(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Answered => "answered",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
            Self::Failed => "failed",
            Self::Other(s) => s,
        }
    }

    pub fn is_pending(&self) -> bool {
        matches!(self, Self::Pending)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Interaction {
    pub id: String,
    pub company_id: String,
    pub issue_id: String,
    pub kind: InteractionKind,
    pub status: InteractionStatus,
    pub title: Option<String>,
    pub created_by_agent_id: Option<String>,
    pub created_at: String,
    pub resolved_by_user_id: Option<String>,
    pub resolved_by_agent_id: Option<String>,
    pub body: InteractionBody,
    pub result: Option<InteractionResult>,
}

impl Interaction {
    pub fn is_governed(&self) -> bool {
        matches!(
            self.body,
            InteractionBody::ToolAction(_) | InteractionBody::SecretProposal(_)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InteractionBody {
    Questions(QuestionForm),
    Confirmation(Confirmation),
    Checkbox(CheckboxConfirmation),
    Verdicts(ItemVerdicts),
    ToolAction(ToolAction),
    SecretProposal(SecretProposal),
    Unsupported { reason: UnsupportedReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedReason {
    SuggestTasks,
    ConnectionIntent,
    ConnectionAuthorization,
    UnknownKind,
    Malformed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionForm {
    pub title: Option<String>,
    pub description: Option<String>,
    pub submit_label: Option<String>,
    pub questions: Vec<Question>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    pub header: Option<String>,
    pub prompt: String,
    pub help_text: Option<String>,
    pub required: bool,
    pub mode: AnswerMode,
    pub options: Vec<Choice>,
    /// Free-text alternative to the options ("свой ответ").
    pub custom_answer: Option<CustomAnswer>,
    pub text_validation: Option<TextValidation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerMode {
    Single,
    Multi,
    Text,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomAnswer {
    pub label: Option<String>,
    /// Legacy forms: the option id that carries free text (`freeText: true`).
    pub option_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputType {
    Text,
    Number,
    Integer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextValidation {
    pub min_length: Option<u32>,
    pub max_length: Option<u32>,
    pub pattern: Option<String>,
    pub input_type: InputType,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Confirmation {
    pub prompt: String,
    pub details: Option<String>,
    pub accept_label: Option<String>,
    pub reject_label: Option<String>,
    pub reject_requires_reason: bool,
    pub allow_decline_reason: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckboxConfirmation {
    pub prompt: String,
    pub details: Option<String>,
    pub options: Vec<Choice>,
    pub default_selected: Vec<String>,
    pub min_selected: Option<u32>,
    pub max_selected: Option<u32>,
    pub accept_label: Option<String>,
    pub reject_label: Option<String>,
    pub reject_requires_reason: bool,
    pub allow_decline_reason: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Approve,
    Reject,
    Defer,
}

impl Verdict {
    pub const ALL: [Verdict; 3] = [Verdict::Approve, Verdict::Reject, Verdict::Defer];

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "approve" => Some(Self::Approve),
            "reject" => Some(Self::Reject),
            "defer" => Some(Self::Defer),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Reject => "reject",
            Self::Defer => "defer",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerdictItem {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemVerdicts {
    pub prompt: String,
    pub details: Option<String>,
    pub items: Vec<VerdictItem>,
    pub verdicts: Vec<Verdict>,
    pub require_reason_on: Vec<Verdict>,
    pub allow_bulk_approve: bool,
    /// Verdicts already stored on the server (partial submissions).
    pub decided: Vec<(String, Verdict)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    Read,
    Write,
    Destructive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolAction {
    pub action_request_id: String,
    pub invocation_id: Option<String>,
    pub tool_display_name: String,
    pub app_display_name: Option<String>,
    pub risk: Risk,
    pub preview_markdown: String,
    /// Server-redacted JSON of the arguments; shown only with
    /// `GOVERNED_SHOW_ARGUMENTS=true`.
    pub arguments_summary_json: Option<String>,
    pub arguments_hash: String,
    pub expires_at: Option<String>,
    /// `target.revisionId` of the card.
    pub target_revision_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretProposal {
    pub source_secret_label: String,
    pub config_path: String,
    pub target_agent_name: Option<String>,
    pub justification: Option<String>,
    pub expires_at: Option<String>,
}

/// Stored outcome of a resolved card (subset the bridge needs).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct InteractionResult {
    pub outcome: Option<String>,
    pub reason: Option<String>,
    pub selected_option_ids: Option<Vec<String>>,
    pub answers: Option<Vec<QuestionAnswer>>,
    pub complete: Option<bool>,
    pub expiration_reason: Option<String>,
    pub tool_action: Option<GovernedResult>,
    pub secret_proposal: Option<GovernedResult>,
}

/// `result.toolAction` / `result.secretProposal`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernedResult {
    /// `approved | executing | executed | failed | expired | rejected`.
    pub status: String,
    pub result_summary: Option<String>,
    pub error_code: Option<String>,
}

impl GovernedResult {
    pub fn is_final(&self) -> bool {
        matches!(
            self.status.as_str(),
            "executed" | "failed" | "expired" | "rejected"
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionAnswer {
    pub question_id: String,
    pub option_ids: Vec<String>,
    pub other_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemVerdictAnswer {
    pub id: String,
    pub verdict: Verdict,
    pub reason: Option<String>,
}

/// What the bridge sends back to Paperclip for a card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Resolution {
    Respond {
        answers: Vec<QuestionAnswer>,
    },
    Accept {
        selected_option_ids: Option<Vec<String>>,
    },
    Reject {
        reason: Option<String>,
    },
    Verdicts {
        verdicts: Vec<ItemVerdictAnswer>,
    },
    /// `toolAction` / `secretProposal` accept. For destructive actions the
    /// linked formal approval is approved first.
    GovernedAccept {
        destructive: bool,
        arguments_hash: Option<String>,
        target_revision_id: Option<String>,
    },
    GovernedReject {
        reason: Option<String>,
        destructive: bool,
    },
}

impl Resolution {
    pub fn action_name(&self) -> &'static str {
        match self {
            Self::Respond { .. } => "respond",
            Self::Accept { .. } => "accept",
            Self::Reject { .. } => "reject",
            Self::Verdicts { .. } => "verdicts",
            Self::GovernedAccept { .. } => "governed_accept",
            Self::GovernedReject { .. } => "governed_reject",
        }
    }

    pub fn is_governed(&self) -> bool {
        matches!(
            self,
            Self::GovernedAccept { .. } | Self::GovernedReject { .. }
        )
    }
}

/// Approval (board approval) linked to an issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    pub id: String,
    pub status: String,
    pub kind: Option<String>,
    pub source: Option<String>,
    pub action_request_id: Option<String>,
    pub invocation_id: Option<String>,
    pub arguments_hash: Option<String>,
}

impl Approval {
    /// The formal approval created by the tool gateway for this action
    /// (by `actionRequestId`, falling back to `invocationId` / `argumentsHash`).
    pub fn matches_action(&self, action: &ToolAction) -> bool {
        if self.source.as_deref().is_some_and(|s| s != "tool_gateway") {
            return false;
        }
        if let Some(id) = &self.action_request_id {
            return *id == action.action_request_id;
        }
        if let (Some(a), Some(b)) = (&self.invocation_id, &action.invocation_id) {
            return a == b;
        }
        self.arguments_hash.as_deref() == Some(action.arguments_hash.as_str())
    }
}
