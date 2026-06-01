use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue};
use axum::response::Response;
use chrono::Utc;
use http_body_util::BodyExt;
use hyper::header::CONTENT_TYPE;
use hyper::StatusCode;
use serde_json::Value;

use super::func_agent_mgr::FuncRouteTarget;
use super::http_gateway::{dispatch_func_call, GATEWAY_CONFIG, HttpGateway};

const SKILL_CHAIN_CHILD_HEADER: &str = "X-Inferx-Skill-Chain-Child";
const SKILL_CHAIN_DEPTH_HEADER: &str = "X-Chain-Depth";
const SKILL_CHAIN_TIMEOUT_HEADER: &str = "X-Inferx-Timeout";
const SKILL_CHAIN_MAX_DEPTH: u32 = 5;
const SKILL_CHAIN_TOOL_NAME: &str = "call_skillep";
const CLIENT_PROVIDED_TOOLS_ERROR: &str = "skill endpoint does not accept client-provided tools";

#[derive(Clone, Debug)]
struct SkillChainRequestState {
    template: Value,
    history: Vec<Value>,
    headers: HeaderMap,
    current_depth: u32,
    timeout_budget_sec: f64,
    started_at: std::time::Instant,
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SkillChainRequestError {
    BadRequest,
    ClientProvidedTools,
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
        format!("{}...[truncated {} bytes]", text, body.len() - MAX_LOG_BYTES)
    } else {
        text
    }
}

pub(super) async fn handle_skill_call_chain(
    gw: &HttpGateway,
    req: Request,
    route: FuncRouteTarget,
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

    let mut chain =
        match parse_skill_chain_request(&req_headers, &req_bytes, route.policy.queueTimeout) {
            Ok(chain) => chain,
            Err(SkillChainRequestError::BadRequest)
                if req_headers.get(SKILL_CHAIN_CHILD_HEADER).is_some() =>
            {
                warn!(
                    "skill_chain invalid child depth logical={}/{}/{} path={}",
                    route.logical.tenant,
                    route.logical.namespace,
                    route.logical.funcname,
                    remain_path
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
                    .unwrap())
            }
            Err(SkillChainRequestError::ClientProvidedTools) => {
                warn!(
                    "skill_chain rejected client_provided_tools logical={}/{}/{} path={}",
                    route.logical.tenant,
                    route.logical.namespace,
                    route.logical.funcname,
                    remain_path
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
                    .unwrap())
            }
        };

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
        let body_bytes = match build_skill_chain_request_body(&chain.template, &chain.history) {
            Ok(body) => body,
            Err(status) => {
                return Ok(Response::builder()
                    .status(status)
                    .body(Body::from("service failure: failed to build request body"))
                    .unwrap())
            }
        };

        let mut model_headers = chain.headers.clone();
        model_headers.remove(SKILL_CHAIN_CHILD_HEADER);
        model_headers.remove(SKILL_CHAIN_DEPTH_HEADER);
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

        let mut model_resp = dispatch_func_call(
            gw,
            model_req,
            route.clone(),
            calling_tenant.clone(),
            skills_namespace.to_string(),
            logical_funcname.clone(),
            remain_path.clone(),
            Some(prefix),
        )
        .await?;
        let status = model_resp.status();
        let headers = model_resp.headers().clone();
        let resp_bytes = match model_resp.body_mut().collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(_) => {
                return Ok(Response::builder()
                    .status(StatusCode::BAD_GATEWAY)
                    .body(Body::from("service failure: failed to read model response"))
                .unwrap())
            }
        };
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
                    route.logical.tenant,
                    route.logical.namespace,
                    route.logical.funcname,
                    status
                );
                let mut resp = Response::builder().status(status);
                for (key, value) in &headers {
                    resp = resp.header(key, value);
                }
                return Ok(resp.body(Body::from(resp_bytes)).unwrap());
            }
        };

        if parsed.tool_calls.is_empty() {
            info!(
                "skill_chain final_response logical={}/{}/{} status={} final_text_present={}",
                route.logical.tenant,
                route.logical.namespace,
                route.logical.funcname,
                status,
                parsed.final_text.as_ref().map(|s| !s.is_empty()).unwrap_or(false)
            );
            let mut resp = Response::builder().status(status);
            for (key, value) in &headers {
                resp = resp.header(key, value);
            }
            return Ok(resp.body(Body::from(resp_bytes)).unwrap());
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
                                "Error: timeout budget exhausted".to_string()
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
                                if let Some(v) = chain
                                    .headers
                                    .get("X-Tenant")
                                    .and_then(|v| v.to_str().ok())
                                {
                                    child_req = child_req.header("X-Tenant", v);
                                }

                                match child_req.send().await {
                                    Err(e) => {
                                        warn!(
                                            "skill_chain child_call_transport_error logical={}/{}/{} tool_call_id={} child={}/{}/{} err={}",
                                            route.logical.tenant,
                                            route.logical.namespace,
                                            route.logical.funcname,
                                            call.id,
                                            child_owner_tenant,
                                            child_namespace,
                                            child_skillname,
                                            e
                                        );
                                        format_child_tool_error(e.to_string())
                                    }
                                    Ok(resp) => {
                                        let child_status = resp.status();
                                        match resp.bytes().await {
                                            Err(e) => {
                                                warn!(
                                                    "skill_chain child_call_read_error logical={}/{}/{} tool_call_id={} child={}/{}/{} err={}",
                                                    route.logical.tenant,
                                                    route.logical.namespace,
                                                    route.logical.funcname,
                                                    call.id,
                                                    child_owner_tenant,
                                                    child_namespace,
                                                    child_skillname,
                                                    e
                                                );
                                                format_child_tool_error(e.to_string())
                                            }
                                            Ok(bytes) if !child_status.is_success() => {
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
                                                let detail = String::from_utf8_lossy(&bytes)
                                                    .trim()
                                                    .to_string();
                                                if detail.is_empty() {
                                                    format_child_tool_error(format!(
                                                        "child call failed with {}",
                                                        child_status
                                                    ))
                                                } else {
                                                    format_child_tool_error(detail)
                                                }
                                            }
                                            Ok(bytes) => match parse_skill_response(&bytes) {
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
                                                    parsed_child
                                                        .final_text
                                                        .filter(|s| !s.is_empty())
                                                        .unwrap_or_else(|| {
                                                            "Error: child returned empty response"
                                                                .to_string()
                                                        })
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
                                                    format_child_tool_error(
                                                        "child returned invalid response",
                                                    )
                                                }
                                            },
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
                            "Error: max chain depth exceeded".to_string()
                        }
                    }
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
        let parsed_final =
            parse_skill_response(&serde_json::to_vec(&final_resp).unwrap()).unwrap();
        assert!(parsed_final.tool_calls.is_empty());
        assert_eq!(parsed_final.final_text.as_deref(), Some("done"));
    }
}
