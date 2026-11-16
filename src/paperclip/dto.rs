//! Paperclip JSON ⇄ domain. The only module that knows the wire format
//! (docs/paperclip-api.md). Parsing is lenient: unknown fields are ignored,
//! a malformed payload becomes an unsupported card instead of an error.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::domain::*;

fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|x| !x.is_empty())
}

fn b(v: &Value, key: &str) -> Option<bool> {
    v.get(key).and_then(Value::as_bool)
}

fn u(v: &Value, key: &str) -> Option<u32> {
    v.get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
}

fn f(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64)
}

fn str_list(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Lists come either as a bare array or wrapped in an object.
pub fn list(v: &Value, keys: &[&str]) -> Option<Vec<Value>> {
    if let Some(a) = v.as_array() {
        return Some(a.clone());
    }
    keys.iter()
        .find_map(|k| v.get(*k).and_then(Value::as_array))
        .cloned()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorBody {
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
}

pub fn health(v: &Value) -> HealthInfo {
    HealthInfo {
        status: s(v, "status").unwrap_or_default(),
        version: s(v, "version"),
    }
}

pub struct AttentionPage {
    pub items: Vec<AttentionItem>,
    pub total_count: Option<u64>,
    pub raw_count: usize,
    pub next_cursor: Option<String>,
}

pub fn attention_page(v: &Value) -> Option<AttentionPage> {
    let raw = v.get("items")?.as_array()?;
    let items = raw.iter().map(attention_item).collect();
    Some(AttentionPage {
        items,
        total_count: v.get("totalCount").and_then(Value::as_u64),
        raw_count: raw.len(),
        next_cursor: s(v, "nextCursor"),
    })
}

fn attention_item(v: &Value) -> AttentionItem {
    let subject = v.get("subject").cloned().unwrap_or(Value::Null);
    let meta = subject.get("metadata").cloned().unwrap_or(Value::Null);
    let related = v.get("relatedIssue").cloned().unwrap_or(Value::Null);
    let activity_at = s(v, "activityAt");
    match v.get("sourceKind").and_then(Value::as_str) {
        Some("issue_thread_interaction") => {
            let Some(interaction_id) = s(&subject, "id") else {
                return AttentionItem::Other;
            };
            let Some(issue_id) = s(&meta, "issueId").or_else(|| s(&related, "id")) else {
                return AttentionItem::Other;
            };
            AttentionItem::Interaction {
                interaction_id,
                issue_id,
                issue_ref: s(&related, "identifier"),
                issue_title: s(&related, "title"),
                activity_at,
            }
        }
        Some("failed_run") => {
            let Some(run_id) = s(&subject, "id") else {
                return AttentionItem::Other;
            };
            AttentionItem::FailedRun {
                run_id,
                agent_id: s(&meta, "agentId"),
                issue_id: s(&meta, "issueId").or_else(|| s(&related, "id")),
                issue_ref: s(&related, "identifier"),
                error_code: s(&meta, "errorCode"),
                activity_at,
            }
        }
        _ => AttentionItem::Other,
    }
}

pub fn interactions(v: &Value) -> Option<Vec<Interaction>> {
    let items = list(v, &["interactions", "items", "data"])?;
    Some(items.iter().filter_map(interaction).collect())
}

pub fn interaction(v: &Value) -> Option<Interaction> {
    let id = s(v, "id")?;
    let kind = InteractionKind::parse(v.get("kind")?.as_str()?);
    let status = InteractionStatus::parse(v.get("status")?.as_str()?);
    let payload = v.get("payload").cloned().unwrap_or(Value::Null);
    let body = body(&kind, &payload, v.get("result"));
    Some(Interaction {
        id,
        company_id: s(v, "companyId").unwrap_or_default(),
        issue_id: s(v, "issueId").unwrap_or_default(),
        kind,
        status,
        title: s(v, "title"),
        created_by_agent_id: s(v, "createdByAgentId"),
        created_at: s(v, "createdAt").unwrap_or_default(),
        resolved_by_user_id: s(v, "resolvedByUserId"),
        resolved_by_agent_id: s(v, "resolvedByAgentId"),
        addressee_agent_id: s(v, "addresseeAgentId"),
        addressee_user_id: s(v, "addresseeUserId"),
        body,
        result: v.get("result").filter(|r| r.is_object()).map(result),
    })
}

fn body(kind: &InteractionKind, p: &Value, result: Option<&Value>) -> InteractionBody {
    let parsed = match kind {
        InteractionKind::AskUserQuestions => questions(p).map(InteractionBody::Questions),
        InteractionKind::RequestConfirmation => {
            if p.get("connectionAuthorization")
                .is_some_and(|x| !x.is_null())
            {
                Some(InteractionBody::Unsupported {
                    reason: UnsupportedReason::ConnectionAuthorization,
                })
            } else if let Some(ta) = p.get("toolAction").filter(|x| x.is_object()) {
                tool_action(ta, p).map(InteractionBody::ToolAction)
            } else if let Some(sp) = p.get("secretProposal").filter(|x| x.is_object()) {
                secret_proposal(sp).map(InteractionBody::SecretProposal)
            } else {
                confirmation(p).map(InteractionBody::Confirmation)
            }
        }
        InteractionKind::RequestCheckboxConfirmation => checkbox(p).map(InteractionBody::Checkbox),
        InteractionKind::RequestItemVerdicts => verdicts(p, result).map(InteractionBody::Verdicts),
        InteractionKind::SuggestTasks => Some(InteractionBody::Unsupported {
            reason: UnsupportedReason::SuggestTasks,
        }),
        InteractionKind::ConnectionIntent => Some(InteractionBody::Unsupported {
            reason: UnsupportedReason::ConnectionIntent,
        }),
        InteractionKind::Other(_) => Some(InteractionBody::Unsupported {
            reason: UnsupportedReason::UnknownKind,
        }),
    };
    parsed.unwrap_or(InteractionBody::Unsupported {
        reason: UnsupportedReason::Malformed,
    })
}

fn choices(v: &Value) -> Vec<Choice> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|o| {
                    Some(Choice {
                        id: s(o, "id")?,
                        label: s(o, "label").unwrap_or_else(|| s(o, "id").unwrap_or_default()),
                        description: s(o, "description"),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub const TEXT_ANSWER_OPTION: &str = "paperclip_text_answer";
pub const CUSTOM_ANSWER_OPTION: &str = "paperclip_custom_answer";

fn questions(p: &Value) -> Option<QuestionForm> {
    // Canonical questionSet first, legacy questions[] otherwise (API §6.1).
    if let Some(qs) = p.get("questionSet").filter(|x| x.is_object()) {
        let questions = qs
            .get("questions")?
            .as_array()?
            .iter()
            .map(question_set_item)
            .collect::<Option<Vec<_>>>()?;
        return Some(QuestionForm {
            title: s(qs, "title"),
            description: s(qs, "description"),
            submit_label: s(qs, "submitLabel"),
            questions,
        });
    }
    let questions = p
        .get("questions")?
        .as_array()?
        .iter()
        .map(legacy_question)
        .collect::<Option<Vec<_>>>()?;
    Some(QuestionForm {
        title: s(p, "title"),
        description: None,
        submit_label: None,
        questions,
    })
}

fn question_set_item(q: &Value) -> Option<Question> {
    let mode = match q.get("answerMode")?.as_str()? {
        "single_select" => AnswerMode::Single,
        "multi_select" => AnswerMode::Multi,
        "text" => AnswerMode::Text,
        _ => return None,
    };
    let custom_answer = q
        .get("customAnswer")
        .filter(|c| b(c, "enabled").unwrap_or(false))
        .map(|c| CustomAnswer {
            label: s(c, "label"),
            option_id: None,
        });
    let text_validation =
        q.get("textValidation")
            .filter(|x| x.is_object())
            .map(|t| TextValidation {
                min_length: u(t, "minLength"),
                max_length: u(t, "maxLength"),
                pattern: s(t, "pattern"),
                input_type: match t.get("inputType").and_then(Value::as_str) {
                    Some("number") => InputType::Number,
                    Some("integer") => InputType::Integer,
                    _ => InputType::Text,
                },
                minimum: f(t, "minimum"),
                maximum: f(t, "maximum"),
            });
    Some(Question {
        id: s(q, "id")?,
        header: s(q, "header"),
        prompt: s(q, "prompt").unwrap_or_default(),
        help_text: s(q, "helpText"),
        required: b(q, "required").unwrap_or(false),
        mode,
        options: if mode == AnswerMode::Text {
            Vec::new()
        } else {
            choices(q.get("options").unwrap_or(&Value::Null))
        },
        custom_answer,
        text_validation,
    })
}

fn legacy_question(q: &Value) -> Option<Question> {
    let raw = q
        .get("options")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut options = Vec::new();
    let mut free_text: Option<(String, Option<String>)> = None;
    for o in &raw {
        let Some(id) = s(o, "id") else { continue };
        if b(o, "freeText").unwrap_or(false) {
            free_text = Some((id, s(o, "label")));
        } else {
            options.push(Choice {
                label: s(o, "label").unwrap_or_else(|| id.clone()),
                id,
                description: s(o, "description"),
            });
        }
    }
    let multi = q.get("selectionMode").and_then(Value::as_str) == Some("multi");
    let (mode, custom_answer) = match free_text {
        Some((id, _)) if options.is_empty() && id == TEXT_ANSWER_OPTION => (AnswerMode::Text, None),
        Some((id, label)) => (
            if multi {
                AnswerMode::Multi
            } else {
                AnswerMode::Single
            },
            Some(CustomAnswer {
                label,
                option_id: Some(id),
            }),
        ),
        None => (
            if multi {
                AnswerMode::Multi
            } else {
                AnswerMode::Single
            },
            b(q, "allowOther").unwrap_or(false).then_some(CustomAnswer {
                label: None,
                option_id: None,
            }),
        ),
    };
    Some(Question {
        id: s(q, "id")?,
        header: None,
        prompt: s(q, "prompt").unwrap_or_default(),
        help_text: s(q, "helpText"),
        required: b(q, "required").unwrap_or(false),
        mode,
        options,
        custom_answer,
        text_validation: None,
    })
}

fn confirmation(p: &Value) -> Option<Confirmation> {
    Some(Confirmation {
        prompt: s(p, "prompt").unwrap_or_default(),
        details: s(p, "detailsMarkdown"),
        accept_label: s(p, "acceptLabel"),
        reject_label: s(p, "rejectLabel"),
        reject_requires_reason: b(p, "rejectRequiresReason").unwrap_or(false),
        allow_decline_reason: b(p, "allowDeclineReason").unwrap_or(false),
    })
}

fn checkbox(p: &Value) -> Option<CheckboxConfirmation> {
    let options = choices(p.get("options")?);
    if options.is_empty() {
        return None;
    }
    Some(CheckboxConfirmation {
        prompt: s(p, "prompt").unwrap_or_default(),
        details: s(p, "detailsMarkdown"),
        default_selected: str_list(p, "defaultSelectedOptionIds"),
        options,
        min_selected: u(p, "minSelected"),
        max_selected: u(p, "maxSelected"),
        accept_label: s(p, "acceptLabel"),
        reject_label: s(p, "rejectLabel"),
        reject_requires_reason: b(p, "rejectRequiresReason").unwrap_or(false),
        allow_decline_reason: b(p, "allowDeclineReason").unwrap_or(false),
    })
}

fn verdicts(p: &Value, result: Option<&Value>) -> Option<ItemVerdicts> {
    let items: Vec<VerdictItem> = p
        .get("items")?
        .as_array()?
        .iter()
        .filter_map(|i| {
            Some(VerdictItem {
                id: s(i, "id")?,
                label: s(i, "label").unwrap_or_default(),
                description: s(i, "description"),
            })
        })
        .collect();
    if items.is_empty() {
        return None;
    }
    let parse_list = |key: &str| -> Vec<Verdict> {
        str_list(p, key)
            .iter()
            .filter_map(|v| Verdict::parse(v))
            .collect()
    };
    let mut allowed = parse_list("verdicts");
    if allowed.is_empty() {
        allowed = Verdict::ALL.to_vec();
    }
    let decided = result
        .and_then(|r| r.get("items"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|i| Some((s(i, "id")?, Verdict::parse(i.get("verdict")?.as_str()?)?)))
                .collect()
        })
        .unwrap_or_default();
    Some(ItemVerdicts {
        prompt: s(p, "prompt").unwrap_or_default(),
        details: s(p, "detailsMarkdown"),
        items,
        verdicts: allowed,
        require_reason_on: parse_list("requireReasonOn"),
        allow_bulk_approve: b(p, "allowBulkApprove").unwrap_or(true),
        decided,
    })
}

fn tool_action(ta: &Value, payload: &Value) -> Option<ToolAction> {
    let risk = match ta.get("risk")?.as_str()? {
        "read" => Risk::Read,
        "write" => Risk::Write,
        "destructive" => Risk::Destructive,
        _ => return None,
    };
    Some(ToolAction {
        action_request_id: s(ta, "actionRequestId")?,
        invocation_id: s(ta, "invocationId"),
        tool_display_name: s(ta, "toolDisplayName").unwrap_or_else(|| "—".into()),
        app_display_name: s(ta, "appDisplayName"),
        risk,
        preview_markdown: s(ta, "previewMarkdown").unwrap_or_default(),
        arguments_summary_json: s(ta, "argumentsSummaryJson"),
        arguments_hash: s(ta, "argumentsHash")?,
        expires_at: s(ta, "expiresAt"),
        target_revision_id: payload.get("target").and_then(|t| s(t, "revisionId")),
    })
}

fn secret_proposal(sp: &Value) -> Option<SecretProposal> {
    Some(SecretProposal {
        source_secret_label: s(sp, "sourceSecretLabel")?,
        config_path: s(sp, "configPath")?,
        target_agent_name: s(sp, "targetAgentName"),
        justification: s(sp, "justification"),
        expires_at: s(sp, "expiresAt"),
    })
}

fn governed_result(v: &Value) -> Option<GovernedResult> {
    Some(GovernedResult {
        status: s(v, "status")?,
        result_summary: s(v, "resultSummary"),
        error_code: s(v, "errorCode"),
    })
}

fn result(r: &Value) -> InteractionResult {
    InteractionResult {
        outcome: s(r, "outcome"),
        reason: s(r, "reason"),
        selected_option_ids: r
            .get("selectedOptionIds")
            .is_some()
            .then(|| str_list(r, "selectedOptionIds")),
        answers: r.get("answers").and_then(Value::as_array).map(|a| {
            a.iter()
                .filter_map(|x| {
                    Some(QuestionAnswer {
                        question_id: s(x, "questionId")?,
                        option_ids: str_list(x, "optionIds"),
                        other_text: s(x, "otherText"),
                    })
                })
                .collect()
        }),
        complete: b(r, "complete"),
        expiration_reason: s(r, "expirationReason"),
        tool_action: r.get("toolAction").and_then(governed_result),
        secret_proposal: r.get("secretProposal").and_then(governed_result),
    }
}

/// Request body for an ordinary resolution. `None` for governed actions,
/// which have their own methods.
pub fn resolution_body(r: &Resolution) -> Option<(&'static str, Value)> {
    Some(match r {
        Resolution::Respond { answers } => (
            "respond",
            json!({
                "answers": answers.iter().map(|a| {
                    let mut o = json!({"questionId": a.question_id, "optionIds": a.option_ids});
                    if let Some(t) = &a.other_text {
                        o["otherText"] = json!(t);
                    }
                    o
                }).collect::<Vec<_>>(),
                "summaryMarkdown": Value::Null,
            }),
        ),
        Resolution::Accept {
            selected_option_ids,
        } => (
            "accept",
            match selected_option_ids {
                Some(ids) => json!({ "selectedOptionIds": ids }),
                None => json!({}),
            },
        ),
        Resolution::Reject { reason } => (
            "reject",
            match reason {
                Some(r) => json!({ "reason": r }),
                None => json!({}),
            },
        ),
        Resolution::Verdicts { verdicts } => (
            "verdicts",
            json!({
                "verdicts": verdicts.iter().map(|v| {
                    let mut o = json!({"id": v.id, "verdict": v.verdict.as_str()});
                    if let Some(r) = &v.reason {
                        o["reason"] = json!(r);
                    }
                    o
                }).collect::<Vec<_>>(),
            }),
        ),
        Resolution::GovernedAccept { .. } | Resolution::GovernedReject { .. } => return None,
    })
}

pub fn approvals(v: &Value) -> Option<Vec<Approval>> {
    let items = list(v, &["approvals", "items", "data"])?;
    Some(
        items
            .iter()
            .filter_map(|a| {
                let payload = a.get("payload").cloned().unwrap_or(Value::Null);
                Some(Approval {
                    id: s(a, "id")?,
                    status: s(a, "status").unwrap_or_default(),
                    kind: s(a, "type"),
                    source: s(&payload, "source"),
                    action_request_id: s(&payload, "actionRequestId"),
                    invocation_id: s(&payload, "invocationId"),
                    arguments_hash: s(&payload, "argumentsHash"),
                })
            })
            .collect(),
    )
}

pub fn issues(v: &Value) -> Option<Vec<IssueSummary>> {
    let items = list(v, &["issues", "items", "data"])?;
    Some(
        items
            .iter()
            .filter_map(|i| {
                Some(IssueSummary {
                    id: s(i, "id")?,
                    identifier: s(i, "identifier"),
                    title: s(i, "title").unwrap_or_default(),
                    status: s(i, "status").unwrap_or_default(),
                    priority: s(i, "priority"),
                    assignee_agent_id: s(i, "assigneeAgentId"),
                    updated_at: s(i, "updatedAt"),
                })
            })
            .collect(),
    )
}

pub fn runs(v: &Value) -> Option<Vec<RunInfo>> {
    let items = list(v, &["runs", "items", "data", "heartbeatRuns"])?;
    Some(
        items
            .iter()
            .filter_map(|r| {
                let ctx = r.get("contextSnapshot").cloned().unwrap_or(Value::Null);
                Some(RunInfo {
                    id: s(r, "id")?,
                    agent_id: s(r, "agentId"),
                    issue_id: s(r, "issueId").or_else(|| s(&ctx, "issueId")),
                    status: s(r, "status").unwrap_or_default(),
                    error_code: s(r, "errorCode"),
                    created_at: s(r, "createdAt"),
                    finished_at: s(r, "finishedAt"),
                    scheduled_retry_at: s(r, "scheduledRetryAt"),
                })
            })
            .collect(),
    )
}

pub fn agents(v: &Value) -> Option<Vec<Agent>> {
    let items = list(v, &["agents", "items", "data"])?;
    Some(
        items
            .iter()
            .filter_map(|a| {
                Some(Agent {
                    id: s(a, "id")?,
                    name: s(a, "name")
                        .or_else(|| s(a, "displayName"))
                        .unwrap_or_else(|| "агент".into()),
                })
            })
            .collect(),
    )
}
