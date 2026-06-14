use anyhow::Result as AResult;
use futures::StreamExt;
use rmcp::{
    model::*, service::RequestContext, service::ServiceError, ErrorData, RoleServer, ServerHandler,
};
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;



use super::secret::SqlSecret;
use crate::{gateway::skill_chain::{Message, SkillCallRequest}, print::verbose_category};

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
        let SseMessage::Event { event, data } = message;
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

#[derive(Default)]
struct SseParser {
    pending_event: Option<String>,
    data_lines: Vec<String>,
}

enum SseMessage {
    Event { event: Option<String>, data: String },
}

impl SseParser {
    fn push_line(&mut self, line: &str) -> Option<SseMessage> {
        if line.starts_with(':') {
            return None;
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

#[derive(Debug, Deserialize)]
struct SkillTraceEventPayload {
    #[serde(rename = "type")]
    event_type: String,
    skill: String,
    #[serde(default)]
    depth: u32,
    #[serde(default)]
    phase: Option<String>,
    #[serde(default)]
    elapsed_ms: Option<u64>,
    #[serde(default)]
    result_code: Option<String>,
    #[serde(default)]
    fail_reason: Option<String>,
    #[serde(default)]
    parent_call_id: Option<String>,
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

        let body = SkillCallRequest {
            messages: vec![Message {
                role: "user".to_string(),
                content: query.to_string(),
            }],
            max_tokens: 5000,
            temperature: 0.0,
            stream: false,
            skill_trace: true,
            tenant: tenant.clone(),
        };

        let body_json = serde_json::to_value(&body).unwrap();

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

        let response = self
            .client
            .post(&endpoint)
            .header("Authorization", authorization)
            .json(&body_json)
            .send()
            .await
            .map_err(|e| Self::internal_error(format!("failed to connect to endpoint: {}", e)))?;

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
            let body = response.bytes().await.map_err(|e| {
                Self::internal_error(format!("failed to read endpoint response: {}", e))
            })?;
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

        while let Some(chunk) = stream.next().await {
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
