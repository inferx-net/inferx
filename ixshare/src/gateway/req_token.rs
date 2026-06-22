// Token usage tracking for function calls

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
    }
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

/// Wrapper endpoint for FuncCall that handles token usage tracking
pub async fn FuncCallWithTokenTracking(
    token: &Arc<AccessToken>,
    gw: &HttpGateway,
    req: Request,
) -> Result<Response, StatusCode> {
    let path = req.uri().path().to_string();

    let pathParts: Vec<&str> = path.split('/').collect();
    if pathParts.len() < 5 {
        return Err(StatusCode::BAD_REQUEST);
    }

    let (mut reqParts, body) = req.into_parts();
    let bodyBytes = match axum::body::to_bytes(body, 20 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return Err(StatusCode::BAD_REQUEST),
    };

    let isStreaming = IsStreamingRequest(&bodyBytes);
    let usageEnabled = if isStreaming {
        RequestHasUsageEnabled(&bodyBytes)
    } else {
        true
    };

    // If streaming and usage is not enabled, add stream_options.include_usage to the request
    let requestBodyBytes = if isStreaming && !usageEnabled {
        if let Ok(mut value) = serde_json::from_slice::<Value>(&bodyBytes) {
            if let Value::Object(ref mut obj) = value {
                let stream_options = obj
                    .entry("stream_options")
                    .or_insert(Value::Object(serde_json::Map::new()));
                if let Value::Object(ref mut opts) = *stream_options {
                    opts.insert("include_usage".to_string(), Value::Bool(true));
                }
            }
            let newBytes = serde_json::to_vec(&value).unwrap_or_else(|_| bodyBytes.to_vec());

            reqParts.headers.remove(hyper::header::CONTENT_LENGTH);
            reqParts.headers.insert(
                hyper::header::CONTENT_LENGTH,
                hyper::header::HeaderValue::from(newBytes.len()),
            );
            newBytes.into()
        } else {
            bodyBytes
        }
    } else {
        bodyBytes
    };

    let req = Request::from_parts(reqParts, axum::body::Body::from(requestBodyBytes));
    let response = FuncCall1(token, gw, req).await?;

    let status = response.status();
    let headers: Vec<_> = response
        .headers()
        .iter()
        .filter(|(k, _)| *k != hyper::header::CONTENT_LENGTH)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let bodyStream = if isStreaming {
        let (tx, rx) = mpsc::channel::<SResult<Bytes, Infallible>>(128);
        let responseBodyBody = response.into_body();

        tokio::spawn(async move {
            let mut body = responseBodyBody;

            loop {
                let frame = body.frame().await;
                let bytes = match frame {
                    None => return,
                    Some(b) => match b {
                        Ok(b) => b,
                        Err(e) => {
                            error!("Stream frame error: {:?}", e);
                            return;
                        }
                    },
                };
                let bytes: Bytes = bytes.into_data().unwrap_or_default();

                // Log token usage if present in the chunk
                if let Ok(text) = std::str::from_utf8(&bytes) {
                    for line in text.lines() {
                        if let Some(json_str) = line
                            .strip_prefix("data: ")
                            .or_else(|| line.strip_prefix("data:"))
                        {
                            if json_str.trim() == "[DONE]" {
                                break;
                            }
                            if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(json_str)
                            {
                                if let Some(Value::Object(usage_obj)) = obj.get("usage") {
                                    let prompt_tokens = usage_obj
                                        .get("prompt_tokens")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(0) as u32;
                                    let completion_tokens = usage_obj
                                        .get("completion_tokens")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(0) as u32;
                                    let total_tokens = usage_obj
                                        .get("total_tokens")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(0) as u32;

                                    let usage_info = UsageInfo {
                                        promptTokens: prompt_tokens,
                                        completionTokens: completion_tokens,
                                        totalTokens: total_tokens,
                                    };
                                    error!("Token usage (stream): {:?}", usage_info);
                                    break;
                                }
                            }
                        }
                    }
                }

                if tx.send(Ok(bytes)).await.is_err() {
                    return;
                }
            }
        });

        let stream = ReceiverStream::new(rx);
        axum::body::Body::from_stream(http_body_util::StreamBody::new(stream))
    } else {
        let responseBody = response
            .into_body()
            .collect()
            .await
            .map(|b| b.to_bytes())
            .unwrap_or_default();
        
        // Log usage for non-streaming if present
        if let Ok(text) = std::str::from_utf8(&responseBody) {
            if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(text) {
                if let Some(Value::Object(usage_obj)) = obj.get("usage") {
                    let prompt_tokens = usage_obj
                        .get("prompt_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    let completion_tokens = usage_obj
                        .get("completion_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    let total_tokens = usage_obj
                        .get("total_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;

                    let usage_info = UsageInfo {
                        promptTokens: prompt_tokens,
                        completionTokens: completion_tokens,
                        totalTokens: total_tokens,
                    };
                    error!("Token usage (non-streaming): {:?}", usage_info);
                }
            }
        }
        
        axum::body::Body::from(responseBody)
    };

    let mut newResponse = Response::new(bodyStream);
    *newResponse.status_mut() = status;

    for (key, value) in headers {
        if key != hyper::header::CONTENT_LENGTH {
            newResponse.headers_mut().insert(key.clone(), value.clone());
        }
    }

    Ok(newResponse)
}
