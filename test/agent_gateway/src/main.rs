mod session;

use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{routing::get, routing::post, Json, Router};
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

use session::{PromptRequest, SessionListResponse, SessionResponse, SessionStore};

pub const MODEL_ENDPOINT: &str =
    "https://dev4.inferx.net/skills/tn-a3t79iogb2/default/pricing/v1/chat/completions";
pub const MODEL_API_KEY: &str = "ix_0929e8b02da13c0437ec37b4a41ba46e936d59c92ae5605a3c3448953d621ba0";
pub const DEFAULT_MODEL: &str = "Qwen/Qwen3.6-35B-A3B-FP8";
const WEBUI_DIR: &str = "webui";

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

async fn list_sessions(axum::extract::State(state): axum::extract::State<Arc<AppState>>) -> impl IntoResponse {
    let sessions = state.sessions.list_sessions().await;
    Json(SessionListResponse { sessions })
}

async fn create_session(axum::extract::State(state): axum::extract::State<Arc<AppState>>) -> impl IntoResponse {
    let session = state.sessions.create_session().await;
    (StatusCode::CREATED, Json(SessionResponse { session }))
}

async fn get_session(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    match state.sessions.get_session(&session_id).await {
        Some(session) => Json(SessionResponse { session }).into_response(),
        None => (StatusCode::NOT_FOUND, "Session not found").into_response(),
    }
}

async fn get_session_messages(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    Path(session_id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<session::MessageListQuery>,
) -> impl IntoResponse {
    let messages = state.sessions.get_messages(&session_id, query.limit).await;
    Json(messages)
}

async fn prompt(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Json(req): Json<PromptRequest>,
) -> impl IntoResponse {
    match session::handle_prompt(&state, &session_id, req).await {
        Ok(response) => response.into_response(),
        Err((status, msg)) => (status, Json(msg)).into_response(),
    }
}

async fn prompt_stream(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Json(req): Json<PromptRequest>,
) -> impl IntoResponse {
    session::handle_prompt_stream(&state, &session_id, req).await.into_response()
}

async fn serve_index() -> impl IntoResponse {
    let path = format!("{}/index.html", WEBUI_DIR);
    match tokio::fs::read(path).await {
        Ok(contents) => (
            [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
            contents,
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "Index not found").into_response(),
    }
}

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
        Err(_) => (
            StatusCode::NOT_FOUND,
            format!("File not found: {}", full_path),
        )
            .into_response(),
    }
}

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

    let state_clone = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(
            session::SESSION_CLEANUP_INTERVAL_SECS,
        ));
        loop {
            interval.tick().await;
            let cleaned = state_clone.sessions.cleanup_timed_out_sessions().await;
            if cleaned > 0 {
                tracing::info!("Cleaned up {} timed-out sessions", cleaned);
            }
        }
    });

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([axum::http::Method::GET, axum::http::Method::POST, axum::http::Method::DELETE, axum::http::Method::OPTIONS])
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
