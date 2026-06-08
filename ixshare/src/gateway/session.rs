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

use crate::gateway::http_gateway::HttpGateway;

const SessionTimeoutMinutes: i64 = 10;
pub const SessionCleanupIntervalSecs: u64 = 60;
const ModelEndpoint: &str =
    "https://dev4.inferx.net/funccall/tn-a3t79iogb2/endpoints/Qwen3.6-35B-A3B-FP8-no-think/v1/chat/completions";
const ModelApiKey: &str = "ix_0929e8b02da13c0437ec37b4a41ba46e936d59c92ae5605a3c3448953d621ba0";
const PricingToolEndpoint: &str =
    "https://dev4.inferx.net/skills/tn-a3t79iogb2/default/pricing/v1/chat/completions";
const PricingToolApiKey: &str =
    "ix_0929e8b02da13c0437ec37b4a41ba46e936d59c92ae5605a3c3448953d621ba0";
const DefaultModel: &str = "Qwen/Qwen3.6-35B-A3B-FP8";

// ============================================================================
// Data Structures
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub createdAt: DateTime<Utc>,
    pub updatedAt: DateTime<Utc>,
    pub messages: Vec<Message>,
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

    pub async fn CreateSession(&self) -> Session {
        let id = Uuid::new_v4().to_string();
        self.CreateSessionWithId(&id).await
    }

    pub async fn CreateSessionWithId(&self, id: &str) -> Session {
        let now = Utc::now();
        let session = Session {
            id: id.to_string(),
            title: format!("Session {}", &id[..8.min(id.len())]),
            createdAt: now,
            updatedAt: now,
            messages: Vec::new(),
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

    pub async fn ListSessions(&self) -> Vec<Session> {
        self.sessions.read().await.values().cloned().collect()
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

    pub async fn CheckSessionTimeout(&self, session_id: &str) -> bool {
        let sessions = self.sessions.read().await;
        if let Some(session) = sessions.get(session_id) {
            if let Some(lastMsg) = session.messages.last() {
                let now = Utc::now();
                let duration = now.signed_duration_since(lastMsg.createdAt);
                return duration.num_minutes() > SessionTimeoutMinutes;
            }
        }
        false
    }

    pub async fn ResetSession(&self, session_id: &str) -> bool {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.messages.clear();
            session.updatedAt = Utc::now();
            true
        } else {
            false
        }
    }

    pub async fn CheckAndDeleteIfTimedOut(&self, session_id: &str) -> bool {
        let sessions = self.sessions.read().await;
        if let Some(session) = sessions.get(session_id) {
            if let Some(lastMsg) = session.messages.last() {
                let now = Utc::now();
                let duration = now.signed_duration_since(lastMsg.createdAt);
                if duration.num_minutes() > SessionTimeoutMinutes {
                    let session_id_clone = session.id.clone();
                    drop(sessions); // Release lock before logging and removing
                    error!(
                        "Deleting timed-out session {} (idle for {} mins)",
                        session_id_clone,
                        duration.num_minutes()
                    );
                    // Remove the session
                    let mut sessions = self.sessions.write().await;
                    sessions.remove(&session_id_clone);
                    return true;
                }
            }
        }
        false
    }

    pub async fn CleanupTimedOutSessions(&self) -> usize {
        let now = Utc::now();
        let mut sessions = self.sessions.write().await;
        let mut to_remove = Vec::new();

        for (id, session) in sessions.iter() {
            if let Some(lastMsg) = session.messages.last() {
                let duration = now.signed_duration_since(lastMsg.createdAt);
                if duration.num_minutes() > SessionTimeoutMinutes {
                    to_remove.push(id.clone());
                }
            }
        }

        let cleanedCount = to_remove.len();
        for id in to_remove {
            sessions.remove(&id);
        }

        if cleanedCount > 0 {
            error!("Cleaned up {} timed-out sessions", cleanedCount);
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
// Session Compaction
// ============================================================================

pub fn ShouldCompact(messages: &[Message]) -> bool {
    messages.len() > 6
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
}

#[derive(Debug, serde::Deserialize, Clone)]
struct ModelMessageIn {
    role: String,
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ToolCall>>,
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

#[derive(Debug, Clone)]
struct ToolCallBuilder {
    id: String,
    r#type: String,
    name: String,
    arguments: String,
}

#[derive(Debug, serde::Deserialize, Clone)]
struct FunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, serde::Serialize, Clone)]
struct ToolMessage {
    role: String,
    content: String,
    tool_call_id: String,
}

// ============================================================================
// Model Backend Calls
// ============================================================================

async fn CallModelBackend(
    messages: &[Message],
    user_prompt: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let client = Client::new();

    let mut model_messages = Vec::new();
            model_messages.push(ModelMessage {
                role: "system".to_string(),
                content: "You are a helpful assistant. When responding, ALWAYS use proper markdown formatting with explicit newlines:\n\n1. Use double newlines (\\n\\n) to separate paragraphs\n2. Use single newlines for line breaks within sections\n3. Format lists with each item on its own line\n4. Format tables with each row on its own line\n5. Use headers (###) on their own lines with blank lines before and after\n6. NEVER run text together without proper spacing\n\nYour responses should be well-formatted markdown that renders correctly.".to_string(),
                tool_calls: None,
            });

    for msg in messages {
        model_messages.push(ModelMessage {
            role: msg.role.clone(),
            content: msg.content.clone(),
            tool_calls: None,
        });
    }

    model_messages.push(ModelMessage {
        role: "user".to_string(),
        content: user_prompt.to_string(),
        tool_calls: None,
    });

    let request_body = ModelRequest {
        model: Some(DefaultModel.to_string()),
        messages: model_messages,
        stream: false,
        max_tokens: Some(20000),
        temperature: Some(0.0),
        tools: None,
    };

    let response = client
        .post(ModelEndpoint)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", ModelApiKey))
        .json(&request_body)
        .send()
        .await;

    let response = match response {
        Ok(r) => {
            error!("Got response with status: {}", r.status());
            r
        }
        Err(e) => {
            log::error!("Request failed: {}", e);
            return Err(format!("Request failed: {}", e).into());
        }
    };

    if !response.status().is_success() {
        let error_text = response.text().await?;
        log::error!("Model API error: {}", error_text);
        return Err(format!("Model API error: {}", error_text).into());
    }

    let model_response: serde_json::Value = response.json().await?;

    let content = model_response
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("No response from model");

    Ok(content.trim().to_string())
}

// ============================================================================
// Session Compaction
// ============================================================================

async fn CompactSession(
    messages: &[Message],
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    error!("=== COMPACT SESSION STARTED ===");
    let client = Client::new();

    let mut conversation_text = String::new();
    for msg in messages {
        if msg.role == "user" {
            conversation_text.push_str(&format!("[User]: {}\n", msg.content));
        } else if msg.role == "assistant" {
            conversation_text.push_str(&format!("[Assistant]: {}\n", msg.content));
        }
    }

    let compact_prompt = format!(
        r#"Output exactly the Markdown structure shown inside <template> and keep the section order unchanged. Do not include the <template> tags in your response.
<template>
## Goal
- [single-sentence task summary]

## Constraints & Preferences
- [user constraints, preferences, specs, or "(none)"]

## Progress
### Done
- [completed work or "(none)"]

### In Progress
- [current work or "(none)"]

### Blocked
- [blockers or "(none)"]

## Key Decisions
- [decision and why, or "(none)"]

## Next Steps
- [ordered next actions or "(none)"]

## Critical Context
- [important technical facts, errors, open questions, or "(none)"]

## Relevant Files
- [file or directory path: why it matters, or "(none)"]
</template>

Rules:
- Keep every section, even when empty.
- Use terse bullets, not prose paragraphs.
- Preserve exact file paths, commands, error strings, and identifiers when known.
- Do not mention the summary process or that context was compacted.

Conversation history:

{}"#,
        conversation_text
    );

    let request_body = ModelRequest {
        model: Some(DefaultModel.to_string()),
        messages: vec![ModelMessage {
            role: "user".to_string(),
            content: compact_prompt,
            tool_calls: None,
        }],
        stream: false,
        max_tokens: Some(4096),
        temperature: Some(0.0),
        tools: None,
    };

    let response = client
        .post(ModelEndpoint)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", ModelApiKey))
        .json(&request_body)
        .send()
        .await?;

    if !response.status().is_success() {
        let error_text = response.text().await?;
        return Err(format!("Model API error: {}", error_text).into());
    }

    let model_response: serde_json::Value = response.json().await?;

    let content = model_response
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first())
        .and_then(|c| c.get("message"))
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
    // Check if session exists
    if gw.sessions.GetSession(&sessionId).await.is_none() {
        // Session doesn't exist, create one with the provided ID
        gw.sessions.CreateSessionWithId(&sessionId).await;
        error!("Created new session {} for prompt", sessionId);
    }

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
        );
    }

    let session = gw.sessions.GetSession(&sessionId).await;
    let messages = session.map(|s| s.messages).unwrap_or_default();

    match CallModelBackend(&messages, &req.message).await {
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

            let session = gw.sessions.GetSession(&sessionId).await;
            if session
                .as_ref()
                .map(|s| ShouldCompact(&s.messages))
                .unwrap_or(false)
            {
                error!("Compacting session {} due to message limit", sessionId);
                match CompactSession(&session.unwrap().messages).await {
                    Ok(summary) => {
                        error!("Session compacted, summary length: {} chars", summary.len());
                    }
                    Err(e) => {
                        log::warn!("Failed to compact session: {}", e);
                    }
                }
            }

            (
                StatusCode::OK,
                Json(PromptResponse {
                    messageId: assistantMessageId,
                    content: responseText,
                    parts: parts,
                }),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(PromptResponse {
                messageId: String::new(),
                content: format!("Model error: {}", e),
                parts: vec![],
            }),
        ),
    }
}

// ============================================================================
// Tool Handlers
// ============================================================================

async fn HandlePricingTool(arguments: &str) -> String {
    log::info!("Pricing tool called with arguments: {}", arguments);

    // Parse the arguments to extract the query
    let query: String = serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|v| {
            v.get("query")
                .and_then(|q| q.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "SaaS pricing advice".to_string());

    // Call the pricing tool API
    let client = Client::new();
    let pricing_request = serde_json::json!({
        "max_tokens": 20000,
        "messages": [
            {"content": "You are a helpful assistant.", "role": "system"},
            {"content": query, "role": "user"}
        ],
        "model": "Qwen/Qwen3.6-35B-A3B-FP8",
        "stream": false,
        "temperature": 0
    });

    match client
        .post(PricingToolEndpoint)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", PricingToolApiKey))
        .json(&pricing_request)
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
                            log::info!("Pricing tool response received");
                            return content.to_string();
                        }
                    }
                    Err(e) => {
                        log::error!("Failed to parse pricing response: {}", e);
                    }
                }
            } else {
                log::error!("Pricing tool API error: {}", status);
            }
            format!("Pricing tool returned an error: {}", status)
        }
        Err(e) => {
            log::error!("Failed to call pricing tool: {}", e);
            format!("Pricing tool call failed: {}", e)
        }
    }
}

// ============================================================================
// Stream Handlers
// ============================================================================

pub async fn HandlePromptStream(
    State(gw): State<HttpGateway>,
    Path(sessionId): Path<String>,
    Json(req): Json<PromptRequest>,
) -> Sse<ReceiverStream<Result<Event, Infallible>>> {
    error!("prompt_stream started for session {}", sessionId);

    // Check if session exists
    if gw.sessions.GetSession(&sessionId).await.is_none() {
        // Session doesn't exist, create one with the provided ID
        gw.sessions.CreateSessionWithId(&sessionId).await;
        error!("Created new session {} for prompt_stream", sessionId);
    } else {
        // Session exists, check if it's timed out and delete if so
        if gw.sessions.CheckAndDeleteIfTimedOut(&sessionId).await {
            error!(
                "Session {} was deleted due to timeout, creating new one",
                sessionId
            );
            gw.sessions.CreateSessionWithId(&sessionId).await;
        }
    }

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

    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(100);

    let gwClone = gw.clone();
    let sessionIdClone = sessionId.to_string();

    tokio::spawn(async move {
        let client = Client::new();

        let session = gwClone.sessions.GetSession(&sessionIdClone).await;
        let messages = session.clone().map(|s| s.messages).unwrap_or_default();

        let mut modelMessages = Vec::new();
        modelMessages.push(ModelMessage {
            role: "system".to_string(),
            content: "You are a helpful assistant. When responding, ALWAYS use proper markdown formatting with explicit newlines:\n\n1. Use double newlines (\\n\\n) to separate paragraphs\n2. Use single newlines for line breaks within sections\n3. Format lists with each item on its own line\n4. Format tables with each row on its own line\n5. Use headers (###) on their own lines with blank lines before and after\n6. NEVER run text together without proper spacing\n\nYour responses should be well-formatted markdown that renders correctly.".to_string(),
            tool_calls: None,
        });

        for msg in &messages {
            modelMessages.push(ModelMessage {
                role: msg.role.clone(),
                content: msg.content.clone(),
                tool_calls: None,
            });
        }

        // Define the pricing tool
        let pricing_tool = Tool {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: "pricing".to_string(),
                description: "the user wants help with pricing decisions, packaging, or monetization strategy. Also use when the user mentions 'pricing,' 'pricing tiers,' 'freemium,' 'free trial,' 'packaging,' 'price increase,' 'value metric,' 'Van Westendorp,' 'willingness to pay,' 'monetization,' 'how much should I charge,' 'my pricing is wrong,' 'pricing page,' 'annual vs monthly,' 'per seat pricing,' or 'should I offer a free plan.' Use this whenever someone is figuring out what to charge or how to structure their plans. For in-app upgrade screens, see paywalls.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "The user's pricing question or context"
                        }
                    },
                    "required": ["query"]
                }),
            },
        };

        let requestBody = ModelRequest {
            model: None,
            messages: modelMessages,
            stream: true,
            max_tokens: Some(2000),
            temperature: Some(0.0),
            tools: Some(vec![pricing_tool]),
        };

        let _ = tx.send(Ok(Event::default().data(""))).await;

        match client
            .post(ModelEndpoint)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", ModelApiKey))
            .json(&requestBody)
            .send()
            .await
        {
            Ok(response) => {
                if !response.status().is_success() {
                    let errorText = response.text().await.unwrap_or_default();
                    let _ = tx
                        .send(Ok(Event::default().data(format!("Error: {}", errorText))))
                        .await;
                    return;
                }

                // Stream the response from the model
                let mut stream = response.bytes_stream();
                let mut full_content = String::new();
                let mut tool_calls: Vec<ToolCall> = Vec::new();

                while let Some(result) = stream.next().await {
                    match result {
                        Ok(chunk) => {
                            let chunk_str = String::from_utf8_lossy(chunk.as_ref());

                            // Parse SSE data lines
                            for line in chunk_str.lines() {
                                if line.starts_with("data: ") {
                                    let data = &line[6..];

                                    // Check for completion marker
                                    if data == "[DONE]" {
                                        break;
                                    }

                                    // Parse the JSON chunk
                                    if let Ok(json) =
                                        serde_json::from_str::<serde_json::Value>(data)
                                    {
                                        // Check for usage info
                                        if let Some(usage) = json.get("usage") {
                                            let prompt_tokens = usage
                                                .get("prompt_tokens")
                                                .and_then(|v| v.as_u64())
                                                .unwrap_or(0);
                                            let completion_tokens = usage
                                                .get("completion_tokens")
                                                .and_then(|v| v.as_u64())
                                                .unwrap_or(0);
                                            error!(
                                                "Token usage: prompt={}, completion={}",
                                                prompt_tokens, completion_tokens
                                            );
                                            continue;
                                        }

                                        if let Some(choice) = json
                                            .get("choices")
                                            .and_then(|c| c.as_array())
                                            .and_then(|a| a.first())
                                        {
                                            // Check for content
                                            if let Some(delta) = choice
                                                .get("delta")
                                                .and_then(|d| d.get("content"))
                                                .and_then(|c| c.as_str())
                                            {
                                                full_content.push_str(delta);
                                                let _ = tx
                                                    .send(Ok(
                                                        Event::default().data(delta.to_string())
                                                    ))
                                                    .await;
                                            }

                                            // Check for tool_calls
                                            if let Some(tc_array) = choice
                                                .get("delta")
                                                .and_then(|d| d.get("tool_calls"))
                                                .and_then(|a| a.as_array())
                                            {
                                                for tc in tc_array {
                                                    if let Some(tc_obj) = tc.as_object() {
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

                                                        if let Some(function) = tc_obj
                                                            .get("function")
                                                            .and_then(|f| f.as_object())
                                                        {
                                                            let name = function
                                                                .get("name")
                                                                .and_then(|v| v.as_str())
                                                                .unwrap_or("")
                                                                .to_string();
                                                            let arguments = function
                                                                .get("arguments")
                                                                .and_then(|v| v.as_str())
                                                                .unwrap_or("")
                                                                .to_string();

                                                            if !name.is_empty()
                                                                || !arguments.is_empty()
                                                            {
                                                                // Check if this is a new tool call or continuation
                                                                if !id.is_empty() {
                                                                    // New tool call
                                                                    tool_calls.push(ToolCall {
                                                                        id: id.clone(),
                                                                        r#type: tc_type.clone(),
                                                                        function: FunctionCall {
                                                                            name: name.clone(),
                                                                            arguments: arguments
                                                                                .clone(),
                                                                        },
                                                                    });
                                                                } else if let Some(last) =
                                                                    tool_calls.last_mut()
                                                                {
                                                                    // Continuation of previous tool call
                                                                    if !arguments.is_empty() {
                                                                        last.function
                                                                            .arguments
                                                                            .push_str(&arguments);
                                                                    }
                                                                    if !name.is_empty()
                                                                        && last
                                                                            .function
                                                                            .name
                                                                            .is_empty()
                                                                    {
                                                                        last.function.name = name;
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }

                                            // Check for finish_reason
                                            if let Some(finish_reason) =
                                                choice.get("finish_reason").and_then(|v| v.as_str())
                                            {
                                                if finish_reason == "tool_calls" {
                                                    error!(
                                                        "call llm with Tool calls complete: {}",
                                                        tool_calls.len()
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            log::error!("Stream error: {}", e);
                            let _ = tx
                                .send(Ok(Event::default().data(format!("Stream error: {}", e))))
                                .await;
                            break;
                        }
                    }
                }

                // Handle tool calls if any
                if !tool_calls.is_empty() {
                    error!("Tool calls detected: {}", tool_calls.len());

                    // Process each tool call
                    for tool_call in &tool_calls {
                        if tool_call.function.name == "pricing" {
                            error!(
                                "Calling pricing tool with args: {}",
                                tool_call.function.arguments
                            );

                            // Call the pricing tool handler
                            let tool_response =
                                HandlePricingTool(&tool_call.function.arguments).await;

                            // Add tool response to messages and make another call to the model
                            let mut updated_messages =
                                session.clone().map(|s| s.messages).unwrap_or_default();

                            // Add assistant message with tool call
                            let assistantMessageId = Uuid::new_v4().to_string();
                            let assistantMessage = Message {
                                id: assistantMessageId.clone(),
                                role: "assistant".to_string(),
                                content: "".to_string(),
                                createdAt: Utc::now(),
                                parts: vec![Part::Text {
                                    text: "".to_string(),
                                }],
                            };
                            updated_messages.push(assistantMessage);

                            // Add tool response message
                            let toolMessageId = Uuid::new_v4().to_string();
                            let toolMessage = Message {
                                id: toolMessageId,
                                role: "tool".to_string(),
                                content: tool_response.clone(),
                                createdAt: Utc::now(),
                                parts: vec![Part::Text {
                                    text: tool_response.clone(),
                                }],
                            };
                            updated_messages.push(toolMessage);

                            // Now call the model again with the tool response
                            let client = Client::new();
                            let mut model_messages = Vec::new();
                            model_messages.push(ModelMessage {
                                role: "system".to_string(),
                                content: "You are a helpful assistant.".to_string(),
                                tool_calls: None,
                            });

                            for msg in &updated_messages {
                                model_messages.push(ModelMessage {
                                    role: msg.role.clone(),
                                    content: msg.content.clone(),
                                    tool_calls: None,
                                });
                            }

                            let final_request = ModelRequest {
                                model: None,
                                messages: model_messages,
                                stream: false,
                                max_tokens: Some(2000),
                                temperature: Some(0.0),
                                tools: None,
                            };

                            match client
                                .post(ModelEndpoint)
                                .header("Content-Type", "application/json")
                                .header("Authorization", format!("Bearer {}", ModelApiKey))
                                .json(&final_request)
                                .send()
                                .await
                            {
                                Ok(final_response) => {
                                    if final_response.status().is_success() {
                                        if let Ok(model_response) =
                                            final_response.json::<serde_json::Value>().await
                                        {
                                            if let Some(content) = model_response
                                                .get("choices")
                                                .and_then(|c| c.as_array())
                                                .and_then(|c| c.first())
                                                .and_then(|c| c.get("message"))
                                                .and_then(|m| m.get("content"))
                                                .and_then(|c| c.as_str())
                                            {
                                                let final_content = content.trim().to_string();
                                                let _ = tx
                                                    .send(Ok(Event::default()
                                                        .data(format!("\n{}", final_content))))
                                                    .await;

                                                // Save the final response
                                                let finalMessageId = Uuid::new_v4().to_string();
                                                let finalMessage = Message {
                                                    id: finalMessageId,
                                                    role: "assistant".to_string(),
                                                    content: final_content.clone(),
                                                    createdAt: Utc::now(),
                                                    parts: vec![Part::Text {
                                                        text: final_content,
                                                    }],
                                                };
                                                gwClone
                                                    .sessions
                                                    .AddMessage(&sessionIdClone, finalMessage)
                                                    .await;
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    log::error!("Final model call error: {}", e);
                                }
                            }
                        }
                    }
                } else if !full_content.is_empty() {
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

                    let _ = tx
                        .send(Ok(Event::default().event("complete").data(format!(
                            "{{\"message_id\": \"{}\"}}",
                            assistantMessageId
                        ))))
                        .await;
                }
            }
            Err(e) => {
                let _ = tx
                    .send(Ok(Event::default().data(format!("Connection error: {}", e))))
                    .await;
            }
        }
    });

    Sse::new(ReceiverStream::new(rx)).keep_alive(axum::response::sse::KeepAlive::new())
}

// ============================================================================
// Session Route Handlers
// ============================================================================

pub async fn ListSessions(State(gw): State<HttpGateway>) -> impl IntoResponse {
    let sessions = gw.sessions.ListSessions().await;
    Json(SessionListResponse { sessions })
}

pub async fn CreateSession(State(gw): State<HttpGateway>) -> impl IntoResponse {
    let session = gw.sessions.CreateSession().await;
    (StatusCode::CREATED, Json(SessionResponse { session }))
}

pub async fn GetSession(
    State(gw): State<HttpGateway>,
    Path(sessionId): Path<String>,
) -> impl IntoResponse {
    match gw.sessions.GetSession(&sessionId).await {
        Some(session) => Json(SessionResponse { session }).into_response(),
        None => (StatusCode::NOT_FOUND, "Session not found").into_response(),
    }
}

pub async fn GetSessionMessages(
    State(gw): State<HttpGateway>,
    Path(sessionId): Path<String>,
    Query(query): Query<MessageListQuery>,
) -> impl IntoResponse {
    let messages = gw.sessions.GetMessages(&sessionId, query.limit).await;
    Json(messages)
}

pub async fn Prompt(
    State(gw): State<HttpGateway>,
    Path(sessionId): Path<String>,
    Json(req): Json<PromptRequest>,
) -> impl IntoResponse {
    HandlePrompt(State(gw), Path(sessionId), Json(req)).await
}

pub async fn PromptStream(
    State(gw): State<HttpGateway>,
    Path(sessionId): Path<String>,
    Json(req): Json<PromptRequest>,
) -> impl IntoResponse {
    HandlePromptStream(State(gw), Path(sessionId), Json(req))
        .await
        .into_response()
}
