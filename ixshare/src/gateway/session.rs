//! Session-related data structures, storage, and business logic.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::{response::IntoResponse, Json};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::RwLock;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::gateway::http_gateway::{HttpGateway, GATEWAY_CONFIG};
use crate::node_config::NODE_CONFIG;
use crate::print::verbose_category::AGENT;

const SessionTimeoutMinutes: i64 = 10;
pub const SessionCleanupIntervalSecs: u64 = 60;

// ============================================================================
// Data Structures
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub tenant: String,
    pub createdAt: DateTime<Utc>,
    pub updatedAt: DateTime<Utc>,
    pub messages: Vec<Message>,
    /// Compacted model working context used to build outbound requests.
    /// Never serialized to clients (transcript/working-context split).
    #[serde(skip)]
    pub model_messages: Vec<Message>,
    #[serde(skip)]
    pub skill_api_key: String,
    #[serde(skip)]
    pub last_prompt_tokens: u64,
    /// Published-endpoint slug selected as the agent (main) model for this
    /// session. **Mandatory and always present**: `CreateSession` rejects a
    /// slug-less request, so by prompt time the slug is guaranteed set (there is
    /// no gateway default model). Serialized to the client so the dashboard can
    /// compare a resumed session's endpoint against the locally-selected slug.
    pub agent_endpoint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: String,
    pub content: String,
    pub createdAt: DateTime<Utc>,
    pub parts: Vec<Part>,
}

const INTERRUPTED_TOOL_TURN_MESSAGE: &str =
    "Request interrupted after tool execution before a final answer was produced.";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Part {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool")]
    Tool {
        toolName: String,
        input: serde_json::Value,
        output: Option<String>,
        status: String,
    },
    #[serde(rename = "thinking")]
    Thinking { text: String },
    /// Marker left in the visible transcript when older history was summarized
    /// into the model working context. Carries the generated summary for
    /// optional UI expansion. Never forwarded to the model.
    #[serde(rename = "compaction")]
    Compaction { summary: String },
}

// ============================================================================
// Storage
// ============================================================================

#[derive(Default, Clone, Debug)]
pub struct SessionStore {
    sessions: Arc<RwLock<HashMap<String, Session>>>,
    /// Per-session async mutexes guarding the preflight -> compact -> rebuild ->
    /// send critical section. Kept outside the `sessions` `RwLock` so the global
    /// session map lock is never held across awaits. Entries are created lazily
    /// on first prompt and removed when the session is removed.
    session_locks: Arc<std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    active_stream_cancels: Arc<std::sync::Mutex<HashMap<String, CancellationToken>>>,
}

impl SessionStore {
    pub fn New() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            session_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
            active_stream_cancels: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    pub async fn CreateSession(
        &self,
        tenant: String,
        skill_api_key: String,
        agent_endpoint: String,
    ) -> Session {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let session = Session {
            id: id.to_string(),
            title: format!("Session {}", &id[..8.min(id.len())]),
            tenant,
            createdAt: now,
            updatedAt: now,
            messages: Vec::new(),
            model_messages: Vec::new(),
            skill_api_key,
            last_prompt_tokens: 0,
            agent_endpoint,
        };
        self.sessions
            .write()
            .await
            .insert(id.to_string(), session.clone());
        session
    }

    pub async fn GetSession(&self, id: &str) -> Option<Session> {
        self.sessions.read().await.get(id).cloned()
    }

    /// Get-or-create the per-session critical-section mutex. The lock-map guard
    /// is dropped before returning; callers `.lock_owned().await` on the returned
    /// `Arc` and hold only that across preflight -> compact -> rebuild -> send.
    pub fn SessionLock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut map = self.session_locks.lock().unwrap();
        map.entry(id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    fn remove_session_lock(&self, id: &str) {
        self.session_locks.lock().unwrap().remove(id);
    }

    fn clear_active_stream_cancel(&self, id: &str) {
        if let Some(token) = self.active_stream_cancels.lock().unwrap().remove(id) {
            token.cancel();
        }
    }

    pub fn register_active_stream_cancel(&self, id: &str, token: CancellationToken) {
        self.active_stream_cancels
            .lock()
            .unwrap()
            .insert(id.to_string(), token);
    }

    pub fn interrupt_active_stream(&self, id: &str) -> bool {
        if let Some(token) = self.active_stream_cancels.lock().unwrap().get(id).cloned() {
            token.cancel();
            true
        } else {
            false
        }
    }

    pub async fn ListSessionsForTenant(&self, tenant: &str) -> Vec<Session> {
        self.sessions
            .read()
            .await
            .values()
            .filter(|s| s.tenant == tenant)
            .cloned()
            .collect()
    }

    pub async fn UpdateTokens(&self, session_id: &str, prompt_tokens: u64) {
        if let Some(session) = self.sessions.write().await.get_mut(session_id) {
            session.last_prompt_tokens = prompt_tokens;
        }
    }

    /// Rewrite only the compacted model working context. Sets
    /// `model_messages = [summary system message] + token-budgeted tail`.
    /// Does **not** touch `messages` (the append-only visible transcript).
    pub async fn ReplaceModelMessagesWithSummary(
        &self,
        session_id: &str,
        summary: String,
        tail: Vec<Message>,
    ) {
        if let Some(session) = self.sessions.write().await.get_mut(session_id) {
            let summary_msg = Message {
                id: Uuid::new_v4().to_string(),
                role: "system".to_string(),
                content: summary,
                createdAt: Utc::now(),
                parts: vec![],
            };
            session.model_messages = std::iter::once(summary_msg).chain(tail).collect();
            session.last_prompt_tokens = 0;
            session.updatedAt = Utc::now();
        }
    }

    /// Append one compaction marker to the visible transcript only.
    /// Preserves the `messages` append-only invariant; never written to
    /// `model_messages`, so it never reaches the model.
    pub async fn AppendCompactionMarker(&self, session_id: &str, summary: String) {
        if let Some(session) = self.sessions.write().await.get_mut(session_id) {
            let marker = Message {
                id: Uuid::new_v4().to_string(),
                role: "compaction".to_string(),
                content: String::new(),
                createdAt: Utc::now(),
                parts: vec![Part::Compaction { summary }],
            };
            session.messages.push(marker);
            session.updatedAt = Utc::now();
        }
    }

    /// Append to the visible transcript and mirror the same message into the
    /// model working context, keeping the two in sync until compaction diverges
    /// them. Both the stream and non-stream pipelines go through here.
    pub async fn AddMessage(&self, session_id: &str, message: Message) -> Option<Message> {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.messages.push(message.clone());
            session.model_messages.push(message.clone());
            session.updatedAt = Utc::now();
            Some(message)
        } else {
            None
        }
    }

    /// Roll back a just-appended message from both the visible transcript and the
    /// model working context, matching on id. Used to undo the user message of a
    /// turn that failed before producing any assistant reply, so the next turn's
    /// preflight does not count an unanswered user message.
    pub async fn RemoveMessageById(&self, session_id: &str, message_id: &str) {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.messages.retain(|m| m.id != message_id);
            session.model_messages.retain(|m| m.id != message_id);
            session.updatedAt = Utc::now();
        }
    }

    pub async fn GetMessages(&self, session_id: &str, limit: usize) -> Vec<Message> {
        let sessions = self.sessions.read().await;
        if let Some(session) = sessions.get(session_id) {
            let msgs = session.messages.clone();
            if limit > 0 && msgs.len() > limit {
                msgs.into_iter().rev().take(limit).rev().collect()
            } else {
                msgs
            }
        } else {
            Vec::new()
        }
    }

    pub async fn CheckAndDeleteIfTimedOut(&self, session_id: &str) -> bool {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get(session_id) {
            let last_activity = session
                .messages
                .last()
                .map(|m| m.createdAt)
                .unwrap_or(session.createdAt);
            let now = Utc::now();
            let duration = now.signed_duration_since(last_activity);
            if duration.num_minutes() > SessionTimeoutMinutes {
                let session_id_clone = session.id.clone();
                ctrace!(
                    AGENT,
                    "Deleting timed-out session {} (idle for {} mins)",
                    session_id_clone,
                    duration.num_minutes()
                );
                sessions.remove(&session_id_clone);
                self.remove_session_lock(&session_id_clone);
                self.clear_active_stream_cancel(&session_id_clone);
                return true;
            }
        }
        false
    }

    /// Explicitly drop a single session: remove it from the store, drop its
    /// critical-section lock, and cancel any in-flight stream. Mirrors the
    /// timeout-cleanup trio, scoped to one id. Returns `true` if a session was
    /// removed, `false` if no such session existed.
    pub async fn DeleteSession(&self, id: &str) -> bool {
        let removed = self.sessions.write().await.remove(id).is_some();
        if removed {
            self.remove_session_lock(id);
            self.clear_active_stream_cancel(id);
        }
        removed
    }

    pub async fn CleanupTimedOutSessions(&self) -> usize {
        let now = Utc::now();
        let mut sessions = self.sessions.write().await;
        let mut to_remove = Vec::new();

        for (id, session) in sessions.iter() {
            let last_activity = session
                .messages
                .last()
                .map(|m| m.createdAt)
                .unwrap_or(session.createdAt);
            let duration = now.signed_duration_since(last_activity);
            if duration.num_minutes() > SessionTimeoutMinutes {
                to_remove.push(id.clone());
            }
        }

        let cleanedCount = to_remove.len();
        for id in to_remove {
            sessions.remove(&id);
            self.remove_session_lock(&id);
            self.clear_active_stream_cancel(&id);
        }

        if cleanedCount > 0 {
            ctrace!(AGENT, "Cleaned up {} timed-out sessions", cleanedCount);
        }

        cleanedCount
    }
}

// ============================================================================
// Request/Response Types
// ============================================================================

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionListResponse {
    pub sessions: Vec<Session>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionResponse {
    pub session: Session,
    pub tool_count: usize,
}

#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    pub api_key: String,
    /// Published-endpoint slug ("namespace/modelname") selected as the agent
    /// model. Omitted by old clients / when no model is picked, in which case
    /// resolution falls through to the env -> config -> loopback default.
    #[serde(default)]
    pub agent_endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PromptRequest {
    pub message: String,
    #[serde(default)]
    pub stream: bool,
}

#[derive(Debug, Serialize)]
pub struct PromptResponse {
    pub messageId: String,
    pub content: String,
    pub parts: Vec<Part>,
}

#[derive(Debug, Serialize)]
pub struct InterruptSessionResponse {
    pub interrupted: bool,
}

#[derive(Debug, Deserialize)]
pub struct MessageListQuery {
    #[serde(default = "DefaultLimit")]
    pub limit: usize,
}

fn DefaultLimit() -> usize {
    50
}

// ============================================================================
// Model Types
// ============================================================================

#[derive(Debug, serde::Serialize, Clone)]
struct ModelRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    messages: Vec<ModelMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<Tool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    enable_thinking: Option<bool>,
    // Request that streamed responses include a usage chunk. Without this, the
    // backend may omit usage on streamed responses, which would leave the
    // synthesis preflight anchor (prompt_tokens) at 0 and undercount the prompt
    // by the entire prior history.
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
}

#[derive(Debug, serde::Serialize, Clone)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Debug, serde::Serialize, Clone)]
struct Tool {
    r#type: String,
    function: ToolFunction,
}

#[derive(Debug, serde::Serialize, Clone)]
struct ToolFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, serde::Serialize, Clone)]
struct ModelMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ToolCallOut>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, serde::Serialize, Clone)]
struct ToolCallOut {
    id: String,
    r#type: String,
    function: FunctionCallOut,
}

#[derive(Debug, serde::Deserialize, Clone)]
struct ToolCall {
    id: String,
    r#type: String,
    function: FunctionCall,
}

#[derive(Debug, serde::Serialize, Clone)]
struct FunctionCallOut {
    name: String,
    arguments: String,
}

#[derive(Debug, serde::Deserialize, Clone)]
struct FunctionCall {
    name: String,
    arguments: String,
}

// ============================================================================
// Helpers
// ============================================================================

fn caller_tenant(token: &crate::gateway::auth_layer::AccessToken) -> String {
    token
        .restrictTenant
        .clone()
        .or_else(|| token.defaultTenant.clone())
        .unwrap_or_default()
}

fn session_forbidden_response() -> axum::response::Response {
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({
            "error": "forbidden",
            "message": "Session belongs to a different tenant"
        })),
    )
        .into_response()
}

fn build_tools_from_routes(routes: &[crate::gateway::secret::TenantToolRoute]) -> Vec<Tool> {
    routes
        .iter()
        .map(|r| Tool {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: r.tool_name.clone(),
                description: r.description.clone().unwrap_or_default(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": { "query": { "type": "string" } },
                    "required": ["query"]
                }),
            },
        })
        .collect()
}

fn skill_endpoint(
    gateway_base_url: &str,
    route: &crate::gateway::secret::TenantToolRoute,
) -> String {
    format!(
        "{}/skills/{}/{}/{}/v1/chat/completions",
        gateway_base_url, route.owner_tenant, route.owner_namespace, route.skillname
    )
}

fn agent_base_url() -> String {
    let public_base = std::env::var("INFERX_PUBLIC_API_BASE_URL")
        .unwrap_or_default()
        .trim()
        .trim_end_matches('/')
        .to_string();
    if !public_base.is_empty() {
        return public_base;
    }

    format!("http://127.0.0.1:{}", GATEWAY_CONFIG.gatewayPort)
}

/// Resolve the agent model endpoint URL from the session-bound published-endpoint
/// `slug` (the funcname under the virtual "endpoints" namespace). The slug is
/// **mandatory** — `CreateSession` rejects a slug-less request — so there is no
/// env/config/loopback default model. An empty slug (which should not occur)
/// produces a URL that fails to route, surfacing as a "pick a model" error rather
/// than silently selecting some other model.
fn agent_model_endpoint(tenant: &str, slug: &str) -> String {
    let slug = slug.trim().trim_start_matches('/');
    let bare = slug.strip_prefix("endpoints/").unwrap_or(slug);
    agent_endpoint_url(tenant, &format!("endpoints/{}", bare))
}

/// Build the funccall URL for an endpoint reference (`ep`), which may be an
/// absolute URL, an `endpoints/<slug>` reference, a `namespace/modelname`
/// slug, or a raw path fragment.
fn agent_endpoint_url(tenant: &str, ep: &str) -> String {
    let ep = ep.trim();
    if ep.starts_with("http://") || ep.starts_with("https://") {
        return ep.to_string();
    }

    let base = agent_base_url();
    let normalized = ep.trim_start_matches('/');
    if let Some(slug) = normalized.strip_prefix("endpoints/") {
        let slug = slug.trim_matches('/');
        return format!(
            "{}/funccall/{}/endpoints/{}/v1/chat/completions",
            base, tenant, slug
        );
    }

    if let Some((namespace, modelname)) = normalized.split_once('/') {
        let namespace = namespace.trim_matches('/');
        let modelname = modelname.trim_matches('/');
        if !namespace.is_empty() && !modelname.is_empty() {
            return format!(
                "{}/funccall/{}/{}/{}/v1/chat/completions",
                base, tenant, namespace, modelname
            );
        }
    }

    format!("{}/{}", base, normalized)
}

fn strip_think_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        match rest.find("<think>") {
            None => {
                out.push_str(rest);
                break;
            }
            Some(start) => {
                out.push_str(&rest[..start]);
                match rest[start..].find("</think>") {
                    // Unclosed tag — discard only the thinking fragment; emit nothing for it.
                    None => break,
                    Some(end) => rest = &rest[start + end + "</think>".len()..],
                }
            }
        }
    }
    out.trim().to_string()
}

/// Shared state accumulated while draining a content-only model stream.
/// `prompt_tokens` is 0 unless the backend emitted a usage chunk.
struct ContentStreamState {
    content: String,
    prompt_tokens: u64,
}

/// Result of draining a content-only model stream (synthesis / retry).
enum ContentStreamOutcome {
    Completed(ContentStreamState),
    Interrupted(ContentStreamState),
    StreamError {
        state: ContentStreamState,
        error: String,
    },
}

enum ParsedContentStreamLine {
    Ignore,
    Done,
    Usage { prompt_tokens: u64 },
    ContentDelta(String),
}

fn extract_prompt_tokens(json: &serde_json::Value) -> Option<u64> {
    json.get("usage")
        .and_then(|usage| usage.get("prompt_tokens"))
        .and_then(|v| v.as_u64())
}

fn extract_content_delta(json: &serde_json::Value) -> Option<&str> {
    json.get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("delta"))
        .and_then(|d| d.get("content"))
        .and_then(|c| c.as_str())
}

fn parse_content_stream_line(line: &str) -> ParsedContentStreamLine {
    let Some(data) = line.strip_prefix("data: ") else {
        return ParsedContentStreamLine::Ignore;
    };
    if data == "[DONE]" {
        return ParsedContentStreamLine::Done;
    }
    let Ok(json) = serde_json::from_str::<serde_json::Value>(data) else {
        return ParsedContentStreamLine::Ignore;
    };
    if let Some(prompt_tokens) = extract_prompt_tokens(&json) {
        return ParsedContentStreamLine::Usage { prompt_tokens };
    }
    if let Some(delta) = extract_content_delta(&json) {
        return ParsedContentStreamLine::ContentDelta(delta.to_string());
    }
    ParsedContentStreamLine::Ignore
}

/// Drain a content-only SSE model response: forward each content delta to `tx`,
/// capture the latest `prompt_tokens` usage, and stop early if `cancel_token`
/// fires. Used by both the synthesis and the post-compaction retry calls, which
/// (unlike the first call) never carry tool_calls. Tool-call parsing stays
/// inline in the first-call loop.
async fn consume_content_stream(
    response: reqwest::Response,
    cancel_token: &CancellationToken,
    tx: &mpsc::Sender<Result<Event, Infallible>>,
) -> ContentStreamOutcome {
    let mut stream = response.bytes_stream();
    let mut state = ContentStreamState {
        content: String::new(),
        prompt_tokens: 0,
    };

    loop {
        let next_chunk = tokio::select! {
            _ = cancel_token.cancelled() => {
                return ContentStreamOutcome::Interrupted(state);
            }
            chunk = stream.next() => chunk,
        };
        let Some(result) = next_chunk else {
            break;
        };
        match result {
            Ok(chunk) => {
                let chunk_str = String::from_utf8_lossy(chunk.as_ref());
                for line in chunk_str.lines() {
                    match parse_content_stream_line(line) {
                        ParsedContentStreamLine::Ignore => {}
                        ParsedContentStreamLine::Done => break,
                        ParsedContentStreamLine::Usage { prompt_tokens } => {
                            if prompt_tokens > 0 {
                                state.prompt_tokens = prompt_tokens;
                            }
                        }
                        ParsedContentStreamLine::ContentDelta(delta) => {
                            state.content.push_str(&delta);
                            let _ = tx.send(Ok(Event::default().data(delta.to_string()))).await;
                        }
                    }
                }
            }
            Err(e) => {
                return ContentStreamOutcome::StreamError {
                    state,
                    error: format!("{}", e),
                };
            }
        }
    }

    ContentStreamOutcome::Completed(state)
}

/// Conservative built-in context window used only when a published endpoint's
/// `Endpoints.context_length` is NULL/missing or its lookup errors. It is a
/// modest, model-agnostic floor — never 0 (which would disable compaction) — and
/// deliberately NOT the old global `AGENT_MODEL_CONTEXT_LENGTH`: that value is
/// the configured *default* model's window, so applying it to a *different*
/// user-chosen endpoint would silently compact against the wrong window.
const AGENT_FALLBACK_CTX: u64 = 8192;

/// Resolve the agent model context length for the turn.
///
/// The `slug` is the session-bound published-endpoint slug (mandatory). The value
/// is read live from `Endpoints.context_length` (assumed present for a published
/// endpoint). A NULL/missing/non-positive value or any lookup failure falls back
/// to [`AGENT_FALLBACK_CTX`] — never 0, and never another model's window.
async fn agent_model_context_length(
    secret: &crate::gateway::secret::SqlSecret,
    slug: &str,
) -> u64 {
    let slug = slug.trim();
    if slug.is_empty() {
        return AGENT_FALLBACK_CTX;
    }
    match secret.GetEndpointMetadata(slug).await {
        Ok(Some(meta)) => match meta.context_length {
            Some(ctx) if ctx > 0 => ctx as u64,
            _ => AGENT_FALLBACK_CTX,
        },
        Ok(None) => AGENT_FALLBACK_CTX,
        Err(e) => {
            error!("GetEndpointMetadata({}) failed: {:?}", slug, e);
            AGENT_FALLBACK_CTX
        }
    }
}

/// Post-turn (reactive) compaction ratio (percent of context window). Applied
/// after a turn against the provider-reported prompt_tokens, distinct from the
/// preflight ratios that run before a request against an estimate.
/// Env `AGENT_POSTTURN_COMPACTION_RATIO` overrides config. Values above 100 are
/// clamped to 100 so invalid config degrades predictably.
fn agent_postturn_compaction_ratio() -> u64 {
    std::env::var("AGENT_POSTTURN_COMPACTION_RATIO")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(NODE_CONFIG.agent_postturn_compaction_ratio)
        .min(100)
}

/// Post-turn compaction threshold for the turn's resolved context length
/// (`ctx`). `ctx` is resolved once at prompt start and threaded through the
/// turn so the window cannot change mid-turn.
fn postturn_compaction_threshold(ctx: u64) -> Option<u64> {
    if ctx == 0 {
        None
    } else {
        Some(ctx.saturating_mul(agent_postturn_compaction_ratio()) / 100)
    }
}

/// Anchored synthesis preflight threshold (percent of context window).
/// Env `AGENT_PREFLIGHT_COMPACTION_RATIO` overrides config (env over config).
fn agent_preflight_compaction_ratio() -> u64 {
    std::env::var("AGENT_PREFLIGHT_COMPACTION_RATIO")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(NODE_CONFIG.agent_preflight_compaction_ratio)
}

/// Whole-request first-call preflight threshold (percent of context window).
/// Env `AGENT_FIRST_CALL_COMPACTION_RATIO` overrides config.
fn agent_first_call_compaction_ratio() -> u64 {
    std::env::var("AGENT_FIRST_CALL_COMPACTION_RATIO")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(NODE_CONFIG.agent_first_call_compaction_ratio)
}

/// Token budget for the recent-context tail retained verbatim after compaction.
/// Env `AGENT_COMPACTION_RECENT_CONTEXT_TOKEN_BUDGET` overrides config.
fn agent_compaction_recent_context_token_budget() -> u64 {
    std::env::var("AGENT_COMPACTION_RECENT_CONTEXT_TOKEN_BUDGET")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(NODE_CONFIG.agent_compaction_recent_context_token_budget)
}

/// Codex-style rough estimator: serialize to JSON and treat ~4 bytes as 1 token.
fn estimate_serialized_tokens<T: serde::Serialize>(value: &T) -> u64 {
    let bytes = serde_json::to_vec(value).map(|v| v.len()).unwrap_or(0);
    ((bytes as u64) + 3) / 4
}

/// Anchored synthesis estimate: provider-reported `prompt_tokens` from the
/// immediately preceding model call plus a local estimate of the synthesis-only
/// input appended after that call. Never subtracts from the provider anchor.
fn estimate_synthesis_tokens<T: serde::Serialize>(
    prior_call_prompt_tokens: u64,
    synthesis_only_added_input: &T,
) -> u64 {
    prior_call_prompt_tokens + estimate_serialized_tokens(synthesis_only_added_input)
}

/// Pure preflight decision: returns true when compaction should run before the
/// request. `ratio_pct` is a percent of `ctx`. A `ctx` of 0 disables preflight.
fn preflight_decision(estimate_tokens: u64, ctx: u64, ratio_pct: u64) -> bool {
    if ctx == 0 {
        return false;
    }
    estimate_tokens >= ctx.saturating_mul(ratio_pct) / 100
}

// Minimum number of most-recent messages always retained verbatim after
// compaction, even if a single message exceeds the recent-context budget.
const COMPACTION_MIN_TAIL: usize = 2;

const FIRST_CALL_SYSTEM_PROMPT: &str = "You are a helpful assistant. When responding, ALWAYS use proper markdown formatting with explicit newlines:\n\n1. Use double newlines (\\n\\n) to separate paragraphs\n2. Use single newlines for line breaks within sections\n3. Format lists with each item on its own line\n4. Format tables with each row on its own line\n5. Use headers (###) on their own lines with blank lines before and after\n6. NEVER run text together without proper spacing\n\nYour responses should be well-formatted markdown that renders correctly.";

const SYNTHESIS_SYSTEM_PROMPT: &str = "You are a helpful assistant.";

/// Build the first-call model message vector: system prompt + history.
fn build_first_call_messages(history: &[Message]) -> Vec<ModelMessage> {
    let mut v = Vec::with_capacity(history.len() + 1);
    v.push(ModelMessage {
        role: "system".to_string(),
        content: FIRST_CALL_SYSTEM_PROMPT.to_string(),
        tool_calls: None,
        tool_call_id: None,
    });
    for msg in history {
        v.push(ModelMessage {
            role: msg.role.clone(),
            content: msg.content.clone(),
            tool_calls: None,
            tool_call_id: None,
        });
    }
    v
}

/// Build the synthesis-only input appended after the first model call: the
/// assistant `tool_calls` wrapper plus one `tool` message per result. Used both
/// to construct the request and to estimate the anchored synthesis delta.
fn build_synthesis_suffix(
    tool_calls: &[ToolCall],
    tool_results: &[(String, String)],
) -> Vec<ModelMessage> {
    let mut v = Vec::with_capacity(tool_results.len() + 1);
    v.push(ModelMessage {
        role: "assistant".to_string(),
        content: String::new(),
        tool_calls: Some(
            tool_calls
                .iter()
                .map(|tc| ToolCallOut {
                    id: tc.id.clone(),
                    r#type: tc.r#type.clone(),
                    function: FunctionCallOut {
                        name: tc.function.name.clone(),
                        arguments: tc.function.arguments.clone(),
                    },
                })
                .collect(),
        ),
        tool_call_id: None,
    });
    for (tc_id, result) in tool_results {
        v.push(ModelMessage {
            role: "tool".to_string(),
            content: result.clone(),
            tool_calls: None,
            tool_call_id: Some(tc_id.clone()),
        });
    }
    v
}

/// Build the full synthesis request message vector: synthesis system prompt +
/// compacted model history + the synthesis suffix.
fn build_synthesis_messages(history: &[Message], suffix: &[ModelMessage]) -> Vec<ModelMessage> {
    let mut v = Vec::with_capacity(history.len() + suffix.len() + 1);
    v.push(ModelMessage {
        role: "system".to_string(),
        content: SYNTHESIS_SYSTEM_PROMPT.to_string(),
        tool_calls: None,
        tool_call_id: None,
    });
    for msg in history {
        v.push(ModelMessage {
            role: msg.role.clone(),
            content: msg.content.clone(),
            tool_calls: None,
            tool_call_id: None,
        });
    }
    v.extend(suffix.iter().cloned());
    v
}

enum CompactionResult {
    Compacted,
    NoChange,
    NoOp,
    Failed,
}

const COMPACTION_SYSTEM_PROMPT: &str = "\
You are a conversation summarizer. Summarize the conversation history provided \
by the user into a concise structured summary that preserves all information \
needed to continue the conversation seamlessly. \
Do not answer the conversation. Do not mention that you are summarizing. \
Respond with the summary only, using this structure:

## Summary
[One sentence describing the overall goal or topic]

## Progress
[Bullet list of what has been accomplished]

## Key Facts
[Bullet list of important technical details, decisions, file paths, values]

## Last Exchange
[Brief description of the most recent topic before the tail turns]";

async fn compact_session(
    gw: &HttpGateway,
    session_id: &str,
    endpoint: &str,
    api_key: &str,
) -> CompactionResult {
    let session = match gw.sessions.GetSession(session_id).await {
        Some(s) => s,
        None => return CompactionResult::NoOp,
    };

    // Compact the model working context, not the visible transcript.
    let messages = &session.model_messages;

    // Token-budgeted recent tail. Walk newest -> oldest accumulating
    // estimated tokens; everything older than the cut is summarized. Always
    // retain at least the most recent user/assistant pair verbatim even if a
    // single message exceeds the budget, so compaction never yields an empty
    // tail. The current-turn tool_calls wrapper and tool results are not part of
    // `model_messages`, so they are excluded from this accounting by construction.
    let budget = agent_compaction_recent_context_token_budget();
    let mut acc: u64 = 0;
    let mut tail_start = messages.len();
    for i in (0..messages.len()).rev() {
        let t = estimate_serialized_tokens(&messages[i]);
        // Stop once adding this message would exceed the budget, but only after
        // at least the most recent pair (2 messages) has been retained.
        if acc + t > budget && (messages.len() - i) > COMPACTION_MIN_TAIL {
            break;
        }
        acc += t;
        tail_start = i;
    }

    // "Too short to compact": the entire history already fits within the
    // recent-context budget, so there is nothing older to summarize.
    if tail_start == 0 {
        return CompactionResult::NoOp;
    }

    let head = &messages[..tail_start];
    let tail = messages[tail_start..].to_vec();

    // Build the history text to summarize
    let history: String = head
        .iter()
        .filter(|m| m.role != "system")
        .map(|m| format!("{}: {}", m.role, m.content))
        .collect::<Vec<_>>()
        .join("\n\n");

    // Check if head is the previous summary (starts with "## Summary")
    let previous_summary = head
        .first()
        .filter(|m| m.role == "system" && m.content.starts_with("## Summary"))
        .map(|m| m.content.as_str());

    let user_content = if let Some(prev) = previous_summary {
        format!(
            "Update the summary below with the new conversation history.\n\
            <previous-summary>\n{}\n</previous-summary>\n\nNew history:\n{}",
            prev, history
        )
    } else {
        format!("Summarize this conversation history:\n\n{}", history)
    };

    let request_body = serde_json::json!({
        "messages": [
            {"role": "system", "content": COMPACTION_SYSTEM_PROMPT},
            {"role": "user", "content": user_content}
        ],
        "stream": false,
        "max_tokens": 2000,
        "temperature": 0.0
    });

    let client = Client::new();
    match client
        .post(endpoint)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&request_body)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if let Some(summary) = json
                    .get("choices")
                    .and_then(|c| c.as_array())
                    .and_then(|a| a.first())
                    .and_then(|c| c.get("message"))
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                {
                    let trimmed = summary.trim().to_string();
                    if trimmed.is_empty() {
                        CompactionResult::NoChange
                    } else {
                        ctrace!(
                            AGENT,
                            "Compacting session {} ({} head msgs → summary)",
                            session_id,
                            head.len()
                        );
                        // Rewrite the model working context, and leave a
                        // marker in the visible transcript (append-only) so the UI
                        // can show that older history was summarized.
                        gw.sessions
                            .ReplaceModelMessagesWithSummary(session_id, trimmed.clone(), tail)
                            .await;
                        gw.sessions
                            .AppendCompactionMarker(session_id, trimmed)
                            .await;
                        CompactionResult::Compacted
                    }
                } else {
                    CompactionResult::NoChange
                }
            } else {
                CompactionResult::NoChange
            }
        }
        Ok(resp) => {
            error!("Compaction model error: {}", resp.status());
            CompactionResult::Failed
        }
        Err(e) => {
            error!("Compaction request failed: {}", e);
            CompactionResult::Failed
        }
    }
}

fn is_context_overflow(body: &str) -> bool {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(msg) = json.pointer("/error/message").and_then(|v| v.as_str()) {
            return msg.contains("maximum context length")
                || msg.contains("context_length_exceeded");
        }
    }
    body.contains("maximum context length") || body.contains("context_length_exceeded")
}

fn session_not_found_response() -> axum::response::Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "error": "session_not_found",
            "message": "Session not found or expired"
        })),
    )
        .into_response()
}

fn session_interrupted_response() -> axum::response::Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({
            "error": "interrupted",
            "message": "Request interrupted"
        })),
    )
        .into_response()
}

enum InterruptedTurnStorageDecision {
    DropUserPrompt,
    StoreAssistantText(String),
    StoreInterruptedMarker,
}

fn interrupted_turn_storage_decision(
    partial_content: &str,
    tool_calls: &[ToolCall],
) -> InterruptedTurnStorageDecision {
    let stored_content = strip_think_tags(partial_content);
    if !stored_content.is_empty() {
        InterruptedTurnStorageDecision::StoreAssistantText(stored_content)
    } else if tool_calls.is_empty() {
        InterruptedTurnStorageDecision::DropUserPrompt
    } else {
        InterruptedTurnStorageDecision::StoreInterruptedMarker
    }
}

async fn emit_interrupted_event(tx: &mpsc::Sender<Result<Event, Infallible>>) {
    let _ = tx
        .send(Ok(Event::default().event("interrupted").data(
            serde_json::json!({
                "error": "interrupted",
                "message": "Request interrupted"
            })
            .to_string(),
        )))
        .await;
    let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
}

async fn append_assistant_message(gw: &HttpGateway, session_id: &str, content: String) {
    let assistant_message = Message {
        id: Uuid::new_v4().to_string(),
        role: "assistant".to_string(),
        content: content.clone(),
        createdAt: Utc::now(),
        parts: vec![Part::Text { text: content }],
    };
    gw.sessions.AddMessage(session_id, assistant_message).await;
}

async fn finalize_interrupted_prompt(
    gw: &HttpGateway,
    session_id: &str,
    user_message_id: &str,
    partial_content: &str,
    tool_calls: &[ToolCall],
) {
    match interrupted_turn_storage_decision(partial_content, tool_calls) {
        InterruptedTurnStorageDecision::DropUserPrompt => {
            gw.sessions
                .RemoveMessageById(session_id, user_message_id)
                .await;
        }
        InterruptedTurnStorageDecision::StoreAssistantText(text) => {
            append_assistant_message(gw, session_id, text).await;
        }
        InterruptedTurnStorageDecision::StoreInterruptedMarker => {
            append_assistant_message(gw, session_id, INTERRUPTED_TOOL_TURN_MESSAGE.to_string())
                .await;
        }
    }
}

// ============================================================================
// Tool Backend Calls
// ============================================================================

async fn CallSkillTool(
    endpoint_url: &str,
    api_key: &str,
    arguments: &str,
    cancel_token: &CancellationToken,
) -> String {
    let query: String = serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|v| {
            v.get("query")
                .and_then(|q| q.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| arguments.to_string());

    let client = Client::new();
    let skill_request = serde_json::json!({
        "max_tokens": 8000,
        "messages": [
            {"content": "You are a helpful assistant. /no_think", "role": "system"},
            {"content": query, "role": "user"}
        ],
        "stream": false,
        "temperature": 0,
        "enable_thinking": false
    });

    let response = tokio::select! {
        _ = cancel_token.cancelled() => {
            return "Request interrupted".to_string();
        }
        response = client
            .post(endpoint_url)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", api_key))
            .json(&skill_request)
            .send() => response,
    };

    match response {
        Ok(response) => {
            let status = response.status();
            if status.is_success() {
                match tokio::select! {
                    _ = cancel_token.cancelled() => {
                        return "Request interrupted".to_string();
                    }
                    json = response.json::<serde_json::Value>() => json,
                } {
                    Ok(json) => {
                        if let Some(content) = json
                            .get("choices")
                            .and_then(|c| c.as_array())
                            .and_then(|a| a.first())
                            .and_then(|c| c.get("message"))
                            .and_then(|m| m.get("content"))
                            .and_then(|c| c.as_str())
                        {
                            // Strip <think>…</think> blocks — they waste synthesis tokens
                            // and can confuse the main model with the skill's internal reasoning.
                            let stripped = strip_think_tags(content);
                            ctrace!(
                                AGENT,
                                "Skill tool response received from {} len={}",
                                endpoint_url,
                                stripped.len()
                            );
                            return stripped;
                        }
                        "No content in skill response".to_string()
                    }
                    Err(e) => {
                        error!("Failed to parse skill response: {}", e);
                        format!("Failed to parse skill response: {}", e)
                    }
                }
            } else {
                error!("Skill tool API error: {}", status);
                format!("Skill tool returned error: {}", status)
            }
        }
        Err(e) => {
            error!("Failed to call skill tool at {}: {}", endpoint_url, e);
            format!("Skill tool call failed: {}", e)
        }
    }
}

// ============================================================================
// Model Backend Calls
// ============================================================================

/// Error from the non-stream model backend. `tools_executed` distinguishes a
/// failure before any tool ran (the user prompt produced no work and is safe to
/// roll back) from a failure after skills were dispatched (a real multi-step
/// turn whose synthesis failed — rolling back would erase the initiating user
/// message). This mirrors the streaming path's no-rollback-after-tools rule.
#[derive(Debug)]
struct ModelBackendError {
    message: String,
    tools_executed: bool,
}

impl std::fmt::Display for ModelBackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// A session-bound endpoint can become unpublished/unroutable after the session
/// was created. Dispatch to it then fails with an "unpublished" / "endpoint not
/// found" message (the existing dispatch-time published check). Classify that
/// case so both prompt paths return a clear, actionable error telling the user
/// to pick a model / start a new session, rather than a generic "Model error"
/// — and crucially without falling through to any default endpoint. Returns
/// `None` for unrelated backend failures, which keep their original message.
fn classify_endpoint_dispatch_error(raw: &str) -> Option<String> {
    let lowered = raw.to_lowercase();
    // The funccall route normalizes a deleted/unpublished endpoint to
    // "service failure: endpoint not found" (NotExist from GetFunc, missing
    // funcstatus, and unpublished all collapse to this). The raw internal
    // strings ("is unpublished", "... does not exist") are matched too in case
    // any path surfaces them directly.
    if lowered.contains("is unpublished")
        || lowered.contains("endpoint not found")
        || lowered.contains("does not exist")
    {
        Some(
            "The selected model is no longer available (it may have been unpublished). \
             Pick a model and start a new session."
                .to_string(),
        )
    } else {
        None
    }
}

/// On success returns `(content, prompt_tokens)` where `prompt_tokens` is the
/// provider-reported usage from the final model call (synthesis when tools ran),
/// 0 if the backend omitted usage.
async fn CallModelBackend(
    messages: &[Message],
    endpoint: &str,
    api_key: &str,
    tools: Option<Vec<Tool>>,
    routes: &[crate::gateway::secret::TenantToolRoute],
) -> Result<(String, u64), ModelBackendError> {
    let client = Client::new();

    let mut model_messages = Vec::new();
    model_messages.push(ModelMessage {
        role: "system".to_string(),
        content: "You are a helpful assistant. When responding, ALWAYS use proper markdown formatting with explicit newlines:\n\n1. Use double newlines (\\n\\n) to separate paragraphs\n2. Use single newlines for line breaks within sections\n3. Format lists with each item on its own line\n4. Format tables with each row on its own line\n5. Use headers (###) on their own lines with blank lines before and after\n6. NEVER run text together without proper spacing\n\nYour responses should be well-formatted markdown that renders correctly.".to_string(),
        tool_calls: None,
    tool_call_id: None,
    });

    for msg in messages {
        model_messages.push(ModelMessage {
            role: msg.role.clone(),
            content: msg.content.clone(),
            tool_calls: None,
            tool_call_id: None,
        });
    }

    let request_body = ModelRequest {
        model: None,
        messages: model_messages,
        stream: false,
        max_tokens: Some(20000),
        temperature: Some(0.0),
        tools,
        enable_thinking: Some(false),
        stream_options: None,
    };

    let response = client
        .post(endpoint)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&request_body)
        .send()
        .await;

    let response = match response {
        Ok(r) => {
            ctrace!(AGENT, "Got response with status: {}", r.status());
            r
        }
        Err(e) => {
            error!("Request failed: {}", e);
            return Err(ModelBackendError {
                message: format!("Request failed: {}", e),
                tools_executed: false,
            });
        }
    };

    if !response.status().is_success() {
        let error_text = response.text().await.unwrap_or_default();
        error!("Model API error: {}", error_text);
        return Err(ModelBackendError {
            message: format!("Model API error: {}", error_text),
            tools_executed: false,
        });
    }

    let model_response: serde_json::Value =
        response.json().await.map_err(|e| ModelBackendError {
            message: format!("Failed to parse model response: {}", e),
            tools_executed: false,
        })?;

    let message = model_response
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first())
        .and_then(|c| c.get("message"));

    let tool_calls_json = message
        .and_then(|m| m.get("tool_calls"))
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();

    if !tool_calls_json.is_empty() {
        let tool_calls: Vec<ToolCall> = tool_calls_json
            .iter()
            .filter_map(|tc| serde_json::from_value(tc.clone()).ok())
            .collect();

        ctrace!(
            AGENT,
            "Non-streaming tool calls detected: {}",
            tool_calls.len()
        );

        let mut synthesis_messages = Vec::new();
        synthesis_messages.push(ModelMessage {
            role: "system".to_string(),
            content: "You are a helpful assistant.".to_string(),
            tool_calls: None,
            tool_call_id: None,
        });
        for msg in messages {
            synthesis_messages.push(ModelMessage {
                role: msg.role.clone(),
                content: msg.content.clone(),
                tool_calls: None,
                tool_call_id: None,
            });
        }

        // Assistant message carrying the tool_calls array (required by OpenAI protocol)
        synthesis_messages.push(ModelMessage {
            role: "assistant".to_string(),
            content: String::new(),
            tool_calls: Some(
                tool_calls
                    .iter()
                    .map(|tc| ToolCallOut {
                        id: tc.id.clone(),
                        r#type: tc.r#type.clone(),
                        function: FunctionCallOut {
                            name: tc.function.name.clone(),
                            arguments: tc.function.arguments.clone(),
                        },
                    })
                    .collect(),
            ),
            tool_call_id: None,
        });

        // Dispatch all skill calls concurrently, same as the streaming path.
        // Assumption: skills are stateless with no ordering dependencies in the same turn.
        let gw_base = format!("http://127.0.0.1:{}", GATEWAY_CONFIG.gatewayPort);
        let skill_futures = tool_calls.iter().enumerate().map(|(idx, tool_call)| {
            let gw_base = gw_base.clone();
            let api_key = api_key.to_string();
            let routes = routes.to_vec();
            let cancel_token = CancellationToken::new();
            async move {
                let tool_response = if let Some(route) = routes
                    .iter()
                    .find(|r| r.tool_name == tool_call.function.name)
                {
                    let url = skill_endpoint(&gw_base, route);
                    CallSkillTool(&url, &api_key, &tool_call.function.arguments, &cancel_token)
                        .await
                } else {
                    format!("Unknown tool: {}", tool_call.function.name)
                };
                (idx, tool_call.id.clone(), tool_response)
            }
        });
        let mut skill_results = futures::future::join_all(skill_futures).await;
        skill_results.sort_by_key(|(idx, _, _)| *idx);
        for (_, tc_id, tool_response) in skill_results {
            synthesis_messages.push(ModelMessage {
                role: "tool".to_string(),
                content: tool_response,
                tool_calls: None,
                tool_call_id: Some(tc_id),
            });
        }

        let synthesis_request = ModelRequest {
            model: None,
            messages: synthesis_messages,
            stream: false,
            max_tokens: Some(20000),
            temperature: Some(0.0),
            tools: None,
            enable_thinking: Some(false),
            stream_options: None,
        };

        let synthesis_response = client
            .post(endpoint)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", api_key))
            .json(&synthesis_request)
            .send()
            .await
            .map_err(|e| ModelBackendError {
                message: format!("Synthesis request failed: {}", e),
                tools_executed: true,
            })?;

        if !synthesis_response.status().is_success() {
            let error_text = synthesis_response.text().await.unwrap_or_default();
            return Err(ModelBackendError {
                message: format!("Synthesis model API error: {}", error_text),
                tools_executed: true,
            });
        }

        let synthesis_json: serde_json::Value =
            synthesis_response
                .json()
                .await
                .map_err(|e| ModelBackendError {
                    message: format!("Failed to parse synthesis response: {}", e),
                    tools_executed: true,
                })?;
        let content = synthesis_json
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|c| c.first())
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .map(|c| c.trim())
            .filter(|c| !c.is_empty());

        // Empty synthesis content after tools ran: treat as a no-reply turn,
        // matching the streaming path's empty_response handling. tools_executed
        // is true so the caller keeps the user message (real multi-step turn).
        let synth_prompt_tokens = extract_prompt_tokens(&synthesis_json).unwrap_or(0);
        return match content {
            Some(c) => Ok((c.to_string(), synth_prompt_tokens)),
            None => Err(ModelBackendError {
                message: "Model returned no content".to_string(),
                tools_executed: true,
            }),
        };
    }

    // No tools ran on this turn. Empty first-call content is a no-reply turn,
    // matching the streaming empty_response branch; tools_executed is false so
    // the caller rolls back the user message.
    let content = message
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(|c| c.trim())
        .filter(|c| !c.is_empty());

    let prompt_tokens = extract_prompt_tokens(&model_response).unwrap_or(0);
    match content {
        Some(c) => Ok((c.to_string(), prompt_tokens)),
        None => Err(ModelBackendError {
            message: "Model returned no content".to_string(),
            tools_executed: false,
        }),
    }
}

// ============================================================================
// Prompt Handlers
// ============================================================================

pub async fn HandlePrompt(
    State(gw): State<HttpGateway>,
    Path(sessionId): Path<String>,
    Json(req): Json<PromptRequest>,
) -> impl IntoResponse {
    let session = match gw.sessions.GetSession(&sessionId).await {
        Some(s) => s,
        None => return session_not_found_response(),
    };

    if gw.sessions.CheckAndDeleteIfTimedOut(&sessionId).await {
        return session_not_found_response();
    }

    let tenant = session.tenant.clone();
    let agent_endpoint = session.agent_endpoint.clone();
    let endpoint = agent_model_endpoint(&tenant, &agent_endpoint);
    // Resolve the context window once for the whole turn (one metadata read),
    // mirroring the streaming path, so post-turn compaction is computed against
    // the selected model's window.
    let ctx = agent_model_context_length(&gw.sqlSecret, &agent_endpoint).await;
    let api_key = session.skill_api_key.clone();
    let routes = gw
        .sqlSecret
        .GetToolsForTenant(&tenant)
        .await
        .unwrap_or_default();
    let tools_for_request: Option<Vec<Tool>> = if routes.is_empty() {
        None
    } else {
        Some(build_tools_from_routes(&routes))
    };

    let userMessageId = Uuid::new_v4().to_string();
    let userMessage = Message {
        id: userMessageId.clone(),
        role: "user".to_string(),
        content: req.message.clone(),
        createdAt: Utc::now(),
        parts: vec![Part::Text {
            text: req.message.clone(),
        }],
    };

    gw.sessions.AddMessage(&sessionId, userMessage).await;

    if req.stream {
        return (
            StatusCode::TEMPORARY_REDIRECT,
            Json(PromptResponse {
                messageId: String::new(),
                content: "Use /sessions/{id}/prompt_stream for streaming".to_string(),
                parts: vec![],
            }),
        )
            .into_response();
    }

    // Build requests from the compacted model working context.
    let session = gw.sessions.GetSession(&sessionId).await;
    let messages = session.map(|s| s.model_messages).unwrap_or_default();

    match CallModelBackend(&messages, &endpoint, &api_key, tools_for_request, &routes).await {
        Ok((responseText, prompt_tokens)) => {
            let assistantMessageId = Uuid::new_v4().to_string();
            let parts = vec![Part::Text {
                text: responseText.clone(),
            }];

            let assistantMessage = Message {
                id: assistantMessageId.clone(),
                role: "assistant".to_string(),
                content: responseText.clone(),
                createdAt: Utc::now(),
                parts: parts.clone(),
            };

            gw.sessions.AddMessage(&sessionId, assistantMessage).await;

            // Update stored token count and compact if over the selected model's
            // post-turn threshold, mirroring the streaming path.
            if prompt_tokens > 0 {
                gw.sessions.UpdateTokens(&sessionId, prompt_tokens).await;
                if postturn_compaction_threshold(ctx).map_or(false, |t| prompt_tokens >= t) {
                    ctrace!(
                        AGENT,
                        "Session {} at {} prompt tokens, compacting (non-stream)",
                        sessionId, prompt_tokens
                    );
                    compact_session(&gw, &sessionId, &endpoint, &api_key).await;
                }
            }

            (
                StatusCode::OK,
                Json(PromptResponse {
                    messageId: assistantMessageId,
                    content: responseText,
                    parts,
                }),
            )
                .into_response()
        }
        Err(e) => {
            // Roll back the user message only when the turn produced no work and
            // nothing was returned: i.e. the first call failed before any tools
            // ran. If tools already executed, this is a real multi-step turn
            // whose synthesis failed — keep the user message, matching the
            // streaming path's no-rollback-after-tools rule.
            if !e.tools_executed {
                gw.sessions
                    .RemoveMessageById(&sessionId, &userMessageId)
                    .await;
            }
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(PromptResponse {
                    messageId: String::new(),
                    content: classify_endpoint_dispatch_error(&e.message)
                        .unwrap_or_else(|| format!("Model error: {}", e)),
                    parts: vec![],
                }),
            )
                .into_response()
        }
    }
}

/// Outcome of the post-compaction synthesis retry.
enum SynthesisRetryOutcome {
    /// The turn was interrupted mid-retry. The caller should stop and return;
    /// `finalize_interrupted_prompt` + `emit_interrupted_event` already ran.
    Interrupted,
    /// The retry finished (success, empty, send error, or HTTP error). Any
    /// error was already reported on `tx`. Carries the latest `prompt_tokens`
    /// for the caller to fold back into the turn.
    Done { prompt_tokens: u64 },
}

/// Run the synthesis call again after a context-overflow compaction, building
/// the request from the freshly compacted `model_messages`. Extracted from the
/// `CompactionResult::Compacted` branch to keep `HandlePromptStream` shallow.
/// Any error path emits its own SSE `error` event; only an interrupt asks the
/// caller to bail out of the turn.
#[allow(clippy::too_many_arguments)]
async fn run_synthesis_retry(
    gw: &HttpGateway,
    session_id: &str,
    user_message_id: &str,
    endpoint: &str,
    api_key: &str,
    tool_calls: &[ToolCall],
    tool_results: &[(String, String)],
    full_content: &str,
    ctx: u64,
    prompt_tokens: u64,
    cancel_token: &CancellationToken,
    tx: &mpsc::Sender<Result<Event, Infallible>>,
) -> SynthesisRetryOutcome {
    let mut prompt_tokens = prompt_tokens;

    // The fallback retry must also build from model_messages so it uses
    // compacted working context and never forwards a compaction marker.
    let compacted_prior = gw
        .sessions
        .GetSession(session_id)
        .await
        .map(|s| s.model_messages)
        .unwrap_or_default();
    let retry_messages = build_synthesis_messages(
        &compacted_prior,
        &build_synthesis_suffix(tool_calls, tool_results),
    );
    let retry_max_tokens = if ctx > 0 { (ctx / 4).max(512) } else { 4096 };
    let retry_request = ModelRequest {
        model: None,
        messages: retry_messages,
        stream: true,
        max_tokens: Some(retry_max_tokens as i32),
        temperature: Some(0.0),
        tools: None,
        enable_thinking: Some(false),
        stream_options: Some(StreamOptions {
            include_usage: true,
        }),
    };

    let send_result = tokio::select! {
        _ = cancel_token.cancelled() => {
            finalize_interrupted_prompt(gw, session_id, user_message_id, full_content, tool_calls)
                .await;
            emit_interrupted_event(tx).await;
            return SynthesisRetryOutcome::Interrupted;
        }
        response = Client::new()
            .post(endpoint)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", api_key))
            .json(&retry_request)
            .send() => response,
    };

    match send_result {
        Ok(retry_resp) if retry_resp.status().is_success() => {
            match consume_content_stream(retry_resp, cancel_token, tx).await {
                ContentStreamOutcome::Completed(state) => {
                    let retry_content = state.content;
                    if state.prompt_tokens > 0 {
                        prompt_tokens = state.prompt_tokens;
                    }
                    if !retry_content.is_empty() {
                        let stored_content = strip_think_tags(&retry_content);
                        let retry_msg_id = Uuid::new_v4().to_string();
                        let retry_msg = Message {
                            id: retry_msg_id.clone(),
                            role: "assistant".to_string(),
                            content: stored_content.clone(),
                            createdAt: Utc::now(),
                            parts: vec![Part::Text {
                                text: stored_content,
                            }],
                        };
                        gw.sessions.AddMessage(session_id, retry_msg).await;
                        if postturn_compaction_threshold(ctx).map_or(false, |t| prompt_tokens >= t) {
                            ctrace!(
                                AGENT,
                                "Session {} at {} prompt tokens, compacting after retry synthesis",
                                session_id, prompt_tokens
                            );
                            let _ = tx
                                .send(Ok(Event::default().event("compacting").data("")))
                                .await;
                            compact_session(gw, session_id, endpoint, api_key).await;
                            let _ = tx
                                .send(Ok(Event::default().event("compacted").data("")))
                                .await;
                        }
                        let _ = tx
                            .send(Ok(Event::default()
                                .event("complete")
                                .data(format!("{{\"message_id\": \"{}\"}}", retry_msg_id))))
                            .await;
                    } else {
                        // Retry stream finished cleanly but emitted no content.
                        // Surface an explicit error instead of ending silently.
                        // No user-message rollback: tools already executed this turn.
                        error!(
                            "[agent] empty synthesis retry response session={} (no content after tools)",
                            session_id
                        );
                        let _ = tx
                            .send(Ok(Event::default().event("error").data(
                                serde_json::json!({
                                    "error": "empty_response",
                                    "message": "Model returned no content"
                                })
                                .to_string(),
                            )))
                            .await;
                    }
                }
                ContentStreamOutcome::Interrupted(state) => {
                    finalize_interrupted_prompt(
                        gw,
                        session_id,
                        user_message_id,
                        &state.content,
                        tool_calls,
                    )
                    .await;
                    emit_interrupted_event(tx).await;
                    return SynthesisRetryOutcome::Interrupted;
                }
                ContentStreamOutcome::StreamError { state, error: err } => {
                    // Keep any usage seen before the transport failed: the turn
                    // still falls through to the common post-turn footer, where a
                    // stale last_prompt_tokens would skew the next turn's
                    // anchored preflight.
                    if state.prompt_tokens > 0 {
                        prompt_tokens = state.prompt_tokens;
                    }
                    error!("Synthesis retry stream error: {}", err);
                    let _ = tx
                        .send(Ok(Event::default().event("error").data(
                            serde_json::json!({
                                "error": "stream_error",
                                "message": format!("Skill synthesis call failed: {}", err)
                            })
                            .to_string(),
                        )))
                        .await;
                }
            }
        }
        Ok(retry_resp) => {
            if retry_resp.status().as_u16() == 400 {
                let _ = tx.send(Ok(Event::default().event("error").data(
                    serde_json::json!({
                        "error": "upstream_error",
                        "message": "Synthesis failed after compaction; tool results may exceed context window"
                    }).to_string()
                ))).await;
            } else {
                error!("Synthesis retry error status: {}", retry_resp.status());
                let _ = tx
                    .send(Ok(Event::default().event("error").data(
                        serde_json::json!({
                            "error": "upstream_error",
                            "message": "Skill synthesis call failed"
                        })
                        .to_string(),
                    )))
                    .await;
            }
        }
        Err(e) => {
            error!("Synthesis retry request failed: {}", e);
            let _ = tx
                .send(Ok(Event::default().event("error").data(
                    serde_json::json!({
                        "error": "upstream_error",
                        "message": format!("Skill synthesis call failed: {}", e)
                    })
                    .to_string(),
                )))
                .await;
        }
    }

    SynthesisRetryOutcome::Done { prompt_tokens }
}

// ============================================================================
// Stream Handlers
// ============================================================================

pub async fn HandlePromptStream(
    State(gw): State<HttpGateway>,
    Path(sessionId): Path<String>,
    Json(req): Json<PromptRequest>,
) -> impl IntoResponse {
    ctrace!(AGENT, "prompt_stream start session={}", sessionId);

    let session = match gw.sessions.GetSession(&sessionId).await {
        Some(s) => s,
        None => return session_not_found_response(),
    };

    if gw.sessions.CheckAndDeleteIfTimedOut(&sessionId).await {
        return session_not_found_response();
    }

    let tenant = session.tenant.clone();
    let agent_endpoint = session.agent_endpoint.clone();
    let endpoint_str = agent_model_endpoint(&tenant, &agent_endpoint);
    let api_key_str = session.skill_api_key.clone();
    let routes = gw
        .sqlSecret
        .GetToolsForTenant(&tenant)
        .await
        .unwrap_or_default();
    ctrace!(
        AGENT,
        "tenant={} endpoint={} tools={}",
        tenant,
        endpoint_str,
        routes.len()
    );

    let userMessageId = Uuid::new_v4().to_string();
    let userMessage = Message {
        id: userMessageId.clone(),
        role: "user".to_string(),
        content: req.message.clone(),
        createdAt: Utc::now(),
        parts: vec![Part::Text {
            text: req.message.clone(),
        }],
    };

    let client = Client::new();

    // Serialize preflight -> compact -> rebuild -> send per session.
    // Hold only the per-session mutex (never the global session map lock) across
    // awaits. The guard is moved into the spawned task so it also covers the
    // synthesis critical section.
    //
    // Acquire the per-session lock BEFORE appending the user message. AddMessage
    // mirrors into model_messages, which is the candidate-request source read
    // below; if a concurrent prompt for the same session appended its user
    // message between this write and the first-call read, its message would
    // contaminate this turn's request. Holding the lock across both the write and
    // the read keeps the whole build-candidate -> estimate -> compact -> rebuild
    // -> send sequence serialized.
    let session_lock = gw.sessions.SessionLock(&sessionId);
    let session_guard = session_lock.lock_owned().await;

    gw.sessions.AddMessage(&sessionId, userMessage).await;
    let cancel_token = CancellationToken::new();
    gw.sessions
        .register_active_stream_cancel(&sessionId, cancel_token.clone());

    // Resolve the context window once for the whole turn (one metadata read),
    // then thread it through first-call preflight, synthesis preflight, and
    // post-turn compaction so the window cannot change mid-turn.
    let ctx = agent_model_context_length(&gw.sqlSecret, &agent_endpoint).await;
    let tools_option = if routes.is_empty() {
        None
    } else {
        Some(build_tools_from_routes(&routes))
    };

    // First-call preflight. There is no same-turn provider anchor, so
    // estimate the whole request (including tool schemas) and use the more
    // conservative first-call threshold. Keep an explicit output cap because the
    // whole-request estimator has no provider anchor to bound undercount.
    let first_call_max_tokens = if ctx > 0 {
        (ctx / 2).max(512) as i32
    } else {
        20000
    };
    let first_call_history = gw
        .sessions
        .GetSession(&sessionId)
        .await
        .map(|s| s.model_messages)
        .unwrap_or_default();
    let mut requestBody = ModelRequest {
        model: None,
        messages: build_first_call_messages(&first_call_history),
        stream: true,
        max_tokens: Some(first_call_max_tokens),
        temperature: Some(0.0),
        tools: tools_option,
        enable_thinking: Some(false),
        // Guarantee a usage chunk so the synthesis preflight has a real
        // provider anchor instead of falling back to prompt_tokens == 0.
        stream_options: Some(StreamOptions {
            include_usage: true,
        }),
    };

    let first_call_ratio = agent_first_call_compaction_ratio();
    let fc_estimate = estimate_serialized_tokens(&requestBody);
    let fc_should_compact = preflight_decision(fc_estimate, ctx, first_call_ratio);
    ctrace!(
        AGENT,
        "first-call preflight session={} estimate={} threshold={}% ctx={} compact={}",
        sessionId, fc_estimate, first_call_ratio, ctx, fc_should_compact
    );
    if fc_should_compact {
        compact_session(&gw, &sessionId, &endpoint_str, &api_key_str).await;
        let rebuilt = gw
            .sessions
            .GetSession(&sessionId)
            .await
            .map(|s| s.model_messages)
            .unwrap_or_default();
        requestBody.messages = build_first_call_messages(&rebuilt);
        ctrace!(
            AGENT,
            "first-call rebuilt after compaction session={} estimate={}",
            sessionId,
            estimate_serialized_tokens(&requestBody)
        );
    }

    ctrace!(
        AGENT,
        "sending {} messages to model",
        requestBody.messages.len()
    );
    ctrace!(
        AGENT,
        "model request messages={}",
        requestBody.messages.len()
    );

    let initial_response = match tokio::select! {
        _ = cancel_token.cancelled() => {
            gw.sessions.RemoveMessageById(&sessionId, &userMessageId).await;
            gw.sessions.clear_active_stream_cancel(&sessionId);
            return session_interrupted_response();
        }
        response = client
            .post(&endpoint_str)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", api_key_str))
            .json(&requestBody)
            .send() => response,
    } {
        Ok(r) => r,
        Err(e) => {
            error!("Model request failed before stream: {}", e);
            // No assistant reply will follow; roll back the user message so the
            // next turn's preflight does not count an unanswered prompt.
            gw.sessions
                .RemoveMessageById(&sessionId, &userMessageId)
                .await;
            gw.sessions.clear_active_stream_cancel(&sessionId);
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": "model_unavailable",
                    "message": format!("Failed to reach model: {}", e)
                })),
            )
                .into_response();
        }
    };

    if !initial_response.status().is_success() {
        let error_text = initial_response.text().await.unwrap_or_default();
        error!("Model API error before stream: {}", error_text);
        // No assistant reply will follow; roll back the user message so the
        // next turn's preflight does not count an unanswered prompt.
        gw.sessions
            .RemoveMessageById(&sessionId, &userMessageId)
            .await;
        gw.sessions.clear_active_stream_cancel(&sessionId);
        return (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "error": "model_error",
                "message": classify_endpoint_dispatch_error(&error_text)
                    .unwrap_or_else(|| format!("Model error: {}", error_text))
            })),
        )
            .into_response();
    }

    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(100);

    let gwClone = gw.clone();
    let sessionIdClone = sessionId.to_string();
    let gw_base = format!("http://127.0.0.1:{}", GATEWAY_CONFIG.gatewayPort);

    tokio::spawn(async move {
        defer!(gwClone.sessions.clear_active_stream_cancel(&sessionIdClone));
        // Hold the per-session critical-section guard for the whole turn, so the
        // synthesis preflight -> compact -> rebuild -> send sequence below is
        // serialized against any concurrent prompt for the same session.
        let _session_guard = session_guard;
        ctrace!(AGENT, "stream started session={}", sessionIdClone);
        let mut stream = initial_response.bytes_stream();
        let mut full_content = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut stream_error: Option<String> = None;
        let mut prompt_tokens: u64 = 0;
        let mut finish_reason: Option<String> = None;
        let mut interrupted = false;

        loop {
            let next_chunk = tokio::select! {
                _ = cancel_token.cancelled() => {
                    interrupted = true;
                    None
                }
                chunk = stream.next() => chunk,
            };
            let Some(result) = next_chunk else {
                break;
            };
            match result {
                Ok(chunk) => {
                    let chunk_str = String::from_utf8_lossy(chunk.as_ref());

                    for line in chunk_str.lines() {
                        if !line.is_empty() {
                            ctrace!(AGENT, "model stream line: {}", line);
                        }
                        if line.starts_with("data: ") {
                            let data = &line[6..];

                            if data == "[DONE]" {
                                break;
                            }

                            if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                                if let Some(usage) = json.get("usage") {
                                    let p = usage
                                        .get("prompt_tokens")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(0);
                                    let c = usage
                                        .get("completion_tokens")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(0);
                                    ctrace!(AGENT, "Token usage: prompt={}, completion={}", p, c);
                                    if p > 0 {
                                        prompt_tokens = p;
                                    }
                                    ctrace!(
                                        AGENT,
                                        "token usage session={} prompt={} completion={}",
                                        sessionIdClone, p, c
                                    );
                                    continue;
                                }

                                if let Some(choice) = json
                                    .get("choices")
                                    .and_then(|c| c.as_array())
                                    .and_then(|a| a.first())
                                {
                                    if let Some(delta) = choice
                                        .get("delta")
                                        .and_then(|d| d.get("content"))
                                        .and_then(|c| c.as_str())
                                    {
                                        full_content.push_str(delta);
                                        let _ = tx
                                            .send(Ok(Event::default().data(delta.to_string())))
                                            .await;
                                    }

                                    if let Some(tc_array) = choice
                                        .get("delta")
                                        .and_then(|d| d.get("tool_calls"))
                                        .and_then(|a| a.as_array())
                                    {
                                        for tc in tc_array {
                                            if let Some(tc_obj) = tc.as_object() {
                                                // Use the streamed index to route chunks
                                                // to the correct tool call slot.
                                                let idx = tc_obj
                                                    .get("index")
                                                    .and_then(|v| v.as_u64())
                                                    .unwrap_or(0)
                                                    as usize;

                                                let id = tc_obj
                                                    .get("id")
                                                    .and_then(|v| v.as_str())
                                                    .unwrap_or("")
                                                    .to_string();
                                                let tc_type = tc_obj
                                                    .get("type")
                                                    .and_then(|v| v.as_str())
                                                    .unwrap_or("function")
                                                    .to_string();

                                                // Grow vec to cover idx, filling gaps
                                                if tool_calls.len() <= idx {
                                                    tool_calls.resize_with(idx + 1, || ToolCall {
                                                        id: String::new(),
                                                        r#type: "function".to_string(),
                                                        function: FunctionCall {
                                                            name: String::new(),
                                                            arguments: String::new(),
                                                        },
                                                    });
                                                }
                                                if !id.is_empty() && tool_calls[idx].id.is_empty() {
                                                    tool_calls[idx].id = id.clone();
                                                    tool_calls[idx].r#type = tc_type.clone();
                                                }

                                                if let Some(slot) = tool_calls.get_mut(idx) {
                                                    if let Some(function) = tc_obj
                                                        .get("function")
                                                        .and_then(|f| f.as_object())
                                                    {
                                                        if let Some(name) = function
                                                            .get("name")
                                                            .and_then(|v| v.as_str())
                                                        {
                                                            if slot.function.name.is_empty() {
                                                                slot.function.name =
                                                                    name.to_string();
                                                            }
                                                        }
                                                        if let Some(args) = function
                                                            .get("arguments")
                                                            .and_then(|v| v.as_str())
                                                        {
                                                            slot.function.arguments.push_str(args);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    if let Some(fr) =
                                        choice.get("finish_reason").and_then(|v| v.as_str())
                                    {
                                        finish_reason = Some(fr.to_string());
                                        ctrace!(
                                            AGENT,
                                            "finish_reason={} tool_calls={}",
                                            fr,
                                            tool_calls.len()
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Stream error: {}", e);
                    stream_error = Some(format!("{}", e));
                    break;
                }
            }
        }

        if interrupted {
            ctrace!(AGENT, "stream interrupted session={}", sessionIdClone);
            finalize_interrupted_prompt(
                &gwClone,
                &sessionIdClone,
                &userMessageId,
                &full_content,
                &tool_calls,
            )
            .await;
            emit_interrupted_event(&tx).await;
            return;
        }

        if let Some(err) = stream_error {
            // Roll back the user message only if the stream failed before
            // delivering any assistant content or tool calls — a true
            // no-reply turn, equivalent to the pre-stream failures. If any
            // content was already streamed, this is a truncated reply, not
            // a no-reply turn, so the user message is kept.
            if full_content.is_empty() && tool_calls.is_empty() {
                gwClone
                    .sessions
                    .RemoveMessageById(&sessionIdClone, &userMessageId)
                    .await;
            }
            let _ = tx
                .send(Ok(Event::default().event("error").data(
                    serde_json::json!({
                        "error": "stream_error",
                        "message": err
                    })
                    .to_string(),
                )))
                .await;
            let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
            return;
        }

        if !tool_calls.is_empty() || finish_reason.as_deref() == Some("tool_calls") {
            ctrace!(
                AGENT,
                "model requested {} tool call(s) session={}",
                tool_calls.len(),
                sessionIdClone
            );

            if cancel_token.is_cancelled() {
                finalize_interrupted_prompt(
                    &gwClone,
                    &sessionIdClone,
                    &userMessageId,
                    &full_content,
                    &tool_calls,
                )
                .await;
                emit_interrupted_event(&tx).await;
                return;
            }

            // Emit tool_call events for all tools first so the browser shows all spinners.
            // Use index as the correlation key so duplicate tool names in one turn work.
            for (idx, tool_call) in tool_calls.iter().enumerate() {
                let _ = tx
                    .send(Ok(Event::default().event("tool_call").data(
                        serde_json::json!({
                            "index": idx,
                            "name": tool_call.function.name,
                            "args": tool_call.function.arguments
                        })
                        .to_string(),
                    )))
                    .await;
                ctrace!(
                    AGENT,
                    "dispatching tool='{}' idx={} session={}",
                    tool_call.function.name, idx, sessionIdClone
                );
            }

            // Dispatch all skill calls concurrently; emit tool_done as each one finishes
            // so the browser updates immediately rather than waiting for the slowest skill.
            // Assumption: skills are stateless and have no ordering dependencies between
            // calls in the same turn. The sort below restores model-protocol message order
            // for synthesis. If ordered execution is ever needed, revert to a for loop.
            let mut skill_futures: futures::stream::FuturesUnordered<_> = tool_calls
                .iter()
                .enumerate()
                .map(|(idx, tool_call)| {
                    let gw_base = gw_base.clone();
                    let api_key_str = api_key_str.clone();
                    let routes = routes.clone();
                    let cancel_token = cancel_token.clone();
                    async move {
                        let tool_response = if let Some(route) = routes
                            .iter()
                            .find(|r| r.tool_name == tool_call.function.name)
                        {
                            let url = skill_endpoint(&gw_base, route);
                            CallSkillTool(
                                &url,
                                &api_key_str,
                                &tool_call.function.arguments,
                                &cancel_token,
                            )
                            .await
                        } else {
                            format!("Unknown tool: {}", tool_call.function.name)
                        };
                        (
                            idx,
                            tool_call.id.clone(),
                            tool_call.function.name.clone(),
                            tool_response,
                        )
                    }
                })
                .collect();

            // Collect into a vec sorted by index so synthesis messages are in model order.
            let mut tool_results: Vec<(String, String)> = Vec::new();
            let mut completed: Vec<(usize, String, String, String)> = Vec::new();
            loop {
                let next_tool = tokio::select! {
                    _ = cancel_token.cancelled() => None,
                    next = futures::StreamExt::next(&mut skill_futures) => next,
                };
                let Some((idx, tc_id, name, tool_response)) = next_tool else {
                    if cancel_token.is_cancelled() {
                        finalize_interrupted_prompt(
                            &gwClone,
                            &sessionIdClone,
                            &userMessageId,
                            &full_content,
                            &tool_calls,
                        )
                        .await;
                        emit_interrupted_event(&tx).await;
                        return;
                    }
                    break;
                };
                ctrace!(
                    AGENT,
                    "tool='{}' idx={} response_len={} session={}",
                    name,
                    idx,
                    tool_response.len(),
                    sessionIdClone
                );
                let _ = tx
                    .send(Ok(Event::default().event("tool_done").data(
                        serde_json::json!({
                            "index": idx,
                            "name": name,
                            "response": tool_response
                        })
                        .to_string(),
                    )))
                    .await;
                completed.push((idx, tc_id, name, tool_response));
            }
            // Restore model order for the synthesis message sequence.
            completed.sort_by_key(|(idx, _, _, _)| *idx);
            for (_, tc_id, _, tool_response) in completed {
                tool_results.push((tc_id, tool_response));
            }

            ctrace!(
                AGENT,
                "all tools done, sending synthesis request session={}",
                sessionIdClone
            );
            let client = Client::new();

            // Anchored synthesis preflight. Anchor on the
            // provider-reported prompt_tokens from the immediately
            // preceding model call and add only the estimated
            // synthesis-only delta (assistant tool_calls wrapper + tool
            // results). tools:None on synthesis is treated as conservative
            // headroom; removed tool-schema bytes are never subtracted.
            let synthesis_suffix = build_synthesis_suffix(&tool_calls, &tool_results);
            let added_estimate = estimate_serialized_tokens(&synthesis_suffix);
            let syn_ratio = agent_preflight_compaction_ratio();

            let mut prior_messages = gwClone
                .sessions
                .GetSession(&sessionIdClone)
                .await
                .map(|s| s.model_messages)
                .unwrap_or_default();

            // The anchored estimate is only trustworthy when the preceding
            // model call actually reported usage. If prompt_tokens is 0
            // (backend omitted usage / did not honor include_usage), the
            // anchored value collapses to just the suffix delta and would
            // skip compaction on a large history. Fall back to estimating
            // the whole rebuilt request locally so the preflight stays safe
            // regardless of backend usage behavior.
            let syn_estimate = if prompt_tokens > 0 {
                estimate_synthesis_tokens(prompt_tokens, &synthesis_suffix)
            } else {
                let candidate = build_synthesis_messages(&prior_messages, &synthesis_suffix);
                estimate_serialized_tokens(&candidate)
            };
            let syn_should_compact = preflight_decision(syn_estimate, ctx, syn_ratio);
            ctrace!(
                        AGENT,
                        "synthesis preflight session={} anchor={} added={} estimate={} anchored={} threshold={}% ctx={} compact={}",
                        sessionIdClone, prompt_tokens, added_estimate, syn_estimate, prompt_tokens > 0, syn_ratio, ctx, syn_should_compact
                    );

            if syn_should_compact {
                // Compact stored history before sending, then
                // rebuild the synthesis request from the compacted model
                // working context plus the re-appended current-turn suffix.
                let _ = tx
                    .send(Ok(Event::default().event("compacting").data("")))
                    .await;
                compact_session(&gwClone, &sessionIdClone, &endpoint_str, &api_key_str).await;
                let _ = tx
                    .send(Ok(Event::default().event("compacted").data("")))
                    .await;
                prior_messages = gwClone
                    .sessions
                    .GetSession(&sessionIdClone)
                    .await
                    .map(|s| s.model_messages)
                    .unwrap_or_default();
                // The provider anchor is stale after compaction, so
                // re-estimate the whole rebuilt request locally.
                let rebuilt = build_synthesis_messages(&prior_messages, &synthesis_suffix);
                let post_estimate = estimate_serialized_tokens(&rebuilt);
                ctrace!(
                            AGENT,
                            "synthesis re-estimate after compaction session={} estimate={} threshold={}%",
                            sessionIdClone, post_estimate, syn_ratio
                        );
                if preflight_decision(post_estimate, ctx, syn_ratio) {
                    // History compaction was not enough — the
                    // current-turn tool results themselves still exceed the
                    // usable context window. Fail explicitly instead of
                    // sending an oversized request. Oversized tool results
                    // remain the limiting factor here.
                    error!(
                                "[agent] oversized tool results remain after compaction session={} estimate={} ctx={}",
                                sessionIdClone, post_estimate, ctx
                            );
                    let _ = tx.send(Ok(Event::default().event("error").data(
                                serde_json::json!({
                                    "error": "tool_results_too_large",
                                    "message": "Current-turn tool results still exceed the usable context window after history compaction"
                                }).to_string()
                            ))).await;
                    let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
                    return;
                }
            }

            // Anchored preflight replaces the stale
            // ctx.saturating_sub(prompt_tokens) budget; max_tokens omitted.
            let final_request = ModelRequest {
                model: None,
                messages: build_synthesis_messages(&prior_messages, &synthesis_suffix),
                stream: true,
                max_tokens: None,
                temperature: Some(0.0),
                tools: None,
                enable_thinking: Some(false),
                stream_options: Some(StreamOptions {
                    include_usage: true,
                }),
            };

            ctrace!(
                AGENT,
                "synthesis request messages={}",
                final_request.messages.len()
            );

            match tokio::select! {
                _ = cancel_token.cancelled() => {
                    finalize_interrupted_prompt(
                        &gwClone,
                        &sessionIdClone,
                        &userMessageId,
                        &full_content,
                        &tool_calls,
                    )
                    .await;
                    emit_interrupted_event(&tx).await;
                    return;
                }
                response = client
                    .post(&endpoint_str)
                    .header("Content-Type", "application/json")
                    .header("Authorization", format!("Bearer {}", api_key_str))
                    .json(&final_request)
                    .send() => response,
            } {
                Ok(final_response) => {
                    if final_response.status().is_success() {
                        match consume_content_stream(final_response, &cancel_token, &tx).await {
                            ContentStreamOutcome::Completed(state) => {
                                let final_content = state.content;
                                if state.prompt_tokens > 0 {
                                    prompt_tokens = state.prompt_tokens;
                                }
                                if !final_content.is_empty() {
                                    // Strip <think>…</think> before storing in session history.
                                    let stored_content = strip_think_tags(&final_content);
                                    let finalMessageId = Uuid::new_v4().to_string();
                                    let finalMessage = Message {
                                        id: finalMessageId,
                                        role: "assistant".to_string(),
                                        content: stored_content.clone(),
                                        createdAt: Utc::now(),
                                        parts: vec![Part::Text {
                                            text: stored_content,
                                        }],
                                    };
                                    gwClone
                                        .sessions
                                        .AddMessage(&sessionIdClone, finalMessage)
                                        .await;

                                    if postturn_compaction_threshold(ctx)
                                        .map_or(false, |t| prompt_tokens >= t)
                                    {
                                        ctrace!(AGENT, "Session {} at {} prompt tokens, compacting after tool synthesis", sessionIdClone, prompt_tokens);
                                        let _ = tx
                                            .send(Ok(Event::default().event("compacting").data("")))
                                            .await;
                                        compact_session(
                                            &gwClone,
                                            &sessionIdClone,
                                            &endpoint_str,
                                            &api_key_str,
                                        )
                                        .await;
                                        let _ = tx
                                            .send(Ok(Event::default().event("compacted").data("")))
                                            .await;
                                    }
                                } else {
                                    // Synthesis stream finished cleanly but emitted
                                    // no content. No assistant message is stored;
                                    // surface an explicit error instead of ending
                                    // silently with [DONE]. No user-message
                                    // rollback: tools already executed this turn.
                                    error!(
                                                "[agent] empty synthesis response session={} (no content after tools)",
                                                sessionIdClone
                                            );
                                    let _ = tx
                                        .send(Ok(Event::default().event("error").data(
                                            serde_json::json!({
                                                "error": "empty_response",
                                                "message": "Model returned no content"
                                            })
                                            .to_string(),
                                        )))
                                        .await;
                                }
                            }
                            ContentStreamOutcome::Interrupted(state) => {
                                finalize_interrupted_prompt(
                                    &gwClone,
                                    &sessionIdClone,
                                    &userMessageId,
                                    &state.content,
                                    &tool_calls,
                                )
                                .await;
                                emit_interrupted_event(&tx).await;
                                return;
                            }
                            ContentStreamOutcome::StreamError { state, error: err } => {
                                // No user-message rollback: by synthesis the
                                // turn has already executed tool calls and may
                                // have streamed content; it is a real
                                // multi-step turn, not a no-reply prompt.
                                // Keep any usage seen before the transport
                                // failed: the turn still falls through to the
                                // common post-turn footer, where a stale
                                // last_prompt_tokens would skew the next turn's
                                // anchored preflight.
                                if state.prompt_tokens > 0 {
                                    prompt_tokens = state.prompt_tokens;
                                }
                                error!("Synthesis stream error: {}", err);
                                let _ = tx
                                    .send(Ok(Event::default().event("error").data(
                                        serde_json::json!({
                                            "error": "stream_error",
                                            "message": format!("Skill synthesis call failed: {}", err)
                                        })
                                        .to_string(),
                                    )))
                                    .await;
                            }
                        }
                    } else {
                        let status = final_response.status();
                        let body = final_response.text().await.unwrap_or_default();
                        if status.as_u16() == 400 && is_context_overflow(&body) {
                            let _ = tx
                                .send(Ok(Event::default().event("compacting").data("")))
                                .await;
                            let compaction_result = compact_session(
                                &gwClone,
                                &sessionIdClone,
                                &endpoint_str,
                                &api_key_str,
                            )
                            .await;
                            let _ = tx
                                .send(Ok(Event::default().event("compacted").data("")))
                                .await;
                            match compaction_result {
                                CompactionResult::Failed => {
                                    let _ = tx.send(Ok(Event::default().event("error").data(
                                                serde_json::json!({
                                                    "error": "upstream_error",
                                                    "message": "Synthesis failed and history compaction also failed"
                                                }).to_string()
                                            ))).await;
                                }
                                CompactionResult::NoOp => {
                                    let _ = tx.send(Ok(Event::default().event("error").data(
                                                serde_json::json!({
                                                    "error": "upstream_error",
                                                    "message": "Request exceeds context window; history is too short to compact further"
                                                }).to_string()
                                            ))).await;
                                }
                                CompactionResult::NoChange => {
                                    let _ = tx.send(Ok(Event::default().event("error").data(
                                                serde_json::json!({
                                                    "error": "upstream_error",
                                                    "message": "Request exceeds context window; compaction returned no usable summary"
                                                }).to_string()
                                            ))).await;
                                }
                                CompactionResult::Compacted => {
                                    match run_synthesis_retry(
                                        &gwClone,
                                        &sessionIdClone,
                                        &userMessageId,
                                        &endpoint_str,
                                        &api_key_str,
                                        &tool_calls,
                                        &tool_results,
                                        &full_content,
                                        ctx,
                                        prompt_tokens,
                                        &cancel_token,
                                        &tx,
                                    )
                                    .await
                                    {
                                        SynthesisRetryOutcome::Interrupted => return,
                                        SynthesisRetryOutcome::Done { prompt_tokens: pt } => {
                                            prompt_tokens = pt;
                                        }
                                    }
                                }
                            }
                        } else {
                            error!("Final model call error status: {}", status);
                            let _ = tx
                                .send(Ok(Event::default().event("error").data(
                                    serde_json::json!({
                                        "error": "upstream_error",
                                        "message": "Skill synthesis call failed"
                                    })
                                    .to_string(),
                                )))
                                .await;
                        }
                    }
                }
                Err(e) => {
                    // No user-message rollback: tool calls already ran
                    // this turn, so it is a real multi-step turn whose
                    // synthesis send failed, not a no-reply prompt.
                    error!("Final model call error: {}", e);
                    let _ = tx
                        .send(Ok(Event::default().event("error").data(
                            serde_json::json!({
                                "error": "upstream_error",
                                "message": format!("Skill synthesis call failed: {}", e)
                            })
                            .to_string(),
                        )))
                        .await;
                }
            }
        } else if !full_content.is_empty() && finish_reason.as_deref() != Some("tool_calls") {
            let assistantMessageId = Uuid::new_v4().to_string();
            let parts = vec![Part::Text {
                text: full_content.clone(),
            }];

            let assistantMessage = Message {
                id: assistantMessageId.clone(),
                role: "assistant".to_string(),
                content: full_content.clone(),
                createdAt: Utc::now(),
                parts: parts.clone(),
            };

            gwClone
                .sessions
                .AddMessage(&sessionIdClone, assistantMessage)
                .await;
            ctrace!(
                AGENT,
                "assistant message stored len={} session={}",
                full_content.len(),
                sessionIdClone
            );

            let _ = tx
                .send(Ok(Event::default().event("complete").data(format!(
                    "{{\"message_id\": \"{}\"}}",
                    assistantMessageId
                ))))
                .await;
        } else {
            // First call completed cleanly (no stream error) but produced
            // neither tool calls nor assistant content. No assistant
            // message is stored, so roll back the user message to avoid an
            // unanswered prompt in history, and surface an explicit error
            // rather than ending the turn silently.
            error!(
                "[agent] empty model response session={} (no content, no tool calls)",
                sessionIdClone
            );
            gwClone
                .sessions
                .RemoveMessageById(&sessionIdClone, &userMessageId)
                .await;
            let _ = tx
                .send(Ok(Event::default().event("error").data(
                    serde_json::json!({
                        "error": "empty_response",
                        "message": "Model returned no content"
                    })
                    .to_string(),
                )))
                .await;
        }

        ctrace!(AGENT, "stream done session={}", sessionIdClone);
        let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;

        // Update stored token count and compact if over threshold
        if prompt_tokens > 0 {
            gwClone
                .sessions
                .UpdateTokens(&sessionIdClone, prompt_tokens)
                .await;
            if postturn_compaction_threshold(ctx).map_or(false, |t| prompt_tokens >= t) {
                ctrace!(
                    AGENT,
                    "Session {} at {} prompt tokens, compacting",
                    sessionIdClone, prompt_tokens
                );
                let _ = tx
                    .send(Ok(Event::default().event("compacting").data("")))
                    .await;
                compact_session(&gwClone, &sessionIdClone, &endpoint_str, &api_key_str).await;
                let _ = tx
                    .send(Ok(Event::default().event("compacted").data("")))
                    .await;
            }
        }
    });

    Sse::new(ReceiverStream::new(rx))
        .keep_alive(axum::response::sse::KeepAlive::new())
        .into_response()
}

// ============================================================================
// Session Route Handlers
// ============================================================================

pub async fn GetCurrentSession(
    State(gw): State<HttpGateway>,
    axum::extract::Extension(token): axum::extract::Extension<
        std::sync::Arc<crate::gateway::auth_layer::AccessToken>,
    >,
) -> impl IntoResponse {
    let tenant = caller_tenant(&token);
    let mut sessions = gw.sessions.ListSessionsForTenant(&tenant).await;
    if sessions.is_empty() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "no_session" })),
        )
            .into_response();
    }
    sessions.sort_by(|a, b| b.updatedAt.cmp(&a.updatedAt));
    let session = sessions.into_iter().next().unwrap();
    let tool_count = gw
        .sqlSecret
        .GetToolsForTenant(&tenant)
        .await
        .map(|t| t.len())
        .unwrap_or(0);
    Json(SessionResponse {
        session,
        tool_count,
    })
    .into_response()
}

pub async fn CreateSession(
    State(gw): State<HttpGateway>,
    axum::extract::Extension(token): axum::extract::Extension<
        std::sync::Arc<crate::gateway::auth_layer::AccessToken>,
    >,
    Json(req): Json<CreateSessionRequest>,
) -> impl IntoResponse {
    let tenant = token
        .restrictTenant
        .clone()
        .or_else(|| token.defaultTenant.clone())
        .unwrap_or_default();

    if tenant.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "no_tenant",
                "message": "No tenant context: set a default tenant or use a tenant-restricted API key"
            })),
        )
            .into_response();
    }

    // Validate a selected agent endpoint against the authoritative published
    // check (function status), not Endpoints row presence, so failures surface
    // here at session creation rather than on the first model call. Store only
    // the bare slug on the session; the context length is looked up live.
    let agent_endpoint = match req.agent_endpoint.as_deref() {
        Some(raw) if !raw.trim().is_empty() => {
            let normalized = raw.trim().trim_start_matches('/');
            let normalized = normalized
                .strip_prefix("endpoints/")
                .unwrap_or(normalized)
                .to_string();
            if let Err(msg) = crate::gateway::http_gateway::validate_agent_endpoint_published(
                &gw, &tenant, &normalized,
            ) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "error": "invalid_agent_endpoint",
                        "message": format!(
                            "Selected model '{}' is not available: {}",
                            normalized, msg
                        )
                    })),
                )
                    .into_response();
            }
            normalized
        }
        // The slug is mandatory: the dashboard owns the default model and always
        // sends one. There is no gateway-side default model to fall back to, so a
        // slug-less request is rejected rather than silently routed somewhere.
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "no_agent_model",
                    "message": "select a model"
                })),
            )
                .into_response();
        }
    };

    let session = gw
        .sessions
        .CreateSession(tenant.clone(), req.api_key, agent_endpoint)
        .await;
    let tool_count = gw
        .sqlSecret
        .GetToolsForTenant(&tenant)
        .await
        .map(|t| t.len())
        .unwrap_or(0);

    (
        StatusCode::CREATED,
        Json(SessionResponse {
            session,
            tool_count,
        }),
    )
        .into_response()
}

pub async fn GetSession(
    State(gw): State<HttpGateway>,
    axum::extract::Extension(token): axum::extract::Extension<
        std::sync::Arc<crate::gateway::auth_layer::AccessToken>,
    >,
    Path(sessionId): Path<String>,
) -> impl IntoResponse {
    let tenant = caller_tenant(&token);
    match gw.sessions.GetSession(&sessionId).await {
        Some(session) if session.tenant == tenant => {
            let tool_count = gw
                .sqlSecret
                .GetToolsForTenant(&tenant)
                .await
                .map(|t| t.len())
                .unwrap_or(0);
            Json(SessionResponse {
                session,
                tool_count,
            })
            .into_response()
        }
        Some(_) => session_forbidden_response(),
        None => (StatusCode::NOT_FOUND, "Session not found").into_response(),
    }
}

pub async fn DeleteSession(
    State(gw): State<HttpGateway>,
    axum::extract::Extension(token): axum::extract::Extension<
        std::sync::Arc<crate::gateway::auth_layer::AccessToken>,
    >,
    Path(sessionId): Path<String>,
) -> impl IntoResponse {
    let tenant = caller_tenant(&token);
    match gw.sessions.GetSession(&sessionId).await {
        Some(session) if session.tenant == tenant => {
            gw.sessions.DeleteSession(&sessionId).await;
            StatusCode::NO_CONTENT.into_response()
        }
        // Per the design contract, a session not owned by the caller is treated
        // as not found (404) — same as a missing id — rather than 403, so the
        // delete endpoint does not reveal that another tenant's session exists.
        _ => (StatusCode::NOT_FOUND, "Session not found").into_response(),
    }
}

pub async fn GetSessionMessages(
    State(gw): State<HttpGateway>,
    axum::extract::Extension(token): axum::extract::Extension<
        std::sync::Arc<crate::gateway::auth_layer::AccessToken>,
    >,
    Path(sessionId): Path<String>,
    Query(query): Query<MessageListQuery>,
) -> impl IntoResponse {
    let tenant = caller_tenant(&token);
    match gw.sessions.GetSession(&sessionId).await {
        Some(session) if session.tenant == tenant => {
            let messages = gw.sessions.GetMessages(&sessionId, query.limit).await;
            Json(messages).into_response()
        }
        Some(_) => session_forbidden_response(),
        None => (StatusCode::NOT_FOUND, "Session not found").into_response(),
    }
}

pub async fn Prompt(
    State(gw): State<HttpGateway>,
    axum::extract::Extension(token): axum::extract::Extension<
        std::sync::Arc<crate::gateway::auth_layer::AccessToken>,
    >,
    Path(sessionId): Path<String>,
    Json(req): Json<PromptRequest>,
) -> impl IntoResponse {
    let tenant = caller_tenant(&token);
    match gw.sessions.GetSession(&sessionId).await {
        Some(session) if session.tenant != tenant => return session_forbidden_response(),
        None => return session_not_found_response(),
        _ => {}
    }
    HandlePrompt(State(gw), Path(sessionId), Json(req))
        .await
        .into_response()
}

pub async fn PromptStream(
    State(gw): State<HttpGateway>,
    axum::extract::Extension(token): axum::extract::Extension<
        std::sync::Arc<crate::gateway::auth_layer::AccessToken>,
    >,
    Path(sessionId): Path<String>,
    Json(req): Json<PromptRequest>,
) -> impl IntoResponse {
    let tenant = caller_tenant(&token);
    match gw.sessions.GetSession(&sessionId).await {
        Some(session) if session.tenant != tenant => return session_forbidden_response(),
        None => return session_not_found_response(),
        _ => {}
    }
    HandlePromptStream(State(gw), Path(sessionId), Json(req))
        .await
        .into_response()
}

pub async fn InterruptSession(
    State(gw): State<HttpGateway>,
    axum::extract::Extension(token): axum::extract::Extension<
        std::sync::Arc<crate::gateway::auth_layer::AccessToken>,
    >,
    Path(sessionId): Path<String>,
) -> impl IntoResponse {
    let tenant = caller_tenant(&token);
    match gw.sessions.GetSession(&sessionId).await {
        Some(session) if session.tenant != tenant => return session_forbidden_response(),
        None => return session_not_found_response(),
        _ => {}
    }

    let interrupted = gw.sessions.interrupt_active_stream(&sessionId);
    (
        StatusCode::OK,
        Json(InterruptSessionResponse { interrupted }),
    )
        .into_response()
}

// ============================================================================
// Tests: pure preflight/estimator logic
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> Message {
        Message {
            id: "test".to_string(),
            role: role.to_string(),
            content: content.to_string(),
            createdAt: Utc::now(),
            parts: vec![],
        }
    }

    #[test]
    fn preflight_decision_disabled_when_ctx_zero() {
        // ctx == 0 means compaction is disabled regardless of estimate.
        assert!(!preflight_decision(u64::MAX, 0, 90));
    }

    #[test]
    fn preflight_decision_threshold_crossing() {
        // 90% of 1000 = 900. Below stays false, at/above flips true.
        assert!(!preflight_decision(899, 1000, 90));
        assert!(preflight_decision(900, 1000, 90));
        assert!(preflight_decision(901, 1000, 90));
    }

    #[test]
    fn estimate_serialized_tokens_uses_bytes_over_four() {
        // A bare JSON string serializes to "x" plus the two quote bytes = 3 bytes.
        let s = "x".to_string();
        assert_eq!(estimate_serialized_tokens(&s), (3 + 3) / 4);
    }

    #[test]
    fn estimate_synthesis_tokens_adds_delta_to_anchor() {
        let suffix = vec![msg("tool", "result")];
        let added = estimate_serialized_tokens(&suffix);
        assert_eq!(estimate_synthesis_tokens(1000, &suffix), 1000 + added);
        // The anchor is a floor: estimate is always >= the provider anchor.
        assert!(estimate_synthesis_tokens(1000, &suffix) >= 1000);
    }

    #[test]
    fn first_call_preflight_threshold_crossing() {
        // A first-call whole-request estimate over the model context window
        // should trip the conservative first-call ratio.
        let history: Vec<Message> = (0..50)
            .map(|i| msg("user", &"lorem ipsum dolor sit amet ".repeat(20 + i)))
            .collect();
        let req = ModelRequest {
            model: None,
            messages: build_first_call_messages(&history),
            stream: true,
            max_tokens: Some(512),
            temperature: Some(0.0),
            tools: None,
            enable_thinking: Some(false),
            stream_options: None,
        };
        let estimate = estimate_serialized_tokens(&req);
        // Tiny window: the estimate should cross 85%.
        assert!(preflight_decision(estimate, 100, 85));
        // Huge window: well under threshold.
        assert!(!preflight_decision(estimate, 100_000_000, 85));
    }

    #[test]
    fn synthesis_anchored_threshold_crossing() {
        let tool_calls = vec![ToolCall {
            id: "tc1".to_string(),
            r#type: "function".to_string(),
            function: FunctionCall {
                name: "search".to_string(),
                arguments: "{\"query\":\"x\"}".to_string(),
            },
        }];
        let tool_results = vec![("tc1".to_string(), "a".repeat(4000))];
        let suffix = build_synthesis_suffix(&tool_calls, &tool_results);
        // Anchor already near a 1000-token window; the suffix pushes it over 90%.
        let estimate = estimate_synthesis_tokens(800, &suffix);
        assert!(estimate > 900);
        assert!(preflight_decision(estimate, 1000, 90));
    }

    #[test]
    fn still_too_large_after_compaction_decision() {
        // Even with an empty compacted history, the synthesis suffix alone can
        // exceed the window — the still-too-large hard-fail path.
        let tool_results = vec![("tc1".to_string(), "a".repeat(40_000))];
        let suffix = build_synthesis_suffix(&[], &tool_results);
        let rebuilt = build_synthesis_messages(&[], &suffix);
        let post_estimate = estimate_serialized_tokens(&rebuilt);
        assert!(preflight_decision(post_estimate, 1000, 90));
    }

    #[test]
    fn compaction_marker_never_serializes_a_provider_role() {
        // The compaction marker uses a role the provider does not accept, so it
        // is loud if accidentally forwarded.
        let part = Part::Compaction {
            summary: "s".to_string(),
        };
        let json = serde_json::to_string(&part).unwrap();
        assert!(json.contains("\"type\":\"compaction\""));
    }

    #[test]
    fn interrupted_turn_drops_pure_thinking_when_no_tools_started() {
        match interrupted_turn_storage_decision("<think>planning</think>", &[]) {
            InterruptedTurnStorageDecision::DropUserPrompt => {}
            _ => panic!("expected pure-thinking interrupted turn to be dropped"),
        }
    }

    #[test]
    fn interrupted_turn_keeps_visible_assistant_text_only() {
        match interrupted_turn_storage_decision(
            "<think>planning</think>Partial visible answer",
            &[],
        ) {
            InterruptedTurnStorageDecision::StoreAssistantText(text) => {
                assert_eq!(text, "Partial visible answer");
            }
            _ => panic!("expected interrupted visible output to be kept"),
        }
    }

    #[test]
    fn interrupted_turn_after_tools_without_visible_text_stores_marker() {
        let tool_calls = vec![ToolCall {
            id: "tc1".to_string(),
            r#type: "function".to_string(),
            function: FunctionCall {
                name: "search".to_string(),
                arguments: "{\"query\":\"x\"}".to_string(),
            },
        }];
        match interrupted_turn_storage_decision("<think>planning</think>", &tool_calls) {
            InterruptedTurnStorageDecision::StoreInterruptedMarker => {}
            _ => panic!("expected interrupted tool-started turn to store a marker"),
        }
    }
}
