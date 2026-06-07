mod session;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use tower_http::cors::{Any, CorsLayer};
use axum::response::sse::{Event, Sse};
use chrono::Utc;
use futures::StreamExt;
use reqwest::Client;
use serde::Serialize;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;

use session::{
    Message, Part, PromptRequest, PromptResponse, SessionListResponse,
    SessionResponse, SessionStore,
};

// ============================================================================
// Configuration
// ============================================================================

const MODEL_ENDPOINT: &str = "https://dev4.inferx.net/skills/tn-a3t79iogb2/default/pricing/v1/chat/completions";
const MODEL_API_KEY: &str = "ix_0929e8b02da13c0437ec37b4a41ba46e936d59c92ae5605a3c3448953d621ba0";
const DEFAULT_MODEL: &str = "Qwen/Qwen3.6-35B-A3B-FP8";
const WEBUI_DIR: &str = "webui";

#[derive(Debug, Serialize)]
struct ModelRequest {
    model: String,
    messages: Vec<ModelMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Debug, Serialize)]
struct ModelMessage {
    role: String,
    content: String,
}

// ============================================================================
// Application State
// ============================================================================

#[derive(Clone)]
pub struct AppState {
    pub sessions: SessionStore,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            sessions: SessionStore::new(),
        }
    }
}

// ============================================================================
// API Handlers
// ============================================================================

// GET /sessions - List all sessions
async fn list_sessions(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let sessions = state.sessions.list_sessions().await;
    Json(SessionListResponse { sessions })
}

// POST /sessions - Create a new session
async fn create_session(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let session = state.sessions.create_session().await;
    (StatusCode::CREATED, Json(SessionResponse { session }))
}

// GET /sessions/:id - Get a specific session
async fn get_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    match state.sessions.get_session(&session_id).await {
        Some(session) => Json(SessionResponse { session }).into_response(),
        None => (StatusCode::NOT_FOUND, "Session not found").into_response(),
    }
}

// GET /sessions/:id/messages - Get messages in a session
async fn get_session_messages(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<session::MessageListQuery>,
) -> impl IntoResponse {
    let messages = state.sessions.get_messages(&session_id, query.limit).await;
    Json(messages)
}

// POST /sessions/:id/prompt - Send a prompt (non-streaming)
async fn prompt(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Json(req): Json<PromptRequest>,
) -> (StatusCode, Json<PromptResponse>) {
    if state.sessions.get_session(&session_id).await.is_none() {
        return (StatusCode::NOT_FOUND, Json(PromptResponse {
            message_id: String::new(),
            content: "Session not found".to_string(),
            parts: vec![],
        }));
    }

    // Create user message
    let user_message_id = Uuid::new_v4().to_string();
    let user_message = Message {
        id: user_message_id,
        role: "user".to_string(),
        content: req.message.clone(),
        created_at: Utc::now(),
        parts: vec![Part::Text { text: req.message.clone() }],
    };

    state.sessions.add_message(&session_id, user_message).await;

    if req.stream {
        (StatusCode::TEMPORARY_REDIRECT, Json(PromptResponse {
            message_id: String::new(),
            content: "Use /sessions/{id}/prompt_stream for streaming".to_string(),
            parts: vec![],
        }))
    } else {
        match call_model_backend(&state, &session_id, &req.message).await {
            Ok(response_text) => {
                let assistant_message_id = Uuid::new_v4().to_string();
                let parts = vec![Part::Text { text: response_text.clone() }];
                
                let assistant_message = Message {
                    id: assistant_message_id.clone(),
                    role: "assistant".to_string(),
                    content: response_text.clone(),
                    created_at: Utc::now(),
                    parts: parts.clone(),
                };

                state.sessions.add_message(&session_id, assistant_message).await;

                (StatusCode::OK, Json(PromptResponse {
                    message_id: assistant_message_id,
                    content: response_text,
                    parts,
                }))
            }
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(PromptResponse {
                message_id: String::new(),
                content: format!("Model error: {}", e),
                parts: vec![],
            }))
        }
    }
}

// Call the model backend (non-streaming)
async fn call_model_backend(
    state: &AppState,
    session_id: &str,
    user_prompt: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let client = Client::new();
    
    let session = state.sessions.get_session(session_id).await;
    let messages = session.map(|s| s.messages).unwrap_or_default();
    
    let mut model_messages = Vec::new();
    model_messages.push(ModelMessage {
        role: "system".to_string(),
        content: "You are a helpful assistant.".to_string(),
    });
    
    for msg in messages {
        model_messages.push(ModelMessage {
            role: msg.role,
            content: msg.content,
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
    
    // Remove thinking process - everything before ✅ and  markers
    let content = content.replace("Here's a thinking process:\n\n", "");
    let content = content.replace("Here's a thinking process:", "");
    
    // Find the start of the actual response (after thinking markers)
    // The model outputs: ... ✅ </think>\n\nActual response
    // where ✅ is U+2705 (check mark) and  is U+202F (narrow no-break space) + </thinking> XML
    let final_content = if let Some(pos) = content.find("✅") {
        // Get everything after ✅ marker
        let marker_len = "✅".len();
        let after_emoji = &content[(pos + marker_len)..];
        // Look for  which is U+202F followed by XML closing tag
        if let Some(pos2) = after_emoji.find("</think>") {
            let marker2_len = "</think>".len();
            after_emoji[(pos2 + marker2_len)..].trim()
        } else {
            after_emoji.trim()
        }
    } else if let Some(pos) = content.find("Finch") {
        // Fallback: Get everything after  marker  
        let marker_len = "Finch".len();
        content[(pos + marker_len)..].trim()
    } else {
        // No marker found, return as-is
        content.trim()
    };
    
    Ok(final_content.to_string())
}

// POST /sessions/:id/prompt_stream - Send a prompt (streaming)
async fn prompt_stream(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Json(req): Json<PromptRequest>,
) -> Sse<ReceiverStream<Result<Event, Infallible>>> {
    if state.sessions.get_session(&session_id).await.is_none() {
        let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(1);
        drop(tx);
        return Sse::new(ReceiverStream::new(rx));
    }

    let user_message_id = Uuid::new_v4().to_string();
    let user_message = Message {
        id: user_message_id,
        role: "user".to_string(),
        content: req.message.clone(),
        created_at: Utc::now(),
        parts: vec![Part::Text { text: req.message.clone() }],
    };

    state.sessions.add_message(&session_id, user_message).await;

    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(100);

    let state_clone = state.clone();
    let session_id_clone = session_id.clone();
    let prompt_clone = req.message.clone();
    
    tokio::spawn(async move {
        let client = Client::new();
        
        let session = state_clone.sessions.get_session(&session_id_clone).await;
        let messages = session.map(|s| s.messages).unwrap_or_default();
        
        let mut model_messages = Vec::new();
        model_messages.push(ModelMessage {
            role: "system".to_string(),
            content: "You are a helpful assistant.".to_string(),
        });
        
        for msg in messages {
            model_messages.push(ModelMessage {
                role: msg.role,
                content: msg.content,
            });
        }
        
        model_messages.push(ModelMessage {
            role: "user".to_string(),
            content: prompt_clone.clone(),
        });
        
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
                    let _ = tx.send(Ok(Event::default().data(format!("Error: {}", error_text)))).await;
                    return;
                }
                
                let model_response: serde_json::Value = response.json().await.unwrap_or(serde_json::json!({}));
                
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
                
                // Send the full content at once
                let _ = tx.send(Ok(Event::default().data(cleaned_content_clone))).await;
                
                let assistant_message_id = Uuid::new_v4().to_string();
                let parts = vec![Part::Text { text: cleaned_content.to_string() }];
                
                let assistant_message = Message {
                    id: assistant_message_id.clone(),
                    role: "assistant".to_string(),
                    content: cleaned_content.to_string(),
                    created_at: Utc::now(),
                    parts: parts.clone(),
                };

                state_clone.sessions.add_message(&session_id_clone, assistant_message).await;

                let _ = tx
                    .send(Ok(Event::default()
                        .event("complete")
                        .data(format!("{{\"message_id\": \"{}\"}}", assistant_message_id))))
                    .await;
            }
            Err(e) => {
                let _ = tx.send(Ok(Event::default().data(format!("Connection error: {}", e)))).await;
            }
        }
    });

    Sse::new(ReceiverStream::new(rx))
        .keep_alive(axum::response::sse::KeepAlive::new())
}

// Serve the main index.html
async fn serve_index() -> impl IntoResponse {
    let path = format!("{}/index.html", WEBUI_DIR);
    match tokio::fs::read(path).await {
        Ok(contents) => (
            [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
            contents,
        ).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "Index not found").into_response(),
    }
}

// Fallback for static files
async fn serve_static_file(Path(path): Path<String>) -> impl IntoResponse {
    let full_path = format!("{}/{}", WEBUI_DIR, path);
    match tokio::fs::read(&full_path).await {
        Ok(contents) => {
            let content_type = if path.ends_with(".css") {
                "text/css"
            } else if path.ends_with(".js") {
                "application/javascript"
            } else if path.ends_with(".png") || path.ends_with(".jpg") || path.ends_with(".gif") {
                "image/*"
            } else {
                "text/plain"
            };
            ([(axum::http::header::CONTENT_TYPE, content_type)], contents).into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, format!("File not found: {}", full_path)).into_response(),
    }
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    let state = Arc::new(AppState::new());

    let _default_session = state.sessions.create_session().await;
    tracing::info!("Created default session");

    // Create CORS layer to allow all origins (for development)
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::OPTIONS])
        .allow_headers(Any);

    let app = Router::new()
        .route("/", get(serve_index))
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/sessions/:id", get(get_session))
        .route("/sessions/:id/messages", get(get_session_messages))
        .route("/sessions/:id/prompt", post(prompt))
        .route("/sessions/:id/prompt_stream", post(prompt_stream))
        .route("/health", get(|| async { "OK" }))
        .fallback(serve_static_file)
        .layer(cors)
        .with_state(state);

    let addr = "0.0.0.0:3000";
    tracing::info!("Server starting on {}", addr);
    
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn test_create_session() {
        let state = Arc::new(AppState::new());
        let app = Router::new()
            .route("/sessions", post(create_session))
            .with_state(state.clone());

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/sessions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn test_prompt() {
        let state = Arc::new(AppState::new());
        let session = state.sessions.create_session().await;
        
        let app = Router::new()
            .route("/sessions/:id/prompt", post(prompt))
            .with_state(state.clone());

        let body = serde_json::json!({
            "message": "Hello, world!",
            "stream": false
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/sessions/{}/prompt", session.id))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }
}


