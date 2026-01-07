use axum::body::Body;
use axum::extract::Path;
use axum::http::header::CONTENT_TYPE;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Json;
use axum::Router;
use prometheus_client::encoding::text::encode;
use serde_json::json;

use crate::gateway::metrics::METRICS_REGISTRY;
use crate::print::{set_trace_logging, trace_logging_enabled};
use crate::scheduler::scheduler::SCHEDULER;

pub async fn MetricsHandler() -> impl IntoResponse {
    let state = METRICS_REGISTRY.lock().await;
    let mut buffer = String::new();
    encode(&mut buffer, &*state).unwrap();

    Response::builder()
        .status(StatusCode::OK)
        .header(
            CONTENT_TYPE,
            "application/openmetrics-text; version=1.0.0; charset=utf-8",
        )
        .body(Body::from(buffer))
        .unwrap()
}

async fn root() -> &'static str {
    "InferX Scheduler"
}

pub async fn DumpState() -> impl IntoResponse {
    match SCHEDULER.DumpState().await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => {
            let body = json!({ "error": format!("{:?}", e) });
            (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response()
        }
    }
}

pub async fn SetTraceLogging(Path(state): Path<String>) -> Response {
    let lower = state.to_ascii_lowercase();
    let enable = match lower.as_str() {
        "on" | "enable" | "enabled" | "true" | "1" => Some(true),
        "off" | "disable" | "disabled" | "false" | "0" => Some(false),
        _ => None,
    };

    let enable = match enable {
        Some(v) => v,
        None => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from(format!("invalid state '{}', use on/off", state)))
                .unwrap();
        }
    };

    set_trace_logging(enable);
    info!("TRACE_SCHEDULER_LOG: {}", trace_logging_enabled());

    Response::builder()
        .status(StatusCode::OK)
        .body(Body::from(if enable {
            "trace logging enabled"
        } else {
            "trace logging disabled"
        }))
        .unwrap()
}

pub async fn SchedulerHttpSrv() {
    let router = Router::new()
        .route("/metrics", get(MetricsHandler))
        .route("/trace/:state", get(SetTraceLogging))
        .route("/debug/state", get(DumpState))
        .route("/", get(root));
    let port = 80;
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .unwrap();

    axum::serve(listener, router).await.unwrap();
}
