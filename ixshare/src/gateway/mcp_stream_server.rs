use anyhow::Result as AResult;
use lazy_static::lazy_static;
use rmcp::{model::*, service::RequestContext, ErrorData, RoleServer, ServerHandler};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

pub struct McpStreamServer {
    history: Arc<RwLock<Vec<serde_json::Value>>>,
}

// Tool definition constant for the model
lazy_static! {
    static ref TOOL_DEFINITION: serde_json::Value = json!({
        "type": "function",
        "function": {
            "name": "call_skillep",
            "description": "Call a skill endpoint to get assistance with the user's request",
            "parameters": {
                "type": "object",
                "properties": {
                    "skillep_id": {
                        "type": "string",
                        "description": "The skill endpoint id to call"
                    },
                    "query": {
                        "type": "string",
                        "description": "The user's complete request and related history"
                    }
                },
                "required": ["skillep_id", "query"]
            }
        }
    });
}

#[derive(Clone, Debug)]
struct ToolConfig {
    Description: String,
    Url: String,
}

/// Represents a tool call extracted from the vLLM response
#[derive(Debug, Clone)]
struct ParsedToolCall {
    Id: String,
    Name: String,
    Arguments: serde_json::Value,
}

/// Handles vLLM API calls with tool support
struct VllmCaller {
    Endpoint: String,
    ApiKey: Option<String>,
}

impl VllmCaller {
    fn New(endpoint: String, apiKey: Option<String>) -> Self {
        Self {
            Endpoint: endpoint,
            ApiKey: apiKey,
        }
    }

    /// Call the vLLM endpoint with user content and history
    async fn Call(
        &self,
        userContent: &str,
        history: &[serde_json::Value],
        tools: &[serde_json::Value],
    ) -> AResult<VllmResponse> {
        // Build messages array
        let mut messages = Vec::new();

        // Add history messages
        for msg in history {
            messages.push(msg.clone());
        }

        // Add current user message
        messages.push(json!({
            "role": "user",
            "content": userContent
        }));

        // Build the request body
        let mut body = json!({
            "messages": messages,
            "max_tokens": 5000,
            "temperature": 0,
            "stream": true
        });

        // Add tools if provided
        if !tools.is_empty() {
            body["tools"] = json!(tools);
            body["tool_choice"] = json!("auto");
        }

        let client = reqwest::Client::new();
        let mut req = client.post(&self.Endpoint).json(&body);

        // Add authorization header if API key exists
        if let Some(ref key) = self.ApiKey {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        let response = req
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to vLLM endpoint: {}", e))?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "vLLM endpoint returned error: {}",
                response.status()
            ));
        }

        // Buffer the entire streaming response
        let body = response.bytes_stream();
        tokio::pin!(body);

        let mut buffer = Vec::new();
        while let Some(chunkResult) = futures::StreamExt::next(&mut body).await {
            let chunk =
                chunkResult.map_err(|e| anyhow::anyhow!("Failed to read response chunk: {}", e))?;
            buffer.extend_from_slice(&chunk);
        }

        // Parse the buffered response
        self.ParseResponse(&buffer)
    }

    /// Parse the buffered vLLM response and extract tool calls or text
    fn ParseResponse(&self, buffer: &[u8]) -> AResult<VllmResponse> {
        let bufferStr = String::from_utf8_lossy(buffer);
        let mut toolCalls: Vec<ParsedToolCall> = Vec::new();
        let mut fullText = String::new();
        let mut hasToolCall = false;

        for line in bufferStr.lines() {
            if line.starts_with("data: ") {
                let data = line[6..].trim();
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }

                if let Ok(jsonData) = serde_json::from_str::<serde_json::Value>(data) {
                    if let Some(choice) = jsonData.get("choices").and_then(|c| c.get(0)) {
                        // Check for tool_calls
                        let toolCallsData = choice
                            .get("delta")
                            .and_then(|d| d.get("tool_calls"))
                            .or_else(|| choice.get("message").and_then(|m| m.get("tool_calls")));

                        if let Some(tc) = toolCallsData {
                            if let Some(arr) = tc.as_array() {
                                if !arr.is_empty() {
                                    hasToolCall = true;
                                    // Parse each tool call
                                    for toolCall in arr {
                                        if let Some(id) =
                                            toolCall.get("id").and_then(|i| i.as_str())
                                        {
                                            if let Some(func) = toolCall.get("function") {
                                                if let Some(name) =
                                                    func.get("name").and_then(|n| n.as_str())
                                                {
                                                    let args = func
                                                        .get("arguments")
                                                        .cloned()
                                                        .unwrap_or_else(|| json!({}));

                                                    toolCalls.push(ParsedToolCall {
                                                        Id: id.to_string(),
                                                        Name: name.to_string(),
                                                        Arguments: args,
                                                    });
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        // Check finish_reason
                        if let Some(finishReason) =
                            choice.get("finish_reason").and_then(|f| f.as_str())
                        {
                            if finishReason == "tool_calls" {
                                hasToolCall = true;
                            }
                        }

                        // Extract text content if not a tool call
                        if !hasToolCall {
                            if let Some(content) = choice
                                .get("delta")
                                .and_then(|d| d.get("content"))
                                .and_then(|c| c.as_str())
                            {
                                fullText.push_str(content);
                            }
                            if let Some(content) = choice
                                .get("message")
                                .and_then(|m| m.get("content"))
                                .and_then(|c| c.as_str())
                            {
                                fullText.push_str(content);
                            }
                        }
                    }
                }
            }
        }

        Ok(VllmResponse {
            hasToolCall,
            toolCalls,
            text: fullText,
        })
    }
}

/// Response from vLLM caller
struct VllmResponse {
    hasToolCall: bool,
    toolCalls: Vec<ParsedToolCall>,
    text: String,
}

impl McpStreamServer {
    pub fn New() -> Self {
        Self {
            history: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Clear history for a new request
    fn ClearHistory(&self) {
        if let Ok(mut history) = self.history.write() {
            history.clear();
        }
    }

    /// Add tool call and result to history
    fn AddToolCallToHistory(&self, toolCall: &ParsedToolCall, result: &str) {
        if let Ok(mut history) = self.history.write() {
            // Add tool call as assistant message
            history.push(json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": toolCall.Id,
                    "type": "function",
                    "function": {
                        "name": toolCall.Name,
                        "arguments": toolCall.Arguments
                    }
                }]
            }));

            // Add tool result as tool message
            history.push(json!({
                "role": "tool",
                "tool_call_id": toolCall.Id,
                "content": result
            }));
        }
    }

    /// Add error tool call and result to history
    fn AddToolCallErrorToHistory(&self, toolCall: &ParsedToolCall, error: &str) {
        if let Ok(mut history) = self.history.write() {
            // Add tool call as assistant message
            history.push(json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": toolCall.Id,
                    "type": "function",
                    "function": {
                        "name": toolCall.Name,
                        "arguments": toolCall.Arguments
                    }
                }]
            }));

            // Add error result as tool message
            history.push(json!({
                "role": "tool",
                "tool_call_id": toolCall.Id,
                "content": format!("Error: {}", error)
            }));
        }
    }

    /// Get a clone of the current history
    fn GetHistory(&self) -> Vec<serde_json::Value> {
        self.history.read().map(|h| h.clone()).unwrap_or_default()
    }

    fn GetToolConfigs() -> BTreeMap<String, ToolConfig> {
        let mut configs = BTreeMap::new();
        configs.insert(
            "ads".to_string(),
            ToolConfig {
                Description:
                    "Get advertising campaign strategy from the ads endpoint via HTTP streaming"
                        .to_string(),
                Url: "http://192.168.0.44:8000/ads/v1/chat/completions".to_string(),
            },
        );
        configs.insert(
            "price".to_string(),
            ToolConfig {
                Description: "Get pricing strategy from the pricing endpoint via HTTP streaming"
                    .to_string(),
                Url: "http://192.168.0.44:8000/price/v1/chat/completions".to_string(),
            },
        );
        configs
    }

    /// Execute a parsed tool call by calling the appropriate endpoint
    /// For sub-tool calls, no history is passed to keep them isolated
    async fn ExecuteToolCall(
        toolCall: &ParsedToolCall,
        configs: &BTreeMap<String, ToolConfig>,
        apiKey: Option<String>,
        _history: &Vec<serde_json::Value>, // History is ignored for sub-tool calls
    ) -> AResult<String> {
        log::info!(
            "Executing tool call: id={}, name={}, args={}",
            toolCall.Id,
            toolCall.Name,
            toolCall.Arguments
        );

        // Look up the tool configuration
        let config = configs
            .get(&toolCall.Name)
            .ok_or_else(|| anyhow::anyhow!("Unknown tool: {}", toolCall.Name))?;

        // Create a vLLM caller for this tool's endpoint
        let caller = VllmCaller::New(config.Url.clone(), apiKey);

        // Prepare the query from tool arguments
        let query = toolCall
            .Arguments
            .get("query")
            .and_then(|q| q.as_str())
            .unwrap_or("");

        // Call the tool endpoint with NO history (isolated execution)
        let response = caller.Call(query, &[], &[]).await?;

        // Return the result
        if response.hasToolCall {
            // Nested tool calls - handle recursively if needed
            log::warn!("Nested tool call detected in tool: {}", toolCall.Name);
            Ok(format!(
                "Nested tool calls in {}: {:?}",
                toolCall.Name, response.toolCalls
            ))
        } else {
            Ok(response.text)
        }
    }
}

impl ServerHandler for McpStreamServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2025_03_26)
            .with_server_info(Implementation::new("mcp-stream-server", "1.0.0"))
            .with_instructions("vLLM MCP HTTP streaming server with multiple tools")
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let configs = McpStreamServer::GetToolConfigs();
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

        let endpoint = config.Url.clone();

        // Extract API key from context
        let apiKey = context
            .extensions
            .get::<http::request::Parts>()
            .and_then(|parts| parts.headers.get("Authorization"))
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(|s| s.to_string());

        // Clear history for this new request
        self.ClearHistory();

        // Prepare tools array for the model
        let tools = vec![TOOL_DEFINITION.clone()];

        // Add initial user message to history
        if let Ok(mut h) = self.history.write() {
            h.push(json!({
                "role": "user",
                "content": query
            }));
        }

        // Loop until the model stops making tool calls
        let mut finalText = String::new();
        let maxIterations = 10; // Prevent infinite loops
        let mut iteration = 0;

        while iteration < maxIterations {
            iteration += 1;

            // Create vLLM caller
            let caller = VllmCaller::New(endpoint.clone(), apiKey.clone());

            // Get current history for this call
            let history = self.GetHistory();

            // Call vLLM with current history
            let response = match caller.Call(query, &history, &tools).await {
                Ok(r) => r,
                Err(e) => {
                    return Err(ErrorData::new(
                        rmcp::model::ErrorCode::INTERNAL_ERROR,
                        format!("vLLM call failed: {}", e),
                        None,
                    ));
                }
            };

            // Check if model made tool calls
            if response.hasToolCall && !response.toolCalls.is_empty() {
                log::info!(
                    "Detected {} tool call(s). Executing them.",
                    response.toolCalls.len()
                );

                // Execute all tool calls and collect results
                for toolCall in &response.toolCalls {
                    match McpStreamServer::ExecuteToolCall(
                        toolCall,
                        &configs,
                        apiKey.clone(),
                        &history,
                    )
                    .await
                    {
                        Ok(result) => {
                            log::info!(
                                "Tool '{}' executed successfully: {}",
                                toolCall.Name,
                                result
                            );

                            // Add tool call and result to history
                            self.AddToolCallToHistory(toolCall, &result);
                        }
                        Err(e) => {
                            log::error!("Tool '{}' execution failed: {}", toolCall.Name, e);

                            // Add tool call and error to history
                            self.AddToolCallErrorToHistory(toolCall, &e.to_string());
                        }
                    }
                }

                // Continue loop to let model process the tool results
                continue;
            }

            // No tool calls - we have the final response
            finalText = response.text;
            break;
        }

        if iteration >= maxIterations {
            log::warn!(
                "Max iterations ({}) reached. Returning accumulated results.",
                maxIterations
            );
            return Err(ErrorData::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                format!("Max tool call iterations ({}) reached", maxIterations),
                None,
            ));
        }

        // Return the final text response
        Ok(CallToolResult::success(vec![Content::text(finalText)]))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> AResult<rmcp::model::ListToolsResult, ErrorData> {
        let configs = McpStreamServer::GetToolConfigs();
        let tools: Vec<Tool> = configs
            .iter()
            .map(|(name, config)| {
                Tool::new(
                    name.clone(),
                    config.Description.clone(),
                    std::sync::Arc::new(
                        serde_json::from_value(serde_json::json!({
                            "type": "object",
                            "properties": {
                                "query": {
                                    "type": "string",
                                    "description": "The user's complete request and related history. Format:\n1. Start with the user's current question\n2. '[Relevant History]: {summary of history relevant to the current question}'\n3. Include any specific requirements or constraints"
                                }
                            },
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

/*

curl "https://model.inferx.net/funccall/tn-a3t79iogb2/endpoints/Qwen3.5-122B-A10B-FP8/v1/chat/completions" \
-H "Content-Type: application/json" \
-d '{
    "messages": [
        {
            "role": "system",
            "content": "When calling the call_skillep tool, format the query parameter as follows:\n1. Start with the user'\''s current question\n2. Add '\''[Relevant History]: {summary of any relevant history}'\''\n3. Include any specific requirements or constraints\n\nAlways include all three parts when the information is available."
        },
        {
            "role": "user",
            "content": "# Marketing Campaign Request\n\n## Current Question\nWhat advertising campaign strategy should we use for our new product launch?\n\n## Task\nPlease help me create an advertising campaign strategy.\n\n## Available Tools\nYou have access to the following skill endpoints:\n- **ads**: Get advertising campaign strategy from the ads"
        }
    ],
    "tools": [
        {
            "type": "function",
            "function": {
                "name": "call_skillep",
                "description": "Call a skill endpoint to get assistance with the user'\''s request",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "skillep_id": {
                            "type": "string",
                            "description": "The skill endpoint id to call"
                        },
                        "query": {
                            "type": "string",
                            "description": "The user'\''s complete request formatted with current question, relevant history, and requirements"
                        }
                    },
                    "required": ["skillep_id", "query"]
                }
            }
        }
    ],
    "tool_choice": "auto",
    "stream": true
}'

{
  "id": "chatcmpl-96614bf1405f5f3c",
  "object": "chat.completion",
  "created": 1780068832,
  "model": "Qwen/Qwen3.5-122B-A10B-FP8",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": null,
        "refusal": null,
        "annotations": null,
        "audio": null,
        "function_call": null,
        "tool_calls": [
          {
            "id": "chatcmpl-tool-859adfe7adf95b70",
            "type": "function",
            "function": {
              "name": "call_skillep",
              "arguments": "{\"skillep_id\": \"ads\", \"query\": \"What advertising campaign strategy should we use for our new product launch?\\n\\n[Relevant History]: None\\n\\nRequirements: Please provide a comprehensive advertising campaign strategy tailored for a new product launch.\"}"
            }
          }
        ],
        "reasoning": null
      },
      "logprobs": null,
      "finish_reason": "tool_calls",
      "stop_reason": null,
      "token_ids": null
    }
  ],
  "service_tier": null,
  "system_fingerprint": null,
  "usage": {
    "prompt_tokens": 452,
    "total_tokens": 531,
    "completion_tokens": 79,
    "prompt_tokens_details": null
  },
  "prompt_logprobs": null,
  "prompt_token_ids": null,
  "kv_transfer_params": null
}


*/
