// Token usage tracking for function calls

use std::convert::Infallible;
use std::result::Result as SResult;
use std::sync::Arc;

use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::Response;
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use hyper::body::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::audit::{TokenUsageEvent, TOKEN_USAGE_AGENT};
use crate::gateway::auth_layer::AccessToken;
use crate::gateway::http_gateway::{GatewayId, HttpGateway};

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
    pub cachedTokens: u32,
    pub totalTokens: u32,
}

/// Per-request context needed to emit a billed token-usage event. When present,
/// the response processor emits a `TokenUsageEvent` on the final `usage`; when
/// absent (e.g. the legacy `/modelcall` path), usage is only logged.
#[derive(Debug, Clone)]
pub struct TokenMeterCtx {
    pub gateway_request_id: String,
    pub client_request_id: Option<String>,
    pub caller_tenant: String,
    pub model_slug: String,
    pub source: String,
    pub request_ts: DateTime<Utc>,
}

/// Extract token counts from a `usage` object (prompt/completion/total plus the
/// cached-prompt subset from `prompt_tokens_details.cached_tokens`).
fn parse_usage(usage_obj: &serde_json::Map<String, Value>) -> UsageInfo {
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
    let cached_tokens = usage_obj
        .get("prompt_tokens_details")
        .and_then(|v| v.as_object())
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    UsageInfo {
        promptTokens: prompt_tokens,
        completionTokens: completion_tokens,
        cachedTokens: cached_tokens,
        totalTokens: total_tokens,
    }
}

/// Emit a billed event, or log when there is no meter context.
fn record_usage(meter: &Option<TokenMeterCtx>, usage: &UsageInfo, streaming: bool) {
    match meter {
        Some(ctx) => {
            TOKEN_USAGE_AGENT.Audit(TokenUsageEvent {
                gateway_request_id: ctx.gateway_request_id.clone(),
                client_request_id: ctx.client_request_id.clone(),
                caller_tenant: ctx.caller_tenant.clone(),
                model_slug: ctx.model_slug.clone(),
                source: ctx.source.clone(),
                prompt_tokens: usage.promptTokens as i64,
                completion_tokens: usage.completionTokens as i64,
                cached_tokens: usage.cachedTokens as i64,
                ts: ctx.request_ts,
                gateway_id: Some(GatewayId()),
            });
        }
        None => {
            let kind = if streaming { "stream" } else { "non-streaming" };
            error!("Token usage ({}): {:?}", kind, usage);
        }
    }
}

/// Process an upstream OpenAI-shaped response: tee/parse the `usage` object,
/// emit or log it, and (when the caller did not ask for usage) strip the
/// injected usage line from the streamed body. Shared by the legacy
/// `/modelcall` path and the shared-endpoint dispatch.
pub async fn process_usage_response(
    response: Response,
    is_streaming: bool,
    should_filter_usage: bool,
    meter: Option<TokenMeterCtx>,
    // External-endpoint concurrency permit, held for the whole response lifetime:
    // moved into the streaming task, or dropped when the non-streaming collect
    // returns. `None` on the self-hosted funccall and legacy `/modelcall` paths.
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
) -> Response {
    let status = response.status();
    let headers: Vec<_> = response
        .headers()
        .iter()
        .filter(|(k, _)| *k != hyper::header::CONTENT_LENGTH)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let bodyStream = if is_streaming {
        let (tx, rx) = mpsc::channel::<SResult<Bytes, Infallible>>(128);
        let responseBodyBody = response.into_body();

        tokio::spawn(async move {
            // Hold the permit until this task exits, so the slot spans the generation.
            let _permit = permit;
            let mut body = responseBodyBody;
            // Track the latest `usage` seen and bill it only when the stream ends
            // normally. With stream_options.continuous_usage_stats, intermediate
            // chunks carry partial counts, so the FINAL one (last seen) is the total.
            let mut last_usage: Option<UsageInfo> = None;

            loop {
                let frame = body.frame().await;
                let bytes = match frame {
                    None => {
                        // Stream completed: bill the final usage total.
                        if let Some(usage) = last_usage.as_ref() {
                            record_usage(&meter, usage, true);
                        }
                        return;
                    }
                    Some(b) => match b {
                        Ok(b) => b,
                        Err(e) => {
                            // Truncated/errored stream: no reliable final total, do not bill.
                            error!("Stream frame error: {:?}", e);
                            return;
                        }
                    },
                };
                let bytes: Bytes = bytes.into_data().unwrap_or_default();

                let outputBytes = if let Ok(text) = std::str::from_utf8(&bytes) {
                    let mut filteredLines = String::new();

                    for line in text.lines() {
                        if let Some(json_str) = line
                            .strip_prefix("data: ")
                            .or_else(|| line.strip_prefix("data:"))
                        {
                            if json_str.trim() == "[DONE]" {
                                filteredLines.push_str(line);
                                filteredLines.push('\n');
                                continue;
                            }

                            if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(json_str) {
                                if let Some(Value::Object(usage_obj)) = obj.get("usage") {
                                    last_usage = Some(parse_usage(usage_obj));
                                    if should_filter_usage {
                                        continue;
                                    }
                                }
                            }
                        }
                        filteredLines.push_str(line);
                        filteredLines.push('\n');
                    }

                    Bytes::from(filteredLines)
                } else {
                    bytes
                };

                if tx.send(Ok(outputBytes)).await.is_err() {
                    // Client half went away before the stream finished: not billed
                    // (aborted). TODO: to bill aborted/disconnected requests, record
                    // `last_usage` here (requires stream_options.continuous_usage_stats).
                    return;
                }
            }
        });

        let stream = ReceiverStream::new(rx);
        axum::body::Body::from_stream(http_body_util::StreamBody::new(stream))
    } else {
        // Collect the whole non-streaming body so we can parse `usage` and bill it.
        let responseBody = response
            .into_body()
            .collect()
            .await
            .map(|b| b.to_bytes())
            .unwrap_or_default();

        if let Ok(text) = std::str::from_utf8(&responseBody) {
            if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(text) {
                if let Some(Value::Object(usage_obj)) = obj.get("usage") {
                    record_usage(&meter, &parse_usage(usage_obj), false);
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
    newResponse
}

/// Parse the request body to determine streaming/usage settings and inject
/// `stream_options.include_usage` when streaming without it. Returns the
/// possibly-rewritten request plus `(is_streaming, usage_requested)`.
pub async fn inject_usage_options(req: Request) -> SResult<(Request, bool, bool), StatusCode> {
    let (mut reqParts, body) = req.into_parts();
    let bodyBytes = match axum::body::to_bytes(body, 20 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return Err(StatusCode::BAD_REQUEST),
    };

    let (isStreaming, usageEnabled, requestBodyBytes) =
        if let Ok(mut value) = serde_json::from_slice::<Value>(&bodyBytes) {
            let isStreaming = value
                .as_object()
                .and_then(|obj| obj.get("stream"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let usageEnabled = if isStreaming {
                value
                    .as_object()
                    .and_then(|obj| obj.get("stream_options"))
                    .and_then(|v| v.as_object())
                    .and_then(|opts| opts.get("include_usage"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            } else {
                true
            };

            let requestBodyBytes = if isStreaming && !usageEnabled {
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
            };

            (isStreaming, usageEnabled, requestBodyBytes)
        } else {
            (false, true, bodyBytes)
        };

    let req = Request::from_parts(reqParts, axum::body::Body::from(requestBodyBytes));
    Ok((req, isStreaming, usageEnabled))
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

    let (req, isStreaming, usageEnabled) = inject_usage_options(req).await?;
    let response = FuncCall1(token, gw, req).await?;

    // No meter context on the legacy /modelcall path: usage is logged only.
    // Legacy `/modelcall` path: no external limiter permit.
    Ok(process_usage_response(response, isStreaming, !usageEnabled, None, None).await)
}
