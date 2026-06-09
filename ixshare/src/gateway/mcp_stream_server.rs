use anyhow::Result as AResult;
use futures::StreamExt;
use rmcp::{model::*, service::RequestContext, ErrorData, RoleServer, ServerHandler};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use super::secret::SqlSecret;
use crate::print::verbose_category;

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
                Self::try_notify_progress_message(context, Self::format_trace_message(&event))
                    .await;
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
    phase: Option<String>,
    #[serde(default)]
    elapsed_ms: Option<u64>,
    #[serde(default)]
    result_code: Option<String>,
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
        let route = (
            subscription.owner_tenant,
            subscription.owner_namespace,
            subscription.skillname,
        );

        let query = request
            .arguments
            .as_ref()
            .and_then(|args| args.get("query"))
            .and_then(|q| q.as_str())
            .unwrap_or("");

        let endpoint = self.skill_url(&route.0, &route.1, &route.2);
        Self::try_notify_progress_message(&context, format!("Forwarding to {}", endpoint)).await;

        let body = json!({
            "messages": [{"role": "user", "content": query}],
            "max_tokens": 5000,
            "temperature": 0,
            "stream": false
        });

        ctrace!(
            verbose_category::MCP,
            "MCP forwarding: tool={} -> endpoint={}",
            requested_name,
            endpoint
        );

        let response = self
            .client
            .post(&endpoint)
            .header("Authorization", authorization)
            .header("X-Tenant", tenant.clone())
            .header("X-Skill-Trace", "1")
            .json(&body)
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
            return Ok(CallToolResult::success(vec![Content::text(full_response)]));
        }

        let mut parser = SseParser::default();
        let mut buf = String::new();
        let mut final_result: Option<Value> = None;
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
                    Self::handle_sse_message(&context, message, &mut final_result, &mut saw_done)
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
                Self::handle_sse_message(&context, message, &mut final_result, &mut saw_done)
                    .await?;
            }
            if let Some(message) = parser.push_line("") {
                Self::handle_sse_message(&context, message, &mut final_result, &mut saw_done)
                    .await?;
            }
        }

        if !saw_done && final_result.is_none() {
            return Err(Self::internal_error("trace stream ended before [DONE]"));
        }
        let final_result = final_result
            .ok_or_else(|| Self::internal_error("trace stream missing skill_result"))?;
        let full_response = Self::extract_text_from_completion(&final_result)
            .ok_or_else(|| Self::internal_error("skill response did not contain final text"))?;

        Ok(CallToolResult::success(vec![Content::text(full_response)]))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> AResult<rmcp::model::ListToolsResult, ErrorData> {
        let (_, tenant) = self.resolve_tenant(&context).await?;
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
                    std::sync::Arc::new(
                        serde_json::from_value(serde_json::json!({
                            "type": "object",
                            "properties": {"query": {"type": "string"}},
                            "required": ["query"]
                        }))
                        .unwrap(),
                    ),
                )
            })
            .collect();

        Ok(rmcp::model::ListToolsResult {
            tools,
            meta: None,
            next_cursor: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::McpStreamServer;
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
}
