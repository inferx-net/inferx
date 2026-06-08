//! Session-related data structures, storage, and business logic.

use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::{Json, response::IntoResponse};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::RwLock;
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;

use super::AppState;
use super::MODEL_API_KEY;
use super::MODEL_ENDPOINT;
use super::DEFAULT_MODEL;

const SESSION_TIMEOUT_MINUTES: i64 = 10;
pub const SESSION_CLEANUP_INTERVAL_SECS: u64 = 60;

// ============================================================================
// Data Structures
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: String,
    pub content: String,
    pub created_at: DateTime<Utc>,
    pub parts: Vec<Part>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Part {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool")]
    Tool {
        tool_name: String,
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

#[derive(Default, Clone)]
pub struct SessionStore {
    sessions: Arc<RwLock<HashMap<String, Session>>>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn create_session(&self) -> Session {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let session = Session {
            id: id.clone(),
            title: format!("Session {}", &id[..8]),
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
        };

        self.sessions.write().await.insert(id, session.clone());
        session
    }

    pub async fn get_session(&self, id: &str) -> Option<Session> {
        self.sessions.read().await.get(id).cloned()
    }

    pub async fn list_sessions(&self) -> Vec<Session> {
        self.sessions.read().await.values().cloned().collect()
    }

    pub async fn add_message(&self, session_id: &str, message: Message) -> Option<Message> {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.messages.push(message.clone());
            session.updated_at = Utc::now();
            Some(message)
        } else {
            None
        }
    }

    pub async fn get_messages(&self, session_id: &str, limit: usize) -> Vec<Message> {
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

    pub async fn check_session_timeout(&self, session_id: &str) -> bool {
        let sessions = self.sessions.read().await;
        if let Some(session) = sessions.get(session_id) {
            if let Some(last_msg) = session.messages.last() {
                let now = Utc::now();
                let duration = now.signed_duration_since(last_msg.created_at);
                return duration.num_minutes() > SESSION_TIMEOUT_MINUTES;
            }
        }
        false
    }

    pub async fn reset_session(&self, session_id: &str) -> bool {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.messages.clear();
            session.updated_at = Utc::now();
            true
        } else {
            false
        }
    }

    pub async fn check_and_reset_if_timed_out(&self, session_id: &str) -> bool {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(session_id) {
            if let Some(last_msg) = session.messages.last() {
                let now = Utc::now();
                let duration = now.signed_duration_since(last_msg.created_at);
                if duration.num_minutes() > SESSION_TIMEOUT_MINUTES {
                    let count = session.messages.len();
                    session.messages.clear();
                    session.updated_at = Utc::now();
                    tracing::info!(
                        "Reset session {} (timed out {} mins ago, {} messages cleared)",
                        session_id,
                        duration.num_minutes(),
                        count
                    );
                    return true;
                }
            }
        }
        false
    }

    pub async fn cleanup_timed_out_sessions(&self) -> usize {
        let mut sessions = self.sessions.write().await;
        let mut cleaned_count = 0;
        let now = Utc::now();

        for session in sessions.values_mut() {
            if let Some(last_msg) = session.messages.last() {
                let duration = now.signed_duration_since(last_msg.created_at);
                if duration.num_minutes() > SESSION_TIMEOUT_MINUTES {
                    let count = session.messages.len();
                    session.messages.clear();
                    session.updated_at = Utc::now();
                    tracing::info!(
                        "Cleaned up {} messages from session {} (timed out {} mins ago)",
                        count,
                        session.id,
                        duration.num_minutes()
                    );
                    cleaned_count += 1;
                }
            }
        }

        cleaned_count
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
    pub message_id: String,
    pub content: String,
    pub parts: Vec<Part>,
}

#[derive(Debug, Deserialize)]
pub struct MessageListQuery {
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    50
}

// ============================================================================
// Model Types
// ============================================================================

#[derive(Debug, serde::Serialize, Clone)]
struct ModelRequest {
    model: String,
    messages: Vec<ModelMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Debug, serde::Serialize, Clone)]
struct ModelMessage {
    role: String,
    content: String,
}

// ============================================================================
// Session Compaction
// ============================================================================

pub fn should_compact(messages: &[Message]) -> bool {
    messages.len() > 6
}

async fn compact_session(messages: &[Message]) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    tracing::info!("=== COMPACT SESSION STARTED ===");
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
        model: DEFAULT_MODEL.to_string(),
        messages: vec![ModelMessage {
            role: "user".to_string(),
            content: compact_prompt,
        }],
        stream: false,
        max_tokens: Some(4096),
        temperature: Some(0.0),
    };

    let response = client
        .post(MODEL_ENDPOINT)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", MODEL_API_KEY))
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
// Model Backend Calls
// ============================================================================

async fn call_model_backend(messages: &[Message], user_prompt: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let client = Client::new();

    let mut model_messages = Vec::new();
    model_messages.push(ModelMessage {
        role: "system".to_string(),
        content: "You are a helpful assistant.".to_string(),
    });

    for msg in messages {
        model_messages.push(ModelMessage {
            role: msg.role.clone(),
            content: msg.content.clone(),
        });
    }

    model_messages.push(ModelMessage {
        role: "user".to_string(),
        content: user_prompt.to_string(),
    });

    let request_body = ModelRequest {
        model: DEFAULT_MODEL.to_string(),
        messages: model_messages,
        stream: false,
        max_tokens: Some(20000),
        temperature: Some(0.0),
    };

    let response = client
        .post(MODEL_ENDPOINT)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", MODEL_API_KEY))
        .json(&request_body)
        .send()
        .await;

    let response = match response {
        Ok(r) => {
            tracing::info!("Got response with status: {}", r.status());
            r
        }
        Err(e) => {
            tracing::error!("Request failed: {}", e);
            return Err(format!("Request failed: {}", e).into());
        }
    };

    if !response.status().is_success() {
        let error_text = response.text().await?;
        tracing::error!("Model API error: {}", error_text);
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

pub async fn handle_prompt(
    state: &Arc<AppState>,
    session_id: &str,
    req: PromptRequest,
) -> Result<impl IntoResponse, (StatusCode, PromptResponse)> {
    if state.sessions.get_session(session_id).await.is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            PromptResponse {
                message_id: String::new(),
                content: "Session not found".to_string(),
                parts: vec![],
            },
        ));
    }

    if state.sessions.check_and_reset_if_timed_out(session_id).await {
        tracing::info!("Session {} was reset due to timeout", session_id);
    }

    let user_message_id = Uuid::new_v4().to_string();
    let user_message = Message {
        id: user_message_id,
        role: "user".to_string(),
        content: req.message.clone(),
        created_at: Utc::now(),
        parts: vec![Part::Text {
            text: req.message.clone(),
        }],
    };

    state.sessions.add_message(session_id, user_message).await;

    if req.stream {
        return Err((
            StatusCode::TEMPORARY_REDIRECT,
            PromptResponse {
                message_id: String::new(),
                content: "Use /sessions/{id}/prompt_stream for streaming".to_string(),
                parts: vec![],
            },
        ));
    }

    let session = state.sessions.get_session(session_id).await;
    let messages = session.map(|s| s.messages).unwrap_or_default();

    match call_model_backend(&messages, &req.message).await {
        Ok(response_text) => {
            let assistant_message_id = Uuid::new_v4().to_string();
            let parts = vec![Part::Text {
                text: response_text.clone(),
            }];

            let assistant_message = Message {
                id: assistant_message_id.clone(),
                role: "assistant".to_string(),
                content: response_text.clone(),
                created_at: Utc::now(),
                parts: parts.clone(),
            };

            state
                .sessions
                .add_message(session_id, assistant_message)
                .await;

            let session = state.sessions.get_session(session_id).await;
            if session
                .as_ref()
                .map(|s| should_compact(&s.messages))
                .unwrap_or(false)
            {
                tracing::info!("Compacting session {} due to message limit", session_id);
                match compact_session(&session.unwrap().messages).await {
                    Ok(summary) => {
                        tracing::info!(
                            "Session compacted, summary length: {} chars",
                            summary.len()
                        );
                    }
                    Err(e) => {
                        tracing::warn!("Failed to compact session: {}", e);
                    }
                }
            }

            Ok(Json(PromptResponse {
                message_id: assistant_message_id,
                content: response_text,
                parts,
            }))
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            PromptResponse {
                message_id: String::new(),
                content: format!("Model error: {}", e),
                parts: vec![],
            },
        )),
    }
}

pub async fn handle_prompt_stream(
    state: &Arc<AppState>,
    session_id: &str,
    req: PromptRequest,
) -> Sse<ReceiverStream<Result<Event, Infallible>>> {
    tracing::info!("prompt_stream started for session {}", session_id);
    
    if state.sessions.get_session(session_id).await.is_none() {
        let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(1);
        drop(tx);
        return Sse::new(ReceiverStream::new(rx));
    }

    if state.sessions.check_and_reset_if_timed_out(session_id).await {
        tracing::info!("Session {} was reset due to timeout", session_id);
    }

    let user_message_id = Uuid::new_v4().to_string();
    let user_message = Message {
        id: user_message_id,
        role: "user".to_string(),
        content: req.message.clone(),
        created_at: Utc::now(),
        parts: vec![Part::Text {
            text: req.message.clone(),
        }],
    };

    state.sessions.add_message(session_id, user_message).await;

    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(100);

    let state_clone = state.clone();
    let session_id_clone = session_id.to_string();

    tokio::spawn(async move {
        let client = Client::new();

        let session = state_clone.sessions.get_session(&session_id_clone).await;
        let messages = session.map(|s| s.messages).unwrap_or_default();

        let mut model_messages = Vec::new();
        model_messages.push(ModelMessage {
            role: "system".to_string(),
            content: "You are a helpful assistant.".to_string(),
        });

        for msg in &messages {
            model_messages.push(ModelMessage {
                role: msg.role.clone(),
                content: msg.content.clone(),
            });
        }

        let request_body = ModelRequest {
            model: DEFAULT_MODEL.to_string(),
            messages: model_messages,
            stream: false,
            max_tokens: Some(20000),
            temperature: Some(0.0),
        };

        let _ = tx.send(Ok(Event::default().data(""))).await;

        match client
            .post(MODEL_ENDPOINT)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", MODEL_API_KEY))
            .json(&request_body)
            .send()
            .await
        {
            Ok(response) => {
                if !response.status().is_success() {
                    let error_text = response.text().await.unwrap_or_default();
                    let _ = tx
                        .send(Ok(Event::default().data(format!("Error: {}", error_text))))
                        .await;
                    return;
                }

                let model_response: serde_json::Value =
                    response.json().await.unwrap_or(serde_json::json!({}));

                let content = model_response
                    .get("choices")
                    .and_then(|c| c.as_array())
                    .and_then(|c| c.first())
                    .and_then(|c| c.get("message"))
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                    .unwrap_or("No response from model");

                let cleaned_content = content.trim().to_string();
                let cleaned_content_clone = cleaned_content.clone();

                let _ = tx
                    .send(Ok(Event::default().data(cleaned_content_clone)))
                    .await;

                let assistant_message_id = Uuid::new_v4().to_string();
                let parts = vec![Part::Text {
                    text: cleaned_content.to_string(),
                }];

                let assistant_message = Message {
                    id: assistant_message_id.clone(),
                    role: "assistant".to_string(),
                    content: cleaned_content.to_string(),
                    created_at: Utc::now(),
                    parts: parts.clone(),
                };

                state_clone
                    .sessions
                    .add_message(&session_id_clone, assistant_message)
                    .await;

                let _ = tx
                    .send(Ok(Event::default().event("complete").data(format!(
                        "{{\"message_id\": \"{}\"}}",
                        assistant_message_id
                    ))))
                    .await;
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
