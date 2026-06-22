// Token usage tracking and filtering for function calls

use std::convert::Infallible;
use std::result::Result as SResult;
use std::sync::Arc;

use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::Response;
use http_body_util::BodyExt;
use hyper::body::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::gateway::auth_layer::AccessToken;
use crate::gateway::http_gateway::HttpGateway;

use super::http_gateway::FuncCall1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamOptions {
    #[serde(default)]
    pub include_usage: bool,
    #[serde(default)]
    pub continuous_usage_stats: bool,
}

#[derive(Debug)]
pub struct UsageInfo {
    pub promptTokens: u32,
    pub completionTokens: u32,
    pub totalTokens: u32,
}

/// Check if the request body has stream_options.include_usage enabled
pub fn RequestHasUsageEnabled(body: &[u8]) -> bool {
    if let Ok(Value::Object(obj)) = serde_json::from_slice::<Value>(body) {
        if let Some(Value::Object(stream_opts)) = obj.get("stream_options") {
            if let Some(Value::Bool(include_usage)) = stream_opts.get("include_usage") {
                return *include_usage;
            }
        }
        // If streaming but no stream_options, usage is not enabled
        if let Some(Value::Bool(is_stream)) = obj.get("stream") {
            return *is_stream && false;
        }
    }
    // Non-streaming requests always include usage by default
    false
}

/// Check if the request is a streaming request
pub fn IsStreamingRequest(body: &[u8]) -> bool {
    if let Ok(Value::Object(obj)) = serde_json::from_slice::<Value>(body) {
        if let Some(Value::Bool(is_stream)) = obj.get("stream") {
            return *is_stream;
        }
    }
    false
}

/// Filter usage from a JSON response chunk
fn FilterUsageFromJson(value: Value) -> Value {
    if let Value::Object(obj) = value {
        let mut filtered = serde_json::Map::new();
        for (key, val) in obj {
            if key == "usage" {
                // Skip the usage field
                continue;
            }
            filtered.insert(key, FilterUsageFromJson(val));
        }
        Value::Object(filtered)
    } else if let Value::Array(arr) = value {
        Value::Array(arr.into_iter().map(FilterUsageFromJson).collect())
    } else {
        value
    }
}

// Process non-streaming response to extract usage
fn ProcessNonStreamingResponse(body: Bytes, shouldFilterUsage: bool) -> (Bytes, Option<UsageInfo>) {
    if !shouldFilterUsage {
        return (body, None);
    }

    let text = match std::str::from_utf8(&body) {
        Ok(t) => t,
        Err(_) => return (body, None),
    };

    if let Ok(value) = serde_json::from_str::<Value>(text) {
        let usage = if let Value::Object(ref obj) = value {
            obj.get("usage").and_then(|u| {
                if let Value::Object(usageObj) = u {
                    let promptTokens = usageObj
                        .get("prompt_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    let completionTokens = usageObj
                        .get("completion_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    let totalTokens = usageObj
                        .get("total_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;

                    Some(UsageInfo {
                        promptTokens,
                        completionTokens,
                        totalTokens,
                    })
                } else {
                    None
                }
            })
        } else {
            None
        };

        let filtered = FilterUsageFromJson(value);
        match serde_json::to_vec(&filtered) {
            Ok(filteredBytes) => (Bytes::from(filteredBytes), usage),
            Err(_) => (body, None),
        }
    } else {
        (body, None)
    }
}

/// Wrapper endpoint for FuncCall that handles token usage filtering
pub async fn FuncCallWithTokenTracking(
    token: &Arc<AccessToken>,
    gw: &HttpGateway,
    req: Request,
) -> Result<Response, StatusCode> {
    // Note: The token extension is already consumed by the wrapper in http_gateway.rs
    // We need to extract tenant/namespace/funcname from path and proceed
    let path = req.uri().path().to_string();
    let _method = req.method().clone();

    // Extract tenant, namespace, funcname and remaining path from path
    // Path format: /modelcall/tenant/namespace/funcname/*remainPath
    let pathParts: Vec<&str> = path.split('/').collect();
    if pathParts.len() < 5 {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Extract body to check for stream_options
    let (mut reqParts, body) = req.into_parts();
    let bodyBytes = match axum::body::to_bytes(body, 20 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return Err(StatusCode::BAD_REQUEST),
    };

    let isStreaming = IsStreamingRequest(&bodyBytes);
    let usageEnabled = if isStreaming {
        RequestHasUsageEnabled(&bodyBytes)
    } else {
        // Non-streaming: usage is always included by vLLM, we'll filter if needed
        true
    };

    // If streaming and usage is not enabled, add stream_options.include_usage to the request
    let requestBodyBytes = if isStreaming && !usageEnabled {
        if let Ok(mut value) = serde_json::from_slice::<Value>(&bodyBytes) {
            if let Value::Object(ref mut obj) = value {
                // Add or update stream_options with include_usage: true
                let stream_options = obj
                    .entry("stream_options")
                    .or_insert(Value::Object(serde_json::Map::new()));
                if let Value::Object(ref mut opts) = *stream_options {
                    opts.insert("include_usage".to_string(), Value::Bool(true));
                }
            }
            let newBytes = serde_json::to_vec(&value).unwrap_or_else(|_| bodyBytes.to_vec());

            // Update content-length header if it exists
            reqParts.headers.remove(hyper::header::CONTENT_LENGTH);
            reqParts.headers.insert(
                hyper::header::CONTENT_LENGTH,
                hyper::header::HeaderValue::from(newBytes.len()),
            );
            newBytes.into()
        } else {
            error!("Failed to parse request body as JSON");
            bodyBytes
        }
    } else {
        bodyBytes
    };

    // Reconstruct request - extensions are preserved in reqParts
    let req = Request::from_parts(reqParts, axum::body::Body::from(requestBodyBytes));

    let response = FuncCall1(token, gw, req).await?;

    // Save status and headers before consuming the response
    let status = response.status();
    let headers: Vec<_> = response
        .headers()
        .iter()
        .filter(|(k, _)| *k != hyper::header::CONTENT_LENGTH)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    // Determine if we need to filter usage
    let shouldFilterUsage = !usageEnabled;

    error!("FuncCallWithTokenTracking 1");

    let (bodyStream, usageInfo) = if isStreaming {
        let (tx, rx) = mpsc::channel::<SResult<Bytes, Infallible>>(128);

        // Stream the response body through the processor
        let responseBodyBody = response.into_body();
        tokio::spawn(async move {
            let mut body = responseBodyBody;

            loop {
                let frame = body.frame().await;
                let bytes = match frame {
                    None => {
                        return;
                    }
                    Some(b) => match b {
                        Ok(b) => b,
                        Err(e) => {
                            error!("Stream frame error: {:?}", e);
                            return;
                        }
                    },
                };
                let bytes: Bytes = bytes.into_data().unwrap_or_default();

                // Parse and log token usage if present
                if let Ok(text) = std::str::from_utf8(&bytes) {
                    for line in text.lines() {
                        if line.starts_with("data: ") || line.starts_with("data:") {
                            let json_str = line
                                .strip_prefix("data: ")
                                .or(line.strip_prefix("data:"))
                                .unwrap_or(line);
                            if json_str.trim() == "[DONE]" {
                                break;
                            }
                            if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(json_str)
                            {
                                if let Some(Value::Object(usage_obj)) = obj.get("usage") {
                                    let prompt_tokens = usage_obj
                                        .get("prompt_tokens")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(0);
                                    let completion_tokens = usage_obj
                                        .get("completion_tokens")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(0);
                                    let total_tokens = usage_obj
                                        .get("total_tokens")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(0);
                                    error!(
                                        "Token usage - prompt: {}, completion: {}, total: {}",
                                        prompt_tokens, completion_tokens, total_tokens
                                    );
                                }
                            }
                        }
                    }
                }

                // Send bytes unchanged - just stream back as-is
                match tx.send(Ok(bytes)).await {
                    Err(_) => {
                        return;
                    }
                    Ok(()) => (),
                }
            }
        });

        let usage = None;
        let stream = ReceiverStream::new(rx);
        let body = http_body_util::StreamBody::new(stream);
        (axum::body::Body::from_stream(body), usage)
    } else {
        error!("FuncCallWithTokenTracking 3 {}", shouldFilterUsage);
        let responseBody = response
            .into_body()
            .collect()
            .await
            .map(|b| b.to_bytes())
            .unwrap_or_default();
        let (filteredBody, usage) = ProcessNonStreamingResponse(responseBody, true);
        (axum::body::Body::from(filteredBody), usage)
    };
    error!("FuncCallWithTokenTracking 4");

    // Record usage if we captured it (this would integrate with your billing system)
    if let Some(usage) = &usageInfo {
        // TODO: Record usage to audit/billing system
        // REQ_AUDIT_AGENT.Audit(ReqAudit {
        //     tenant,
        //     namespace,
        //     fpname: funcname,
        //     keepalive: false,
        //     ttft: 0,
        //     latency: 0,
        //     prompt_tokens: usage.prompt_tokens,
        //     completion_tokens: usage.completion_tokens,
        // });
        error!(
            "Token usage - prompt: {}, completion: {}, total: {}",
            usage.promptTokens, usage.completionTokens, usage.totalTokens
        );
    }

    // Rebuild response
    let mut newResponse = Response::new(bodyStream);
    *newResponse.status_mut() = status;

    // Copy headers (excluding content-length since body changed)
    for (key, value) in headers {
        if key != hyper::header::CONTENT_LENGTH {
            newResponse.headers_mut().insert(key.clone(), value.clone());
        }
    }

    Ok(newResponse)
}
