//! Stateful mock of the Paperclip API v2 on wiremock (ADR-0010 §3.1).
//! Placeholders only.
//!
//! One catch-all wiremock responder dispatches by path over a shared
//! [`State`]: keys (owner, visible companies, revoked), companies, agents,
//! conversations, comments, forced errors, and every POST for assertions.
//! Behaviour follows the live server where it matters for the bridge:
//! comments `order` defaults to `desc`, `after` returns the anchor itself
//! again, an unknown `after` gives `[]`, a comment bumps `updatedAt` of the
//! conversation (receipts do not), POST is idempotent by `clientRequestId`.
//! Extend by adding fields to `State` and branches to `respond`.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{Value, json};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// Default board key (the harness writes it to `PAPERCLIP_TOKEN_FILE`).
pub const KEY: &str = "pcp_board_placeholder";
/// Owner of the default key and of the conversations.
pub const USER: &str = "user-1";

#[derive(Debug, Clone)]
pub struct KeyInfo {
    pub user_id: String,
    /// Companies returned by `?scope=accessible`.
    pub companies: Vec<String>,
    pub revoked: bool,
}

#[derive(Debug, Clone)]
pub struct ChatRec {
    pub company_id: String,
    pub issue_id: String,
    pub identifier: String,
    pub agent_id: String,
    pub user_id: String,
    pub updated_at: String,
}

/// A forced answer: status, JSON body, optional `Retry-After`.
#[derive(Debug, Clone)]
pub struct Forced {
    pub status: u16,
    pub body: Value,
    pub retry_after: Option<u64>,
}

impl Forced {
    pub fn new(status: u16, body: Value) -> Self {
        Self {
            status,
            body,
            retry_after: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Post {
    pub issue_id: String,
    pub body: Option<String>,
    pub client_request_id: Option<String>,
    pub status: u16,
}

#[derive(Debug)]
pub struct State {
    pub keys: HashMap<String, KeyInfo>,
    pub companies: Vec<Value>,
    pub agents: HashMap<String, Vec<Value>>,
    pub chats: Vec<ChatRec>,
    /// Forced answer of `/chats` per company (every call).
    pub chats_error: HashMap<String, Forced>,
    /// Comments per issue, oldest first.
    pub comments: HashMap<String, Vec<Value>>,
    /// `after` includes the anchor (live server behaviour, ADR-0010 §1.4).
    pub anchor_inclusive: bool,
    /// Forced answers of the next POSTs per issue (popped one by one).
    pub post_errors: HashMap<String, VecDeque<Forced>>,
    /// Successful user comments get a receipt and an agent reply.
    pub auto_reply: bool,
    pub posts: Vec<Post>,
    /// Heartbeat runs by id (`GET /api/heartbeat-runs/{id}`); unknown → 404.
    pub runs: HashMap<String, Value>,
    /// Forced answer of `GET /api/heartbeat-runs/{id}` per run (every call).
    pub run_errors: HashMap<String, Forced>,
    /// Delay of every POST answer (a request in flight during shutdown).
    pub post_delay: Option<std::time::Duration>,
    pub health: Value,
    seq: u64,
}

impl Default for State {
    fn default() -> Self {
        Self {
            keys: HashMap::from([(
                KEY.to_string(),
                KeyInfo {
                    user_id: USER.into(),
                    companies: Vec::new(),
                    revoked: false,
                },
            )]),
            companies: Vec::new(),
            agents: HashMap::new(),
            chats: Vec::new(),
            chats_error: HashMap::new(),
            comments: HashMap::new(),
            anchor_inclusive: true,
            post_errors: HashMap::new(),
            auto_reply: false,
            posts: Vec::new(),
            runs: HashMap::new(),
            run_errors: HashMap::new(),
            post_delay: None,
            health: json!({"status": "ok", "version": "2026.1106.0"}),
            seq: 0,
        }
    }
}

impl State {
    /// Next timestamp: 1 s per event from a fixed base.
    fn tick(&mut self) -> String {
        self.seq += 1;
        let s = self.seq;
        format!(
            "2026-11-19T{:02}:{:02}:{:02}.000Z",
            10 + s / 3600,
            (s / 60) % 60,
            s % 60
        )
    }

    fn chat_mut(&mut self, issue_id: &str) -> Option<&mut ChatRec> {
        self.chats.iter_mut().find(|c| c.issue_id == issue_id)
    }

    fn agent_of(&self, issue_id: &str) -> Option<String> {
        self.chats
            .iter()
            .find(|c| c.issue_id == issue_id)
            .map(|c| c.agent_id.clone())
    }

    /// Appends a comment; `bump` moves `updatedAt` of the conversation.
    pub fn push_comment(&mut self, issue_id: &str, mut c: Value, bump: bool) -> String {
        let at = self.tick();
        let id = format!("comment-{}", self.seq);
        c["id"] = json!(id);
        c["issueId"] = json!(issue_id);
        c["createdAt"] = json!(at);
        if c.get("deletedAt").is_none() {
            c["deletedAt"] = Value::Null;
        }
        self.comments
            .entry(issue_id.to_string())
            .or_default()
            .push(c);
        if bump && let Some(chat) = self.chat_mut(issue_id) {
            chat.updated_at = at;
        }
        id
    }

    fn token<'a>(&self, req: &'a Request) -> Option<&'a str> {
        req.headers
            .get("authorization")?
            .to_str()
            .ok()?
            .strip_prefix("Bearer ")
    }
}

fn json_response(status: u16, body: Value) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(body)
}

fn forced(f: &Forced) -> ResponseTemplate {
    let mut r = json_response(f.status, f.body.clone());
    if let Some(ra) = f.retry_after {
        r = r.insert_header("Retry-After", ra.to_string().as_str());
    }
    r
}

fn query(req: &Request, key: &str) -> Option<String> {
    req.url
        .query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

#[derive(Clone, Default)]
pub struct Handle(pub Arc<Mutex<State>>);

impl Respond for Handle {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let mut st = self.0.lock().unwrap();
        let segs: Vec<String> = req
            .url
            .path_segments()
            .map(|s| s.map(str::to_string).collect())
            .unwrap_or_default();
        let segs: Vec<&str> = segs.iter().map(String::as_str).collect();
        let method = req.method.as_str();
        if segs == ["api", "health"] {
            return json_response(200, st.health.clone());
        }
        let key = match st.token(req).and_then(|t| st.keys.get(t)) {
            Some(k) if !k.revoked => k.clone(),
            _ => return json_response(401, json!({"error": "Unauthorized"})),
        };
        match (method, segs.as_slice()) {
            ("GET", ["api", "companies"]) => {
                let list: Vec<Value> = if query(req, "scope").as_deref() == Some("accessible") {
                    st.companies
                        .iter()
                        .filter(|c| key.companies.iter().any(|id| c["id"] == json!(id)))
                        .cloned()
                        .collect()
                } else {
                    st.companies.clone()
                };
                json_response(200, Value::Array(list))
            }
            ("GET", ["api", "companies", company, "agents"]) => json_response(
                200,
                Value::Array(st.agents.get(*company).cloned().unwrap_or_default()),
            ),
            ("GET", ["api", "companies", company, "chats"]) => {
                if let Some(f) = st.chats_error.get(*company) {
                    return forced(f);
                }
                if !key.companies.iter().any(|c| c == company) {
                    return json_response(403, json!({"error": "Forbidden"}));
                }
                let list: Vec<Value> = st
                    .chats
                    .iter()
                    .filter(|c| c.company_id == *company && c.user_id == key.user_id)
                    .map(|c| {
                        json!({"id": c.issue_id, "identifier": c.identifier,
                               "title": "Chat with agent", "conversationAgentId": c.agent_id,
                               "conversationUserId": c.user_id, "conversationState": "waiting",
                               "updatedAt": c.updated_at})
                    })
                    .collect();
                json_response(200, Value::Array(list))
            }
            ("GET", ["api", "heartbeat-runs", run]) => {
                if let Some(f) = st.run_errors.get(*run) {
                    return forced(f);
                }
                match st.runs.get(*run) {
                    Some(r) => json_response(200, r.clone()),
                    None => json_response(404, json!({"error": "Heartbeat run not found"})),
                }
            }
            ("GET", ["api", "issues", issue, "comments"]) => {
                let all = st.comments.get(*issue).cloned().unwrap_or_default();
                let asc = query(req, "order").as_deref() == Some("asc");
                let mut list: Vec<Value> = match query(req, "after") {
                    Some(after) => match all.iter().position(|c| c["id"] == json!(after)) {
                        None => Vec::new(),
                        Some(i) => {
                            let from = if st.anchor_inclusive { i } else { i + 1 };
                            all[from..].to_vec()
                        }
                    },
                    None => all,
                };
                if !asc {
                    list.reverse();
                }
                if let Some(limit) = query(req, "limit").and_then(|l| l.parse::<usize>().ok()) {
                    list.truncate(limit);
                }
                json_response(200, Value::Array(list))
            }
            ("POST", ["api", "issues", issue, "comments"]) => {
                let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
                let text = body["body"].as_str().map(str::to_string);
                let crid = body["clientRequestId"].as_str().map(str::to_string);
                let issue = issue.to_string();
                let (status, resp) =
                    post_comment(&mut st, &key, &issue, text.clone(), crid.clone());
                st.posts.push(Post {
                    issue_id: issue,
                    body: text,
                    client_request_id: crid,
                    status: resp_status(&status),
                });
                let r = match status {
                    PostResult::Forced(f) => forced(&f),
                    PostResult::Status(s) => json_response(s, resp),
                };
                match st.post_delay {
                    Some(d) => r.set_delay(d),
                    None => r,
                }
            }
            ("POST", ["api", "cli-auth", "revoke-current"]) => {
                if let Some(t) = st.token(req).map(str::to_string)
                    && let Some(k) = st.keys.get_mut(&t)
                {
                    k.revoked = true;
                }
                json_response(200, json!({}))
            }
            _ => json_response(404, json!({"error": "Not found"})),
        }
    }
}

enum PostResult {
    Status(u16),
    Forced(Forced),
}

fn resp_status(r: &PostResult) -> u16 {
    match r {
        PostResult::Status(s) => *s,
        PostResult::Forced(f) => f.status,
    }
}

fn post_comment(
    st: &mut State,
    key: &KeyInfo,
    issue: &str,
    text: Option<String>,
    crid: Option<String>,
) -> (PostResult, Value) {
    if let Some(f) = st.post_errors.get_mut(issue).and_then(VecDeque::pop_front) {
        return (PostResult::Forced(f), Value::Null);
    }
    let Some(chat) = st.chats.iter().find(|c| c.issue_id == issue).cloned() else {
        return (PostResult::Status(404), json!({"error": "Issue not found"}));
    };
    if chat.user_id != key.user_id {
        return (
            PostResult::Status(403),
            json!({"error": "Only the conversation owner can send messages to this chat"}),
        );
    }
    let Some(crid) = crid else {
        return (
            PostResult::Status(422),
            json!({"error": "Chat messages require a clientRequestId for safe retries"}),
        );
    };
    let text = text.unwrap_or_default();
    let existing = st.comments.get(issue).and_then(|l| {
        l.iter()
            .find(|c| {
                c["clientRequestId"] == json!(crid) && c["authorUserId"] == json!(key.user_id)
            })
            .cloned()
    });
    if let Some(c) = existing {
        if c["body"] == json!(text) {
            return (PostResult::Status(201), c);
        }
        return (
            PostResult::Status(409),
            json!({"error": "Message request ID was already used for different content"}),
        );
    }
    let id = st.push_comment(
        issue,
        json!({"body": text, "authorType": "user", "authorUserId": key.user_id,
               "authorAgentId": null, "origin": "comment", "clientRequestId": crid,
               "createdByRunId": null}),
        true,
    );
    let created = st.comments[issue]
        .iter()
        .find(|c| c["id"] == json!(id))
        .cloned()
        .unwrap_or_default();
    if st.auto_reply {
        let agent = chat.agent_id.clone();
        st.push_comment(issue, receipt_json(&agent, "Принято, отвечу скоро."), false);
        st.push_comment(
            issue,
            agent_json(&agent, &format!("Ответ на: {text}")),
            true,
        );
    }
    (PostResult::Status(201), created)
}

pub fn agent_json(agent_id: &str, body: &str) -> Value {
    json!({"body": body, "authorType": "agent", "authorAgentId": agent_id,
           "authorUserId": null, "origin": "comment", "createdByRunId": "run-1"})
}

pub fn receipt_json(agent_id: &str, body: &str) -> Value {
    json!({"body": body, "authorType": "agent", "authorAgentId": agent_id,
           "authorUserId": null, "origin": "fast_response", "fastResponseRequestId": "fr-1"})
}

/// Mock server plus a handle to its state.
pub struct PaperclipMock {
    pub server: MockServer,
    pub state: Handle,
}

impl PaperclipMock {
    pub async fn start() -> Self {
        let server = MockServer::start().await;
        let state = Handle::default();
        Mock::given(any())
            .respond_with(state.clone())
            .mount(&server)
            .await;
        Self { server, state }
    }

    pub fn uri(&self) -> String {
        self.server.uri()
    }

    pub fn st(&self) -> MutexGuard<'_, State> {
        self.state.0.lock().unwrap()
    }

    /// Adds a company visible to the default key.
    pub fn company(&self, id: &str, prefix: &str) -> &Self {
        let mut st = self.st();
        st.companies
            .push(json!({"id": id, "name": prefix, "issuePrefix": prefix, "status": "active"}));
        if let Some(k) = st.keys.get_mut(KEY) {
            k.companies.push(id.to_string());
        }
        drop(st);
        self
    }

    /// Adds another key with its owner and visible companies.
    pub fn key(&self, token: &str, user_id: &str, companies: &[&str]) -> &Self {
        self.st().keys.insert(
            token.to_string(),
            KeyInfo {
                user_id: user_id.into(),
                companies: companies.iter().map(|c| c.to_string()).collect(),
                revoked: false,
            },
        );
        self
    }

    pub fn revoke(&self, token: &str) {
        if let Some(k) = self.st().keys.get_mut(token) {
            k.revoked = true;
        }
    }

    pub fn agent(&self, company: &str, id: &str, name: &str) -> &Self {
        self.st()
            .agents
            .entry(company.to_string())
            .or_default()
            .push(json!({"id": id, "name": name, "status": "idle"}));
        self
    }

    /// Adds a conversation of the default key owner.
    pub fn chat(&self, company: &str, issue_id: &str, identifier: &str, agent_id: &str) -> &Self {
        self.chat_of(company, issue_id, identifier, agent_id, USER)
    }

    pub fn chat_of(
        &self,
        company: &str,
        issue_id: &str,
        identifier: &str,
        agent_id: &str,
        user_id: &str,
    ) -> &Self {
        let mut st = self.st();
        let at = st.tick();
        st.chats.push(ChatRec {
            company_id: company.into(),
            issue_id: issue_id.into(),
            identifier: identifier.into(),
            agent_id: agent_id.into(),
            user_id: user_id.into(),
            updated_at: at,
        });
        drop(st);
        self
    }

    pub fn remove_chat(&self, issue_id: &str) {
        self.st().chats.retain(|c| c.issue_id != issue_id);
    }

    /// A reply of the conversation agent. Returns the comment id.
    pub fn agent_says(&self, issue_id: &str, body: &str) -> String {
        let mut st = self.st();
        let agent = st.agent_of(issue_id).unwrap_or_else(|| "agent-x".into());
        st.push_comment(issue_id, agent_json(&agent, body), true)
    }

    /// A receipt (`origin: fast_response`): does not bump `updatedAt`.
    pub fn receipt(&self, issue_id: &str, body: &str) -> String {
        let mut st = self.st();
        let agent = st.agent_of(issue_id).unwrap_or_else(|| "agent-x".into());
        st.push_comment(issue_id, receipt_json(&agent, body), false)
    }

    /// A user comment written in the UI.
    pub fn user_says(&self, issue_id: &str, body: &str) -> String {
        self.st().push_comment(
            issue_id,
            json!({"body": body, "authorType": "user", "authorUserId": USER,
                   "origin": "comment", "clientRequestId": "ui-request"}),
            true,
        )
    }

    pub fn system_says(&self, issue_id: &str, body: &str) -> String {
        self.st().push_comment(
            issue_id,
            json!({"body": body, "authorType": "system", "origin": "comment",
                   "presentation": {"kind": "system_notice"}}),
            true,
        )
    }

    /// A run with its final status and error.
    pub fn run(&self, run_id: &str, status: &str, error: Option<&str>) -> &Self {
        self.st().runs.insert(
            run_id.to_string(),
            json!({"id": run_id, "status": status, "error": error}),
        );
        self
    }

    /// `GET /api/heartbeat-runs/{id}` fails with this answer.
    pub fn fail_run(&self, run_id: &str, f: Forced) -> &Self {
        self.st().run_errors.insert(run_id.to_string(), f);
        self
    }

    /// Requests for `GET /api/heartbeat-runs/*` received so far.
    pub async fn run_lookups(&self) -> usize {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.url.path().starts_with("/api/heartbeat-runs/"))
            .count()
    }

    /// A service notice: Paperclip posts the first line of the run error on
    /// behalf of the conversation agent (same author, origin and metadata as a
    /// reply, docs/paperclip-api.md).
    pub fn service_notice(&self, issue_id: &str, run_id: &str, error: &str) -> String {
        self.run(run_id, "failed", Some(error));
        let first = error.lines().next().unwrap_or_default().to_string();
        let mut st = self.st();
        let agent = st.agent_of(issue_id).unwrap_or_else(|| "agent-x".into());
        let mut c = agent_json(&agent, &first);
        c["createdByRunId"] = json!(run_id);
        st.push_comment(issue_id, c, true)
    }

    /// Any comment JSON (fields are completed with id, issueId, createdAt).
    pub fn comment(&self, issue_id: &str, c: Value, bump: bool) -> String {
        self.st().push_comment(issue_id, c, bump)
    }

    /// Removes a comment from the list (the cursor anchor disappears).
    pub fn delete_comment(&self, issue_id: &str, comment_id: &str) {
        if let Some(l) = self.st().comments.get_mut(issue_id) {
            l.retain(|c| c["id"] != json!(comment_id));
        }
    }

    /// Moves `updatedAt` of the conversation without a comment.
    pub fn touch(&self, issue_id: &str) {
        let mut st = self.st();
        let at = st.tick();
        if let Some(c) = st.chat_mut(issue_id) {
            c.updated_at = at;
        }
    }

    pub fn fail_post(&self, issue_id: &str, f: Forced) {
        self.st()
            .post_errors
            .entry(issue_id.to_string())
            .or_default()
            .push_back(f);
    }

    pub fn fail_chats(&self, company: &str, f: Forced) {
        self.st().chats_error.insert(company.to_string(), f);
    }

    pub fn heal_chats(&self, company: &str) {
        self.st().chats_error.remove(company);
    }

    pub fn posts(&self) -> Vec<Post> {
        self.st().posts.clone()
    }

    /// Successful user comments in Paperclip (created by the bridge).
    pub fn user_comments(&self, issue_id: &str) -> Vec<Value> {
        self.st()
            .comments
            .get(issue_id)
            .map(|l| {
                l.iter()
                    .filter(|c| c["authorType"] == "user" && c["clientRequestId"] != "ui-request")
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Requests received for a path suffix.
    pub async fn requests(&self, method: &str, suffix: &str) -> usize {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.method.as_str() == method && r.url.path().ends_with(suffix))
            .count()
    }
}
