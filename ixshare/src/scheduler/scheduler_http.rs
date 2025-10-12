use axum::body::Body;
use axum::http::header::CONTENT_TYPE;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use prometheus_client::encoding::text::encode;

use crate::gateway::metrics::METRICS_REGISTRY;

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

pub async fn SchedulerHttpSrv() {
    let router = Router::new()
        .route("/metrics", get(MetricsHandler))
        .route("/", get(root));
    let port = 80;
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .unwrap();

    axum::serve(listener, router).await.unwrap();
}
