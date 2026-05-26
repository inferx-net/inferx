use anyhow::Result as AResult;
use rmcp::{model::*, service::RequestContext, ErrorData, RoleServer, ServerHandler};
use serde_json::json;
use std::collections::BTreeMap;
use http::request::Parts;

pub struct McpStreamServer;

#[derive(Clone, Debug)]
struct ToolConfig {
    description: String,
    url: String,
}

impl McpStreamServer {
    pub fn new() -> Self {
        Self {}
    }

    fn get_tool_configs() -> BTreeMap<String, ToolConfig> {
        let mut configs = BTreeMap::new();
        configs.insert(
            "ads".to_string(),
            ToolConfig {
                description:
                    "Get advertising campaign strategy from the ads endpoint via HTTP streaming"
                        .to_string(),
                url: "http://192.168.0.44:8000/ads/v1/chat/completions".to_string(),
            },
        );
        configs.insert(
            "price".to_string(),
            ToolConfig {
                description: "Get pricing strategy from the pricing endpoint via HTTP streaming"
                    .to_string(),
                url: "https://model.inferx.net/funccall/tn-a3t79iogb2/endpoints/Qwen3.6-35B-A3B-FP8/v1/chat/completions".to_string(),
            },
        );
        configs
    }
}

impl ServerHandler for McpStreamServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new("mcp-stream-server", "1.0.0"))
            .with_instructions("vLLM MCP HTTP streaming server with multiple tools")
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let configs = McpStreamServer::get_tool_configs();
        let config = configs.get(&*request.name).ok_or_else(|| {
            ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                format!("Unknown tool: {}", request.name),
                None,
            )
        })?;

        error!("call_tool get config {:?}", &config);

        let query = request
            .arguments
            .as_ref()
            .and_then(|args| args.get("query"))
            .and_then(|q| q.as_str())
            .unwrap_or("");

        let endpoint = config.url.clone();

        let progress_param = ProgressNotificationParam {
            progress_token: ProgressToken(NumberOrString::Number(0)),
            progress: 0.0,
            total: None,
            message: Some(format!("Forwarding to {} with query: {}", endpoint, query)),
        };
        context
            .peer
            .notify_progress(progress_param)
            .await
            .map_err(|e| {
                ErrorData::new(
                    rmcp::model::ErrorCode::INTERNAL_ERROR,
                    format!("Failed to notify progress: {}", e),
                    None,
                )
            })?;

        let body = json!({
            "messages": [{"role": "user", "content": query}],
            "max_tokens": 5000,
            "temperature": 0,
            "stream": true
        });

        let client = reqwest::Client::new();
        let mut req = client.post(&endpoint);

        let api_key = context
            .extensions
            .get::<http::request::Parts>()
            .and_then(|parts| parts.headers.get("Authorization"))
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(|s| s.to_string());
        
        if let Some(ref key) = api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }
        let response = req.json(&body).send().await.map_err(|e| {
            ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                format!("Failed to connect to endpoint: {}", e),
                None,
            )
        })?;

        if !response.status().is_success() {
            return Err(ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                format!("Endpoint returned error: {}", response.status()),
                None,
            ));
        }

        let mut counter = 0;
        let mut full_response = String::new();
        let body = response.bytes_stream();
        tokio::pin!(body);

        loop {
            match futures::StreamExt::next(&mut body).await {
                Some(Ok(chunk)) => {
                    let chunk_str = String::from_utf8_lossy(&chunk);
                    for line in chunk_str.lines() {
                        if line.starts_with("data: ") {
                            let data = line[6..].trim().to_string();
                            if data.is_empty() || data == "[DONE]" {
                                continue;
                            }
                            if let Ok(json_data) = serde_json::from_str::<serde_json::Value>(&data)
                            {
                                if let Some(choice) =
                                    json_data.get("choices").and_then(|c| c.get(0))
                                {
                                    if let Some(delta) = choice.get("delta") {
                                        if let Some(token) =
                                            delta.get("content").and_then(|c| c.as_str())
                                        {
                                            if !token.is_empty() {
                                                full_response.push_str(token);
                                                context
                                                    .peer
                                                    .notify_progress(ProgressNotificationParam {
                                                        progress_token: ProgressToken(
                                                            NumberOrString::Number(counter),
                                                        ),
                                                        progress: counter as f64,
                                                        total: None,
                                                        message: Some(token.to_string()),
                                                    })
                                                    .await
                                                    .map_err(|e| {
                                                        ErrorData::new(
                                                            rmcp::model::ErrorCode::INTERNAL_ERROR,
                                                            format!(
                                                                "Failed to notify progress: {}",
                                                                e
                                                            ),
                                                            None,
                                                        )
                                                    })?;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Some(Err(e)) => {
                    return Err(ErrorData::new(
                        rmcp::model::ErrorCode::INTERNAL_ERROR,
                        format!("Failed to read response chunk: {}", e),
                        None,
                    ));
                }
                None => break,
            }
            counter += 1;
        }

        Ok(CallToolResult::success(vec![Content::text(full_response)]))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> AResult<rmcp::model::ListToolsResult, ErrorData> {
        let configs = McpStreamServer::get_tool_configs();
        let tools: Vec<Tool> = configs
            .iter()
            .map(|(name, config)| {
                Tool::new(
                    name.clone(),
                    config.description.clone(),
                    std::sync::Arc::new(
                        serde_json::from_value(serde_json::json!({
                            "type": "object",
                            "properties": {"query": {"type": "string"}},
                            "required": ["query"]
                        }))
                        .unwrap(),
                    ),
                )
            })
            .collect();
        Ok(rmcp::model::ListToolsResult {
            tools,
            meta: None,
            next_cursor: None,
        })
    }
}
