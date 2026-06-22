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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageInfo {
    #[serde(rename = "prompt_tokens")]
    pub promptTokens: u32,
    #[serde(rename = "completion_tokens")]
    pub completionTokens: u32,
    #[serde(rename = "total_tokens")]
    pub totalTokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promptTokensDetails: Option<PromptTokenUsageInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptTokenUsageInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cachedTokens: Option<u32>,
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

/// Parse usage info from a JSON chunk
fn ParseUsageFromChunk(chunk: &Bytes) -> Option<UsageInfo> {
    // Try to parse as SSE data format first (data: {...})
    let text = std::str::from_utf8(chunk).ok()?;
    let jsonStr = if text.starts_with("data: ") {
        text.strip_prefix("data: ").unwrap_or(text)
    } else if text.starts_with("data:") {
        text.strip_prefix("data:").unwrap_or(text)
    } else {
        text
    };

    let jsonStr = jsonStr.trim();
    if jsonStr == "[DONE]" {
        return None;
    }

    if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(jsonStr) {
        if let Some(Value::Object(usageObj)) = obj.get("usage") {
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

            let promptTokensDetails = usageObj.get("prompt_tokens_details").and_then(|v| {
                if let Value::Object(detailsObj) = v {
                    let cached = detailsObj
                        .get("cached_tokens")
                        .and_then(|c| c.as_u64())
                        .map(|c| c as u32);
                    Some(PromptTokenUsageInfo {
                        cachedTokens: cached,
                    })
                } else {
                    None
                }
            });

            return Some(UsageInfo {
                promptTokens,
                completionTokens,
                totalTokens,
                promptTokensDetails,
            });
        }
    }
    None
}

/// Filter usage from SSE data lines
fn FilterUsageFromSseLine(line: &str) -> Option<String> {
    if !line.starts_with("data: ") && !line.starts_with("data:") {
        return Some(line.to_string());
    }

    let prefix = if line.starts_with("data: ") {
        "data: "
    } else {
        "data:"
    };

    let json_str = &line[prefix.len()..];
    let json_str = json_str.trim();

    if json_str == "[DONE]" {
        return Some(line.to_string());
    }

    if let Ok(value) = serde_json::from_str::<Value>(json_str) {
        let filtered = FilterUsageFromJson(value);
        match serde_json::to_string(&filtered) {
            Ok(filtered_str) => Some(format!("{}{}\n", prefix, filtered_str)),
            Err(_) => Some(line.to_string()),
        }
    } else {
        Some(line.to_string())
    }
}

/// Process streaming response to filter usage if needed and extract token counts
async fn ProcessStreamingResponse(
    mut rx: mpsc::Receiver<Option<Bytes>>,
    shouldFilterUsage: bool,
    extractUsage: bool,
) -> (mpsc::Receiver<Option<Bytes>>, Option<UsageInfo>) {
    let (tx, newRx) = mpsc::channel::<Option<Bytes>>(4096);
    let (usageTx, mut usageRx) = mpsc::channel::<Option<UsageInfo>>(1);
    let shouldFilterUsage = Arc::new(shouldFilterUsage);
    let extractUsage = Arc::new(extractUsage);

    tokio::spawn(async move {
        let shouldFilterUsage = Arc::clone(&shouldFilterUsage);
        let extractUsage = Arc::clone(&extractUsage);
        let usageTx = Arc::new(usageTx);
        let mut usageSent = false;

        while let Some(bytesOpt) = rx.recv().await {
            match bytesOpt {
                Some(bytes) => {
                    let mut outputBytes = bytes.clone();

                    if *shouldFilterUsage || *extractUsage {
                        // Try to parse as UTF-8 and process line by line
                        if let Ok(text) = std::str::from_utf8(&bytes) {
                            let mut filteredLines = String::new();

                            for line in text.lines() {
                                if *extractUsage && !usageSent {
                                    if let Some(parsedUsage) =
                                        ParseUsageFromChunk(&Bytes::from(line.to_string()))
                                    {
                                        let _ = usageTx.send(Some(parsedUsage)).await;
                                        usageSent = true;
                                    }
                                }

                                if *shouldFilterUsage {
                                    if let Some(filteredLine) = FilterUsageFromSseLine(line) {
                                        filteredLines.push_str(&filteredLine);
                                    }
                                } else {
                                    filteredLines.push_str(line);
                                    filteredLines.push('\n');
                                }
                            }

                            if *shouldFilterUsage {
                                outputBytes = Bytes::from(filteredLines);
                            }
                        }

                        // If we couldn't parse as UTF-8, try to parse as raw JSON for usage
                        if *extractUsage && !usageSent {
                            if let Some(parsedUsage) = ParseUsageFromChunk(&bytes) {
                                let _ = usageTx.send(Some(parsedUsage)).await;
                                usageSent = true;
                            }
                        }
                    }

                    if tx.send(Some(outputBytes)).await.is_err() {
                        break;
                    }
                }
                None => break,
            }
        }
    });

    let usage = usageRx.recv().await.flatten();
    (newRx, usage)
}

/// Process non-streaming response to extract usage
fn ProcessNonStreamingResponse(body: Bytes, shouldFilterUsage: bool) -> (Bytes, Option<UsageInfo>) {
    error!("ProcessNonStreamingResponse 1");
    if !shouldFilterUsage {
        return (body, None);
    }

    error!("ProcessNonStreamingResponse 2");
    let text = match std::str::from_utf8(&body) {
        Ok(t) => t,
        Err(_) => return (body, None),
    };

    error!("ProcessNonStreamingResponse xx ussge {:#?}", text);
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        let usage = if let Value::Object(ref obj) = value {
            obj.get("usage").and_then(|u| {
                error!("ProcessNonStreamingResponse ussge {:#?}", &u);
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
                        promptTokensDetails: None,
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
    let (reqParts, body) = req.into_parts();
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

    // Reconstruct request - extensions are preserved in reqParts
    let req = Request::from_parts(reqParts, axum::body::Body::from(bodyBytes.clone()));

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn testRequestHasUsageEnabledStreaming() {
        let body = br#"{"stream": true, "stream_options": {"include_usage": true}}"#;
        assert!(RequestHasUsageEnabled(body));
    }

    #[test]
    fn testRequestHasUsageDisabledStreaming() {
        let body = br#"{"stream": true, "stream_options": {"include_usage": false}}"#;
        assert!(!RequestHasUsageEnabled(body));
    }

    #[test]
    fn testIsStreamingRequest() {
        let body = br#"{"stream": true, "messages": []}"#;
        assert!(IsStreamingRequest(body));

        let body = br#"{"stream": false, "messages": []}"#;
        assert!(!IsStreamingRequest(body));
    }

    #[test]
    fn testFilterUsageFromJson() {
        let json = r#"{"id":"test","usage":{"prompt_tokens":10,"completion_tokens":5}}"#;
        let value: Value = serde_json::from_str(json).unwrap();
        let filtered = FilterUsageFromJson(value);

        assert!(filtered.get("usage").is_none());
        assert!(filtered.get("id").is_some());
    }

    #[test]
    fn testParseUsageFromChunk() {
        let chunk = Bytes::from_static(
            b"data: {\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15}}",
        );
        let usage = ParseUsageFromChunk(&chunk);

        assert!(usage.is_some());
        let u = usage.unwrap();
        assert_eq!(u.promptTokens, 10);
        assert_eq!(u.completionTokens, 5);
        assert_eq!(u.totalTokens, 15);
    }
}
