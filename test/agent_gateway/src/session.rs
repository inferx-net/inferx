//! Session-related data structures and storage.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

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
    pub role: String,      // "user" | "assistant" | "system"
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
        status: String, // "pending" | "running" | "completed" | "error"
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
