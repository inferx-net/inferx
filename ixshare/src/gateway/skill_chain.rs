use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue};
use axum::response::Response;
use chrono::Utc;
use futures::StreamExt;
use http_body_util::BodyExt;
use hyper::header::CONTENT_TYPE;
use hyper::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::func_agent_mgr::FuncRouteTarget;
use super::http_gateway::{dispatch_func_call, HttpGateway, GATEWAY_CONFIG};

const SKILL_CHAIN_CHILD_HEADER: &str = "X-Inferx-Skill-Chain-Child";
const SKILL_CHAIN_DEPTH_HEADER: &str = "X-Chain-Depth";
const SKILL_CHAIN_TIMEOUT_HEADER: &str = "X-Inferx-Timeout";
const SKILL_TRACE_HEADER: &str = "X-Skill-Trace";
const SKILL_TRACE_CONTENT_HEADER: &str = "X-Skill-Trace-Content";
const SKILL_CHAIN_MAX_DEPTH: u32 = 5;
const SKILL_CHAIN_TOOL_NAME: &str = "call_skillep";
const CLIENT_PROVIDED_TOOLS_ERROR: &str = "skill endpoint does not accept client-provided tools";
const SKILL_TRACE_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
#[derive(Clone, Debug)]
struct SkillChainRequestState {
    template: Value,
    history: Vec<Value>,
    headers: HeaderMap,
    current_depth: u32,
    timeout_budget_sec: f64,
    started_at: std::time::Instant,
    debug_mocks: std::collections::HashMap<String, String>,
}

#[derive(Clone, Debug)]
struct ParsedSkillToolCall {
    id: String,
    name: String,
    arguments: String,
    raw: Value,
}

#[derive(Clone, Debug)]
struct ParsedSkillResponse {
    tool_calls: Vec<ParsedSkillToolCall>,
    final_text: Option<String>,
    usage: Option<SkillTraceTokenUsage>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SkillChainRequestError {
    BadRequest,
    ClientProvidedTools,
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

#[derive(Clone, Copy, Debug)]
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

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SkillTraceEventPayload {
    #[serde(rename = "type")]
    event_type: String,
    call_id: String,
    #[serde(default)]
    parent_call_id: Option<String>,
    depth: u32,
    skill: String,
    ts_ms: i64,
    #[serde(default)]
    elapsed_ms: Option<u64>,
    #[serde(default)]
    phase: Option<String>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    output: Option<String>,
    #[serde(default)]
    result_code: Option<String>,
    #[serde(default)]
    fail_reason: Option<String>,
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
    #[serde(default)]
    total_tokens: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct SkillTraceTokenUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
}

impl SkillTraceTokenUsage {
    fn from_response_body(body: &Value) -> Option<Self> {
        let usage = body.get("usage")?;
        let prompt_tokens = usage
            .get("prompt_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let completion_tokens = usage
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let total_tokens = usage
            .get("total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(prompt_tokens.saturating_add(completion_tokens));
        if prompt_tokens == 0 && completion_tokens == 0 && total_tokens == 0 {
            return None;
        }
        Some(Self {
            prompt_tokens,
            completion_tokens,
            total_tokens,
        })
    }

    fn add_assign(&mut self, other: &Self) {
        self.prompt_tokens = self.prompt_tokens.saturating_add(other.prompt_tokens);
        self.completion_tokens = self
            .completion_tokens
            .saturating_add(other.completion_tokens);
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
    }
}

#[derive(Default)]
struct SseParser {
    pending_event: Option<String>,
    data_lines: Vec<String>,
}

enum SseMessage {
    Event { event: Option<String>, data: String },
    Comment(String),
}

impl SseParser {
    fn push_line(&mut self, line: &str) -> Option<SseMessage> {
        if let Some(rest) = line.strip_prefix(':') {
            return Some(SseMessage::Comment(rest.trim().to_string()));
        }
        if line.is_empty() {
            let data = self.data_lines.join("\n");
            let event = self.pending_event.take();
            self.data_lines.clear();
            if data.is_empty() && event.is_none() {
                return None;
            }
            return Some(SseMessage::Event { event, data });
        }
        if let Some(rest) = line.strip_prefix("event:") {
            self.pending_event = Some(rest.trim().to_string());
            return None;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            self.data_lines.push(rest.trim().to_string());
        }
        None
    }
}

fn skill_trace_enabled(headers: &HeaderMap) -> bool {
    headers
        .get(SKILL_TRACE_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|v| v == "1")
        .unwrap_or(false)
}

fn skill_trace_content_enabled(headers: &HeaderMap) -> bool {
    headers
        .get(SKILL_TRACE_CONTENT_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|v| v == "1")
        .unwrap_or(false)
}

fn now_ts_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn maybe_set_trace_content(
    trace: &SkillChainTraceState,
    obj: &mut Map<String, Value>,
    query: Option<&str>,
    output: Option<&str>,
) {
    if !trace.include_content {
        return;
    }
    if let Some(query) = query {
        obj.insert("query".to_string(), serde_json::json!(query));
    }
    if let Some(output) = output {
        obj.insert("output".to_string(), serde_json::json!(output));
    }
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
    info!(
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
    let mut payload = serde_json::json!({
        "type": event_type,
        "call_id": call_id,
        "parent_call_id": parent_call_id,
        "depth": depth,
        "skill": skill,
        "ts_ms": now_ts_ms(),
    });

    if let Some(obj) = payload.as_object_mut() {
        if let Some(elapsed_ms) = elapsed_ms {
            obj.insert("elapsed_ms".to_string(), serde_json::json!(elapsed_ms));
        }
        if let Some(phase) = phase {
            obj.insert("phase".to_string(), serde_json::json!(phase));
        }
        maybe_set_trace_content(trace, obj, query, output);
        if let Some(result_code) = result_code {
            obj.insert("result_code".to_string(), serde_json::json!(result_code));
        }
        if result_code.is_some() || fail_reason.is_some() {
            obj.insert(
                "fail_reason".to_string(),
                match fail_reason {
                    Some(reason) => serde_json::json!(reason.as_str()),
                    None => Value::Null,
                },
            );
        }
        if let Some(usage) = usage {
            obj.insert(
                "prompt_tokens".to_string(),
                serde_json::json!(usage.prompt_tokens),
            );
            obj.insert(
                "completion_tokens".to_string(),
                serde_json::json!(usage.completion_tokens),
            );
            obj.insert("total_tokens".to_string(), serde_json::json!(usage.total_tokens));
        }
    }
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
    info!(
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
    info!(
        "skill_chain trace_emit root_call_id={} root_skill={} event_type=done",
        trace.root_call_id, trace.root_skill
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

fn skill_chain_tool_definition() -> Value {
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
    }])
}

fn parse_skill_chain_request(
    headers: &HeaderMap,
    body_bytes: &[u8],
    default_timeout_sec: f64,
) -> Result<SkillChainRequestState, SkillChainRequestError> {
    let mut body: Value =
        serde_json::from_slice(body_bytes).map_err(|_| SkillChainRequestError::BadRequest)?;
    let obj = body
        .as_object_mut()
        .ok_or(SkillChainRequestError::BadRequest)?;
    let messages = obj
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .ok_or(SkillChainRequestError::BadRequest)?;
    let debug_mocks = obj
        .remove("mocks")
        .and_then(|value| match value {
            Value::Object(map) => Some(
                map.into_iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
                    .collect::<std::collections::HashMap<_, _>>(),
            ),
            _ => None,
        })
        .unwrap_or_default();

    let is_child = headers
        .get(SKILL_CHAIN_CHILD_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|v| v == "1")
        .unwrap_or(false);

    if !is_child {
        match obj.get("tools") {
            Some(Value::Array(arr)) if !arr.is_empty() => {
                return Err(SkillChainRequestError::ClientProvidedTools);
            }
            Some(Value::Array(_)) | None => {}
            Some(_) => return Err(SkillChainRequestError::BadRequest),
        }
    }

    let current_depth = if is_child {
        let raw = headers
            .get(SKILL_CHAIN_DEPTH_HEADER)
            .and_then(|v| v.to_str().ok())
            .ok_or(SkillChainRequestError::BadRequest)?;
        let parsed = raw
            .parse::<u32>()
            .map_err(|_| SkillChainRequestError::BadRequest)?;
        if parsed == 0 {
            return Err(SkillChainRequestError::BadRequest);
        }
        parsed
    } else {
        1
    };

    let timeout_budget_sec = headers
        .get(SKILL_CHAIN_TIMEOUT_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<f64>().ok())
        .map(|v| default_timeout_sec.min(v))
        .unwrap_or(default_timeout_sec);

    obj.insert("tools".to_string(), skill_chain_tool_definition());
    obj.insert("stream".to_string(), Value::Bool(false));

    Ok(SkillChainRequestState {
        template: body,
        history: messages,
        headers: headers.clone(),
        current_depth,
        timeout_budget_sec,
        started_at: std::time::Instant::now(),
        debug_mocks,
    })
}

fn build_skill_chain_request_body(
    template: &Value,
    history: &[Value],
) -> Result<Vec<u8>, StatusCode> {
    let mut body = template.clone();
    let obj = body.as_object_mut().ok_or(StatusCode::BAD_REQUEST)?;
    obj.insert("messages".to_string(), Value::Array(history.to_vec()));
    serde_json::to_vec(&body).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

fn parse_skill_response(body_bytes: &[u8]) -> Option<ParsedSkillResponse> {
    let body: Value = serde_json::from_slice(body_bytes).ok()?;
    let choice = body.get("choices")?.as_array()?.first()?;
    let finish_reason = choice.get("finish_reason").and_then(Value::as_str);
    let message = choice.get("message");
    let delta = choice.get("delta");
    let tool_calls_value = message
        .and_then(|v| v.get("tool_calls"))
        .or_else(|| delta.and_then(|v| v.get("tool_calls")));
    let tool_calls = tool_calls_value
        .and_then(Value::as_array)
        .map(|calls| {
            calls
                .iter()
                .map(|call| ParsedSkillToolCall {
                    id: call
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    name: call
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    arguments: call
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    raw: call.clone(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let has_tool_calls = !tool_calls.is_empty() || finish_reason == Some("tool_calls");
    let final_text = message
        .and_then(|m| m.get("content"))
        .and_then(json_text)
        .or_else(|| delta.and_then(|m| m.get("content")).and_then(json_text));

    Some(ParsedSkillResponse {
        tool_calls: if has_tool_calls {
            tool_calls
        } else {
            Vec::new()
        },
        final_text,
        usage: SkillTraceTokenUsage::from_response_body(&body),
    })
}

fn parse_skillep_tool_args(arguments: &str) -> core::result::Result<(String, String), String> {
    let args: Value =
        serde_json::from_str(arguments).map_err(|_| "Error: invalid tool arguments".to_string())?;
    let skillep_id = args
        .get("skillep_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "Error: invalid skillep_id".to_string())?;
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
        return Err("Error: invalid skillep_id".to_string());
    }
    Ok((owner_tenant, namespace, skillname))
}

fn remaining_skill_timeout_sec(state: &SkillChainRequestState) -> f64 {
    (state.timeout_budget_sec - state.started_at.elapsed().as_secs_f64()).max(0.0)
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

fn summarize_response_body_for_log(body: &[u8]) -> String {
    const MAX_LOG_BYTES: usize = 4096;
    let preview = if body.len() > MAX_LOG_BYTES {
        &body[..MAX_LOG_BYTES]
    } else {
        body
    };
    let text = String::from_utf8_lossy(preview).replace('\n', "\\n");
    if body.len() > MAX_LOG_BYTES {
        format!(
            "{}...[truncated {} bytes]",
            text,
            body.len() - MAX_LOG_BYTES
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

    while let Some(chunk) = stream.next().await {
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
                            continue;
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
    mut chain: SkillChainRequestState,
    route: FuncRouteTarget,
    calling_tenant: String,
    logical_funcname: String,
    remain_path: String,
    prefix: String,
    skills_namespace: String,
    trace: Option<SkillChainTraceState>,
) -> Result<SkillChainFinalResponse, SkillChainExecutionError> {
    info!(
        "skill_chain start logical={}/{}/{} physical={}/{}/{} path={} depth={} timeout_budget_sec={:.3}",
        route.logical.tenant,
        route.logical.namespace,
        route.logical.funcname,
        route.physical.tenant,
        route.physical.namespace,
        route.physical.funcname,
        remain_path,
        chain.current_depth,
        chain.timeout_budget_sec
    );

    let child_http_client = reqwest::Client::new();
    let mut aggregate_usage: Option<SkillTraceTokenUsage> = None;

    loop {
        let remaining_timeout_before_model = remaining_skill_timeout_sec(&chain);
        info!(
            "skill_chain model_turn logical={}/{}/{} history_len={} depth={} remaining_timeout_sec={:.3}",
            route.logical.tenant,
            route.logical.namespace,
            route.logical.funcname,
            chain.history.len(),
            chain.current_depth,
            remaining_timeout_before_model
        );
        let body_bytes =
            build_skill_chain_request_body(&chain.template, &chain.history).map_err(|status| {
                SkillChainExecutionError::Response(
                    status,
                    "service failure: failed to build request body",
                )
            })?;

        let mut model_headers = chain.headers.clone();
        model_headers.remove(SKILL_CHAIN_CHILD_HEADER);
        model_headers.remove(SKILL_CHAIN_DEPTH_HEADER);
        model_headers.remove(SKILL_TRACE_HEADER);
        model_headers.remove("X-Skill-Trace-Content");
        model_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        model_headers.insert(
            SKILL_CHAIN_TIMEOUT_HEADER,
            HeaderValue::from_str(&format!("{:.3}", remaining_timeout_before_model))
                .unwrap_or_else(|_| HeaderValue::from_static("0")),
        );

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
        info!(
            "skill_chain model_response logical={}/{}/{} status={} body_preview={}",
            route.logical.tenant,
            route.logical.namespace,
            route.logical.funcname,
            status,
            summarize_response_body_for_log(&resp_bytes)
        );
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
        if let Some(usage) = parsed.usage.as_ref() {
            if let Some(aggregate) = aggregate_usage.as_mut() {
                aggregate.add_assign(usage);
            } else {
                aggregate_usage = Some(usage.clone());
            }
        }

        if parsed.tool_calls.is_empty() {
            info!(
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
            chain.history.push(serde_json::json!({
                "role": "assistant",
                "content": Value::Null,
                "tool_calls": parsed.tool_calls.iter().map(|call| call.raw.clone()).collect::<Vec<_>>(),
            }));
            for call in &parsed.tool_calls {
                let content = if call.name == SKILL_CHAIN_TOOL_NAME {
                    "Error: turn aborted (other tool in same response was unsupported)".to_string()
                } else {
                    format_child_tool_error(format!("unsupported tool '{}'", call.name))
                };
                chain.history.push(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": call.id,
                    "content": content,
                }));
            }
            continue;
        }

        info!(
            "skill_chain tool_turn logical={}/{}/{} tool_call_count={}",
            route.logical.tenant,
            route.logical.namespace,
            route.logical.funcname,
            parsed.tool_calls.len()
        );
        chain.history.push(serde_json::json!({
            "role": "assistant",
            "content": Value::Null,
            "tool_calls": parsed.tool_calls.iter().map(|call| call.raw.clone()).collect::<Vec<_>>(),
        }));

        for call in parsed.tool_calls {
            let tool_result = match parse_skillep_tool_args(&call.arguments) {
                Err(err) => {
                    warn!(
                        "skill_chain invalid_tool_args logical={}/{}/{} tool_call_id={}",
                        route.logical.tenant,
                        route.logical.namespace,
                        route.logical.funcname,
                        call.id
                    );
                    err
                }
                Ok((skillep_id, query)) => {
                    let (child_owner_tenant, child_namespace, child_skillname) =
                        match split_skillep_id(&skillep_id) {
                            Ok(parts) => parts,
                            Err(err) => {
                                warn!(
                                    "skill_chain invalid_skillep_id logical={}/{}/{} tool_call_id={} skillep_id={}",
                                    route.logical.tenant,
                                    route.logical.namespace,
                                    route.logical.funcname,
                                    call.id,
                                    skillep_id
                                );
                                chain.history.push(serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": call.id,
                                    "content": err,
                                }));
                                continue;
                            }
                        };

                    let local_child_depth = 2;
                    let child_trace = trace.as_ref().map(|_| SkillChainChildTraceState {
                        call_id: Uuid::new_v4().to_string(),
                        skill: skillep_id.clone(),
                        started_at: Instant::now(),
                        local_root_call_id: Arc::new(Mutex::new(None)),
                    });
                    if let (Some(trace_state), Some(child_trace)) =
                        (trace.as_ref(), child_trace.as_ref())
                    {
                        let _ = emit_trace_event(
                            trace_state,
                            "subcall_start",
                            &child_trace.call_id,
                            Some(&trace_state.root_call_id),
                            local_child_depth,
                            &child_trace.skill,
                            None,
                            None,
                            Some(query.as_str()),
                            None,
                            None,
                            None,
                            None,
                        )
                        .await;
                    }

                    let mocked_result = chain.debug_mocks.get(&skillep_id).cloned();
                    let (tool_result, fail_reason, child_usage) = if let Some(mocked_result) = mocked_result {
                        (mocked_result, None, None)
                    } else {
                        match chain.current_depth.checked_add(1) {
                            Some(next_depth) if next_depth <= SKILL_CHAIN_MAX_DEPTH => {
                                let remaining_timeout = remaining_skill_timeout_sec(&chain);
                                if remaining_timeout <= 0.0 {
                                    warn!(
                                    "skill_chain timeout_exhausted logical={}/{}/{} tool_call_id={}",
                                    route.logical.tenant,
                                    route.logical.namespace,
                                    route.logical.funcname,
                                    call.id
                                );
                                    (
                                        "Error: timeout budget exhausted".to_string(),
                                        Some(SkillTraceFailReason::Timeout),
                                        None,
                                    )
                                } else {
                                    info!(
                                    "skill_chain child_call logical={}/{}/{} tool_call_id={} child={}/{}/{} next_depth={} remaining_timeout_sec={:.3} query_len={}",
                                    route.logical.tenant,
                                    route.logical.namespace,
                                    route.logical.funcname,
                                    call.id,
                                    child_owner_tenant,
                                    child_namespace,
                                    child_skillname,
                                    next_depth,
                                    remaining_timeout,
                                    query.len()
                                );
                                    let child_body = serde_json::json!({
                                        "messages": [{"role": "user", "content": query}],
                                        "stream": false
                                    });
                                    let child_url = format!(
                                        "http://127.0.0.1:{}/skills/{}/{}/{}/v1/chat/completions",
                                        GATEWAY_CONFIG.gatewayPort,
                                        child_owner_tenant,
                                        child_namespace,
                                        child_skillname
                                    );
                                    let mut child_req = child_http_client
                                        .post(&child_url)
                                        .header(CONTENT_TYPE.as_str(), "application/json")
                                        .header(SKILL_CHAIN_CHILD_HEADER, "1")
                                        .header(SKILL_CHAIN_DEPTH_HEADER, next_depth.to_string())
                                        .header(
                                            SKILL_CHAIN_TIMEOUT_HEADER,
                                            format!("{:.3}", remaining_timeout),
                                        )
                                        .json(&child_body);
                                    if let Some(v) = chain
                                        .headers
                                        .get("Authorization")
                                        .and_then(|v| v.to_str().ok())
                                    {
                                        child_req = child_req.header("Authorization", v);
                                    }
                                    if let Some(v) = chain
                                        .headers
                                        .get("X-Request-Id")
                                        .and_then(|v| v.to_str().ok())
                                    {
                                        child_req = child_req.header("X-Request-Id", v);
                                    }
                                    if let Some(v) = chain
                                        .headers
                                        .get("traceparent")
                                        .and_then(|v| v.to_str().ok())
                                    {
                                        child_req = child_req.header("traceparent", v);
                                    }
                                    if let Some(v) =
                                        chain.headers.get("X-Tenant").and_then(|v| v.to_str().ok())
                                    {
                                        child_req = child_req.header("X-Tenant", v);
                                    }
                                    if let Some(trace_state) = trace.as_ref() {
                                        child_req = child_req.header(SKILL_TRACE_HEADER, "1");
                                        if trace_state.include_content {
                                            child_req =
                                                child_req.header(SKILL_TRACE_CONTENT_HEADER, "1");
                                        }
                                    }

                                    let trace_for_child_call = trace.clone();
                                    let child_trace_for_stream = child_trace.clone();
                                    let child_depth_for_stream = local_child_depth;
                                    let child_call = async {
                                        let resp = child_req.send().await.map_err(|e| {
                                            ChildTraceReadError {
                                                message: e.to_string(),
                                                fail_reason: SkillTraceFailReason::TransportError,
                                                open_descendants: Vec::new(),
                                            }
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
                                        let bytes = resp.bytes().await.map_err(|e| {
                                            ChildTraceReadError {
                                                message: e.to_string(),
                                                fail_reason: SkillTraceFailReason::TransportError,
                                                open_descendants: Vec::new(),
                                            }
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

                                    let child_trace_for_tick = child_trace.clone();
                                    match await_with_trace_heartbeat(
                                        trace.clone(),
                                        child_call,
                                        move |trace_state| {
                                            let child_trace = child_trace_for_tick.clone();
                                            async move {
                                                if let Some(child_trace) = child_trace.as_ref() {
                                                    let _ = emit_trace_heartbeat(
                                                        &trace_state,
                                                        &child_trace.call_id,
                                                        Some(&trace_state.root_call_id),
                                                        local_child_depth,
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
                                            if let Some(trace_state) = trace.as_ref() {
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
                                                                .map(|c| {
                                                                    c.started_at
                                                                        .elapsed()
                                                                        .as_millis()
                                                                        as u64
                                                                })
                                                                .unwrap_or(0),
                                                        ),
                                                        None,
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
                                            route.logical.tenant,
                                            route.logical.namespace,
                                            route.logical.funcname,
                                            call.id,
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
                                        Ok((child_status, child_result))
                                            if !child_status.is_success() =>
                                        {
                                            warn!(
                                            "skill_chain child_call_http_error logical={}/{}/{} tool_call_id={} child={}/{}/{} status={}",
                                            route.logical.tenant,
                                            route.logical.namespace,
                                            route.logical.funcname,
                                            call.id,
                                            child_owner_tenant,
                                            child_namespace,
                                            child_skillname,
                                            child_status
                                        );
                                            let bytes =
                                                child_result.skill_result.unwrap_or_default();
                                            let detail =
                                                String::from_utf8_lossy(&bytes).trim().to_string();
                                            let message = if detail.is_empty() {
                                                format_child_tool_error(format!(
                                                    "child call failed with {}",
                                                    child_status
                                                ))
                                            } else {
                                                format_child_tool_error(detail)
                                            };
                                            (message, Some(SkillTraceFailReason::TransportError), child_result.terminal_usage)
                                        }
                                        Ok((_, child_result)) => {
                                            if !child_result.open_descendants.is_empty() {
                                                if let Some(trace_state) = trace.as_ref() {
                                                    for descendant in &child_result.open_descendants
                                                    {
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
                                                                    .map(|c| {
                                                                        c.started_at
                                                                            .elapsed()
                                                                            .as_millis()
                                                                            as u64
                                                                    })
                                                                    .unwrap_or(0),
                                                            ),
                                                            None,
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
                                                    "Error: child trace stream left open descendant calls"
                                                        .to_string(),
                                                    Some(SkillTraceFailReason::InvalidResponse),
                                                    child_result.terminal_usage,
                                                )
                                            } else if child_result.terminal_result_code.as_deref()
                                                == Some("fail")
                                            {
                                                (
                                                    format_child_fail_reason(
                                                        child_result.terminal_fail_reason,
                                                    ),
                                                    child_result.terminal_fail_reason,
                                                    child_result.terminal_usage,
                                                )
                                            } else {
                                                match child_result.skill_result {
                                                    Some(bytes) => match parse_skill_response(&bytes)
                                                    {
                                                        Some(parsed_child) => {
                                                            info!(
                                                                "skill_chain child_call_ok logical={}/{}/{} tool_call_id={} child={}/{}/{} final_text_present={}",
                                                                route.logical.tenant,
                                                                route.logical.namespace,
                                                                route.logical.funcname,
                                                                call.id,
                                                                child_owner_tenant,
                                                                child_namespace,
                                                                child_skillname,
                                                                parsed_child.final_text.as_ref().map(|s| !s.is_empty()).unwrap_or(false)
                                                            );
                                                            match parsed_child
                                                                .final_text
                                                                .filter(|s| !s.is_empty())
                                                            {
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
                                                                route.logical.tenant,
                                                                route.logical.namespace,
                                                                route.logical.funcname,
                                                                call.id,
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
                                                        "Error: child trace stream missing skill_result"
                                                            .to_string(),
                                                        Some(SkillTraceFailReason::InvalidResponse),
                                                        child_result.terminal_usage,
                                                    ),
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            _ => {
                                warn!(
                                    "skill_chain max_depth_exceeded logical={}/{}/{} current_depth={} tool_call_id={}",
                                    route.logical.tenant,
                                    route.logical.namespace,
                                    route.logical.funcname,
                                    chain.current_depth,
                                    call.id
                                );
                                (
                                    "Error: max chain depth exceeded".to_string(),
                                    Some(SkillTraceFailReason::MaxDepth),
                                    None,
                                )
                            }
                        }
                    };

                    if let (Some(trace_state), Some(child_trace)) =
                        (trace.as_ref(), child_trace.as_ref())
                    {
                        let _ = emit_trace_event(
                            trace_state,
                            "subcall_finish",
                            &child_trace.call_id,
                            Some(&trace_state.root_call_id),
                            local_child_depth,
                            &child_trace.skill,
                            Some(child_trace.started_at.elapsed().as_millis() as u64),
                            None,
                            None,
                            Some(tool_result.as_str()),
                            Some(if fail_reason.is_some() {
                                "fail"
                            } else {
                                "pass"
                            }),
                            fail_reason,
                            child_usage.as_ref(),
                        )
                        .await;
                    }

                    tool_result
                }
            };
            chain.history.push(serde_json::json!({
                "role": "tool",
                "tool_call_id": call.id,
                "content": tool_result,
            }));
        }
    }
}

pub(super) async fn handle_skill_call_chain(
    gw: &HttpGateway,
    req: Request,
    route: FuncRouteTarget,
    display_skill_id: String,
    calling_tenant: String,
    logical_funcname: String,
    remain_path: String,
    prefix: &str,
    skills_namespace: &str,
    max_body_bytes: usize,
) -> Result<Response, StatusCode> {
    let (req_parts, req_body) = req.into_parts();
    let req_headers = req_parts.headers.clone();
    let req_bytes = match axum::body::to_bytes(req_body, max_body_bytes).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from("service failure: invalid request body"))
                .unwrap())
        }
    };

    let chain = match parse_skill_chain_request(&req_headers, &req_bytes, route.policy.queueTimeout)
    {
        Ok(chain) => chain,
        Err(SkillChainRequestError::BadRequest)
            if req_headers.get(SKILL_CHAIN_CHILD_HEADER).is_some() =>
        {
            warn!(
                "skill_chain invalid child depth logical={}/{}/{} path={}",
                route.logical.tenant, route.logical.namespace, route.logical.funcname, remain_path
            );
            return Ok(Response::builder()
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
                    .unwrap());
        }
        Err(SkillChainRequestError::ClientProvidedTools) => {
            warn!(
                "skill_chain rejected client_provided_tools logical={}/{}/{} path={}",
                route.logical.tenant, route.logical.namespace, route.logical.funcname, remain_path
            );
            return Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from(CLIENT_PROVIDED_TOOLS_ERROR))
                .unwrap());
        }
        Err(SkillChainRequestError::BadRequest) => {
            warn!(
                "skill_chain invalid request logical={}/{}/{} path={} status={}",
                route.logical.tenant,
                route.logical.namespace,
                route.logical.funcname,
                remain_path,
                StatusCode::BAD_REQUEST
            );
            return Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from("service failure: invalid request body"))
                .unwrap());
        }
    };

    if !skill_trace_enabled(&req_headers) {
        return match execute_skill_chain(
            gw,
            chain,
            route,
            calling_tenant,
            logical_funcname,
            remain_path,
            prefix.to_string(),
            skills_namespace.to_string(),
            None,
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

    let (tx, rx) = mpsc::channel::<Result<String, Infallible>>(32);
    let trace = SkillChainTraceState {
        tx: tx.clone(),
        root_call_id: Uuid::new_v4().to_string(),
        root_skill: display_skill_id,
        started_at: Instant::now(),
        include_content: skill_trace_content_enabled(&req_headers),
    };

    let gw = gw.clone();
    let route_for_task = route.clone();
    let calling_tenant_for_task = calling_tenant.clone();
    let logical_funcname_for_task = logical_funcname.clone();
    let remain_path_for_task = remain_path.clone();
    let prefix_for_task = prefix.to_string();
    let skills_namespace_for_task = skills_namespace.to_string();

    tokio::spawn(async move {
        let root_query = latest_user_query(&chain.history);
        if !emit_root_call_start(&trace, root_query.as_deref()).await {
            return;
        }

        let trace_for_closed = trace.clone();
        let exec = execute_skill_chain(
            &gw,
            chain,
            route_for_task,
            calling_tenant_for_task,
            logical_funcname_for_task,
            remain_path_for_task,
            prefix_for_task,
            skills_namespace_for_task,
            Some(trace.clone()),
        );
        tokio::pin!(exec);

        let result = tokio::select! {
            _ = trace_for_closed.tx.closed() => return,
            result = &mut exec => result,
        };

        match result {
            Ok(final_resp) if final_resp.is_openai_chat_completion => {
                let _ = emit_root_call_finish(&trace, "pass", None, final_resp.usage.as_ref()).await;
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

pub(super) async fn handle_skill_debug_call(
    gw: &HttpGateway,
    req: Request,
    route: FuncRouteTarget,
    display_skill_id: String,
    calling_tenant: String,
    logical_funcname: String,
    remain_path: String,
    prefix: &str,
    skills_namespace: &str,
    max_body_bytes: usize,
) -> Result<Response, StatusCode> {
    let (req_parts, req_body) = req.into_parts();
    let req_headers = req_parts.headers.clone();
    let req_bytes = match axum::body::to_bytes(req_body, max_body_bytes).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from("service failure: invalid request body"))
                .unwrap())
        }
    };

    let chain = match parse_skill_chain_request(&req_headers, &req_bytes, route.policy.queueTimeout)
    {
        Ok(chain) => chain,
        Err(SkillChainRequestError::ClientProvidedTools) => {
            return Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from(CLIENT_PROVIDED_TOOLS_ERROR))
                .unwrap())
        }
        Err(SkillChainRequestError::BadRequest) => {
            return Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from("service failure: invalid request body"))
                .unwrap())
        }
    };

    let (tx, rx) = mpsc::channel::<Result<String, Infallible>>(32);
    let trace = SkillChainTraceState {
        tx: tx.clone(),
        root_call_id: Uuid::new_v4().to_string(),
        root_skill: display_skill_id,
        started_at: Instant::now(),
        include_content: true,
    };

    let gw = gw.clone();
    let route_for_task = route.clone();
    let calling_tenant_for_task = calling_tenant.clone();
    let logical_funcname_for_task = logical_funcname.clone();
    let remain_path_for_task = remain_path.clone();
    let prefix_for_task = prefix.to_string();
    let skills_namespace_for_task = skills_namespace.to_string();

    tokio::spawn(async move {
        let root_query = latest_user_query(&chain.history);
        if !emit_root_call_start(&trace, root_query.as_deref()).await {
            return;
        }

        let trace_for_closed = trace.clone();
        let exec = execute_skill_chain(
            &gw,
            chain,
            route_for_task,
            calling_tenant_for_task,
            logical_funcname_for_task,
            remain_path_for_task,
            prefix_for_task,
            skills_namespace_for_task,
            Some(trace.clone()),
        );
        tokio::pin!(exec);

        let result = tokio::select! {
            _ = trace_for_closed.tx.closed() => return,
            result = &mut exec => result,
        };

        match result {
            Ok(final_resp) if final_resp.is_openai_chat_completion => {
                let _ = emit_root_call_finish(&trace, "pass", None, final_resp.usage.as_ref()).await;
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
        build_skill_chain_request_body, parse_skill_chain_request, parse_skill_response,
        parse_skillep_tool_args, skill_chain_tool_definition, split_skillep_id,
        SkillChainRequestError, SKILL_CHAIN_CHILD_HEADER, SKILL_CHAIN_DEPTH_HEADER,
    };
    use axum::http::{HeaderMap, HeaderValue};
    use serde_json::{json, Value};

    #[test]
    fn parse_skill_chain_request_rejects_non_empty_top_level_tools() {
        let headers = HeaderMap::new();
        let body =
            br#"{"messages":[{"role":"user","content":"hi"}],"tools":[{"type":"function"}]}"#;
        assert_eq!(
            parse_skill_chain_request(&headers, body, 60.0).unwrap_err(),
            SkillChainRequestError::ClientProvidedTools
        );
    }

    #[test]
    fn parse_skill_chain_request_injects_tool_for_empty_tools() {
        let headers = HeaderMap::new();
        let body = br#"{"messages":[{"role":"user","content":"hi"}],"tools":[],"stream":true}"#;
        let parsed = parse_skill_chain_request(&headers, body, 60.0).unwrap();
        assert_eq!(parsed.current_depth, 1);
        assert_eq!(parsed.history.len(), 1);
        assert_eq!(parsed.template.get("stream"), Some(&Value::Bool(false)));
        assert_eq!(
            parsed.template.get("tools"),
            Some(&skill_chain_tool_definition())
        );
        let rebuilt = build_skill_chain_request_body(&parsed.template, &parsed.history).unwrap();
        let rebuilt_json: Value = serde_json::from_slice(&rebuilt).unwrap();
        assert_eq!(
            rebuilt_json
                .get("messages")
                .and_then(Value::as_array)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn parse_skill_chain_request_rejects_invalid_child_depth() {
        let mut headers = HeaderMap::new();
        headers.insert(SKILL_CHAIN_CHILD_HEADER, HeaderValue::from_static("1"));
        headers.insert(SKILL_CHAIN_DEPTH_HEADER, HeaderValue::from_static("0"));
        let body = br#"{"messages":[{"role":"user","content":"hi"}]}"#;
        assert_eq!(
            parse_skill_chain_request(&headers, body, 60.0).unwrap_err(),
            SkillChainRequestError::BadRequest
        );
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
            "Error: invalid skillep_id"
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
}
