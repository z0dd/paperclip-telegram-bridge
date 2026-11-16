//! reqwest client for Paperclip. Always `.no_proxy()`: the private Paperclip
//! must never be reached through the Telegram proxy (architecture §5.2).

use std::time::Duration;

use reqwest::header::{HeaderMap, RETRY_AFTER};
use reqwest::{Method, RequestBuilder, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};
use url::Url;

use super::{AUDIENCE_CODES, PaperclipApi, PaperclipError, RESOLVED_CODES, Result, dto};
use crate::domain::*;

const ATTENTION_LIMIT: u32 = 100;
const ATTENTION_MAX_PAGES: usize = 10;

pub struct HttpPaperclipClient {
    http: reqwest::Client,
    base: Url,
    token: SecretString,
    request_timeout: Duration,
    governed_timeout: Duration,
}

impl HttpPaperclipClient {
    pub fn new(
        base: &Url,
        token: SecretString,
        connect_timeout: Duration,
        request_timeout: Duration,
        governed_timeout: Duration,
    ) -> std::result::Result<Self, String> {
        crate::logging::install_tls_provider();
        let http = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(connect_timeout)
            .timeout(request_timeout)
            .user_agent(concat!(
                "paperclip-telegram-bridge/",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .map_err(|_| "cannot build Paperclip HTTP client".to_string())?;
        let mut base = base.clone();
        if !base.path().ends_with('/') {
            let path = format!("{}/", base.path());
            base.set_path(&path);
        }
        Ok(Self {
            http,
            base,
            token,
            request_timeout,
            governed_timeout,
        })
    }

    pub fn from_config(cfg: &crate::config::PaperclipConfig) -> std::result::Result<Self, String> {
        Self::new(
            &cfg.url,
            cfg.token.clone(),
            cfg.connect_timeout,
            cfg.request_timeout,
            cfg.governed_timeout,
        )
    }

    fn url(&self, segments: &[&str]) -> Url {
        let mut url = self.base.clone();
        if let Ok(mut path) = url.path_segments_mut() {
            path.pop_if_empty().push("api");
            for s in segments {
                path.push(s);
            }
        }
        url
    }

    fn request(&self, method: Method, segments: &[&str]) -> RequestBuilder {
        self.http
            .request(method, self.url(segments))
            .bearer_auth(self.token.expose_secret())
    }

    async fn send(&self, req: RequestBuilder) -> Result<Value> {
        let resp = req.send().await.map_err(map_reqwest_error)?;
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = resp.bytes().await.map_err(map_reqwest_error)?;
        if status.is_success() {
            if bytes.is_empty() {
                return Ok(Value::Null);
            }
            return serde_json::from_slice(&bytes).map_err(|_| PaperclipError::Decode);
        }
        Err(classify(status, &headers, &bytes))
    }

    async fn get(&self, segments: &[&str], query: &[(&str, String)]) -> Result<Value> {
        let mut url = self.url(segments);
        if !query.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(query.iter().map(|(k, v)| (*k, v.as_str())));
        }
        self.send(self.http.get(url).bearer_auth(self.token.expose_secret()))
            .await
    }

    async fn post(&self, segments: &[&str], body: &Value, timeout: Duration) -> Result<Value> {
        self.send(
            self.request(Method::POST, segments)
                .json(body)
                .timeout(timeout),
        )
        .await
    }

    async fn attention_page(
        &self,
        company_id: &str,
        cursor: Option<&str>,
    ) -> Result<dto::AttentionPage> {
        let mut query = vec![
            ("limit", ATTENTION_LIMIT.to_string()),
            ("sort", "activity".to_string()),
        ];
        if let Some(c) = cursor {
            query.push(("cursor", c.to_string()));
        }
        let v = self
            .get(&["companies", company_id, "attention"], &query)
            .await?;
        dto::attention_page(&v).ok_or(PaperclipError::Decode)
    }

    async fn post_interaction(
        &self,
        issue_id: &str,
        interaction_id: &str,
        action: &str,
        body: &Value,
        timeout: Duration,
    ) -> Result<Option<Interaction>> {
        let v = self
            .post(
                &["issues", issue_id, "interactions", interaction_id, action],
                body,
                timeout,
            )
            .await?;
        Ok(dto::interaction(&v))
    }
}

fn map_reqwest_error(e: reqwest::Error) -> PaperclipError {
    // Never keep the reqwest error itself: its Display contains the URL.
    let e = e.without_url();
    let reason = if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connect"
    } else if e.is_body() || e.is_decode() {
        "body"
    } else {
        "request"
    };
    PaperclipError::Transient {
        reason,
        retry_after: None,
    }
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// Error classification (architecture §7.2, API §1, §7).
pub fn classify(status: StatusCode, headers: &HeaderMap, body: &[u8]) -> PaperclipError {
    let err: dto::ErrorBody = serde_json::from_slice(body).unwrap_or(dto::ErrorBody {
        error: None,
        code: None,
    });
    let code = err.code.filter(|c| !c.is_empty());
    match status.as_u16() {
        401 => PaperclipError::Auth { status: 401 },
        403 => match code {
            Some(c) if AUDIENCE_CODES.contains(&c.as_str()) => {
                PaperclipError::AudienceDenied { code: c }
            }
            _ => PaperclipError::Auth { status: 403 },
        },
        404 => PaperclipError::NotFound,
        409 => match code {
            Some(c) if RESOLVED_CODES.contains(&c.as_str()) || c.starts_with("interaction_") => {
                PaperclipError::Resolved { code: c }
            }
            _ => PaperclipError::Conflict,
        },
        429 => PaperclipError::Transient {
            reason: "rate_limited",
            retry_after: retry_after(headers),
        },
        400 | 422 => PaperclipError::Invalid {
            status: status.as_u16(),
            code,
            message: err.error.unwrap_or_default().chars().take(300).collect(),
        },
        s if s >= 500 => PaperclipError::Server { status: s },
        s => PaperclipError::Invalid {
            status: s,
            code,
            message: String::new(),
        },
    }
}

impl PaperclipApi for HttpPaperclipClient {
    async fn health(&self) -> Result<HealthInfo> {
        let v = self.get(&["health"], &[]).await?;
        Ok(dto::health(&v))
    }

    async fn attention(&self, company_id: &str) -> Result<Vec<AttentionItem>> {
        let first = self.attention_page(company_id, None).await?;
        let mut items = first.items;
        let mut seen = first.raw_count as u64;
        let total = first.total_count.unwrap_or(0);
        let mut cursor = first.next_cursor;
        let mut pages = 1;
        let mut restarted = false;
        while let Some(c) = cursor.take() {
            if seen >= total || pages >= ATTENTION_MAX_PAGES {
                break;
            }
            match self.attention_page(company_id, Some(&c)).await {
                Ok(page) => {
                    seen += page.raw_count as u64;
                    items.extend(page.items);
                    cursor = page.next_cursor;
                    pages += 1;
                }
                // Cursor no longer matches the feed: restart from the first page once.
                Err(PaperclipError::Invalid { status: 400, .. }) if !restarted => {
                    restarted = true;
                    let page = self.attention_page(company_id, None).await?;
                    items = page.items;
                    seen = page.raw_count as u64;
                    cursor = page.next_cursor;
                    pages = 1;
                }
                Err(PaperclipError::Invalid { status: 400, .. }) => break,
                Err(e) => return Err(e),
            }
        }
        Ok(items)
    }

    async fn interactions(&self, issue_id: &str) -> Result<Vec<Interaction>> {
        let v = self.get(&["issues", issue_id, "interactions"], &[]).await?;
        dto::interactions(&v).ok_or(PaperclipError::Decode)
    }

    async fn resolve(
        &self,
        issue_id: &str,
        interaction_id: &str,
        resolution: &Resolution,
    ) -> Result<Option<Interaction>> {
        let (action, body) = dto::resolution_body(resolution).ok_or(PaperclipError::Decode)?;
        self.post_interaction(
            issue_id,
            interaction_id,
            action,
            &body,
            self.request_timeout,
        )
        .await
    }

    async fn governed_accept(
        &self,
        issue_id: &str,
        interaction_id: &str,
    ) -> Result<Option<Interaction>> {
        // `rememberAction` is never sent: no persistent trust rules from Telegram.
        self.post_interaction(
            issue_id,
            interaction_id,
            "accept",
            &json!({}),
            self.governed_timeout,
        )
        .await
    }

    async fn governed_reject(
        &self,
        issue_id: &str,
        interaction_id: &str,
        reason: Option<&str>,
    ) -> Result<Option<Interaction>> {
        let body = match reason {
            Some(r) => json!({ "reason": r }),
            None => json!({}),
        };
        self.post_interaction(
            issue_id,
            interaction_id,
            "reject",
            &body,
            self.governed_timeout,
        )
        .await
    }

    async fn issue_approvals(&self, issue_id: &str) -> Result<Vec<Approval>> {
        let v = self.get(&["issues", issue_id, "approvals"], &[]).await?;
        dto::approvals(&v).ok_or(PaperclipError::Decode)
    }

    async fn approve_approval(&self, approval_id: &str, decision_note: &str) -> Result<()> {
        self.post(
            &["approvals", approval_id, "approve"],
            &json!({ "decisionNote": decision_note }),
            self.request_timeout,
        )
        .await
        .map(|_| ())
    }

    async fn reject_approval(&self, approval_id: &str, decision_note: &str) -> Result<()> {
        self.post(
            &["approvals", approval_id, "reject"],
            &json!({ "decisionNote": decision_note }),
            self.request_timeout,
        )
        .await
        .map(|_| ())
    }

    async fn issues(
        &self,
        company_id: &str,
        statuses: &[&str],
        limit: u32,
    ) -> Result<Vec<IssueSummary>> {
        let v = self
            .get(
                &["companies", company_id, "issues"],
                &[
                    ("status", statuses.join(",")),
                    ("view", "compact".into()),
                    ("limit", limit.to_string()),
                ],
            )
            .await?;
        dto::issues(&v).ok_or(PaperclipError::Decode)
    }

    async fn heartbeat_runs(&self, company_id: &str, limit: u32) -> Result<Vec<RunInfo>> {
        let v = self
            .get(
                &["companies", company_id, "heartbeat-runs"],
                &[("limit", limit.to_string()), ("summary", "1".into())],
            )
            .await?;
        dto::runs(&v).ok_or(PaperclipError::Decode)
    }

    async fn live_runs(&self, company_id: &str) -> Result<Vec<RunInfo>> {
        let v = self
            .get(
                &["companies", company_id, "live-runs"],
                &[("limit", "50".into())],
            )
            .await?;
        dto::runs(&v).ok_or(PaperclipError::Decode)
    }

    async fn agents(&self, company_id: &str) -> Result<Vec<Agent>> {
        let v = self.get(&["companies", company_id, "agents"], &[]).await?;
        dto::agents(&v).ok_or(PaperclipError::Decode)
    }

    async fn revoke_current_key(&self) -> Result<()> {
        self.post(
            &["cli-auth", "revoke-current"],
            &json!({}),
            self.request_timeout,
        )
        .await
        .map(|_| ())
    }
}
