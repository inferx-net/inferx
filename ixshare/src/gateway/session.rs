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
use uuid::Uuid;

use crate::gateway::http_gateway::{HttpGateway, GATEWAY_CONFIG};
use crate::node_config::NODE_CONFIG;

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
    #[serde(skip)]
    pub skill_api_key: String,
    #[serde(skip)]
    pub last_prompt_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: String,
    pub content: String,
    pub createdAt: DateTime<Utc>,
    pub parts: Vec<Part>,
}

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
}

// ============================================================================
// Storage
// ============================================================================

#[derive(Default, Clone, Debug)]
pub struct SessionStore {
    sessions: Arc<RwLock<HashMap<String, Session>>>,
}

impl SessionStore {
    pub fn New() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn CreateSession(
        &self,
        tenant: String,
        skill_api_key: String,
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
            skill_api_key,
            last_prompt_tokens: 0,
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

    pub async fn ReplaceMessagesWithSummary(&self, session_id: &str, summary: String, tail: Vec<Message>) {
        if let Some(session) = self.sessions.write().await.get_mut(session_id) {
            let summary_msg = Message {
                id: Uuid::new_v4().to_string(),
                role: "system".to_string(),
                content: summary,
                createdAt: Utc::now(),
                parts: vec![],
            };
            session.messages = std::iter::once(summary_msg).chain(tail).collect();
            session.last_prompt_tokens = 0;
            session.updatedAt = Utc::now();
        }
    }

    pub async fn AddMessage(&self, session_id: &str, message: Message) -> Option<Message> {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.messages.push(message.clone());
            session.updatedAt = Utc::now();
            Some(message)
        } else {
            None
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
                info!(
                    "Deleting timed-out session {} (idle for {} mins)",
                    session_id_clone,
                    duration.num_minutes()
                );
                sessions.remove(&session_id_clone);
                return true;
            }
        }
        false
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
        }

        if cleanedCount > 0 {
            info!("Cleaned up {} timed-out sessions", cleanedCount);
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

fn skill_endpoint(gateway_base_url: &str, route: &crate::gateway::secret::TenantToolRoute) -> String {
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

fn agent_model_endpoint(tenant: &str) -> String {
    let env_endpoint = std::env::var("AGENT_MODEL_ENDPOINT").unwrap_or_default();
    let ep = env_endpoint
        .trim()
        .to_string();
    let ep = if ep.is_empty() {
        NODE_CONFIG.agent_model_endpoint.trim().to_string()
    } else {
        ep
    };
    if ep.is_empty() {
        return format!(
            "http://127.0.0.1:{}/funccall",
            GATEWAY_CONFIG.gatewayPort
        );
    }

    if ep.starts_with("http://") || ep.starts_with("https://") {
        return ep;
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
            None => { out.push_str(rest); break; }
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

fn agent_model_context_length() -> u64 {
    std::env::var("AGENT_MODEL_CONTEXT_LENGTH")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(NODE_CONFIG.agent_model_context_length)
}

fn compaction_threshold() -> Option<u64> {
    let ctx = agent_model_context_length();
    if ctx == 0 { None } else { Some(ctx * 80 / 100) }
}
// Keep the last 2 user+assistant turn pairs verbatim after compaction.
const COMPACTION_TAIL_TURNS: usize = 2;

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
) {
    let session = match gw.sessions.GetSession(session_id).await {
        Some(s) => s,
        None => return,
    };

    let messages = &session.messages;
    if messages.len() <= COMPACTION_TAIL_TURNS * 2 {
        return;
    }

    // Split: compact the head, keep the tail verbatim
    let tail_start = messages.len().saturating_sub(COMPACTION_TAIL_TURNS * 2);
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
    let previous_summary = head.first()
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
                    .get("choices").and_then(|c| c.as_array()).and_then(|a| a.first())
                    .and_then(|c| c.get("message"))
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                {
                    info!("Compacting session {} ({} head msgs → summary)", session_id, head.len());
                    gw.sessions.ReplaceMessagesWithSummary(
                        session_id,
                        summary.trim().to_string(),
                        tail,
                    ).await;
                }
            }
        }
        Ok(resp) => error!("Compaction model error: {}", resp.status()),
        Err(e) => error!("Compaction request failed: {}", e),
    }
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

// ============================================================================
// Tool Backend Calls
// ============================================================================

async fn CallSkillTool(endpoint_url: &str, api_key: &str, arguments: &str) -> String {
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

    match client
        .post(endpoint_url)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&skill_request)
        .send()
        .await
    {
        Ok(response) => {
            let status = response.status();
            if status.is_success() {
                match response.json::<serde_json::Value>().await {
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
                            info!("Skill tool response received from {} len={}", endpoint_url, stripped.len());
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

async fn CallModelBackend(
    messages: &[Message],
    endpoint: &str,
    api_key: &str,
    tools: Option<Vec<Tool>>,
    routes: &[crate::gateway::secret::TenantToolRoute],
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
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
            debug!("Got response with status: {}", r.status());
            r
        }
        Err(e) => {
            error!("Request failed: {}", e);
            return Err(format!("Request failed: {}", e).into());
        }
    };

    if !response.status().is_success() {
        let error_text = response.text().await?;
        error!("Model API error: {}", error_text);
        return Err(format!("Model API error: {}", error_text).into());
    }

    let model_response: serde_json::Value = response.json().await?;

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

        debug!("Non-streaming tool calls detected: {}", tool_calls.len());

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
            async move {
                let tool_response = if let Some(route) = routes.iter().find(|r| r.tool_name == tool_call.function.name) {
                    let url = skill_endpoint(&gw_base, route);
                    CallSkillTool(&url, &api_key, &tool_call.function.arguments).await
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
        };

        let synthesis_response = client
            .post(endpoint)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", api_key))
            .json(&synthesis_request)
            .send()
            .await
            .map_err(|e| format!("Synthesis request failed: {}", e))?;

        if !synthesis_response.status().is_success() {
            let error_text = synthesis_response.text().await?;
            return Err(format!("Synthesis model API error: {}", error_text).into());
        }

        let synthesis_json: serde_json::Value = synthesis_response.json().await?;
        let content = synthesis_json
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|c| c.first())
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("No response from model");

        return Ok(content.trim().to_string());
    }

    let content = message
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("No response from model");

    Ok(content.trim().to_string())
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
    let endpoint = agent_model_endpoint(&tenant);
    let api_key = session.skill_api_key.clone();
    let routes = gw.sqlSecret.GetToolsForTenant(&tenant).await.unwrap_or_default();
    let tools_for_request: Option<Vec<Tool>> = if routes.is_empty() {
        None
    } else {
        Some(build_tools_from_routes(&routes))
    };

    let userMessageId = Uuid::new_v4().to_string();
    let userMessage = Message {
        id: userMessageId,
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

    let session = gw.sessions.GetSession(&sessionId).await;
    let messages = session.map(|s| s.messages).unwrap_or_default();

    match CallModelBackend(&messages, &endpoint, &api_key, tools_for_request, &routes).await {
        Ok(responseText) => {
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
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(PromptResponse {
                messageId: String::new(),
                content: format!("Model error: {}", e),
                parts: vec![],
            }),
        )
            .into_response(),
    }
}

// ============================================================================
// Stream Handlers
// ============================================================================

pub async fn HandlePromptStream(
    State(gw): State<HttpGateway>,
    Path(sessionId): Path<String>,
    Json(req): Json<PromptRequest>,
) -> impl IntoResponse {
    info!("[agent] prompt_stream start session={}", sessionId);

    let session = match gw.sessions.GetSession(&sessionId).await {
        Some(s) => s,
        None => return session_not_found_response(),
    };

    if gw.sessions.CheckAndDeleteIfTimedOut(&sessionId).await {
        return session_not_found_response();
    }

    let tenant = session.tenant.clone();
    let endpoint_str = agent_model_endpoint(&tenant);
    let api_key_str = session.skill_api_key.clone();
    let routes = gw.sqlSecret.GetToolsForTenant(&tenant).await.unwrap_or_default();
    info!("[agent] tenant={} endpoint={} tools={}", tenant, endpoint_str, routes.len());

    let userMessageId = Uuid::new_v4().to_string();
    let userMessage = Message {
        id: userMessageId,
        role: "user".to_string(),
        content: req.message.clone(),
        createdAt: Utc::now(),
        parts: vec![Part::Text {
            text: req.message.clone(),
        }],
    };

    gw.sessions.AddMessage(&sessionId, userMessage).await;

    let client = Client::new();
    let session_snapshot = gw.sessions.GetSession(&sessionId).await;
    let messages = session_snapshot.map(|s| s.messages).unwrap_or_default();

    let mut modelMessages = Vec::new();
    modelMessages.push(ModelMessage {
        role: "system".to_string(),
        content: "You are a helpful assistant. When responding, ALWAYS use proper markdown formatting with explicit newlines:\n\n1. Use double newlines (\\n\\n) to separate paragraphs\n2. Use single newlines for line breaks within sections\n3. Format lists with each item on its own line\n4. Format tables with each row on its own line\n5. Use headers (###) on their own lines with blank lines before and after\n6. NEVER run text together without proper spacing\n\nYour responses should be well-formatted markdown that renders correctly.".to_string(),
        tool_calls: None,
    tool_call_id: None,
    });

    for msg in &messages {
        modelMessages.push(ModelMessage {
            role: msg.role.clone(),
            content: msg.content.clone(),
            tool_calls: None,
        tool_call_id: None,
        });
    }

    let tools_option = if routes.is_empty() {
        None
    } else {
        Some(build_tools_from_routes(&routes))
    };

    info!("[agent] sending {} messages to model", modelMessages.len());
    let requestBody = ModelRequest {
        model: None,
        messages: modelMessages,
        stream: true,
        max_tokens: Some(20000),
        temperature: Some(0.0),
        tools: tools_option,
        enable_thinking: Some(false),
    };

    debug!("[agent] model request messages={}", requestBody.messages.len());

    let initial_response = match client
        .post(&endpoint_str)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", api_key_str))
        .json(&requestBody)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            error!("Model request failed before stream: {}", e);
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
        return (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "error": "model_error",
                "message": format!("Model error: {}", error_text)
            })),
        )
            .into_response();
    }

    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(100);

    let gwClone = gw.clone();
    let sessionIdClone = sessionId.to_string();
    let gw_base = format!("http://127.0.0.1:{}", GATEWAY_CONFIG.gatewayPort);

    tokio::spawn(async move {
        info!("[agent] stream started session={}", sessionIdClone);
        let mut stream = initial_response.bytes_stream();
                let mut full_content = String::new();
                let mut tool_calls: Vec<ToolCall> = Vec::new();
                let mut stream_error: Option<String> = None;
                let mut prompt_tokens: u64 = 0;
                let mut finish_reason: Option<String> = None;

                while let Some(result) = stream.next().await {
                    match result {
                        Ok(chunk) => {
                            let chunk_str = String::from_utf8_lossy(chunk.as_ref());

                            for line in chunk_str.lines() {
                                if !line.is_empty() {
                                    debug!("[agent] model stream line: {}", line);
                                }
                                if line.starts_with("data: ") {
                                    let data = &line[6..];

                                    if data == "[DONE]" {
                                        break;
                                    }

                                    if let Ok(json) =
                                        serde_json::from_str::<serde_json::Value>(data)
                                    {
                                        if let Some(usage) = json.get("usage") {
                                            let p = usage
                                                .get("prompt_tokens")
                                                .and_then(|v| v.as_u64())
                                                .unwrap_or(0);
                                            let c = usage
                                                .get("completion_tokens")
                                                .and_then(|v| v.as_u64())
                                                .unwrap_or(0);
                                            debug!("Token usage: prompt={}, completion={}", p, c);
                                                            if p > 0 { prompt_tokens = p; }
                                            info!("[agent] token usage session={} prompt={} completion={}", sessionIdClone, p, c);
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
                                                    .send(Ok(Event::default()
                                                        .data(delta.to_string())))
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
                                                            .unwrap_or(0) as usize;

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
                                                                        slot.function.name = name.to_string();
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
                                                debug!("finish_reason={} tool_calls={}", fr, tool_calls.len());
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

                if let Some(err) = stream_error {
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
                    info!("[agent] model requested {} tool call(s) session={}", tool_calls.len(), sessionIdClone);

                    // Emit tool_call events for all tools first so the browser shows all spinners.
                    // Use index as the correlation key so duplicate tool names in one turn work.
                    for (idx, tool_call) in tool_calls.iter().enumerate() {
                        let _ = tx.send(Ok(Event::default().event("tool_call").data(
                            serde_json::json!({
                                "index": idx,
                                "name": tool_call.function.name,
                                "args": tool_call.function.arguments
                            }).to_string()
                        ))).await;
                        info!(
                            "[agent] dispatching tool='{}' idx={} session={}",
                            tool_call.function.name, idx, sessionIdClone
                        );
                    }

                    // Dispatch all skill calls concurrently; emit tool_done as each one finishes
                    // so the browser updates immediately rather than waiting for the slowest skill.
                    // Assumption: skills are stateless and have no ordering dependencies between
                    // calls in the same turn. The sort below restores model-protocol message order
                    // for synthesis. If ordered execution is ever needed, revert to a for loop.
                    let mut skill_futures: futures::stream::FuturesUnordered<_> =
                        tool_calls.iter().enumerate().map(|(idx, tool_call)| {
                            let gw_base = gw_base.clone();
                            let api_key_str = api_key_str.clone();
                            let routes = routes.clone();
                            async move {
                                let tool_response = if let Some(route) = routes.iter().find(|r| r.tool_name == tool_call.function.name) {
                                    let url = skill_endpoint(&gw_base, route);
                                    CallSkillTool(&url, &api_key_str, &tool_call.function.arguments).await
                                } else {
                                    format!("Unknown tool: {}", tool_call.function.name)
                                };
                                (idx, tool_call.id.clone(), tool_call.function.name.clone(), tool_response)
                            }
                        }).collect();

                    // Collect into a vec sorted by index so synthesis messages are in model order.
                    let mut tool_results: Vec<(String, String)> = Vec::new();
                    let mut completed: Vec<(usize, String, String, String)> = Vec::new();
                    while let Some((idx, tc_id, name, tool_response)) =
                        futures::StreamExt::next(&mut skill_futures).await
                    {
                        info!(
                            "[agent] tool='{}' idx={} response_len={} session={}",
                            name, idx, tool_response.len(), sessionIdClone
                        );
                        let _ = tx.send(Ok(Event::default().event("tool_done").data(
                            serde_json::json!({
                                "index": idx,
                                "name": name,
                                "response": tool_response
                            }).to_string()
                        ))).await;
                        completed.push((idx, tc_id, name, tool_response));
                    }
                    // Restore model order for the synthesis message sequence.
                    completed.sort_by_key(|(idx, _, _, _)| *idx);
                    for (_, tc_id, _, tool_response) in completed {
                        tool_results.push((tc_id, tool_response));
                    }

                    info!("[agent] all tools done, sending synthesis request session={}", sessionIdClone);
                    let prior_messages = gwClone
                        .sessions
                        .GetSession(&sessionIdClone)
                        .await
                        .map(|s| s.messages)
                        .unwrap_or_default();

                    let client = Client::new();
                    let mut model_messages = Vec::new();
                    model_messages.push(ModelMessage {
                        role: "system".to_string(),
                        content: "You are a helpful assistant.".to_string(),
                        tool_calls: None,
                        tool_call_id: None,
                    });
                    for msg in &prior_messages {
                        model_messages.push(ModelMessage {
                            role: msg.role.clone(),
                            content: msg.content.clone(),
                            tool_calls: None,
                            tool_call_id: None,
                        });
                    }
                    // Assistant turn with tool_calls (required by OpenAI protocol)
                    model_messages.push(ModelMessage {
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
                    // One tool message per result, each carrying its tool_call_id
                    for (tc_id, result) in &tool_results {
                        model_messages.push(ModelMessage {
                            role: "tool".to_string(),
                            content: result.clone(),
                            tool_calls: None,
                            tool_call_id: Some(tc_id.clone()),
                        });
                    }

                    let ctx = agent_model_context_length();
                    let syn_max_tokens = if ctx > 0 && prompt_tokens > 0 {
                        ctx.saturating_sub(prompt_tokens).max(512)
                    } else {
                        4096
                    };
                    let final_request = ModelRequest {
                        model: None,
                        messages: model_messages,
                        stream: true,
                        max_tokens: Some(syn_max_tokens as i32),
                        temperature: Some(0.0),
                        tools: None,
                        enable_thinking: Some(false),
                    };

                    debug!("[agent] synthesis request messages={}", final_request.messages.len());

                    match client
                        .post(&endpoint_str)
                        .header("Content-Type", "application/json")
                        .header("Authorization", format!("Bearer {}", api_key_str))
                        .json(&final_request)
                        .send()
                        .await
                    {
                        Ok(final_response) => {
                            if final_response.status().is_success() {
                                let mut syn_stream = final_response.bytes_stream();
                                let mut final_content = String::new();
                                let mut syn_error: Option<String> = None;
                                let mut syn_prompt_tokens: u64 = 0;

                                while let Some(result) = syn_stream.next().await {
                                    match result {
                                        Ok(chunk) => {
                                            let chunk_str = String::from_utf8_lossy(chunk.as_ref());
                                            for line in chunk_str.lines() {
                                                if line.starts_with("data: ") {
                                                    let data = &line[6..];
                                                    if data == "[DONE]" { break; }
                                                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                                                        if let Some(usage) = json.get("usage") {
                                                            let p = usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                                                            if p > 0 { syn_prompt_tokens = p; }
                                                            continue;
                                                        }
                                                        if let Some(delta) = json
                                                            .get("choices")
                                                            .and_then(|c| c.as_array())
                                                            .and_then(|a| a.first())
                                                            .and_then(|c| c.get("delta"))
                                                            .and_then(|d| d.get("content"))
                                                            .and_then(|c| c.as_str())
                                                        {
                                                            final_content.push_str(delta);
                                                            let _ = tx.send(Ok(Event::default().data(delta.to_string()))).await;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            syn_error = Some(format!("{}", e));
                                            break;
                                        }
                                    }
                                }
                                if syn_prompt_tokens > 0 { prompt_tokens = syn_prompt_tokens; }

                                if let Some(err) = syn_error {
                                    error!("Synthesis stream error: {}", err);
                                    let _ = tx.send(Ok(Event::default().event("error").data(
                                        serde_json::json!({
                                            "error": "stream_error",
                                            "message": format!("Skill synthesis call failed: {}", err)
                                        }).to_string(),
                                    ))).await;
                                } else if !final_content.is_empty() {
                                    // Strip <think>…</think> before storing in session history.
                                    let stored_content = {
                                        let mut out = String::new();
                                        let mut rest = final_content.as_str();
                                        while !rest.is_empty() {
                                            if let Some(s) = rest.find("<think>") {
                                                out.push_str(&rest[..s]);
                                                rest = &rest[s + "<think>".len()..];
                                                if let Some(e) = rest.find("</think>") {
                                                    rest = &rest[e + "</think>".len()..];
                                                } else {
                                                    break;
                                                }
                                            } else {
                                                out.push_str(rest);
                                                break;
                                            }
                                        }
                                        out.trim().to_string()
                                    };
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

                                    if compaction_threshold().map_or(false, |t| prompt_tokens >= t) {
                                        info!("Session {} at {} prompt tokens, compacting after tool synthesis", sessionIdClone, prompt_tokens);
                                        let _ = tx.send(Ok(Event::default().event("compacting").data(""))).await;
                                        compact_session(&gwClone, &sessionIdClone, &endpoint_str, &api_key_str).await;
                                        let _ = tx.send(Ok(Event::default().event("compacted").data(""))).await;
                                    }
                                }
                            } else {
                                error!(
                                    "Final model call error status: {}",
                                    final_response.status()
                                );
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
                    info!("[agent] assistant message stored len={} session={}", full_content.len(), sessionIdClone);

                    let _ = tx
                        .send(Ok(Event::default().event("complete").data(format!(
                            "{{\"message_id\": \"{}\"}}",
                            assistantMessageId
                        ))))
                        .await;
                }

                info!("[agent] stream done session={}", sessionIdClone);
                let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;

                // Update stored token count and compact if over threshold
                if prompt_tokens > 0 {
                    gwClone.sessions.UpdateTokens(&sessionIdClone, prompt_tokens).await;
                    if compaction_threshold().map_or(false, |t| prompt_tokens >= t) {
                        info!("Session {} at {} prompt tokens, compacting", sessionIdClone, prompt_tokens);
                        let _ = tx.send(Ok(Event::default().event("compacting").data(""))).await;
                        compact_session(&gwClone, &sessionIdClone, &endpoint_str, &api_key_str).await;
                        let _ = tx.send(Ok(Event::default().event("compacted").data(""))).await;
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
    axum::extract::Extension(token): axum::extract::Extension<std::sync::Arc<crate::gateway::auth_layer::AccessToken>>,
) -> impl IntoResponse {
    let tenant = caller_tenant(&token);
    let mut sessions = gw.sessions.ListSessionsForTenant(&tenant).await;
    if sessions.is_empty() {
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "no_session" }))).into_response();
    }
    sessions.sort_by(|a, b| b.updatedAt.cmp(&a.updatedAt));
    let session = sessions.into_iter().next().unwrap();
    let tool_count = gw.sqlSecret.GetToolsForTenant(&tenant).await.map(|t| t.len()).unwrap_or(0);
    Json(SessionResponse { session, tool_count }).into_response()
}

pub async fn CreateSession(
    State(gw): State<HttpGateway>,
    axum::extract::Extension(token): axum::extract::Extension<std::sync::Arc<crate::gateway::auth_layer::AccessToken>>,
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

    let session = gw.sessions.CreateSession(tenant.clone(), req.api_key).await;
    let tool_count = gw.sqlSecret.GetToolsForTenant(&tenant).await.map(|t| t.len()).unwrap_or(0);

    (StatusCode::CREATED, Json(SessionResponse { session, tool_count })).into_response()
}

pub async fn GetSession(
    State(gw): State<HttpGateway>,
    axum::extract::Extension(token): axum::extract::Extension<std::sync::Arc<crate::gateway::auth_layer::AccessToken>>,
    Path(sessionId): Path<String>,
) -> impl IntoResponse {
    let tenant = caller_tenant(&token);
    match gw.sessions.GetSession(&sessionId).await {
        Some(session) if session.tenant == tenant => {
            let tool_count = gw.sqlSecret.GetToolsForTenant(&tenant).await.map(|t| t.len()).unwrap_or(0);
            Json(SessionResponse { session, tool_count }).into_response()
        }
        Some(_) => session_forbidden_response(),
        None => (StatusCode::NOT_FOUND, "Session not found").into_response(),
    }
}

pub async fn GetSessionMessages(
    State(gw): State<HttpGateway>,
    axum::extract::Extension(token): axum::extract::Extension<std::sync::Arc<crate::gateway::auth_layer::AccessToken>>,
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
    axum::extract::Extension(token): axum::extract::Extension<std::sync::Arc<crate::gateway::auth_layer::AccessToken>>,
    Path(sessionId): Path<String>,
    Json(req): Json<PromptRequest>,
) -> impl IntoResponse {
    let tenant = caller_tenant(&token);
    match gw.sessions.GetSession(&sessionId).await {
        Some(session) if session.tenant != tenant => return session_forbidden_response(),
        None => return session_not_found_response(),
        _ => {}
    }
    HandlePrompt(State(gw), Path(sessionId), Json(req)).await.into_response()
}

pub async fn PromptStream(
    State(gw): State<HttpGateway>,
    axum::extract::Extension(token): axum::extract::Extension<std::sync::Arc<crate::gateway::auth_layer::AccessToken>>,
    Path(sessionId): Path<String>,
    Json(req): Json<PromptRequest>,
) -> impl IntoResponse {
    let tenant = caller_tenant(&token);
    match gw.sessions.GetSession(&sessionId).await {
        Some(session) if session.tenant != tenant => return session_forbidden_response(),
        None => return session_not_found_response(),
        _ => {}
    }
    HandlePromptStream(State(gw), Path(sessionId), Json(req)).await.into_response()
}
