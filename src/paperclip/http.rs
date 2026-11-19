//! reqwest client for Paperclip. Always `.no_proxy()`: the private Paperclip
//! must never be reached through the Telegram proxy (architecture §5.2).

use std::time::Duration;

use reqwest::header::{HeaderMap, RETRY_AFTER};
use reqwest::{RequestBuilder, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};
use url::Url;

use super::{MESSAGE_MAX_CHARS, PaperclipApi, PaperclipError, Result, dto};
use crate::domain::*;

pub struct HttpPaperclipClient {
    http: reqwest::Client,
    base: Url,
    token: SecretString,
}

impl HttpPaperclipClient {
    pub fn new(
        base: &Url,
        token: SecretString,
        connect_timeout: Duration,
        request_timeout: Duration,
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
        Ok(Self { http, base, token })
    }

    /// One client per configured key, in configuration order.
    pub fn from_config(
        cfg: &crate::config::PaperclipConfig,
    ) -> std::result::Result<Vec<Self>, String> {
        cfg.tokens
            .iter()
            .map(|t| {
                Self::new(
                    &cfg.url,
                    t.clone(),
                    cfg.connect_timeout,
                    cfg.request_timeout,
                )
            })
            .collect()
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

    async fn post(&self, segments: &[&str], body: &Value) -> Result<Value> {
        self.send(
            self.http
                .post(self.url(segments))
                .bearer_auth(self.token.expose_secret())
                .json(body),
        )
        .await
    }

    async fn comments(&self, issue_id: &str, query: Vec<(&str, String)>) -> Result<Vec<Comment>> {
        let v = self.get(&["issues", issue_id, "comments"], &query).await?;
        dto::comments(&v).ok_or(PaperclipError::Decode)
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

/// Error classification (ADR-0010 §2.3, §2.5).
pub fn classify(status: StatusCode, headers: &HeaderMap, body: &[u8]) -> PaperclipError {
    let message: String = serde_json::from_slice::<dto::ErrorBody>(body)
        .ok()
        .and_then(|b| b.error)
        .unwrap_or_default()
        .chars()
        .take(MESSAGE_MAX_CHARS)
        .collect();
    match status.as_u16() {
        401 => PaperclipError::Auth,
        403 => PaperclipError::Forbidden { message },
        404 => PaperclipError::NotFound { message },
        409 => PaperclipError::Conflict { message },
        429 => PaperclipError::Transient {
            reason: "rate_limited",
            retry_after: retry_after(headers),
        },
        s if s >= 500 => PaperclipError::Server { status: s },
        s => PaperclipError::Invalid { status: s, message },
    }
}

impl PaperclipApi for HttpPaperclipClient {
    async fn health(&self) -> Result<HealthInfo> {
        let v = self.get(&["health"], &[]).await?;
        Ok(dto::health(&v))
    }

    async fn companies(&self) -> Result<Vec<Company>> {
        let v = self
            .get(&["companies"], &[("scope", "accessible".into())])
            .await?;
        dto::companies(&v).ok_or(PaperclipError::Decode)
    }

    async fn agents(&self, company_id: &str) -> Result<Vec<Agent>> {
        let v = self.get(&["companies", company_id, "agents"], &[]).await?;
        dto::agents(&v).ok_or(PaperclipError::Decode)
    }

    async fn chats(&self, company_id: &str) -> Result<Vec<Chat>> {
        let v = self.get(&["companies", company_id, "chats"], &[]).await?;
        dto::chats(&v).ok_or(PaperclipError::Decode)
    }

    async fn comments_after(
        &self,
        issue_id: &str,
        after: Option<&str>,
        limit: u32,
    ) -> Result<Vec<Comment>> {
        let mut query = vec![("order", "asc".to_string()), ("limit", limit.to_string())];
        if let Some(a) = after {
            query.push(("after", a.to_string()));
        }
        self.comments(issue_id, query).await
    }

    async fn comments_latest(&self, issue_id: &str, limit: u32) -> Result<Vec<Comment>> {
        self.comments(
            issue_id,
            vec![("order", "desc".to_string()), ("limit", limit.to_string())],
        )
        .await
    }

    async fn post_comment(
        &self,
        issue_id: &str,
        body: &str,
        client_request_id: &str,
    ) -> Result<Option<String>> {
        let v = self
            .post(
                &["issues", issue_id, "comments"],
                &json!({ "body": body, "clientRequestId": client_request_id }),
            )
            .await?;
        Ok(dto::comment(&v).map(|c| c.id))
    }

    async fn revoke_current_key(&self) -> Result<()> {
        self.post(&["cli-auth", "revoke-current"], &json!({}))
            .await
            .map(|_| ())
    }
}
