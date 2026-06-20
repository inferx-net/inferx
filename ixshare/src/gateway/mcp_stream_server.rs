use anyhow::Result as AResult;
use futures::StreamExt;
use rmcp::{
    model::*, service::RequestContext, service::ServiceError, ErrorData, RoleServer, ServerHandler,
};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::secret::SqlSecret;
use super::skill_chain::{
    build_skill_chat_request, InternalSkillRequestProfile, SkillTraceLevel,
};
use super::skill_trace_sse::{SkillTraceEventPayload, SseMessage, SseParser};
use crate::print::verbose_category;

/// Maps per-request UUIDs to `CancellationToken`s so that `call_tool` can
/// propagate `context.ct` through the loopback HTTP boundary into
/// `handle_skill_call_chain`.  The HTTP handler extracts the UUID from the
/// `X-Mcp-Cancel-Id` request header and retrieves the live token here.
pub struct McpCancelRegistry {
    tokens: Mutex<HashMap<String, CancellationToken>>,
}

static MCP_CANCEL_REGISTRY: OnceLock<McpCancelRegistry> = OnceLock::new();

impl McpCancelRegistry {
    pub fn global() -> &'static Self {
        MCP_CANCEL_REGISTRY.get_or_init(|| McpCancelRegistry {
            tokens: Mutex::new(HashMap::new()),
        })
    }

    /// Insert a token and return the UUID key for retrieval.
    pub fn register(&self, token: CancellationToken) -> String {
        let id = Uuid::new_v4().to_string();
        self.tokens.lock().unwrap().insert(id.clone(), token);
        id
    }

    /// Look up a token by UUID. Returns `None` if already removed or never registered.
    pub fn lookup(&self, id: &str) -> Option<CancellationToken> {
        self.tokens.lock().unwrap().get(id).cloned()
    }

    fn remove(&self, id: &str) {
        self.tokens.lock().unwrap().remove(id);
    }
}

/// Removes the registry entry on drop — handles the case where `call_tool` is
/// cancelled mid-execution and the normal cleanup path is skipped.
struct McpCancelGuard(String);

impl Drop for McpCancelGuard {
    fn drop(&mut self) {
        McpCancelRegistry::global().remove(&self.0);
    }
}

#[derive(Clone)]
pub struct McpStreamServer {
    secret: Arc<SqlSecret>,
    gateway_base_url: String,
    client: reqwest::Client,
}

impl McpStreamServer {
    pub fn new(secret: Arc<SqlSecret>, gateway_base_url: String) -> Self {
        Self {
            secret,
            gateway_base_url,
            client: reqwest::Client::new(),
        }
    }

    fn internal_error(message: impl Into<String>) -> ErrorData {
        ErrorData::new(rmcp::model::ErrorCode::INTERNAL_ERROR, message.into(), None)
    }

    fn extract_authorization_header(
        context: &RequestContext<RoleServer>,
    ) -> Result<String, ErrorData> {
        context
            .extensions
            .get::<http::request::Parts>()
            .and_then(|parts| parts.headers.get("Authorization"))
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .ok_or_else(|| Self::internal_error("missing Authorization header"))
    }

    fn extract_bearer_apikey(
        context: &RequestContext<RoleServer>,
    ) -> Result<(String, String), ErrorData> {
        let authorization = Self::extract_authorization_header(context)?;
        let apikey = authorization
            .strip_prefix("Bearer ")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Self::internal_error("Authorization header must be Bearer <apikey>"))?
            .to_string();
        Ok((authorization, apikey))
    }

    async fn resolve_tenant(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<(String, String), ErrorData> {
        let (authorization, apikey) = Self::extract_bearer_apikey(context)?;
        let key =
            self.secret.GetApikey(&apikey).await.map_err(|e| {
                Self::internal_error(format!("failed to resolve MCP API key: {:?}", e))
            })?;
        let tenant = key.restrict_tenant.ok_or_else(|| {
            Self::internal_error(
                "MCP requires a tenant-restricted API key so skill tools can be resolved",
            )
        })?;
        Ok((authorization, tenant))
    }

    fn skill_url(&self, owner_tenant: &str, owner_namespace: &str, skillname: &str) -> String {
        format!(
            "{}/skills/{}/{}/{}/v1/chat/completions",
            self.gateway_base_url, owner_tenant, owner_namespace, skillname
        )
    }

    fn parse_text_response(body: &[u8]) -> Result<String, ErrorData> {
        let json: Value = serde_json::from_slice(body)
            .map_err(|e| Self::internal_error(format!("invalid skill response JSON: {}", e)))?;
        Self::extract_text_from_completion(&json)
            .ok_or_else(|| Self::internal_error("skill response did not contain final text"))
    }

    fn extract_text_from_completion(json: &Value) -> Option<String> {
        let content = json
            .get("choices")
            .and_then(|v| v.as_array())
            .and_then(|v| v.first())
            .and_then(|choice| choice.get("message"))
            .and_then(|message| message.get("content"))?;
        match content {
            Value::String(s) => Some(s.clone()),
            Value::Array(items) => {
                let text = items
                    .iter()
                    .filter_map(|item| match item {
                        Value::String(s) => Some(s.as_str()),
                        Value::Object(map) => map.get("text").and_then(Value::as_str),
                        _ => None,
                    })
                    .collect::<String>();
                if text.is_empty() {
                    Some(content.to_string())
                } else {
                    Some(text)
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

    fn summarize_str_for_log(text: &str) -> String {
        const MAX_LOG_BYTES: usize = 4096;
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

    fn query_schema_value() -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": r#"The complete context required for the skill to perform its task.

Build the query using the following structure:

[Current User Request]
The user's latest request or question.

[Goal]
The objective that this skill is expected to achieve.

[Relevant Context]
A compressed summary of prior conversation, facts, assumptions, and intermediate conclusions that are necessary for this skill.
Do NOT include irrelevant conversation history.
Preserve important decisions, user preferences, constraints, and previously discovered information.

[Previous Results]
Summaries of outputs from prior skills or reasoning steps that this skill depends on.
Include only information needed to continue the task.

[Constraints]
Any requirements, limitations, formatting instructions, policies, budgets, deadlines, technologies, or other constraints.

[Open Questions]
Any unresolved questions or ambiguities that this skill should help answer.

Guidelines:

- Compress history aggressively.
- Keep facts, decisions, and conclusions.
- Remove greetings, small talk, and redundant discussion.
- Preserve the user's intent rather than exact wording.
- Include enough context so the skill can operate independently without access to the full conversation.
- Prefer concise summaries over raw transcripts.
- Include all information that could affect the correctness of the result.
- When in doubt, keep important decisions and constraints.
- The resulting query should be self-contained and understandable without additional context."#
                }
            },
            "required": ["query"]
        })
    }

    async fn notify_progress_message(
        context: &RequestContext<RoleServer>,
        message: String,
    ) -> Result<(), ErrorData> {
        context
            .peer
            .notify_progress(ProgressNotificationParam {
                progress_token: ProgressToken(NumberOrString::Number(0)),
                progress: 0.0,
                total: None,
                message: Some(message),
            })
            .await
            .map_err(|e| Self::internal_error(format!("failed to notify progress: {}", e)))
    }

    async fn try_notify_progress_message(context: &RequestContext<RoleServer>, message: String) {
        if let Err(e) = Self::notify_progress_message(context, message).await {
            warn!("MCP progress notify failed: {}", e.message);
        }
    }

    async fn handle_sse_message(
        context: &RequestContext<RoleServer>,
        message: SseMessage,
        final_result: &mut Option<Value>,
        terminal_failure: &mut Option<String>,
        saw_done: &mut bool,
    ) -> Result<(), ErrorData> {
        let (event, data) = match message {
            SseMessage::Comment(_) => return Ok(()),
            SseMessage::Event { event, data } => (event, data),
        };
        if data == "[DONE]" {
            *saw_done = true;
            return Ok(());
        }
        match event.as_deref() {
            Some("skill_trace") => {
                let event: SkillTraceEventPayload = serde_json::from_str(&data).map_err(|e| {
                    Self::internal_error(format!("invalid trace event JSON: {}", e))
                })?;
                update_terminal_failure(&event, terminal_failure);
                match context
                    .peer
                    .notify_progress(ProgressNotificationParam {
                        progress_token: ProgressToken(NumberOrString::Number(0)),
                        progress: 0.0,
                        total: None,
                        message: Some(Self::format_trace_message(&event)),
                    })
                    .await
                {
                    Ok(()) => {}
                    Err(ServiceError::TransportClosed) => {
                        return Err(Self::internal_error("transport closed"));
                    }
                    Err(e) => {
                        warn!("MCP progress notify failed: {}", e);
                    }
                }
            }
            Some("skill_result") => {
                let json: Value = serde_json::from_str(&data).map_err(|e| {
                    Self::internal_error(format!("invalid skill result JSON: {}", e))
                })?;
                *final_result = Some(json);
            }
            _ => {}
        }
        Ok(())
    }

    // Resolves the terminal state of a skill trace stream into Ok(result) or Err.
    // Extracted as a pure function so the failure-path logic can be unit-tested without
    // a live MCP transport or SSE stream.
    fn resolve_skill_trace_outcome(
        final_result: Option<Value>,
        terminal_failure: Option<String>,
        saw_done: bool,
    ) -> Result<Value, ErrorData> {
        if !saw_done && final_result.is_none() {
            return Err(Self::internal_error("trace stream ended before [DONE]"));
        }
        match final_result {
            Some(v) => Ok(v),
            None => {
                let msg = terminal_failure
                    .as_deref()
                    .unwrap_or("trace stream missing skill_result");
                Err(Self::internal_error(msg))
            }
        }
    }

    fn format_trace_message(event: &SkillTraceEventPayload) -> String {
        let timing = event
            .elapsed_ms
            .map(|ms| format!(" {:.1}s", ms as f64 / 1000.0))
            .unwrap_or_default();
        match event.event_type.as_str() {
            "call_start" => format!("{}: started", event.skill),
            "subcall_start" => format!("calling {}", event.skill),
            "heartbeat" => {
                let phase = event.phase.as_deref().unwrap_or("working");
                format!("{}: {}{}", event.skill, phase.replace('_', " "), timing)
            }
            "subcall_finish" | "call_finish" => {
                let result = event.result_code.as_deref().unwrap_or("pass");
                format!("{}: {}{}", event.skill, result, timing)
            }
            _ => format!("{}: {}", event.skill, event.event_type),
        }
    }
}

fn update_terminal_failure(event: &SkillTraceEventPayload, terminal_failure: &mut Option<String>) {
    if event.depth == 1
        && event.event_type == "call_finish"
        && event.result_code.as_deref() == Some("fail")
    {
        *terminal_failure = Some(format!(
            "{}: {}",
            event.skill,
            format_skill_fail_reason(event.fail_reason.as_deref())
        ));
    }
}

fn format_skill_fail_reason(reason: Option<&str>) -> &'static str {
    match reason {
        Some("timeout") => "skill timed out",
        Some("transport_error") => "skill transport error",
        Some("invalid_response") => "skill returned invalid response",
        Some("max_depth") => "skill exceeded max chain depth",
        _ => "skill failed",
    }
}


impl ServerHandler for McpStreamServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2025_03_26)
            .with_server_info(Implementation::new("mcp-stream-server", "1.0.0"))
            .with_instructions("InferX MCP server exposing tenant skills and subscriptions")
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let (authorization, tenant) = self.resolve_tenant(&context).await?;

        let requested_name = request.name.to_string();
        info!(
            "MCP call_tool: tool_name={}, tenant={}",
            requested_name, tenant
        );
        let subscription = self
            .secret
            .GetSubscribedSkillRoute(&tenant, &requested_name)
            .await
            .map_err(|e| {
                Self::internal_error(format!("failed to load subscribed skill route: {:?}", e))
            })?;
        let Some(subscription) = subscription else {
            return Err(Self::internal_error("unknown tool or not subscribed"));
        };

        let query = request
            .arguments
            .as_ref()
            .and_then(|args| args.get("query"))
            .and_then(|q| q.as_str())
            .unwrap_or("");

        let endpoint = self.skill_url(
            &subscription.owner_tenant,
            &subscription.owner_namespace,
            &subscription.skillname,
        );
        Self::try_notify_progress_message(&context, format!("Forwarding to {}", endpoint)).await;

        let body = build_skill_chat_request(
            query,
            SkillTraceLevel::Basic,
            InternalSkillRequestProfile::McpToolLoopback,
        );

        ctrace!(
            verbose_category::MCP,
            "MCP call_tool request: tool={} tenant={} owner={}/{}/{} query_len={} input_schema={}",
            requested_name,
            tenant,
            &subscription.owner_tenant,
            &subscription.owner_namespace,
            &subscription.skillname,
            query.len(),
            Self::summarize_value_for_log(&Self::query_schema_value())
        );
        ctrace!(
            verbose_category::SKILL_PAYLOAD,
            "MCP call_tool request payload: tool={} tenant={} query={}",
            requested_name,
            tenant,
            Self::summarize_str_for_log(query)
        );

        let ct = context.ct.clone();
        // Register a child token in the global registry so that the loopback HTTP
        // handler (RunSkill) can retrieve it by UUID and pass it explicitly into
        // handle_skill_call_chain / execute_skill_chain.  The child is automatically
        // cancelled when ct fires, and the guard cleans up the registry entry on drop.
        let child_ct = ct.child_token();
        let cancel_id = McpCancelRegistry::global().register(child_ct);
        let _cancel_guard = McpCancelGuard(cancel_id.clone());

        let response = tokio::select! {
            _ = ct.cancelled() => {
                return Err(Self::internal_error("request cancelled"));
            }
            result = self
                .client
                .post(&endpoint)
                .header("Authorization", authorization)
                .header("X-Mcp-Cancel-Id", &cancel_id)
                .json(&body)
                .send() => {
                result.map_err(|e| Self::internal_error(format!("failed to connect to endpoint: {}", e)))?
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let detail = response.text().await.unwrap_or_default();
            let message = if detail.trim().is_empty() {
                format!("endpoint returned error: {}", status)
            } else {
                format!("endpoint returned error: {}: {}", status, detail.trim())
            };
            return Err(Self::internal_error(message));
        }

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.to_ascii_lowercase())
            .unwrap_or_default();
        if !content_type.contains("text/event-stream") {
            let body = tokio::select! {
                _ = ct.cancelled() => {
                    return Err(Self::internal_error("request cancelled"));
                }
                result = response.bytes() => {
                    result.map_err(|e| Self::internal_error(format!("failed to read endpoint response: {}", e)))?
                }
            };
            let full_response = Self::parse_text_response(&body)?;
            ctrace!(
                verbose_category::MCP,
                "MCP call_tool response: tool={} tenant={} final_text_present={} response_len={}",
                requested_name,
                tenant,
                !full_response.is_empty(),
                full_response.len()
            );
            ctrace!(
                verbose_category::SKILL_PAYLOAD,
                "MCP call_tool response payload: tool={} tenant={} response={}",
                requested_name,
                tenant,
                Self::summarize_str_for_log(&full_response)
            );
            return Ok(CallToolResult::success(vec![Content::text(full_response)]));
        }

        let mut parser = SseParser::default();
        let mut buf = String::new();
        let mut final_result: Option<Value> = None;
        let mut terminal_failure: Option<String> = None;
        let mut saw_done = false;
        let mut stream = response.bytes_stream();

        loop {
            let chunk = tokio::select! {
                _ = ct.cancelled() => {
                    return Err(Self::internal_error("request cancelled"));
                }
                chunk = stream.next() => chunk,
            };
            let Some(chunk) = chunk else { break };
            let chunk = chunk.map_err(|e| {
                Self::internal_error(format!("failed to read endpoint response: {}", e))
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
                    Self::handle_sse_message(
                        &context,
                        message,
                        &mut final_result,
                        &mut terminal_failure,
                        &mut saw_done,
                    )
                    .await?;
                }
            }
        }

        if !buf.is_empty() {
            let mut line = std::mem::take(&mut buf);
            if line.ends_with('\r') {
                line.pop();
            }
            if let Some(message) = parser.push_line(&line) {
                Self::handle_sse_message(
                    &context,
                    message,
                    &mut final_result,
                    &mut terminal_failure,
                    &mut saw_done,
                )
                .await?;
            }
            if let Some(message) = parser.push_line("") {
                Self::handle_sse_message(
                    &context,
                    message,
                    &mut final_result,
                    &mut terminal_failure,
                    &mut saw_done,
                )
                .await?;
            }
        }

        if saw_done {
            if let Some(ref failure) = terminal_failure {
                warn!(
                    "MCP call_tool failed: tool={} reason={}",
                    requested_name, failure
                );
            }
        }
        let final_result =
            Self::resolve_skill_trace_outcome(final_result, terminal_failure, saw_done)?;
        let full_response = Self::extract_text_from_completion(&final_result)
            .ok_or_else(|| Self::internal_error("skill response did not contain final text"))?;
        ctrace!(
            verbose_category::MCP,
            "MCP call_tool response: tool={} tenant={} final_text_present={} response_len={}",
            requested_name,
            tenant,
            !full_response.is_empty(),
            full_response.len()
        );
        ctrace!(
            verbose_category::SKILL_PAYLOAD,
            "MCP call_tool response payload: tool={} tenant={} response={}",
            requested_name,
            tenant,
            Self::summarize_str_for_log(&full_response)
        );
        Ok(CallToolResult::success(vec![Content::text(full_response)]))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> AResult<rmcp::model::ListToolsResult, ErrorData> {
        let (_, tenant) = self.resolve_tenant(&context).await?;
        let query_schema = Self::query_schema_value();
        let tools = self
            .secret
            .GetToolsForTenant(&tenant)
            .await
            .map_err(|e| Self::internal_error(format!("failed to load tenant tools: {:?}", e)))?
            .into_iter()
            .map(|tool| {
                Tool::new(
                    tool.tool_name,
                    tool.description.unwrap_or_default(),
                    std::sync::Arc::new(serde_json::from_value(query_schema.clone()).unwrap()),
                )
            })
            .collect::<Vec<_>>();
        let tool_names = tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        ctrace!(
            verbose_category::MCP,
            "MCP list_tools tenant={} tool_count={} tool_names={:?} input_schema={}",
            tenant,
            tool_names.len(),
            tool_names,
            Self::summarize_value_for_log(&query_schema)
        );

        Ok(rmcp::model::ListToolsResult {
            tools,
            meta: None,
            next_cursor: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{format_skill_fail_reason, update_terminal_failure, McpStreamServer};
    use super::super::skill_trace_sse::SkillTraceEventPayload;
    use serde_json::json;

    #[test]
    fn parse_text_response_extracts_message_content() {
        let body = json!({
            "choices": [{
                "message": {"content": "final answer"}
            }]
        });
        assert_eq!(
            McpStreamServer::parse_text_response(&serde_json::to_vec(&body).unwrap()).unwrap(),
            "final answer"
        );
    }

    #[test]
    fn resolve_outcome_returns_failure_message_when_skill_failed() {
        let result = McpStreamServer::resolve_skill_trace_outcome(
            None,
            Some("skill timed out".to_string()),
            true,
        );
        assert_eq!(result.unwrap_err().message, "skill timed out");
    }

    #[test]
    fn resolve_outcome_returns_skill_result_even_with_terminal_failure_set() {
        let skill_result = json!({"choices": [{"message": {"content": "done"}}]});
        let result = McpStreamServer::resolve_skill_trace_outcome(
            Some(skill_result.clone()),
            Some("skill timed out".to_string()),
            true,
        );
        assert_eq!(result.unwrap(), skill_result);
    }

    fn make_call_finish(
        depth: u32,
        result_code: &str,
        fail_reason: Option<&str>,
    ) -> SkillTraceEventPayload {
        SkillTraceEventPayload {
            event_type: "call_finish".to_string(),
            skill: "acme/default/pricing".to_string(),
            depth,
            result_code: Some(result_code.to_string()),
            fail_reason: fail_reason.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn update_terminal_failure_sets_message_for_root_call_finish_fail() {
        let event = make_call_finish(1, "fail", Some("timeout"));
        let mut terminal_failure: Option<String> = None;
        update_terminal_failure(&event, &mut terminal_failure);
        assert_eq!(
            terminal_failure.as_deref(),
            Some("acme/default/pricing: skill timed out")
        );
    }

    #[test]
    fn update_terminal_failure_ignores_non_root_depth() {
        let event = make_call_finish(2, "fail", Some("timeout"));
        let mut terminal_failure: Option<String> = None;
        update_terminal_failure(&event, &mut terminal_failure);
        assert!(terminal_failure.is_none());
    }

    #[test]
    fn update_terminal_failure_ignores_pass_result() {
        let event = make_call_finish(1, "pass", None);
        let mut terminal_failure: Option<String> = None;
        update_terminal_failure(&event, &mut terminal_failure);
        assert!(terminal_failure.is_none());
    }

    #[test]
    fn update_terminal_failure_parses_fail_reason_from_serde() {
        let json = r#"{"type":"call_finish","skill":"acme/default/pricing","depth":1,"result_code":"fail","fail_reason":"invalid_response"}"#;
        let event: SkillTraceEventPayload = serde_json::from_str(json).unwrap();
        let mut terminal_failure: Option<String> = None;
        update_terminal_failure(&event, &mut terminal_failure);
        assert_eq!(
            terminal_failure.as_deref(),
            Some("acme/default/pricing: skill returned invalid response")
        );
    }

    #[test]
    fn format_skill_fail_reason_maps_known_reasons() {
        assert_eq!(format_skill_fail_reason(Some("timeout")), "skill timed out");
        assert_eq!(
            format_skill_fail_reason(Some("transport_error")),
            "skill transport error"
        );
        assert_eq!(
            format_skill_fail_reason(Some("invalid_response")),
            "skill returned invalid response"
        );
        assert_eq!(
            format_skill_fail_reason(Some("max_depth")),
            "skill exceeded max chain depth"
        );
        assert_eq!(format_skill_fail_reason(None), "skill failed");
        assert_eq!(format_skill_fail_reason(Some("unknown_code")), "skill failed");
    }

    #[test]
    fn mcp_loopback_request_deserializes_as_wire_type() {
        use super::super::skill_chain::{
            build_skill_chat_request, InternalSkillRequestProfile, SkillChatCompletionsWireRequest,
            SkillTraceLevel,
        };
        let body = build_skill_chat_request(
            "hello",
            SkillTraceLevel::Basic,
            InternalSkillRequestProfile::McpToolLoopback,
        );
        let json = serde_json::to_vec(&body).unwrap();
        let wire: SkillChatCompletionsWireRequest = serde_json::from_slice(&json).unwrap();
        assert_eq!(wire.max_tokens, Some(5000));
        assert_eq!(wire.temperature, Some(0.0));
        assert_eq!(wire.stream, Some(false));
        assert_eq!(wire.messages.len(), 1);
        assert_eq!(wire.messages[0]["content"], "hello");
    }
}
