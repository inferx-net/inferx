use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue};
use axum::response::Response;
use chrono::Utc;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use http_body_util::BodyExt;
use hyper::header::CONTENT_TYPE;
use hyper::StatusCode;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::func_agent_mgr::FuncRouteTarget;
use super::http_gateway::{dispatch_func_call, HttpGateway, GATEWAY_CONFIG};
use super::skill_trace_sse::{SkillTraceEventPayload, SseMessage, SseParser};
use crate::print::verbose_category;

pub(super) const SKILL_CHAIN_CHILD_HEADER: &str = "X-Inferx-Skill-Chain-Child";
pub(super) const SKILL_CHAIN_DEPTH_HEADER: &str = "X-Chain-Depth";
const SKILL_CHAIN_MAX_DEPTH: u32 = 5;
const SKILL_CHAIN_TOOL_NAME: &str = "call_skillep";
const SKILL_EP_CONSTRAINT: &str = "IMPORTANT: When using the call_skillep tool, you MUST only call skill endpoint IDs that are explicitly listed below. Do not invent or assume other skill endpoint IDs exist. If a skill endpoint is not listed below, do not call it.";
const CLIENT_PROVIDED_TOOLS_ERROR: &str = "skill endpoint does not accept client-provided tools";
const SKILL_TRACE_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

/// Returns true only when `X-Inferx-Skill-Chain-Child` is present with the exact value `"1"`.
/// Missing, malformed, or any other value is treated as a non-child (root) request.
pub(super) fn extract_is_child_request(headers: &HeaderMap) -> bool {
    headers
        .get(SKILL_CHAIN_CHILD_HEADER)
        .and_then(|v| v.to_str().ok())
        == Some("1")
}
#[derive(Clone, Debug)]
struct SkillChainRequestState {
    template: SkillModelRequestTemplate,
    headers: HeaderMap,
    current_depth: u32,
    debug_mocks: std::collections::HashMap<String, String>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "role", rename_all = "lowercase")]
enum ChatMessage {
    Assistant {
        content: Value,
        tool_calls: Vec<Value>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

#[derive(Clone, Debug)]
struct ParsedSkillToolCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Clone, Debug)]
struct ParsedSkillResponse {
    tool_calls: Vec<ParsedSkillToolCall>,
    final_text: Option<String>,
    usage: Option<SkillTraceTokenUsage>,
}

#[derive(Clone, Debug)]
struct ParallelChildTaskInput {
    original_index: usize,
    call: ParsedSkillToolCall,
}

#[derive(Clone, Debug)]
struct ParallelChildTaskContext {
    current_depth: u32,
    headers: HeaderMap,
    debug_mocks: Arc<HashMap<String, String>>,
    child_http_client: reqwest::Client,
    trace: Option<SkillChainTraceState>,
    direct_child_depth: u32,
    logical_tenant: String,
    logical_namespace: String,
    logical_funcname: String,
    allowed_skillep_ids: Option<Arc<HashSet<String>>>,
}

#[derive(Clone, Debug)]
struct ParallelChildResult {
    original_index: usize,
    tool_call_id: String,
    tool_result: String,
    fail_reason: Option<SkillTraceFailReason>,
    usage: Option<SkillTraceTokenUsage>,
}

#[derive(Clone, Debug)]
struct SkillChainTraceState {
    tx: mpsc::Sender<Result<String, Infallible>>,
    root_call_id: String,
    root_skill: String,
    started_at: Instant,
    include_content: bool,
}

#[derive(Clone, Debug)]
struct SkillChainChildTraceState {
    call_id: String,
    skill: String,
    started_at: Instant,
    local_root_call_id: Arc<Mutex<Option<String>>>,
}

#[derive(Debug)]
struct ChildTraceReadResult {
    skill_result: Option<Vec<u8>>,
    terminal_result_code: Option<String>,
    terminal_fail_reason: Option<SkillTraceFailReason>,
    terminal_usage: Option<SkillTraceTokenUsage>,
    open_descendants: Vec<ForwardedChildTraceNode>,
}

#[derive(Clone, Debug)]
struct ForwardedChildTraceNode {
    call_id: String,
    parent_call_id: Option<String>,
    depth: u32,
    skill: String,
}

#[derive(Debug)]
struct ChildTraceReadError {
    message: String,
    fail_reason: SkillTraceFailReason,
    open_descendants: Vec<ForwardedChildTraceNode>,
}

fn update_open_descendants(
    open_descendants: &mut HashMap<String, ForwardedChildTraceNode>,
    normalized_event_type: &str,
    direct_child_depth: u32,
    node: ForwardedChildTraceNode,
) {
    if node.depth <= direct_child_depth {
        return;
    }
    match normalized_event_type {
        "subcall_start" => {
            open_descendants.insert(node.call_id.clone(), node);
        }
        "subcall_finish" => {
            open_descendants.remove(&node.call_id);
        }
        _ => {}
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SkillTraceFailReason {
    Timeout,
    MaxDepth,
    TransportError,
    InvalidResponse,
}

impl SkillTraceFailReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::MaxDepth => "max_depth",
            Self::TransportError => "transport_error",
            Self::InvalidResponse => "invalid_response",
        }
    }
}

fn parse_fail_reason(reason: Option<&str>) -> Option<SkillTraceFailReason> {
    match reason {
        Some("timeout") => Some(SkillTraceFailReason::Timeout),
        Some("max_depth") => Some(SkillTraceFailReason::MaxDepth),
        Some("transport_error") => Some(SkillTraceFailReason::TransportError),
        Some("invalid_response") => Some(SkillTraceFailReason::InvalidResponse),
        Some(_) | None => None,
    }
}

fn format_child_fail_reason(reason: Option<SkillTraceFailReason>) -> String {
    match reason {
        Some(SkillTraceFailReason::Timeout) => "Error: child timed out".to_string(),
        Some(SkillTraceFailReason::MaxDepth) => "Error: child exceeded max chain depth".to_string(),
        Some(SkillTraceFailReason::TransportError) => "Error: child transport error".to_string(),
        Some(SkillTraceFailReason::InvalidResponse) => {
            "Error: child returned invalid response".to_string()
        }
        None => "Error: child failed".to_string(),
    }
}

struct SkillChainFinalResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
    is_openai_chat_completion: bool,
    usage: Option<SkillTraceTokenUsage>,
}

enum SkillChainExecutionError {
    DownstreamStatus(StatusCode),
    Response(StatusCode, &'static str),
}

// SkillTraceEventPayload is now shared via super::skill_trace_sse.

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct SkillTraceTokenUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
}

impl SkillTraceTokenUsage {
    fn add_assign(&mut self, other: &Self) {
        self.prompt_tokens = self.prompt_tokens.saturating_add(other.prompt_tokens);
        self.completion_tokens = self
            .completion_tokens
            .saturating_add(other.completion_tokens);
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
    }
}

// SseParser/SseMessage are now shared via super::skill_trace_sse.

/// Represents the `skill_trace` field in the wire request body.
///
/// Distinguishes absent (→ SkillTraceLevel::None) from present-but-null (→ 400).
/// Uses a custom deserializer so `#[serde(default)]` yields Absent for missing fields
/// while an explicit JSON value (including null) yields Present(v).
#[derive(Debug, Clone)]
pub(super) enum SkillTraceField {
    Absent,
    Present(Value),
}

impl Default for SkillTraceField {
    fn default() -> Self {
        SkillTraceField::Absent
    }
}

impl SkillTraceField {
    pub(super) fn is_absent(&self) -> bool {
        matches!(self, SkillTraceField::Absent)
    }
}

impl<'de> Deserialize<'de> for SkillTraceField {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(SkillTraceField::Present(Value::deserialize(d)?))
    }
}

impl Serialize for SkillTraceField {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            SkillTraceField::Absent => s.serialize_none(),
            SkillTraceField::Present(v) => v.serialize(s),
        }
    }
}

/// Effective trace verbosity level after normalization.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum SkillTraceLevel {
    None,
    Basic,
    Verbose,
}

impl SkillTraceLevel {
    fn as_u8(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Basic => 1,
            Self::Verbose => 2,
        }
    }

    fn into_field(self) -> SkillTraceField {
        SkillTraceField::Present(Value::Number(self.as_u8().into()))
    }
}

/// Response mode chosen from the effective trace level.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum SkillResponseMode {
    Normal,
    SseTrace,
}

/// Typed wire representation of the client JSON body for `/skills/.../v1/chat/completions`.
///
/// Trusted fields (tenant, route, debug authorization, chain depth) are never present here.
/// Unknown fields pass through via `extra` to preserve OpenAI-compatible parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct SkillChatCompletionsWireRequest {
    pub messages: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(default, skip_serializing_if = "SkillTraceField::is_absent")]
    pub skill_trace: SkillTraceField,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mocks: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

/// Gateway-derived trusted context for a skill invocation.
/// Built by `http_gateway.rs`; never sourced from the client body.
pub(super) struct SkillInvocationContext {
    pub route: FuncRouteTarget,
    pub calling_tenant: String,
    pub display_skill_id: String,
    pub logical_funcname: String,
    pub remain_path: String,
    pub prefix: String,
    pub skills_namespace: String,
    /// True when `X-Inferx-Skill-Chain-Child: 1` is present (exact match).
    pub is_child_request: bool,
    /// Parsed `X-Chain-Depth` value, only set when `is_child_request` is true.
    pub child_chain_depth: Option<u32>,
    pub is_debug_authorized: bool,
    pub allowed_child_skilleps: Option<Arc<HashSet<String>>>,
    pub cancel_token: CancellationToken,
    /// Original request headers forwarded to child calls (Authorization, X-Request-Id, traceparent).
    pub request_headers: HeaderMap,
}

/// Normalized, validated form used by the chain executor.
#[derive(Debug)]
struct NormalizedSkillCall {
    prepared_body: PreparedSkillChainBody,
    effective_trace: SkillTraceLevel,
    response_mode: SkillResponseMode,
    current_depth: u32,
    debug_mocks: HashMap<String, String>,
}

#[derive(Debug)]
struct PreparedSkillChainBody {
    template: SkillModelRequestTemplate,
    history: Vec<Value>,
}

#[derive(Clone, Debug, Serialize)]
struct SkillModelRequestTemplate {
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    skill_trace: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<Value>,
    #[serde(flatten)]
    extra: HashMap<String, Value>,
}

#[derive(Serialize)]
struct SkillModelTurnRequest<'a> {
    #[serde(flatten)]
    template: &'a SkillModelRequestTemplate,
    messages: &'a [Value],
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<RawUsage>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    #[serde(default)]
    finish_reason: Option<String>,
    #[serde(default)]
    message: Option<ChatMessagePayload>,
    #[serde(default)]
    delta: Option<ChatMessagePayload>,
}

#[derive(Debug, Deserialize)]
struct ChatMessagePayload {
    #[serde(default)]
    content: Option<Value>,
    #[serde(default)]
    tool_calls: Option<Vec<RawToolCall>>,
}

#[derive(Debug, Deserialize)]
struct RawToolCall {
    id: Option<String>,
    function: Option<RawToolFunction>,
}

#[derive(Debug, Deserialize)]
struct RawToolFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    total_tokens: Option<u64>,
}

impl RawUsage {
    fn into_usage(self) -> Option<SkillTraceTokenUsage> {
        let total_tokens = self
            .total_tokens
            .unwrap_or(self.prompt_tokens.saturating_add(self.completion_tokens));
        if self.prompt_tokens == 0 && self.completion_tokens == 0 && total_tokens == 0 {
            return None;
        }
        Some(SkillTraceTokenUsage {
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
            total_tokens,
        })
    }
}

/// Error returned from normalization.
#[derive(Debug)]
enum NormalizationError {
    /// Return HTTP status + message directly.
    Http(StatusCode, &'static str),
    /// Return 200 OK with a completion-shaped body (invalid child depth compatibility path).
    CompletionError,
}

/// Validates and normalizes a wire request + trusted context into the execution form.
///
/// This is the single place that:
/// - validates `skill_trace` (absent → None; 0/1/2 → typed level; null/invalid → 400)
/// - enforces root-only verbose-trace authorization
/// - rejects non-empty client-provided tools
/// - derives `current_depth` from trusted context
/// - prepares the execution template
fn normalize_skill_call(
    wire: SkillChatCompletionsWireRequest,
    ctx: &SkillInvocationContext,
) -> Result<NormalizedSkillCall, NormalizationError> {
    // Validate skill_trace and resolve to typed level.
    let effective_trace = match &wire.skill_trace {
        SkillTraceField::Absent => SkillTraceLevel::None,
        SkillTraceField::Present(Value::Number(n)) => match n.as_u64() {
            Some(0) => SkillTraceLevel::None,
            Some(1) => SkillTraceLevel::Basic,
            Some(2) => SkillTraceLevel::Verbose,
            _ => {
                return Err(NormalizationError::Http(
                    StatusCode::BAD_REQUEST,
                    "service failure: invalid skill_trace value",
                ))
            }
        },
        SkillTraceField::Present(_) => {
            return Err(NormalizationError::Http(
                StatusCode::BAD_REQUEST,
                "service failure: invalid skill_trace value",
            ));
        }
    };

    // Verbose trace requires debug privilege on the root request only.
    if effective_trace == SkillTraceLevel::Verbose
        && !ctx.is_child_request
        && !ctx.is_debug_authorized
    {
        return Err(NormalizationError::Http(
            StatusCode::FORBIDDEN,
            "service failure: verbose trace requires debug privilege",
        ));
    }

    // Derive current depth from trusted context.
    let current_depth = if ctx.is_child_request {
        let depth = ctx
            .child_chain_depth
            .ok_or(NormalizationError::CompletionError)?;
        if depth == 0 {
            return Err(NormalizationError::CompletionError);
        }
        depth
    } else {
        1
    };

    // Reject non-empty client-provided tools on root requests.
    if !ctx.is_child_request {
        match wire.tools.as_ref() {
            Some(Value::Array(arr)) if !arr.is_empty() => {
                return Err(NormalizationError::Http(
                    StatusCode::BAD_REQUEST,
                    CLIENT_PROVIDED_TOOLS_ERROR,
                ));
            }
            Some(Value::Array(_)) | None => {}
            Some(_) => {
                return Err(NormalizationError::Http(
                    StatusCode::BAD_REQUEST,
                    "service failure: invalid request body",
                ));
            }
        }
    }

    // Extract debug mocks (client-supplied, string values only).
    let debug_mocks: HashMap<String, String> = wire
        .mocks
        .as_ref()
        .and_then(|v| v.as_object())
        .map(|map| {
            map.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    let history = wire.messages.clone();

    // Build execution template from wire fields, then apply tool injection and force
    // stream=false. Messages stay outside the template and are attached per turn.
    let mut template = SkillModelRequestTemplate {
        max_tokens: wire.max_tokens,
        temperature: wire.temperature,
        stream: false,
        skill_trace: if wire.skill_trace.is_absent() {
            None
        } else {
            Some(effective_trace.as_u8())
        },
        // Canonical skill-chain tools are always injected below, so client-provided
        // tools/tool_choice never survive into the model-turn template.
        tools: None,
        tool_choice: None,
        extra: wire.extra,
    };
    // Apply call_skillep tool injection; strip client tool_choice.
    let allowed_ref = ctx.allowed_child_skilleps.as_ref().map(|arc| arc.as_ref());
    apply_skill_chain_tools(&mut template, allowed_ref);

    let response_mode = match effective_trace {
        SkillTraceLevel::None => SkillResponseMode::Normal,
        SkillTraceLevel::Basic | SkillTraceLevel::Verbose => SkillResponseMode::SseTrace,
    };

    Ok(NormalizedSkillCall {
        prepared_body: PreparedSkillChainBody { template, history },
        effective_trace,
        response_mode,
        current_depth,
        debug_mocks,
    })
}

/// Profile enum for `build_skill_chat_request` to distinguish the two internal loopback callers.
#[derive(Debug, Clone, Copy)]
pub(super) enum InternalSkillRequestProfile {
    /// MCP tool loopback: includes max_tokens=5000 and temperature=0.0.
    McpToolLoopback,
    /// Child skill loopback: omits max_tokens and temperature.
    ChildSkillLoopback,
}

/// Builds the wire request body for internal skill loopback calls.
///
/// For `McpToolLoopback`: sets max_tokens=5000 and temperature=0.0 (current MCP defaults).
/// For `ChildSkillLoopback`: omits max_tokens and temperature.
/// Both profiles set stream=false and include the given skill_trace level.
pub(super) fn build_skill_chat_request(
    query: &str,
    skill_trace: SkillTraceLevel,
    profile: InternalSkillRequestProfile,
) -> SkillChatCompletionsWireRequest {
    let messages = vec![serde_json::json!({"role": "user", "content": query})];
    match profile {
        InternalSkillRequestProfile::McpToolLoopback => SkillChatCompletionsWireRequest {
            messages,
            max_tokens: Some(5000),
            temperature: Some(0.0),
            stream: Some(false),
            skill_trace: skill_trace.into_field(),
            mocks: None,
            tools: None,
            tool_choice: None,
            extra: HashMap::new(),
        },
        InternalSkillRequestProfile::ChildSkillLoopback => SkillChatCompletionsWireRequest {
            messages,
            max_tokens: None,
            temperature: None,
            stream: Some(false),
            skill_trace: skill_trace.into_field(),
            mocks: None,
            tools: None,
            tool_choice: None,
            extra: HashMap::new(),
        },
    }
}

/// Attaches the canonical Option 1 child-call transport headers to a loopback request.
///
/// This is the single canonical place that encodes `X-Inferx-Skill-Chain-Child: 1`
/// and `X-Chain-Depth: <next_depth>`. Keep forwarded caller headers (Authorization,
/// X-Request-Id, traceparent) as a separate concern via `copy_forwarded_child_headers`.
pub(super) fn apply_internal_child_headers(
    req: reqwest::RequestBuilder,
    next_depth: u32,
) -> reqwest::RequestBuilder {
    req.header(SKILL_CHAIN_CHILD_HEADER, "1")
        .header(SKILL_CHAIN_DEPTH_HEADER, next_depth.to_string())
}

fn now_ts_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn maybe_set_trace_content(
    trace: &SkillChainTraceState,
    payload: &mut SkillTraceEventPayload,
    query: Option<&str>,
    output: Option<&str>,
) {
    if !trace.include_content {
        return;
    }
    payload.query = query.map(str::to_string);
    payload.output = output.map(str::to_string);
}

async fn send_trace_chunk(tx: &mpsc::Sender<Result<String, Infallible>>, chunk: String) -> bool {
    tx.send(Ok(chunk)).await.is_ok()
}

async fn emit_trace_comment(trace: &SkillChainTraceState, comment: &str) -> bool {
    send_trace_chunk(&trace.tx, format!(": {}\n\n", comment)).await
}

fn summarize_trace_text(value: Option<&str>) -> String {
    match value {
        Some(text) => text.to_string(),
        None => "-".to_string(),
    }
}

fn log_trace_event_emitted(
    trace: &SkillChainTraceState,
    event_type: &str,
    call_id: &str,
    parent_call_id: Option<&str>,
    depth: u32,
    skill: &str,
    elapsed_ms: Option<u64>,
    phase: Option<&str>,
    query: Option<&str>,
    output: Option<&str>,
    result_code: Option<&str>,
    fail_reason: Option<SkillTraceFailReason>,
    usage: Option<&SkillTraceTokenUsage>,
) {
    ctrace!(
        verbose_category::SKILL,
        "skill_chain trace_emit root_call_id={} root_skill={} event_type={} call_id={} parent_call_id={} depth={} skill={} elapsed_ms={} phase={} result_code={} fail_reason={} prompt_tokens={} completion_tokens={} total_tokens={} query={} output={}",
        trace.root_call_id,
        trace.root_skill,
        event_type,
        call_id,
        parent_call_id.unwrap_or("-"),
        depth,
        skill,
        elapsed_ms
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string()),
        phase.unwrap_or("-"),
        result_code.unwrap_or("-"),
        fail_reason.map(|reason| reason.as_str()).unwrap_or("-"),
        usage
            .map(|value| value.prompt_tokens.to_string())
            .unwrap_or_else(|| "-".to_string()),
        usage
            .map(|value| value.completion_tokens.to_string())
            .unwrap_or_else(|| "-".to_string()),
        usage
            .map(|value| value.total_tokens.to_string())
            .unwrap_or_else(|| "-".to_string()),
        summarize_trace_text(query),
        summarize_trace_text(output),
    );
}

async fn emit_trace_event(
    trace: &SkillChainTraceState,
    event_type: &str,
    call_id: &str,
    parent_call_id: Option<&str>,
    depth: u32,
    skill: &str,
    elapsed_ms: Option<u64>,
    phase: Option<&str>,
    query: Option<&str>,
    output: Option<&str>,
    result_code: Option<&str>,
    fail_reason: Option<SkillTraceFailReason>,
    usage: Option<&SkillTraceTokenUsage>,
) -> bool {
    let mut payload = SkillTraceEventPayload {
        event_type: event_type.to_string(),
        call_id: call_id.to_string(),
        parent_call_id: parent_call_id.map(str::to_string),
        depth,
        skill: skill.to_string(),
        ts_ms: now_ts_ms(),
        elapsed_ms,
        phase: phase.map(str::to_string),
        query: None,
        output: None,
        result_code: result_code.map(str::to_string),
        fail_reason: fail_reason.map(|r| r.as_str().to_string()),
        prompt_tokens: usage.map(|u| u.prompt_tokens),
        completion_tokens: usage.map(|u| u.completion_tokens),
        total_tokens: usage.map(|u| u.total_tokens),
    };
    maybe_set_trace_content(trace, &mut payload, query, output);

    let sent = send_trace_chunk(
        &trace.tx,
        format!(
            "event: skill_trace\ndata: {}\n\n",
            serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string())
        ),
    )
    .await;

    log_trace_event_emitted(
        trace,
        event_type,
        call_id,
        parent_call_id,
        depth,
        skill,
        elapsed_ms,
        phase,
        query,
        output,
        result_code,
        fail_reason,
        usage,
    );

    sent
}

async fn emit_trace_skill_result(trace: &SkillChainTraceState, body: &[u8]) -> bool {
    let data = String::from_utf8_lossy(body);
    ctrace!(
        verbose_category::SKILL,
        "skill_chain trace_emit root_call_id={} root_skill={} event_type=skill_result body_len={} body={}",
        trace.root_call_id,
        trace.root_skill,
        body.len(),
        summarize_trace_text(Some(data.as_ref())),
    );
    send_trace_chunk(
        &trace.tx,
        format!("event: skill_result\ndata: {}\n\n", data),
    )
    .await
}

async fn emit_trace_done(trace: &SkillChainTraceState) -> bool {
    ctrace!(
        verbose_category::SKILL,
        "skill_chain trace_emit root_call_id={} root_skill={} event_type=done",
        trace.root_call_id,
        trace.root_skill
    );
    send_trace_chunk(&trace.tx, "data: [DONE]\n\n".to_string()).await
}

async fn emit_root_call_start(trace: &SkillChainTraceState, query: Option<&str>) -> bool {
    emit_trace_event(
        trace,
        "call_start",
        &trace.root_call_id,
        None,
        1,
        &trace.root_skill,
        None,
        None,
        query,
        None,
        None,
        None,
        None,
    )
    .await
}

async fn emit_root_call_finish(
    trace: &SkillChainTraceState,
    result_code: &str,
    fail_reason: Option<SkillTraceFailReason>,
    usage: Option<&SkillTraceTokenUsage>,
) -> bool {
    emit_trace_event(
        trace,
        "call_finish",
        &trace.root_call_id,
        None,
        1,
        &trace.root_skill,
        Some(trace.started_at.elapsed().as_millis() as u64),
        None,
        None,
        None,
        Some(result_code),
        fail_reason,
        usage,
    )
    .await
}

async fn emit_trace_heartbeat(
    trace: &SkillChainTraceState,
    call_id: &str,
    parent_call_id: Option<&str>,
    depth: u32,
    skill: &str,
    started_at: Instant,
    phase: &str,
) -> bool {
    emit_trace_event(
        trace,
        "heartbeat",
        call_id,
        parent_call_id,
        depth,
        skill,
        Some(started_at.elapsed().as_millis() as u64),
        Some(phase),
        None,
        None,
        None,
        None,
        None,
    )
    .await
}

async fn await_with_trace_heartbeat<F, T, HF, HH>(
    trace: Option<SkillChainTraceState>,
    future: F,
    mut on_tick: HF,
) -> T
where
    F: Future<Output = T>,
    HF: FnMut(SkillChainTraceState) -> HH,
    HH: Future<Output = ()>,
{
    let Some(trace) = trace else {
        return future.await;
    };

    tokio::pin!(future);
    let mut ticker = tokio::time::interval(SKILL_TRACE_HEARTBEAT_INTERVAL);
    ticker.tick().await;

    loop {
        tokio::select! {
            output = &mut future => break output,
            _ = ticker.tick() => {
                let _ = emit_trace_comment(&trace, "heartbeat").await;
                on_tick(trace.clone()).await;
            }
        }
    }
}

fn skill_chain_tool_definition(allowed: Option<&HashSet<String>>) -> Value {
    match allowed {
        Some(ids) if ids.is_empty() => serde_json::json!([]),
        Some(ids) => {
            let mut enum_vals: Vec<&str> = ids.iter().map(String::as_str).collect();
            enum_vals.sort_unstable();
            serde_json::json!([{
                "type": "function",
                "function": {
                    "name": SKILL_CHAIN_TOOL_NAME,
                    "description": "Call another InferX skill endpoint",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "skillep_id": {
                                "type": "string",
                                "description": "Canonical skill endpoint id owner_tenant/namespace/skillname",
                                "enum": enum_vals
                            },
                            "query": {
                                "type": "string",
                                "description": "User request to send to the child skill"
                            }
                        },
                        "required": ["skillep_id", "query"]
                    }
                }
            }])
        }
        None => serde_json::json!([{
            "type": "function",
            "function": {
                "name": SKILL_CHAIN_TOOL_NAME,
                "description": "Call another InferX skill endpoint",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "skillep_id": {
                            "type": "string",
                            "description": "Canonical skill endpoint id owner_tenant/namespace/skillname"
                        },
                        "query": {
                            "type": "string",
                            "description": "User request to send to the child skill"
                        }
                    },
                    "required": ["skillep_id", "query"]
                }
            }
        }]),
    }
}

fn apply_skill_chain_tools(
    template: &mut SkillModelRequestTemplate,
    allowed: Option<&HashSet<String>>,
) {
    let tools = skill_chain_tool_definition(allowed);
    match &tools {
        Value::Array(arr) if arr.is_empty() => {
            template.tools = None;
        }
        _ => {
            template.tools = Some(tools);
        }
    }
    template.tool_choice = None;
}

fn build_skill_chain_request_body(
    template: &SkillModelRequestTemplate,
    history: &[Value],
) -> Result<Vec<u8>, StatusCode> {
    let body = SkillModelTurnRequest {
        template,
        messages: history,
    };
    serde_json::to_vec(&body).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

fn parse_skill_response(body_bytes: &[u8]) -> Option<ParsedSkillResponse> {
    let body: ChatCompletionResponse = serde_json::from_slice(body_bytes).ok()?;
    let choice = body.choices.first()?;
    let tool_calls = choice
        .message
        .as_ref()
        .and_then(|message| message.tool_calls.as_ref())
        .or_else(|| {
            choice
                .delta
                .as_ref()
                .and_then(|delta| delta.tool_calls.as_ref())
        })
        .map(|calls| {
            calls
                .iter()
                .map(|call| ParsedSkillToolCall {
                    id: call.id.clone().unwrap_or_default(),
                    name: call
                        .function
                        .as_ref()
                        .and_then(|function| function.name.clone())
                        .unwrap_or_default(),
                    arguments: call
                        .function
                        .as_ref()
                        .and_then(|function| function.arguments.clone())
                        .unwrap_or_default(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let has_tool_calls =
        !tool_calls.is_empty() || choice.finish_reason.as_deref() == Some("tool_calls");
    let final_text = choice
        .message
        .as_ref()
        .and_then(|message| message.content.as_ref())
        .and_then(json_text)
        .or_else(|| {
            choice
                .delta
                .as_ref()
                .and_then(|delta| delta.content.as_ref())
                .and_then(json_text)
        });

    Some(ParsedSkillResponse {
        tool_calls: if has_tool_calls {
            tool_calls
        } else {
            Vec::new()
        },
        final_text,
        usage: body.usage.and_then(RawUsage::into_usage),
    })
}

fn extract_raw_tool_calls(body_bytes: &[u8]) -> Option<Vec<Value>> {
    let body: Value = serde_json::from_slice(body_bytes).ok()?;
    let choice = body.get("choices")?.as_array()?.first()?;
    let tool_calls = choice
        .get("message")
        .and_then(|message| message.get("tool_calls"))
        .or_else(|| {
            choice
                .get("delta")
                .and_then(|delta| delta.get("tool_calls"))
        })?;
    Some(tool_calls.as_array()?.to_vec())
}

fn parse_skillep_tool_args(arguments: &str) -> core::result::Result<(String, String), String> {
    let args: Value =
        serde_json::from_str(arguments).map_err(|_| "Error: invalid tool arguments".to_string())?;
    let skillep_id = args
        .get("skillep_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            "Error: missing or empty skillep_id. Expected format: owner_tenant/namespace/skillname."
                .to_string()
        })?;
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .ok_or_else(|| "Error: invalid tool arguments".to_string())?;
    Ok((skillep_id.to_string(), query))
}

fn split_skillep_id(skillep_id: &str) -> core::result::Result<(&str, &str, &str), String> {
    let mut parts = skillep_id.split('/');
    let owner_tenant = parts.next().unwrap_or("");
    let namespace = parts.next().unwrap_or("");
    let skillname = parts.next().unwrap_or("");
    if owner_tenant.is_empty()
        || namespace.is_empty()
        || skillname.is_empty()
        || parts.next().is_some()
    {
        return Err(format!(
            "Error: invalid skillep_id '{}'. Expected format: owner_tenant/namespace/skillname. Do not retry with the same value.",
            skillep_id
        ));
    }
    Ok((owner_tenant, namespace, skillname))
}

fn format_child_tool_error(message: impl Into<String>) -> String {
    let message = message.into();
    if message.starts_with("Error: ") {
        message
    } else {
        format!("Error: {}", message)
    }
}

fn json_text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => {
            let mut out = String::new();
            for item in items {
                match item {
                    Value::String(s) => out.push_str(s),
                    Value::Object(map) => {
                        if let Some(Value::String(text)) = map.get("text") {
                            out.push_str(text);
                        }
                    }
                    _ => {}
                }
            }
            if out.is_empty() {
                None
            } else {
                Some(out)
            }
        }
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

fn summarize_value_for_log(value: &Value) -> String {
    const MAX_LOG_BYTES: usize = 4096;
    let text = value.to_string();
    let bytes = text.as_bytes();
    let preview = if bytes.len() > MAX_LOG_BYTES {
        &bytes[..MAX_LOG_BYTES]
    } else {
        bytes
    };
    let preview_text = String::from_utf8_lossy(preview).replace('\n', "\\n");
    if bytes.len() > MAX_LOG_BYTES {
        format!(
            "{}...[truncated {} bytes]",
            preview_text,
            bytes.len() - MAX_LOG_BYTES
        )
    } else {
        preview_text
    }
}

fn summarize_bytes_for_log(bytes: &[u8]) -> String {
    const MAX_LOG_BYTES: usize = 4096;
    let preview = if bytes.len() > MAX_LOG_BYTES {
        &bytes[..MAX_LOG_BYTES]
    } else {
        bytes
    };
    let text = String::from_utf8_lossy(preview).replace('\n', "\\n");
    if bytes.len() > MAX_LOG_BYTES {
        format!(
            "{}...[truncated {} bytes]",
            text,
            bytes.len() - MAX_LOG_BYTES
        )
    } else {
        text
    }
}

fn latest_user_query(history: &[Value]) -> Option<String> {
    history.iter().rev().find_map(|item| {
        let obj = item.as_object()?;
        if obj.get("role").and_then(Value::as_str) != Some("user") {
            return None;
        }
        obj.get("content").and_then(json_text)
    })
}

fn summarize_tool_calls_for_log(tool_calls: &[ParsedSkillToolCall]) -> String {
    let summarized = tool_calls
        .iter()
        .map(|call| {
            let (skillep_id, query_len) = match parse_skillep_tool_args(&call.arguments) {
                Ok((skillep_id, query)) => (skillep_id, query.len().to_string()),
                Err(_) => ("-".to_string(), "invalid".to_string()),
            };
            serde_json::json!({
                "id": call.id,
                "name": call.name,
                "skillep_id": skillep_id,
                "query_len": query_len,
            })
        })
        .collect::<Vec<_>>();
    summarize_value_for_log(&Value::Array(summarized))
}

fn child_result_usage(result: &ParallelChildResult) -> Option<&SkillTraceTokenUsage> {
    result.usage.as_ref()
}

#[cfg(test)]
fn log_invalid_tool_args(
    logical_tenant: &str,
    logical_namespace: &str,
    logical_funcname: &str,
    tool_call_id: &str,
) {
    eprintln!(
        "skill_chain invalid_tool_args logical={}/{}/{} tool_call_id={}",
        logical_tenant, logical_namespace, logical_funcname, tool_call_id
    );
}

#[cfg(not(test))]
fn log_invalid_tool_args(
    logical_tenant: &str,
    logical_namespace: &str,
    logical_funcname: &str,
    tool_call_id: &str,
) {
    warn!(
        "skill_chain invalid_tool_args logical={}/{}/{} tool_call_id={}",
        logical_tenant, logical_namespace, logical_funcname, tool_call_id
    );
}

#[cfg(test)]
fn log_invalid_skillep_id(
    logical_tenant: &str,
    logical_namespace: &str,
    logical_funcname: &str,
    tool_call_id: &str,
    skillep_id: &str,
) {
    eprintln!(
        "skill_chain invalid_skillep_id logical={}/{}/{} tool_call_id={} skillep_id={}",
        logical_tenant, logical_namespace, logical_funcname, tool_call_id, skillep_id
    );
}

#[cfg(not(test))]
fn log_invalid_skillep_id(
    logical_tenant: &str,
    logical_namespace: &str,
    logical_funcname: &str,
    tool_call_id: &str,
    skillep_id: &str,
) {
    warn!(
        "skill_chain invalid_skillep_id logical={}/{}/{} tool_call_id={} skillep_id={}",
        logical_tenant, logical_namespace, logical_funcname, tool_call_id, skillep_id
    );
}

#[cfg(test)]
fn log_skillep_not_allowed(
    logical_tenant: &str,
    logical_namespace: &str,
    logical_funcname: &str,
    tool_call_id: &str,
    skillep_id: &str,
) {
    eprintln!(
        "skill_chain skillep_not_allowed logical={}/{}/{} tool_call_id={} skillep_id={}",
        logical_tenant, logical_namespace, logical_funcname, tool_call_id, skillep_id
    );
}

#[cfg(not(test))]
fn log_skillep_not_allowed(
    logical_tenant: &str,
    logical_namespace: &str,
    logical_funcname: &str,
    tool_call_id: &str,
    skillep_id: &str,
) {
    warn!(
        "skill_chain skillep_not_allowed logical={}/{}/{} tool_call_id={} skillep_id={}",
        logical_tenant, logical_namespace, logical_funcname, tool_call_id, skillep_id
    );
}

fn copy_forwarded_child_headers(
    parent_headers: &HeaderMap,
    child_req: reqwest::RequestBuilder,
) -> reqwest::RequestBuilder {
    let mut child_req = child_req;
    for header_name in ["Authorization", "X-Request-Id", "traceparent"] {
        if let Some(v) = parent_headers
            .get(header_name)
            .and_then(|value| value.to_str().ok())
        {
            child_req = child_req.header(header_name, v);
        }
    }
    child_req
}

async fn execute_parallel_child_call(
    task: ParallelChildTaskInput,
    context: ParallelChildTaskContext,
) -> ParallelChildResult {
    let tool_call_id = task.call.id.clone();
    let skillep_id_and_query = match parse_skillep_tool_args(&task.call.arguments) {
        Ok(parsed) => parsed,
        Err(err) => {
            log_invalid_tool_args(
                &context.logical_tenant,
                &context.logical_namespace,
                &context.logical_funcname,
                &task.call.id,
            );
            return ParallelChildResult {
                original_index: task.original_index,
                tool_call_id,
                tool_result: err,
                fail_reason: None,
                usage: None,
            };
        }
    };

    let (skillep_id, query) = skillep_id_and_query;
    let (child_owner_tenant, child_namespace, child_skillname) = match split_skillep_id(&skillep_id)
    {
        Ok(parts) => parts,
        Err(err) => {
            log_invalid_skillep_id(
                &context.logical_tenant,
                &context.logical_namespace,
                &context.logical_funcname,
                &task.call.id,
                &skillep_id,
            );
            return ParallelChildResult {
                original_index: task.original_index,
                tool_call_id,
                tool_result: err,
                fail_reason: None,
                usage: None,
            };
        }
    };

    if let Some(allowed) = &context.allowed_skillep_ids {
        if !allowed.contains(&skillep_id) {
            log_skillep_not_allowed(
                &context.logical_tenant,
                &context.logical_namespace,
                &context.logical_funcname,
                &task.call.id,
                &skillep_id,
            );
            return ParallelChildResult {
                original_index: task.original_index,
                tool_call_id,
                tool_result: "Error: skill endpoint not in allowed list".to_string(),
                fail_reason: None,
                usage: None,
            };
        }
    }

    let mocked_result = context.debug_mocks.get(&skillep_id).cloned();
    let (tool_result, fail_reason, usage) = if let Some(mocked_result) = mocked_result {
        if let Some(trace_state) = context.trace.as_ref() {
            let child_trace = SkillChainChildTraceState {
                call_id: Uuid::new_v4().to_string(),
                skill: skillep_id.clone(),
                started_at: Instant::now(),
                local_root_call_id: Arc::new(Mutex::new(None)),
            };
            let _ = emit_trace_event(
                trace_state,
                "subcall_start",
                &child_trace.call_id,
                Some(&trace_state.root_call_id),
                context.direct_child_depth,
                &child_trace.skill,
                None,
                Some("child_call_start"),
                Some(query.as_str()),
                None,
                None,
                None,
                None,
            )
            .await;
            let _ = emit_trace_event(
                trace_state,
                "subcall_finish",
                &child_trace.call_id,
                Some(&trace_state.root_call_id),
                context.direct_child_depth,
                &child_trace.skill,
                Some(child_trace.started_at.elapsed().as_millis() as u64),
                Some("child_call_end"),
                None,
                Some(mocked_result.as_str()),
                Some("pass"),
                None,
                None,
            )
            .await;
        }
        (mocked_result, None, None)
    } else {
        match context.current_depth.checked_add(1) {
            Some(next_depth) if next_depth <= SKILL_CHAIN_MAX_DEPTH => {
                let child_trace = context.trace.as_ref().map(|_| SkillChainChildTraceState {
                    call_id: Uuid::new_v4().to_string(),
                    skill: skillep_id.clone(),
                    started_at: Instant::now(),
                    local_root_call_id: Arc::new(Mutex::new(None)),
                });
                if let (Some(trace_state), Some(child_trace)) =
                    (context.trace.as_ref(), child_trace.as_ref())
                {
                    let _ = emit_trace_event(
                        trace_state,
                        "subcall_start",
                        &child_trace.call_id,
                        Some(&trace_state.root_call_id),
                        context.direct_child_depth,
                        &child_trace.skill,
                        None,
                        Some("child_call_start"),
                        Some(query.as_str()),
                        None,
                        None,
                        None,
                        None,
                    )
                    .await;
                }

                ctrace!(
                    verbose_category::SKILL,
                    "skill_chain child_call logical={}/{}/{} tool_call_id={} child={}/{}/{} next_depth={} query_len={}",
                    context.logical_tenant,
                    context.logical_namespace,
                    context.logical_funcname,
                    task.call.id,
                    child_owner_tenant,
                    child_namespace,
                    child_skillname,
                    next_depth,
                    query.len()
                );
                let child_skill_trace = match context.trace.as_ref() {
                    None => SkillTraceLevel::None,
                    Some(t) if t.include_content => SkillTraceLevel::Verbose,
                    Some(_) => SkillTraceLevel::Basic,
                };
                let child_body = build_skill_chat_request(
                    &query,
                    child_skill_trace,
                    InternalSkillRequestProfile::ChildSkillLoopback,
                );
                let child_url = format!(
                    "http://127.0.0.1:{}/skills/{}/{}/{}/v1/chat/completions",
                    GATEWAY_CONFIG.gatewayPort,
                    child_owner_tenant,
                    child_namespace,
                    child_skillname
                );
                let child_req = context
                    .child_http_client
                    .post(&child_url)
                    .header(CONTENT_TYPE.as_str(), "application/json");
                let child_req = apply_internal_child_headers(child_req, next_depth);
                let child_req = child_req.json(&child_body);
                let child_req = copy_forwarded_child_headers(&context.headers, child_req);

                let trace_for_child_call = context.trace.clone();
                let child_trace_for_stream = child_trace.clone();
                let child_depth_for_stream = context.direct_child_depth;
                let child_call = async {
                    let resp = child_req.send().await.map_err(|e| ChildTraceReadError {
                        message: e.to_string(),
                        fail_reason: SkillTraceFailReason::TransportError,
                        open_descendants: Vec::new(),
                    })?;
                    let child_status = resp.status();
                    let child_content_type = resp
                        .headers()
                        .get("content-type")
                        .and_then(|v| v.to_str().ok())
                        .map(|v| v.to_ascii_lowercase());
                    if let (Some(trace_state), Some(child_trace_state)) = (
                        trace_for_child_call.as_ref(),
                        child_trace_for_stream.as_ref(),
                    ) {
                        if child_status.is_success()
                            && child_content_type
                                .as_deref()
                                .map(|v| v.contains("text/event-stream"))
                                .unwrap_or(false)
                        {
                            let child_trace_result = read_child_trace_sse(
                                trace_state,
                                child_trace_state,
                                child_depth_for_stream,
                                resp,
                            )
                            .await?;
                            return Ok::<_, ChildTraceReadError>((
                                child_status,
                                child_trace_result,
                            ));
                        }
                    }
                    let bytes = resp.bytes().await.map_err(|e| ChildTraceReadError {
                        message: e.to_string(),
                        fail_reason: SkillTraceFailReason::TransportError,
                        open_descendants: Vec::new(),
                    })?;
                    Ok::<_, ChildTraceReadError>((
                        child_status,
                        ChildTraceReadResult {
                            skill_result: Some(bytes.to_vec()),
                            terminal_result_code: None,
                            terminal_fail_reason: None,
                            terminal_usage: None,
                            open_descendants: Vec::new(),
                        },
                    ))
                };

                let trace_for_tick = context.trace.clone();
                let child_trace_for_tick = child_trace.clone();
                let (tool_result, fail_reason, usage) = match await_with_trace_heartbeat(
                    trace_for_tick,
                    child_call,
                    move |trace_state| {
                        let child_trace = child_trace_for_tick.clone();
                        async move {
                            if let Some(child_trace) = child_trace.as_ref() {
                                let _ = emit_trace_heartbeat(
                                    &trace_state,
                                    &child_trace.call_id,
                                    Some(&trace_state.root_call_id),
                                    child_depth_for_stream,
                                    &child_trace.skill,
                                    child_trace.started_at,
                                    "child_call",
                                )
                                .await;
                            }
                        }
                    },
                )
                .await
                {
                    Err(err) => {
                        if let Some(trace_state) = context.trace.as_ref() {
                            for descendant in &err.open_descendants {
                                let _ = emit_trace_event(
                                    trace_state,
                                    "subcall_finish",
                                    &descendant.call_id,
                                    descendant.parent_call_id.as_deref(),
                                    descendant.depth,
                                    &descendant.skill,
                                    Some(
                                        child_trace
                                            .as_ref()
                                            .map(|c| c.started_at.elapsed().as_millis() as u64)
                                            .unwrap_or(0),
                                    ),
                                    Some("child_call_end"),
                                    None,
                                    None,
                                    Some("fail"),
                                    Some(err.fail_reason),
                                    None,
                                )
                                .await;
                            }
                        }
                        warn!(
                            "skill_chain child_call_transport_error logical={}/{}/{} tool_call_id={} child={}/{}/{} err={}",
                            context.logical_tenant,
                            context.logical_namespace,
                            context.logical_funcname,
                            task.call.id,
                            child_owner_tenant,
                            child_namespace,
                            child_skillname,
                            err.message
                        );
                        (
                            format_child_tool_error(err.message),
                            Some(err.fail_reason),
                            None,
                        )
                    }
                    Ok((child_status, child_result)) if !child_status.is_success() => {
                        warn!(
                            "skill_chain child_call_http_error logical={}/{}/{} tool_call_id={} child={}/{}/{} status={}",
                            context.logical_tenant,
                            context.logical_namespace,
                            context.logical_funcname,
                            task.call.id,
                            child_owner_tenant,
                            child_namespace,
                            child_skillname,
                            child_status
                        );
                        let bytes = child_result.skill_result.unwrap_or_default();
                        let detail = String::from_utf8_lossy(&bytes).trim().to_string();
                        let message = if detail.is_empty() {
                            format_child_tool_error(format!(
                                "child call failed with {}",
                                child_status
                            ))
                        } else {
                            format_child_tool_error(detail)
                        };
                        (
                            message,
                            Some(SkillTraceFailReason::TransportError),
                            child_result.terminal_usage,
                        )
                    }
                    Ok((_, child_result)) => {
                        if !child_result.open_descendants.is_empty() {
                            if let Some(trace_state) = context.trace.as_ref() {
                                for descendant in &child_result.open_descendants {
                                    let _ = emit_trace_event(
                                        trace_state,
                                        "subcall_finish",
                                        &descendant.call_id,
                                        descendant.parent_call_id.as_deref(),
                                        descendant.depth,
                                        &descendant.skill,
                                        Some(
                                            child_trace
                                                .as_ref()
                                                .map(|c| c.started_at.elapsed().as_millis() as u64)
                                                .unwrap_or(0),
                                        ),
                                        Some("child_call_end"),
                                        None,
                                        None,
                                        Some("fail"),
                                        Some(SkillTraceFailReason::InvalidResponse),
                                        None,
                                    )
                                    .await;
                                }
                            }
                            (
                                "Error: child trace stream left open descendant calls".to_string(),
                                Some(SkillTraceFailReason::InvalidResponse),
                                child_result.terminal_usage,
                            )
                        } else if child_result.terminal_result_code.as_deref() == Some("fail") {
                            (
                                format_child_fail_reason(child_result.terminal_fail_reason),
                                child_result.terminal_fail_reason,
                                child_result.terminal_usage,
                            )
                        } else {
                            match child_result.skill_result {
                                Some(bytes) => match parse_skill_response(&bytes) {
                                    Some(parsed_child) => {
                                        ctrace!(
                                            verbose_category::SKILL,
                                            "skill_chain child_call_ok logical={}/{}/{} tool_call_id={} child={}/{}/{} final_text_present={}",
                                            context.logical_tenant,
                                            context.logical_namespace,
                                            context.logical_funcname,
                                            task.call.id,
                                            child_owner_tenant,
                                            child_namespace,
                                            child_skillname,
                                            parsed_child.final_text.as_ref().map(|s| !s.is_empty()).unwrap_or(false)
                                        );
                                        match parsed_child.final_text.filter(|s| !s.is_empty()) {
                                            Some(text) => (text, None, parsed_child.usage),
                                            None => (
                                                "Error: child returned empty response".to_string(),
                                                Some(SkillTraceFailReason::InvalidResponse),
                                                parsed_child.usage,
                                            ),
                                        }
                                    }
                                    None => {
                                        warn!(
                                            "skill_chain child_call_invalid_response logical={}/{}/{} tool_call_id={} child={}/{}/{}",
                                            context.logical_tenant,
                                            context.logical_namespace,
                                            context.logical_funcname,
                                            task.call.id,
                                            child_owner_tenant,
                                            child_namespace,
                                            child_skillname
                                        );
                                        (
                                            format_child_tool_error(
                                                "child returned invalid response",
                                            ),
                                            Some(SkillTraceFailReason::InvalidResponse),
                                            child_result.terminal_usage,
                                        )
                                    }
                                },
                                None => (
                                    "Error: child trace stream missing skill_result".to_string(),
                                    Some(SkillTraceFailReason::InvalidResponse),
                                    child_result.terminal_usage,
                                ),
                            }
                        }
                    }
                };

                if let (Some(trace_state), Some(child_trace)) =
                    (context.trace.as_ref(), child_trace.as_ref())
                {
                    let _ = emit_trace_event(
                        trace_state,
                        "subcall_finish",
                        &child_trace.call_id,
                        Some(&trace_state.root_call_id),
                        context.direct_child_depth,
                        &child_trace.skill,
                        Some(child_trace.started_at.elapsed().as_millis() as u64),
                        Some("child_call_end"),
                        None,
                        Some(tool_result.as_str()),
                        Some(if fail_reason.is_some() {
                            "fail"
                        } else {
                            "pass"
                        }),
                        fail_reason,
                        usage.as_ref(),
                    )
                    .await;
                }

                (tool_result, fail_reason, usage)
            }
            _ => {
                warn!(
                    "skill_chain max_depth_exceeded logical={}/{}/{} current_depth={} tool_call_id={}",
                    context.logical_tenant,
                    context.logical_namespace,
                    context.logical_funcname,
                    context.current_depth,
                    task.call.id
                );
                (
                    "Error: max chain depth exceeded".to_string(),
                    Some(SkillTraceFailReason::MaxDepth),
                    None,
                )
            }
        }
    };

    ParallelChildResult {
        original_index: task.original_index,
        tool_call_id,
        tool_result,
        fail_reason,
        usage,
    }
}

async fn drive_parallel_child_tasks<F>(tasks: FuturesUnordered<F>) -> Vec<ParallelChildResult>
where
    F: Future<Output = ParallelChildResult>,
{
    let mut tasks = tasks;
    let mut results: Vec<Option<ParallelChildResult>> = Vec::new();
    while let Some(result) = tasks.next().await {
        let result_index = result.original_index;
        if results.len() <= result_index {
            results.resize(result_index + 1, None);
        }
        results[result_index] = Some(result);
    }
    results.into_iter().flatten().collect()
}

async fn forward_child_trace_event(
    trace: &SkillChainTraceState,
    direct_child: &SkillChainChildTraceState,
    direct_child_depth: u32,
    event: SkillTraceEventPayload,
) -> Option<(String, ForwardedChildTraceNode)> {
    if event.depth == 1 && event.event_type == "call_start" {
        if let Ok(mut local_root_call_id) = direct_child.local_root_call_id.lock() {
            *local_root_call_id = Some(event.call_id.clone());
        }
        return None;
    }
    if event.depth == 1 && event.event_type == "call_finish" {
        return None;
    }
    let normalized_type = match event.event_type.as_str() {
        "call_start" => "subcall_start",
        "call_finish" => "subcall_finish",
        other => other,
    };
    let local_root_call_id = direct_child
        .local_root_call_id
        .lock()
        .ok()
        .and_then(|value| value.clone());
    let call_id = if local_root_call_id.as_deref() == Some(event.call_id.as_str()) {
        direct_child.call_id.as_str()
    } else {
        event.call_id.as_str()
    };
    let parent_call_id = if event.depth <= 1 {
        Some(trace.root_call_id.as_str())
    } else if event.parent_call_id.as_deref() == local_root_call_id.as_deref() {
        Some(direct_child.call_id.as_str())
    } else {
        event.parent_call_id.as_deref()
    };
    let usage = match (
        event.prompt_tokens,
        event.completion_tokens,
        event.total_tokens,
    ) {
        (Some(prompt_tokens), Some(completion_tokens), Some(total_tokens)) => {
            Some(SkillTraceTokenUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
            })
        }
        _ => None,
    };
    let _ = emit_trace_event(
        trace,
        normalized_type,
        call_id,
        parent_call_id,
        direct_child_depth + event.depth.saturating_sub(1),
        &event.skill,
        event.elapsed_ms,
        event.phase.as_deref(),
        event.query.as_deref(),
        event.output.as_deref(),
        event.result_code.as_deref(),
        parse_fail_reason(event.fail_reason.as_deref()),
        usage.as_ref(),
    )
    .await;

    Some((
        normalized_type.to_string(),
        ForwardedChildTraceNode {
            call_id: call_id.to_string(),
            parent_call_id: parent_call_id.map(|value| value.to_string()),
            depth: direct_child_depth + event.depth.saturating_sub(1),
            skill: event.skill,
        },
    ))
}

async fn read_child_trace_sse(
    trace: &SkillChainTraceState,
    direct_child: &SkillChainChildTraceState,
    direct_child_depth: u32,
    resp: reqwest::Response,
) -> Result<ChildTraceReadResult, ChildTraceReadError> {
    let mut parser = SseParser::default();
    let mut buf = String::new();
    let mut skill_result: Option<Vec<u8>> = None;
    let mut terminal_result_code: Option<String> = None;
    let mut terminal_fail_reason: Option<SkillTraceFailReason> = None;
    let mut terminal_usage: Option<SkillTraceTokenUsage> = None;
    let mut open_descendants: HashMap<String, ForwardedChildTraceNode> = HashMap::new();
    let mut saw_done = false;
    let mut stream = resp.bytes_stream();

    'outer: while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ChildTraceReadError {
            message: e.to_string(),
            fail_reason: SkillTraceFailReason::TransportError,
            open_descendants: open_descendants.values().cloned().collect(),
        })?;
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(pos) = buf.find('\n') {
            let mut line = buf.drain(..=pos).collect::<String>();
            if line.ends_with('\n') {
                line.pop();
            }
            if line.ends_with('\r') {
                line.pop();
            }
            if let Some(message) = parser.push_line(&line) {
                match message {
                    SseMessage::Comment(_) => {}
                    SseMessage::Event { event, data } => {
                        if data == "[DONE]" {
                            saw_done = true;
                            break 'outer;
                        }
                        match event.as_deref() {
                            Some("skill_trace") => {
                                let payload: SkillTraceEventPayload = serde_json::from_str(&data)
                                    .map_err(|e| {
                                    ChildTraceReadError {
                                        message: e.to_string(),
                                        fail_reason: SkillTraceFailReason::InvalidResponse,
                                        open_descendants: open_descendants
                                            .values()
                                            .cloned()
                                            .collect(),
                                    }
                                })?;
                                if payload.depth == 1 && payload.event_type == "call_finish" {
                                    terminal_result_code = payload.result_code.clone();
                                    terminal_fail_reason =
                                        parse_fail_reason(payload.fail_reason.as_deref());
                                    terminal_usage = match (
                                        payload.prompt_tokens,
                                        payload.completion_tokens,
                                        payload.total_tokens,
                                    ) {
                                        (
                                            Some(prompt_tokens),
                                            Some(completion_tokens),
                                            Some(total_tokens),
                                        ) => Some(SkillTraceTokenUsage {
                                            prompt_tokens,
                                            completion_tokens,
                                            total_tokens,
                                        }),
                                        _ => None,
                                    };
                                }
                                if let Some((normalized_type, node)) = forward_child_trace_event(
                                    trace,
                                    direct_child,
                                    direct_child_depth,
                                    payload,
                                )
                                .await
                                {
                                    update_open_descendants(
                                        &mut open_descendants,
                                        &normalized_type,
                                        direct_child_depth,
                                        node,
                                    );
                                }
                            }
                            Some("skill_result") => {
                                skill_result = Some(data.into_bytes());
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    if !buf.is_empty() {
        if let Some(message) = parser.push_line(buf.trim_end_matches('\r')) {
            match message {
                SseMessage::Comment(_) => {}
                SseMessage::Event { event, data } => {
                    if data == "[DONE]" {
                        saw_done = true;
                    } else if event.as_deref() == Some("skill_trace") {
                        let payload: SkillTraceEventPayload =
                            serde_json::from_str(&data).map_err(|e| ChildTraceReadError {
                                message: e.to_string(),
                                fail_reason: SkillTraceFailReason::InvalidResponse,
                                open_descendants: open_descendants.values().cloned().collect(),
                            })?;
                        if payload.depth == 1 && payload.event_type == "call_finish" {
                            terminal_result_code = payload.result_code.clone();
                            terminal_fail_reason =
                                parse_fail_reason(payload.fail_reason.as_deref());
                            terminal_usage = match (
                                payload.prompt_tokens,
                                payload.completion_tokens,
                                payload.total_tokens,
                            ) {
                                (
                                    Some(prompt_tokens),
                                    Some(completion_tokens),
                                    Some(total_tokens),
                                ) => Some(SkillTraceTokenUsage {
                                    prompt_tokens,
                                    completion_tokens,
                                    total_tokens,
                                }),
                                _ => None,
                            };
                        }
                        if let Some((normalized_type, node)) = forward_child_trace_event(
                            trace,
                            direct_child,
                            direct_child_depth,
                            payload,
                        )
                        .await
                        {
                            update_open_descendants(
                                &mut open_descendants,
                                &normalized_type,
                                direct_child_depth,
                                node,
                            );
                        }
                    } else if event.as_deref() == Some("skill_result") {
                        skill_result = Some(data.into_bytes());
                    }
                }
            }
        }
    }

    if !saw_done {
        return Err(ChildTraceReadError {
            message: "child trace stream ended before [DONE]".to_string(),
            fail_reason: SkillTraceFailReason::InvalidResponse,
            open_descendants: open_descendants.values().cloned().collect(),
        });
    }

    Ok(ChildTraceReadResult {
        skill_result,
        terminal_result_code,
        terminal_fail_reason,
        terminal_usage,
        open_descendants: open_descendants.into_values().collect(),
    })
}

async fn execute_skill_chain(
    gw: &HttpGateway,
    chain: SkillChainRequestState,
    mut history: Vec<Value>,
    route: FuncRouteTarget,
    calling_tenant: String,
    logical_funcname: String,
    remain_path: String,
    prefix: String,
    skills_namespace: String,
    allowed_skillep_ids: Option<Arc<HashSet<String>>>,
    trace: Option<SkillChainTraceState>,
    cancel_token: CancellationToken,
) -> Result<SkillChainFinalResponse, SkillChainExecutionError> {
    ctrace!(
        verbose_category::SKILL,
        "skill_chain start logical={}/{}/{} physical={}/{}/{} path={} depth={}",
        route.logical.tenant,
        route.logical.namespace,
        route.logical.funcname,
        route.physical.tenant,
        route.physical.namespace,
        route.physical.funcname,
        remain_path,
        chain.current_depth
    );

    let prefix = if GATEWAY_CONFIG.skillEPSystemPromptConstraint {
        if prefix.trim().is_empty() {
            SKILL_EP_CONSTRAINT.to_string()
        } else {
            format!("{}\n\n{}", SKILL_EP_CONSTRAINT, prefix.trim())
        }
    } else {
        prefix
    };

    let child_http_client = reqwest::Client::new();
    let mut aggregate_usage: Option<SkillTraceTokenUsage> = None;

    loop {
        if cancel_token.is_cancelled() {
            return Err(SkillChainExecutionError::Response(
                StatusCode::SERVICE_UNAVAILABLE,
                "service failure: chain cancelled",
            ));
        }
        ctrace!(
            verbose_category::SKILL,
            "skill_chain model_turn logical={}/{}/{} history_len={} depth={}",
            route.logical.tenant,
            route.logical.namespace,
            route.logical.funcname,
            history.len(),
            chain.current_depth
        );
        let body_bytes =
            build_skill_chain_request_body(&chain.template, &history).map_err(|status| {
                SkillChainExecutionError::Response(
                    status,
                    "service failure: failed to build request body",
                )
            })?;
        ctrace!(
            verbose_category::SKILL,
            "skill_chain model_request logical={}/{}/{} depth={} latest_user_query={} tools={}",
            route.logical.tenant,
            route.logical.namespace,
            route.logical.funcname,
            chain.current_depth,
            latest_user_query(&history).unwrap_or_else(|| "-".to_string()),
            chain
                .template
                .tools
                .as_ref()
                .map(summarize_value_for_log)
                .unwrap_or_else(|| "-".to_string())
        );
        ctrace!(
            verbose_category::SKILL_PAYLOAD,
            "skill_chain model_request payload logical={}/{}/{} depth={} body={}",
            route.logical.tenant,
            route.logical.namespace,
            route.logical.funcname,
            chain.current_depth,
            summarize_bytes_for_log(&body_bytes)
        );

        let mut model_headers = chain.headers.clone();
        model_headers.remove(SKILL_CHAIN_CHILD_HEADER);
        model_headers.remove(SKILL_CHAIN_DEPTH_HEADER);
        model_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let model_req = Request::builder()
            .method(axum::http::Method::POST)
            .uri(remain_path.as_str())
            .body(Body::from(body_bytes))
            .unwrap();
        let (mut model_parts, model_body) = model_req.into_parts();
        model_parts.headers = model_headers;
        let model_req = Request::from_parts(model_parts, model_body);

        let model_turn = async {
            let mut model_resp = dispatch_func_call(
                gw,
                model_req,
                route.clone(),
                calling_tenant.clone(),
                skills_namespace.clone(),
                logical_funcname.clone(),
                remain_path.clone(),
                Some(prefix.as_str()),
            )
            .await
            .map_err(SkillChainExecutionError::DownstreamStatus)?;
            let status = model_resp.status();
            let headers = model_resp.headers().clone();
            let resp_bytes = model_resp
                .body_mut()
                .collect()
                .await
                .map_err(|_| {
                    SkillChainExecutionError::Response(
                        StatusCode::BAD_GATEWAY,
                        "service failure: failed to read model response",
                    )
                })?
                .to_bytes()
                .to_vec();
            Ok::<_, SkillChainExecutionError>((status, headers, resp_bytes))
        };
        let root_started_at = trace.as_ref().map(|trace_state| trace_state.started_at);
        let root_call_id = trace
            .as_ref()
            .map(|trace_state| trace_state.root_call_id.clone());
        let root_skill = trace
            .as_ref()
            .map(|trace_state| trace_state.root_skill.clone());
        let (status, headers, resp_bytes) =
            await_with_trace_heartbeat(trace.clone(), model_turn, move |trace_state| {
                let root_call_id = root_call_id
                    .clone()
                    .unwrap_or_else(|| trace_state.root_call_id.clone());
                let root_skill = root_skill
                    .clone()
                    .unwrap_or_else(|| trace_state.root_skill.clone());
                async move {
                    let started_at = root_started_at.unwrap_or(trace_state.started_at);
                    let _ = emit_trace_heartbeat(
                        &trace_state,
                        &root_call_id,
                        None,
                        1,
                        &root_skill,
                        started_at,
                        "model_inference",
                    )
                    .await;
                }
            })
            .await?;
        let parsed = match parse_skill_response(&resp_bytes) {
            Some(parsed) => parsed,
            None => {
                warn!(
                    "skill_chain non_openai_response logical={}/{}/{} status={}",
                    route.logical.tenant, route.logical.namespace, route.logical.funcname, status
                );
                return Ok(SkillChainFinalResponse {
                    status,
                    headers,
                    body: resp_bytes,
                    is_openai_chat_completion: false,
                    usage: aggregate_usage,
                });
            }
        };
        ctrace!(
            verbose_category::SKILL,
            "skill_chain model_response logical={}/{}/{} status={} final_text_present={} tool_calls={}",
            route.logical.tenant,
            route.logical.namespace,
            route.logical.funcname,
            status,
            parsed
                .final_text
                .as_ref()
                .map(|s| !s.is_empty())
                .unwrap_or(false),
            summarize_tool_calls_for_log(&parsed.tool_calls)
        );
        ctrace!(
            verbose_category::SKILL_PAYLOAD,
            "skill_chain model_response payload logical={}/{}/{} body={}",
            route.logical.tenant,
            route.logical.namespace,
            route.logical.funcname,
            summarize_bytes_for_log(&resp_bytes)
        );
        if let Some(usage) = parsed.usage.as_ref() {
            if let Some(aggregate) = aggregate_usage.as_mut() {
                aggregate.add_assign(usage);
            } else {
                aggregate_usage = Some(usage.clone());
            }
        }

        if parsed.tool_calls.is_empty() {
            ctrace!(
                verbose_category::SKILL,
                "skill_chain final_response logical={}/{}/{} status={} final_text_present={}",
                route.logical.tenant,
                route.logical.namespace,
                route.logical.funcname,
                status,
                parsed
                    .final_text
                    .as_ref()
                    .map(|s| !s.is_empty())
                    .unwrap_or(false)
            );
            return Ok(SkillChainFinalResponse {
                status,
                headers,
                body: resp_bytes,
                is_openai_chat_completion: true,
                usage: aggregate_usage,
            });
        }

        let raw_tool_calls =
            extract_raw_tool_calls(&resp_bytes).ok_or(SkillChainExecutionError::Response(
                StatusCode::BAD_GATEWAY,
                "service failure: failed to preserve tool call history",
            ))?;

        if parsed
            .tool_calls
            .iter()
            .any(|call| call.name != SKILL_CHAIN_TOOL_NAME)
        {
            warn!(
                "skill_chain unsupported_tools logical={}/{}/{} tool_names={:?}",
                route.logical.tenant,
                route.logical.namespace,
                route.logical.funcname,
                parsed
                    .tool_calls
                    .iter()
                    .map(|call| call.name.clone())
                    .collect::<Vec<_>>()
            );
            history.push(
                serde_json::to_value(ChatMessage::Assistant {
                    content: Value::Null,
                    tool_calls: raw_tool_calls.clone(),
                })
                .unwrap(),
            );
            for call in &parsed.tool_calls {
                let content = if call.name == SKILL_CHAIN_TOOL_NAME {
                    "Error: turn aborted (other tool in same response was unsupported)".to_string()
                } else {
                    format_child_tool_error(format!("unsupported tool '{}'", call.name))
                };
                history.push(
                    serde_json::to_value(ChatMessage::Tool {
                        tool_call_id: call.id.clone(),
                        content,
                    })
                    .unwrap(),
                );
            }
            continue;
        }

        ctrace!(
            verbose_category::SKILL,
            "skill_chain tool_turn logical={}/{}/{} tool_call_count={}",
            route.logical.tenant,
            route.logical.namespace,
            route.logical.funcname,
            parsed.tool_calls.len()
        );
        history.push(
            serde_json::to_value(ChatMessage::Assistant {
                content: Value::Null,
                tool_calls: raw_tool_calls,
            })
            .unwrap(),
        );
        let child_context = ParallelChildTaskContext {
            current_depth: chain.current_depth,
            headers: chain.headers.clone(),
            debug_mocks: Arc::new(chain.debug_mocks.clone()),
            child_http_client: child_http_client.clone(),
            trace: trace.clone(),
            direct_child_depth: 2,
            logical_tenant: route.logical.tenant.clone(),
            logical_namespace: route.logical.namespace.clone(),
            logical_funcname: route.logical.funcname.clone(),
            allowed_skillep_ids: allowed_skillep_ids.clone(),
        };
        let child_tasks = FuturesUnordered::new();
        for (original_index, call) in parsed.tool_calls.into_iter().enumerate() {
            child_tasks.push(execute_parallel_child_call(
                ParallelChildTaskInput {
                    original_index,
                    call,
                },
                child_context.clone(),
            ));
        }

        let child_results = drive_parallel_child_tasks(child_tasks).await;
        for result in &child_results {
            history.push(
                serde_json::to_value(ChatMessage::Tool {
                    tool_call_id: result.tool_call_id.clone(),
                    content: result.tool_result.clone(),
                })
                .unwrap(),
            );
        }
        for result in &child_results {
            if let Some(usage) = child_result_usage(result) {
                if let Some(aggregate) = aggregate_usage.as_mut() {
                    aggregate.add_assign(usage);
                } else {
                    aggregate_usage = Some(usage.clone());
                }
            }
        }
    }
}

fn invalid_child_depth_response() -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "id": "skill-chain-error",
                "object": "chat.completion",
                "created": Utc::now().timestamp(),
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "Error: invalid chain depth"},
                    "finish_reason": "stop"
                }]
            }))
            .unwrap(),
        ))
        .unwrap()
}

pub(super) async fn handle_skill_call_chain(
    gw: &HttpGateway,
    wire: SkillChatCompletionsWireRequest,
    ctx: SkillInvocationContext,
) -> Result<Response, StatusCode> {
    let normalized = match normalize_skill_call(wire, &ctx) {
        Ok(n) => n,
        Err(NormalizationError::Http(status, message)) => {
            if ctx.is_child_request {
                warn!(
                    "skill_chain normalize_error logical={}/{}/{} path={} status={}",
                    ctx.route.logical.tenant,
                    ctx.route.logical.namespace,
                    ctx.route.logical.funcname,
                    ctx.remain_path,
                    status
                );
            }
            return Ok(Response::builder()
                .status(status)
                .body(Body::from(message))
                .unwrap());
        }
        Err(NormalizationError::CompletionError) => {
            warn!(
                "skill_chain invalid child depth logical={}/{}/{} path={}",
                ctx.route.logical.tenant,
                ctx.route.logical.namespace,
                ctx.route.logical.funcname,
                ctx.remain_path
            );
            return Ok(invalid_child_depth_response());
        }
    };

    // Convert normalized form to the internal chain state that execute_skill_chain uses.
    let history = normalized.prepared_body.history;
    let chain = SkillChainRequestState {
        template: normalized.prepared_body.template,
        headers: ctx.request_headers.clone(),
        current_depth: normalized.current_depth,
        debug_mocks: normalized.debug_mocks,
    };

    let allowed_arc = ctx.allowed_child_skilleps.clone();

    if normalized.response_mode == SkillResponseMode::Normal {
        return match execute_skill_chain(
            gw,
            chain,
            history,
            ctx.route,
            ctx.calling_tenant,
            ctx.logical_funcname,
            ctx.remain_path,
            ctx.prefix,
            ctx.skills_namespace,
            allowed_arc,
            None,
            ctx.cancel_token,
        )
        .await
        {
            Ok(final_resp) => {
                let mut resp = Response::builder().status(final_resp.status);
                for (key, value) in &final_resp.headers {
                    resp = resp.header(key, value);
                }
                Ok(resp.body(Body::from(final_resp.body)).unwrap())
            }
            Err(SkillChainExecutionError::DownstreamStatus(status)) => Err(status),
            Err(SkillChainExecutionError::Response(status, message)) => Ok(Response::builder()
                .status(status)
                .body(Body::from(message))
                .unwrap()),
        };
    }

    // SSE trace response (Basic or Verbose).
    let (tx, rx) = mpsc::channel::<Result<String, Infallible>>(32);
    let trace = SkillChainTraceState {
        tx: tx.clone(),
        root_call_id: Uuid::new_v4().to_string(),
        root_skill: ctx.display_skill_id,
        started_at: Instant::now(),
        include_content: normalized.effective_trace == SkillTraceLevel::Verbose,
    };

    let gw = gw.clone();
    let cancel_token = ctx.cancel_token;
    // For HTTP callers cancel_token starts as CancellationToken::new() (no lifecycle
    // source).  Wire tx.closed() to cancel it so that between-turn is_cancelled()
    // checks in execute_skill_chain fire when the SSE client drops the connection,
    // exactly as they do for MCP callers whose token is sourced from context.ct.
    // The oneshot lets the watcher exit (and drop its sender clone) as soon as
    // execution completes normally, so the SSE channel closes without waiting for
    // the HTTP client to disconnect.
    let (exec_done_tx, exec_done_rx) = tokio::sync::oneshot::channel::<()>();
    let cancel_for_watcher = cancel_token.clone();
    let tx_for_watcher = tx.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = tx_for_watcher.closed() => { cancel_for_watcher.cancel(); }
            _ = exec_done_rx => {}
        }
    });

    tokio::spawn(async move {
        let root_query = latest_user_query(&history);
        if !emit_root_call_start(&trace, root_query.as_deref()).await {
            return;
        }

        let trace_for_closed = trace.clone();
        let exec = execute_skill_chain(
            &gw,
            chain,
            history,
            ctx.route,
            ctx.calling_tenant,
            ctx.logical_funcname,
            ctx.remain_path,
            ctx.prefix,
            ctx.skills_namespace,
            allowed_arc,
            Some(trace.clone()),
            cancel_token.clone(),
        );
        tokio::pin!(exec);

        let result = tokio::select! {
            _ = trace_for_closed.tx.closed() => return,
            _ = cancel_token.cancelled() => return,
            result = &mut exec => result,
        };

        match result {
            Ok(final_resp) if final_resp.is_openai_chat_completion => {
                let _ =
                    emit_root_call_finish(&trace, "pass", None, final_resp.usage.as_ref()).await;
                let _ = emit_trace_skill_result(&trace, &final_resp.body).await;
            }
            Ok(_) => {
                let _ = emit_root_call_finish(
                    &trace,
                    "fail",
                    Some(SkillTraceFailReason::InvalidResponse),
                    None,
                )
                .await;
            }
            Err(SkillChainExecutionError::DownstreamStatus(_))
            | Err(SkillChainExecutionError::Response(_, _)) => {
                let _ = emit_root_call_finish(
                    &trace,
                    "fail",
                    Some(SkillTraceFailReason::TransportError),
                    None,
                )
                .await;
            }
        }
        let _ = emit_trace_done(&trace).await;
        let _ = exec_done_tx.send(());
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let body = Body::from_stream(http_body_util::StreamBody::new(stream));
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("X-Accel-Buffering", "no")
        .body(body)
        .unwrap())
}

#[cfg(test)]
mod tests {
    use super::{
        build_skill_chain_request_body, build_skill_chat_request, drive_parallel_child_tasks,
        execute_parallel_child_call, extract_is_child_request, normalize_skill_call,
        parse_skill_response, parse_skillep_tool_args, read_child_trace_sse,
        skill_chain_tool_definition, split_skillep_id, InternalSkillRequestProfile,
        NormalizationError, ParallelChildResult, ParallelChildTaskContext, ParallelChildTaskInput,
        ParsedSkillToolCall, SkillChainChildTraceState, SkillChainTraceState,
        SkillChatCompletionsWireRequest, SkillInvocationContext, SkillResponseMode,
        SkillTraceFailReason, SkillTraceField, SkillTraceLevel, SKILL_CHAIN_CHILD_HEADER,
        SKILL_CHAIN_DEPTH_HEADER,
    };
    use axum::http::{HeaderMap, HeaderValue};
    use futures::future::BoxFuture;
    use futures::stream::FuturesUnordered;
    use futures::FutureExt;
    use serde_json::{json, Value};
    use std::collections::{HashMap, HashSet};
    use std::convert::Infallible;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use tokio::sync::mpsc;

    // extract_is_child_request tests

    #[test]
    fn child_request_exact_one_is_detected() {
        let mut headers = HeaderMap::new();
        headers.insert(SKILL_CHAIN_CHILD_HEADER, HeaderValue::from_static("1"));
        assert!(extract_is_child_request(&headers));
    }

    #[test]
    fn child_request_missing_header_is_not_child() {
        let headers = HeaderMap::new();
        assert!(!extract_is_child_request(&headers));
    }

    #[test]
    fn child_request_non_one_values_are_not_child() {
        for val in ["0", "2", "true", "yes", ""] {
            let mut headers = HeaderMap::new();
            headers.insert(
                SKILL_CHAIN_CHILD_HEADER,
                HeaderValue::from_str(val).unwrap(),
            );
            assert!(
                !extract_is_child_request(&headers),
                "expected not-child for value={:?}",
                val
            );
        }
    }

    #[test]
    fn parse_skillep_tool_args_requires_stringified_json() {
        let parsed =
            parse_skillep_tool_args(r#"{"skillep_id":"acme/default/pricing","query":"what now"}"#)
                .unwrap();
        assert_eq!(parsed.0, "acme/default/pricing");
        assert_eq!(parsed.1, "what now");
        assert_eq!(
            parse_skillep_tool_args("{bad json}").unwrap_err(),
            "Error: invalid tool arguments"
        );
    }

    #[test]
    fn split_skillep_id_requires_three_segments() {
        assert_eq!(
            split_skillep_id("acme/default/pricing").unwrap(),
            ("acme", "default", "pricing")
        );
        assert_eq!(
            split_skillep_id("acme/default").unwrap_err(),
            "Error: invalid skillep_id 'acme/default'. Expected format: owner_tenant/namespace/skillname. Do not retry with the same value."
        );
    }

    #[test]
    fn parse_skill_response_detects_tool_call_and_final_text() {
        let tool_resp = json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "call_skillep",
                            "arguments": "{\"skillep_id\":\"acme/default/pricing\",\"query\":\"q\"}"
                        }
                    }]
                }
            }]
        });
        let parsed_tool = parse_skill_response(&serde_json::to_vec(&tool_resp).unwrap()).unwrap();
        assert_eq!(parsed_tool.tool_calls.len(), 1);
        assert_eq!(parsed_tool.tool_calls[0].name, "call_skillep");

        let final_resp = json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": "done"}
            }]
        });
        let parsed_final = parse_skill_response(&serde_json::to_vec(&final_resp).unwrap()).unwrap();
        assert!(parsed_final.tool_calls.is_empty());
        assert_eq!(parsed_final.final_text.as_deref(), Some("done"));
    }

    #[test]
    fn build_skill_chain_request_body_serializes_messages_outside_template() {
        let wire: SkillChatCompletionsWireRequest = serde_json::from_str(
            r#"{
                "messages":[{"role":"user","content":"hi"}],
                "tool_choice":"auto",
                "top_p":0.9,
                "skill_trace":1
            }"#,
        )
        .unwrap();
        let ctx = make_test_ctx(false, None, false, None);
        let normalized = normalize_skill_call(wire, &ctx).unwrap();

        let template_json = serde_json::to_value(&normalized.prepared_body.template).unwrap();
        assert!(template_json.get("messages").is_none());
        assert_eq!(template_json.get("stream"), Some(&json!(false)));
        assert_eq!(template_json.get("skill_trace"), Some(&json!(1)));
        assert!(template_json.get("tool_choice").is_none());
        assert_eq!(template_json.get("top_p"), Some(&json!(0.9)));

        let body = build_skill_chain_request_body(
            &normalized.prepared_body.template,
            &normalized.prepared_body.history,
        )
        .unwrap();
        let body_json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body_json.get("messages"),
            Some(&json!([{"role":"user","content":"hi"}]))
        );
        assert_eq!(body_json.get("stream"), Some(&json!(false)));
    }

    #[tokio::test]
    async fn drive_parallel_child_tasks_commits_results_in_original_order() {
        let tasks: FuturesUnordered<BoxFuture<'static, ParallelChildResult>> =
            FuturesUnordered::new();
        tasks.push(
            async {
                tokio::time::sleep(Duration::from_millis(40)).await;
                ParallelChildResult {
                    original_index: 2,
                    tool_call_id: "call_3".to_string(),
                    tool_result: "third".to_string(),
                    fail_reason: None,
                    usage: None,
                }
            }
            .boxed(),
        );
        tasks.push(
            async {
                tokio::time::sleep(Duration::from_millis(5)).await;
                ParallelChildResult {
                    original_index: 1,
                    tool_call_id: "call_2".to_string(),
                    tool_result: "second".to_string(),
                    fail_reason: Some(SkillTraceFailReason::TransportError),
                    usage: None,
                }
            }
            .boxed(),
        );
        tasks.push(
            async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                ParallelChildResult {
                    original_index: 0,
                    tool_call_id: "call_1".to_string(),
                    tool_result: "first".to_string(),
                    fail_reason: None,
                    usage: None,
                }
            }
            .boxed(),
        );

        let results = drive_parallel_child_tasks(tasks).await;
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].tool_call_id, "call_1");
        assert_eq!(results[1].tool_call_id, "call_2");
        assert_eq!(results[2].tool_call_id, "call_3");
        assert_eq!(
            results[1].fail_reason,
            Some(SkillTraceFailReason::TransportError)
        );
    }

    #[tokio::test]
    async fn drive_parallel_child_tasks_runs_siblings_concurrently() {
        let tasks: FuturesUnordered<BoxFuture<'static, ParallelChildResult>> =
            FuturesUnordered::new();
        tasks.push(
            async {
                tokio::time::sleep(Duration::from_millis(60)).await;
                ParallelChildResult {
                    original_index: 0,
                    tool_call_id: "call_1".to_string(),
                    tool_result: "one".to_string(),
                    fail_reason: None,
                    usage: None,
                }
            }
            .boxed(),
        );
        tasks.push(
            async {
                tokio::time::sleep(Duration::from_millis(60)).await;
                ParallelChildResult {
                    original_index: 1,
                    tool_call_id: "call_2".to_string(),
                    tool_result: "two".to_string(),
                    fail_reason: None,
                    usage: None,
                }
            }
            .boxed(),
        );

        let started_at = Instant::now();
        let results = drive_parallel_child_tasks(tasks).await;
        let elapsed = started_at.elapsed();

        assert_eq!(results.len(), 2);
        assert!(elapsed < Duration::from_millis(110), "elapsed={elapsed:?}");
    }

    #[tokio::test]
    async fn execute_parallel_child_call_returns_error_for_invalid_args_without_trace_failure() {
        let result = execute_parallel_child_call(
            ParallelChildTaskInput {
                original_index: 0,
                call: ParsedSkillToolCall {
                    id: "call_bad".to_string(),
                    name: "call_skillep".to_string(),
                    arguments: "{bad json}".to_string(),
                },
            },
            ParallelChildTaskContext {
                current_depth: 1,
                headers: HeaderMap::new(),
                debug_mocks: Arc::new(HashMap::new()),
                child_http_client: reqwest::Client::new(),
                trace: None,
                direct_child_depth: 2,
                logical_tenant: "acme".to_string(),
                logical_namespace: "skills".to_string(),
                logical_funcname: "parent".to_string(),
                allowed_skillep_ids: None,
            },
        )
        .await;

        assert_eq!(result.tool_call_id, "call_bad");
        assert_eq!(result.tool_result, "Error: invalid tool arguments");
        assert_eq!(result.fail_reason, None);
    }

    #[tokio::test]
    async fn execute_parallel_child_call_returns_error_for_invalid_skillep_id() {
        let result = execute_parallel_child_call(
            ParallelChildTaskInput {
                original_index: 0,
                call: ParsedSkillToolCall {
                    id: "call_bad_id".to_string(),
                    name: "call_skillep".to_string(),
                    arguments: "{\"skillep_id\":\"acme/default\",\"query\":\"what now\"}"
                        .to_string(),
                },
            },
            ParallelChildTaskContext {
                current_depth: 1,
                headers: HeaderMap::new(),
                debug_mocks: Arc::new(HashMap::new()),
                child_http_client: reqwest::Client::new(),
                trace: None,
                direct_child_depth: 2,
                logical_tenant: "acme".to_string(),
                logical_namespace: "skills".to_string(),
                logical_funcname: "parent".to_string(),
                allowed_skillep_ids: None,
            },
        )
        .await;

        assert_eq!(result.tool_call_id, "call_bad_id");
        assert_eq!(result.tool_result, "Error: invalid skillep_id 'acme/default'. Expected format: owner_tenant/namespace/skillname. Do not retry with the same value.");
        assert_eq!(result.fail_reason, None);
    }

    #[tokio::test]
    async fn execute_parallel_child_call_supports_debug_mock_results() {
        let mut debug_mocks = HashMap::new();
        debug_mocks.insert(
            "acme/default/pricing".to_string(),
            "mock child result".to_string(),
        );
        let result = execute_parallel_child_call(
            ParallelChildTaskInput {
                original_index: 0,
                call: ParsedSkillToolCall {
                    id: "call_mock".to_string(),
                    name: "call_skillep".to_string(),
                    arguments: "{\"skillep_id\":\"acme/default/pricing\",\"query\":\"what now\"}"
                        .to_string(),
                },
            },
            ParallelChildTaskContext {
                current_depth: 1,
                headers: HeaderMap::new(),
                debug_mocks: Arc::new(debug_mocks),
                child_http_client: reqwest::Client::new(),
                trace: None,
                direct_child_depth: 2,
                logical_tenant: "acme".to_string(),
                logical_namespace: "skills".to_string(),
                logical_funcname: "parent".to_string(),
                allowed_skillep_ids: None,
            },
        )
        .await;

        assert_eq!(result.tool_call_id, "call_mock");
        assert_eq!(result.tool_result, "mock child result");
        assert_eq!(result.fail_reason, None);
    }

    #[tokio::test]
    async fn execute_parallel_child_call_emits_trace_events_for_debug_mock_results() {
        let mut debug_mocks = HashMap::new();
        debug_mocks.insert(
            "acme/default/pricing".to_string(),
            "mock child result".to_string(),
        );
        let (tx, mut rx) = mpsc::channel::<Result<String, Infallible>>(8);
        let trace = SkillChainTraceState {
            tx,
            root_call_id: "root_call".to_string(),
            root_skill: "acme/skills/parent".to_string(),
            started_at: Instant::now(),
            include_content: true,
        };

        let result = execute_parallel_child_call(
            ParallelChildTaskInput {
                original_index: 0,
                call: ParsedSkillToolCall {
                    id: "call_mock".to_string(),
                    name: "call_skillep".to_string(),
                    arguments: "{\"skillep_id\":\"acme/default/pricing\",\"query\":\"what now\"}"
                        .to_string(),
                },
            },
            ParallelChildTaskContext {
                current_depth: 1,
                headers: HeaderMap::new(),
                debug_mocks: Arc::new(debug_mocks),
                child_http_client: reqwest::Client::new(),
                trace: Some(trace),
                direct_child_depth: 2,
                logical_tenant: "acme".to_string(),
                logical_namespace: "skills".to_string(),
                logical_funcname: "parent".to_string(),
                allowed_skillep_ids: None,
            },
        )
        .await;

        assert_eq!(result.tool_call_id, "call_mock");
        assert_eq!(result.tool_result, "mock child result");
        assert_eq!(result.fail_reason, None);

        let start_chunk = rx.recv().await.unwrap().unwrap();
        let finish_chunk = rx.recv().await.unwrap().unwrap();
        assert!(start_chunk.contains("event: skill_trace"));
        assert!(start_chunk.contains("\"type\":\"subcall_start\""));
        assert!(start_chunk.contains("\"skill\":\"acme/default/pricing\""));
        assert!(start_chunk.contains("\"query\":\"what now\""));

        assert!(finish_chunk.contains("event: skill_trace"));
        assert!(finish_chunk.contains("\"type\":\"subcall_finish\""));
        assert!(finish_chunk.contains("\"result_code\":\"pass\""));
        assert!(finish_chunk.contains("\"output\":\"mock child result\""));
    }

    #[tokio::test]
    async fn drive_parallel_child_tasks_preserves_mixed_success_and_failure() {
        let tasks: FuturesUnordered<BoxFuture<'static, ParallelChildResult>> =
            FuturesUnordered::new();
        tasks.push(
            async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                ParallelChildResult {
                    original_index: 1,
                    tool_call_id: "call_fail".to_string(),
                    tool_result: "Error: child transport error".to_string(),
                    fail_reason: Some(SkillTraceFailReason::TransportError),
                    usage: None,
                }
            }
            .boxed(),
        );
        tasks.push(
            async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                ParallelChildResult {
                    original_index: 0,
                    tool_call_id: "call_ok".to_string(),
                    tool_result: "child ok".to_string(),
                    fail_reason: None,
                    usage: None,
                }
            }
            .boxed(),
        );

        let results = drive_parallel_child_tasks(tasks).await;
        assert_eq!(results[0].tool_result, "child ok");
        assert_eq!(results[1].tool_result, "Error: child transport error");
        assert_eq!(
            results[1].fail_reason,
            Some(SkillTraceFailReason::TransportError)
        );
    }

    #[test]
    fn skill_chain_tool_definition_none_gives_unconstrained_tool() {
        let tools = skill_chain_tool_definition(None);
        let arr = tools.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        let props = &arr[0]["function"]["parameters"]["properties"]["skillep_id"];
        assert!(props.get("enum").is_none());
        assert_eq!(props["type"].as_str().unwrap(), "string");
    }

    #[test]
    fn skill_chain_tool_definition_empty_gives_no_tools() {
        let tools = skill_chain_tool_definition(Some(&HashSet::new()));
        assert_eq!(tools.as_array().unwrap().len(), 0);
    }

    #[test]
    fn skill_chain_tool_definition_non_empty_gives_enum() {
        let mut allowed = HashSet::new();
        allowed.insert("acme/default/pricing".to_string());
        let tools = skill_chain_tool_definition(Some(&allowed));
        let arr = tools.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        let enum_vals = arr[0]["function"]["parameters"]["properties"]["skillep_id"]["enum"]
            .as_array()
            .unwrap();
        assert_eq!(enum_vals.len(), 1);
        assert_eq!(enum_vals[0].as_str().unwrap(), "acme/default/pricing");
    }

    #[tokio::test]
    async fn execute_parallel_child_call_blocks_empty_allowlist() {
        let mut debug_mocks = HashMap::new();
        debug_mocks.insert(
            "acme/default/pricing".to_string(),
            "mock child result".to_string(),
        );
        let result = execute_parallel_child_call(
            ParallelChildTaskInput {
                original_index: 0,
                call: ParsedSkillToolCall {
                    id: "call_blocked".to_string(),
                    name: "call_skillep".to_string(),
                    arguments: "{\"skillep_id\":\"acme/default/pricing\",\"query\":\"q\"}"
                        .to_string(),
                },
            },
            ParallelChildTaskContext {
                current_depth: 1,
                headers: HeaderMap::new(),
                debug_mocks: Arc::new(debug_mocks),
                child_http_client: reqwest::Client::new(),
                trace: None,
                direct_child_depth: 2,
                logical_tenant: "acme".to_string(),
                logical_namespace: "skills".to_string(),
                logical_funcname: "parent".to_string(),
                allowed_skillep_ids: Some(Arc::new(HashSet::new())),
            },
        )
        .await;

        assert_eq!(result.tool_call_id, "call_blocked");
        assert_eq!(
            result.tool_result,
            "Error: skill endpoint not in allowed list"
        );
        assert_eq!(result.fail_reason, None);
    }

    #[tokio::test]
    async fn execute_parallel_child_call_permits_listed_skillep_id() {
        let mut debug_mocks = HashMap::new();
        debug_mocks.insert(
            "acme/default/pricing".to_string(),
            "mock child result".to_string(),
        );
        let mut allowed = HashSet::new();
        allowed.insert("acme/default/pricing".to_string());
        let result = execute_parallel_child_call(
            ParallelChildTaskInput {
                original_index: 0,
                call: ParsedSkillToolCall {
                    id: "call_ok".to_string(),
                    name: "call_skillep".to_string(),
                    arguments: "{\"skillep_id\":\"acme/default/pricing\",\"query\":\"q\"}"
                        .to_string(),
                },
            },
            ParallelChildTaskContext {
                current_depth: 1,
                headers: HeaderMap::new(),
                debug_mocks: Arc::new(debug_mocks),
                child_http_client: reqwest::Client::new(),
                trace: None,
                direct_child_depth: 2,
                logical_tenant: "acme".to_string(),
                logical_namespace: "skills".to_string(),
                logical_funcname: "parent".to_string(),
                allowed_skillep_ids: Some(Arc::new(allowed)),
            },
        )
        .await;

        assert_eq!(result.tool_call_id, "call_ok");
        assert_eq!(result.tool_result, "mock child result");
        assert_eq!(result.fail_reason, None);
    }

    #[tokio::test]
    async fn execute_parallel_child_call_blocks_unlisted_skillep_id() {
        let mut debug_mocks = HashMap::new();
        debug_mocks.insert(
            "acme/default/pricing".to_string(),
            "mock child result".to_string(),
        );
        let mut allowed = HashSet::new();
        allowed.insert("acme/default/search".to_string());
        let result = execute_parallel_child_call(
            ParallelChildTaskInput {
                original_index: 0,
                call: ParsedSkillToolCall {
                    id: "call_notallowed".to_string(),
                    name: "call_skillep".to_string(),
                    arguments: "{\"skillep_id\":\"acme/default/pricing\",\"query\":\"q\"}"
                        .to_string(),
                },
            },
            ParallelChildTaskContext {
                current_depth: 1,
                headers: HeaderMap::new(),
                debug_mocks: Arc::new(debug_mocks),
                child_http_client: reqwest::Client::new(),
                trace: None,
                direct_child_depth: 2,
                logical_tenant: "acme".to_string(),
                logical_namespace: "skills".to_string(),
                logical_funcname: "parent".to_string(),
                allowed_skillep_ids: Some(Arc::new(allowed)),
            },
        )
        .await;

        assert_eq!(result.tool_call_id, "call_notallowed");
        assert_eq!(
            result.tool_result,
            "Error: skill endpoint not in allowed list"
        );
        assert_eq!(result.fail_reason, None);
    }

    #[tokio::test]
    async fn execute_parallel_child_call_none_allowlist_passthrough() {
        let mut debug_mocks = HashMap::new();
        debug_mocks.insert(
            "acme/default/pricing".to_string(),
            "mock child result".to_string(),
        );
        let result = execute_parallel_child_call(
            ParallelChildTaskInput {
                original_index: 0,
                call: ParsedSkillToolCall {
                    id: "call_pass".to_string(),
                    name: "call_skillep".to_string(),
                    arguments: "{\"skillep_id\":\"acme/default/pricing\",\"query\":\"q\"}"
                        .to_string(),
                },
            },
            ParallelChildTaskContext {
                current_depth: 1,
                headers: HeaderMap::new(),
                debug_mocks: Arc::new(debug_mocks),
                child_http_client: reqwest::Client::new(),
                trace: None,
                direct_child_depth: 2,
                logical_tenant: "acme".to_string(),
                logical_namespace: "skills".to_string(),
                logical_funcname: "parent".to_string(),
                allowed_skillep_ids: None,
            },
        )
        .await;

        assert_eq!(result.tool_call_id, "call_pass");
        assert_eq!(result.tool_result, "mock child result");
        assert_eq!(result.fail_reason, None);
    }

    // Regression: read_child_trace_sse must return promptly after [DONE] even when
    // the server keeps the HTTP connection open (never sends the final chunked EOF).
    // Before the fix, the watcher's tx clone kept the mpsc channel alive indefinitely,
    // causing read_child_trace_sse to block in stream.next() after [DONE] was seen.
    #[tokio::test]
    async fn read_child_trace_sse_completes_at_done_without_transport_eof() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // A complete child skill trace that ends with [DONE].
        // The server will NOT close the connection afterwards, simulating the
        // watcher-sender deadlock that existed before the break 'outer fix.
        let sse_payload = concat!(
            "event: skill_trace\n",
            "data: {\"type\":\"call_start\",\"call_id\":\"c1\",\"parent_call_id\":null,\"depth\":1,\"skill\":\"acme/default/child\",\"ts_ms\":0}\n\n",
            "event: skill_trace\n",
            "data: {\"type\":\"call_finish\",\"call_id\":\"c1\",\"parent_call_id\":null,\"depth\":1,\"skill\":\"acme/default/child\",\"ts_ms\":1,\"result_code\":\"pass\"}\n\n",
            "event: skill_result\n",
            "data: {\"choices\":[{\"finish_reason\":\"stop\",\"message\":{\"content\":\"42\"}}]}\n\n",
            "data: [DONE]\n\n",
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = socket.read(&mut buf).await;
            // Send all SSE data as one HTTP/1.1 chunked response, then hang.
            let chunk = format!("{:x}\r\n{}\r\n", sse_payload.len(), sse_payload);
            let http_resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n{}",
                chunk
            );
            socket.write_all(http_resp.as_bytes()).await.unwrap();
            // Intentionally omit the terminal "0\r\n\r\n" chunk so EOF never arrives.
            tokio::time::sleep(Duration::from_secs(30)).await;
        });

        let resp = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{}", port))
            .send()
            .await
            .unwrap();

        let (tx, _rx) = mpsc::channel::<Result<String, Infallible>>(32);
        let trace = SkillChainTraceState {
            tx,
            root_call_id: "root_id".to_string(),
            root_skill: "acme/default/parent".to_string(),
            started_at: Instant::now(),
            include_content: false,
        };
        let child_trace = SkillChainChildTraceState {
            call_id: "child_call_id".to_string(),
            skill: "acme/default/child".to_string(),
            started_at: Instant::now(),
            local_root_call_id: Arc::new(Mutex::new(None)),
        };

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            read_child_trace_sse(&trace, &child_trace, 2, resp),
        )
        .await
        .expect(
            "read_child_trace_sse must complete after [DONE] without waiting for transport EOF",
        );

        let result = result.unwrap();
        assert!(
            result.skill_result.is_some(),
            "skill_result must be captured"
        );
        assert_eq!(
            result.terminal_result_code.as_deref(),
            Some("pass"),
            "call_finish result_code must be forwarded"
        );
    }

    // ============================================================
    // Context-gating tests
    // ============================================================

    fn make_test_ctx(
        is_child_request: bool,
        child_chain_depth: Option<u32>,
        is_debug_authorized: bool,
        allowed_child_skilleps: Option<Arc<HashSet<String>>>,
    ) -> SkillInvocationContext {
        use super::super::func_agent_mgr::{FuncIdentity, FuncRouteTarget};
        use inferxlib::data_obj::DataObject;
        use inferxlib::obj_mgr::func_mgr::FuncObject;
        use inferxlib::obj_mgr::funcpolicy_mgr::{
            FuncPolicySpec, RuntimeConfig, ScaleOutPolicy, WaitQueueRatio,
        };

        let dummy_id = FuncIdentity {
            tenant: "t".to_string(),
            namespace: "ns".to_string(),
            funcname: "f".to_string(),
            version: 0,
        };
        let dummy_func = DataObject {
            objType: "function".to_string(),
            tenant: "t".to_string(),
            namespace: "ns".to_string(),
            name: "f".to_string(),
            labels: Default::default(),
            annotations: Default::default(),
            channelRev: 0,
            srcEpoch: 0,
            revision: 0,
            object: FuncObject::default(),
        };
        let dummy_policy = FuncPolicySpec {
            minReplica: 0,
            maxReplica: 1,
            standbyPerNode: 0,
            parallel: 50,
            queueLen: 100,
            queueTimeout: 30.0,
            scaleoutPolicy: ScaleOutPolicy::WaitQueueRatio(WaitQueueRatio { waitRatio: 0.1 }),
            scaleinTimeout: 1.0,
            runtimeConfig: RuntimeConfig { GraphSync: false },
        };
        SkillInvocationContext {
            route: FuncRouteTarget {
                logical: dummy_id.clone(),
                physical: dummy_id,
                func: dummy_func,
                policy: dummy_policy,
                caller_tenant: None,
            },
            calling_tenant: "t".to_string(),
            display_skill_id: "t/ns/skill".to_string(),
            logical_funcname: "f".to_string(),
            remain_path: "/v1/chat/completions".to_string(),
            prefix: String::new(),
            skills_namespace: "skills".to_string(),
            is_child_request,
            child_chain_depth,
            is_debug_authorized,
            allowed_child_skilleps,
            cancel_token: tokio_util::sync::CancellationToken::new(),
            request_headers: HeaderMap::new(),
        }
    }

    fn make_wire(skill_trace_json: Option<&str>) -> SkillChatCompletionsWireRequest {
        let json = match skill_trace_json {
            None => r#"{"messages":[{"role":"user","content":"hi"}]}"#.to_string(),
            Some(v) => format!(
                r#"{{"messages":[{{"role":"user","content":"hi"}}],"skill_trace":{}}}"#,
                v
            ),
        };
        serde_json::from_str(&json).unwrap()
    }

    // SkillChatCompletionsWireRequest deserialization

    #[test]
    fn wire_request_absent_skill_trace_is_absent() {
        let wire = make_wire(None);
        assert!(matches!(wire.skill_trace, SkillTraceField::Absent));
    }

    #[test]
    fn wire_request_skill_trace_numeric_values() {
        for (v, json) in [(0u64, "0"), (1, "1"), (2, "2")] {
            let wire = make_wire(Some(json));
            match &wire.skill_trace {
                SkillTraceField::Present(Value::Number(n)) => {
                    assert_eq!(n.as_u64(), Some(v));
                }
                other => panic!("expected Present(Number({})), got {:?}", v, other),
            }
        }
    }

    #[test]
    fn wire_request_null_skill_trace_is_present_null() {
        let wire = make_wire(Some("null"));
        assert!(matches!(
            wire.skill_trace,
            SkillTraceField::Present(Value::Null)
        ));
    }

    #[test]
    fn wire_request_extra_fields_are_captured() {
        let wire: SkillChatCompletionsWireRequest =
            serde_json::from_str(r#"{"messages":[],"top_p":0.9,"stop":["END"]}"#).unwrap();
        assert_eq!(wire.extra.get("top_p"), Some(&json!(0.9)));
        assert_eq!(wire.extra.get("stop"), Some(&json!(["END"])));
    }

    #[test]
    fn wire_request_mcp_serializes_with_max_tokens_and_temperature() {
        let w = build_skill_chat_request(
            "q",
            SkillTraceLevel::Basic,
            InternalSkillRequestProfile::McpToolLoopback,
        );
        let v = serde_json::to_value(&w).unwrap();
        assert_eq!(v["max_tokens"], json!(5000));
        assert_eq!(v["temperature"], json!(0.0));
        assert_eq!(v["stream"], json!(false));
        assert_eq!(v["skill_trace"], json!(1));
    }

    #[test]
    fn wire_request_child_serializes_without_max_tokens() {
        let w = build_skill_chat_request(
            "q",
            SkillTraceLevel::None,
            InternalSkillRequestProfile::ChildSkillLoopback,
        );
        let v = serde_json::to_value(&w).unwrap();
        assert!(!v.as_object().unwrap().contains_key("max_tokens"));
        assert!(!v.as_object().unwrap().contains_key("temperature"));
        assert_eq!(v["stream"], json!(false));
        assert_eq!(v["skill_trace"], json!(0));
    }

    // normalize_skill_call tests

    #[test]
    fn normalize_absent_trace_gives_none_level_and_normal_mode() {
        let wire = make_wire(None);
        let ctx = make_test_ctx(false, None, false, None);
        let n = normalize_skill_call(wire, &ctx).unwrap();
        assert_eq!(n.effective_trace, SkillTraceLevel::None);
        assert_eq!(n.response_mode, SkillResponseMode::Normal);
        assert_eq!(n.current_depth, 1);
    }

    #[test]
    fn normalize_trace_0_gives_none_level() {
        let wire = make_wire(Some("0"));
        let ctx = make_test_ctx(false, None, false, None);
        let n = normalize_skill_call(wire, &ctx).unwrap();
        assert_eq!(n.effective_trace, SkillTraceLevel::None);
        assert_eq!(n.response_mode, SkillResponseMode::Normal);
    }

    #[test]
    fn normalize_trace_1_gives_basic_and_sse_mode() {
        let wire = make_wire(Some("1"));
        let ctx = make_test_ctx(false, None, false, None);
        let n = normalize_skill_call(wire, &ctx).unwrap();
        assert_eq!(n.effective_trace, SkillTraceLevel::Basic);
        assert_eq!(n.response_mode, SkillResponseMode::SseTrace);
    }

    #[test]
    fn normalize_trace_2_gives_verbose_when_debug_authorized() {
        let wire = make_wire(Some("2"));
        let ctx = make_test_ctx(false, None, true, None);
        let n = normalize_skill_call(wire, &ctx).unwrap();
        assert_eq!(n.effective_trace, SkillTraceLevel::Verbose);
        assert_eq!(n.response_mode, SkillResponseMode::SseTrace);
    }

    #[test]
    fn normalize_trace_null_returns_400() {
        let wire = make_wire(Some("null"));
        let ctx = make_test_ctx(false, None, false, None);
        let err = normalize_skill_call(wire, &ctx).unwrap_err();
        assert!(
            matches!(err, NormalizationError::Http(s, _) if s == hyper::StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn normalize_trace_out_of_range_returns_400() {
        for bad in ["3", "\"1\"", "-1"] {
            let wire = make_wire(Some(bad));
            let ctx = make_test_ctx(false, None, false, None);
            let err = normalize_skill_call(wire, &ctx).unwrap_err();
            assert!(
                matches!(err, NormalizationError::Http(s, _) if s == hyper::StatusCode::BAD_REQUEST),
                "expected 400 for skill_trace={}",
                bad
            );
        }
    }

    #[test]
    fn normalize_verbose_without_auth_returns_403() {
        let wire = make_wire(Some("2"));
        let ctx = make_test_ctx(false, None, false, None);
        let err = normalize_skill_call(wire, &ctx).unwrap_err();
        assert!(matches!(err, NormalizationError::Http(s, _) if s == hyper::StatusCode::FORBIDDEN));
    }

    #[test]
    fn normalize_verbose_on_child_does_not_need_auth() {
        let wire = make_wire(Some("2"));
        let ctx = make_test_ctx(true, Some(2), false, None);
        let n = normalize_skill_call(wire, &ctx).unwrap();
        assert_eq!(n.effective_trace, SkillTraceLevel::Verbose);
    }

    #[test]
    fn normalize_child_depth_zero_returns_completion_error() {
        let wire = make_wire(None);
        let ctx = make_test_ctx(true, Some(0), false, None);
        assert!(matches!(
            normalize_skill_call(wire, &ctx).unwrap_err(),
            NormalizationError::CompletionError
        ));
    }

    #[test]
    fn normalize_child_missing_depth_returns_completion_error() {
        let wire = make_wire(None);
        let ctx = make_test_ctx(true, None, false, None);
        assert!(matches!(
            normalize_skill_call(wire, &ctx).unwrap_err(),
            NormalizationError::CompletionError
        ));
    }

    #[test]
    fn normalize_child_valid_depth_propagates() {
        let wire = make_wire(None);
        let ctx = make_test_ctx(true, Some(3), false, None);
        let n = normalize_skill_call(wire, &ctx).unwrap();
        assert_eq!(n.current_depth, 3);
    }

    #[test]
    fn normalize_root_nonempty_tools_returns_400() {
        let wire: SkillChatCompletionsWireRequest =
            serde_json::from_str(r#"{"messages":[],"tools":[{"type":"function"}]}"#).unwrap();
        let ctx = make_test_ctx(false, None, false, None);
        let err = normalize_skill_call(wire, &ctx).unwrap_err();
        assert!(
            matches!(err, NormalizationError::Http(s, _) if s == hyper::StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn normalize_mocks_extracted_into_debug_mocks() {
        let wire: SkillChatCompletionsWireRequest =
            serde_json::from_str(r#"{"messages":[],"mocks":{"acme/default/p":"result"}}"#).unwrap();
        let ctx = make_test_ctx(false, None, false, None);
        let n = normalize_skill_call(wire, &ctx).unwrap();
        assert_eq!(
            n.debug_mocks.get("acme/default/p").map(|s| s.as_str()),
            Some("result")
        );
    }

    // build_skill_chat_request tests

    #[test]
    fn build_mcp_request_includes_max_tokens_and_temperature() {
        let w = build_skill_chat_request(
            "hello",
            SkillTraceLevel::Basic,
            InternalSkillRequestProfile::McpToolLoopback,
        );
        assert_eq!(w.max_tokens, Some(5000));
        assert_eq!(w.temperature, Some(0.0));
        assert_eq!(w.stream, Some(false));
        assert_eq!(
            w.messages,
            vec![json!({"role": "user", "content": "hello"})]
        );
    }

    #[test]
    fn build_child_request_omits_max_tokens_and_temperature() {
        let w = build_skill_chat_request(
            "hello",
            SkillTraceLevel::None,
            InternalSkillRequestProfile::ChildSkillLoopback,
        );
        assert_eq!(w.max_tokens, None);
        assert_eq!(w.temperature, None);
        assert_eq!(w.stream, Some(false));
    }

    #[test]
    fn build_child_request_verbose_trace_encodes_2() {
        let w = build_skill_chat_request(
            "q",
            SkillTraceLevel::Verbose,
            InternalSkillRequestProfile::ChildSkillLoopback,
        );
        match &w.skill_trace {
            SkillTraceField::Present(Value::Number(n)) => assert_eq!(n.as_u64(), Some(2)),
            other => panic!("unexpected skill_trace: {:?}", other),
        }
    }

    // apply_internal_child_headers tests

    #[test]
    fn apply_child_headers_attaches_child_and_depth() {
        let client = reqwest::Client::new();
        let req = client.post("http://127.0.0.1:1/test");
        let req = super::apply_internal_child_headers(req, 3);
        let built = req.build().unwrap();
        assert_eq!(
            built
                .headers()
                .get(SKILL_CHAIN_CHILD_HEADER)
                .and_then(|v| v.to_str().ok()),
            Some("1")
        );
        assert_eq!(
            built
                .headers()
                .get(SKILL_CHAIN_DEPTH_HEADER)
                .and_then(|v| v.to_str().ok()),
            Some("3")
        );
    }

    // skill_trace template forwarding tests

    #[test]
    fn normalize_explicit_skill_trace_is_forwarded_to_template() {
        let wire = make_wire(Some("1"));
        let ctx = make_test_ctx(false, None, false, None);
        let n = normalize_skill_call(wire, &ctx).unwrap();
        let template = serde_json::to_value(&n.prepared_body.template).unwrap();
        assert_eq!(
            template.get("skill_trace"),
            Some(&json!(1)),
            "explicit skill_trace must be forwarded to the template body"
        );
    }

    #[test]
    fn normalize_absent_skill_trace_is_not_inserted_into_template() {
        let wire = make_wire(None);
        let ctx = make_test_ctx(false, None, false, None);
        let n = normalize_skill_call(wire, &ctx).unwrap();
        let template = serde_json::to_value(&n.prepared_body.template).unwrap();
        assert!(
            template.get("skill_trace").is_none(),
            "absent skill_trace must not appear in the template body"
        );
    }

    #[test]
    fn normalize_explicit_zero_skill_trace_is_forwarded_to_template() {
        let wire = make_wire(Some("0"));
        let ctx = make_test_ctx(false, None, false, None);
        let n = normalize_skill_call(wire, &ctx).unwrap();
        let template = serde_json::to_value(&n.prepared_body.template).unwrap();
        assert_eq!(
            template.get("skill_trace"),
            Some(&json!(0)),
            "explicit skill_trace=0 must be forwarded, not silently dropped"
        );
    }

    // invalid child depth completion-error response tests

    #[tokio::test]
    async fn invalid_child_depth_response_is_200_ok_with_completion_json() {
        use axum::http::header::CONTENT_TYPE;
        let resp = super::invalid_child_depth_response();
        assert_eq!(resp.status(), hyper::StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(body["object"], "chat.completion");
        assert_eq!(
            body["choices"][0]["message"]["content"],
            "Error: invalid chain depth"
        );
        assert_eq!(body["choices"][0]["finish_reason"], "stop");
        assert_eq!(body["id"], "skill-chain-error");
    }
}
