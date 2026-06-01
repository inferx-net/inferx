use anyhow::Result as AResult;
use rmcp::{model::*, service::RequestContext, ErrorData, RoleServer, ServerHandler};
use serde_json::json;
use std::sync::Arc;

use super::secret::{compute_own_skill_tool_names, SqlSecret};

#[derive(Clone)]
pub struct McpStreamServer {
    secret: Arc<SqlSecret>,
    gateway_base_url: String,
}

impl McpStreamServer {
    pub fn new(secret: Arc<SqlSecret>, gateway_base_url: String) -> Self {
        Self {
            secret,
            gateway_base_url,
        }
    }

    fn internal_error(message: impl Into<String>) -> ErrorData {
        ErrorData::new(rmcp::model::ErrorCode::INTERNAL_ERROR, message.into(), None)
    }

    fn extract_authorization_header(
        context: &RequestContext<RoleServer>,
    ) -> Result<String, ErrorData> {
        context
            .extensions
            .get::<http::request::Parts>()
            .and_then(|parts| parts.headers.get("Authorization"))
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .ok_or_else(|| Self::internal_error("missing Authorization header"))
    }

    fn extract_bearer_apikey(
        context: &RequestContext<RoleServer>,
    ) -> Result<(String, String), ErrorData> {
        let authorization = Self::extract_authorization_header(context)?;
        let apikey = authorization
            .strip_prefix("Bearer ")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Self::internal_error("Authorization header must be Bearer <apikey>"))?
            .to_string();
        Ok((authorization, apikey))
    }

    async fn resolve_tenant(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<(String, String), ErrorData> {
        let (authorization, apikey) = Self::extract_bearer_apikey(context)?;
        let key =
            self.secret.GetApikey(&apikey).await.map_err(|e| {
                Self::internal_error(format!("failed to resolve MCP API key: {:?}", e))
            })?;
        let tenant = key.restrict_tenant.ok_or_else(|| {
            Self::internal_error(
                "MCP requires a tenant-restricted API key so skill tools can be resolved",
            )
        })?;
        Ok((authorization, tenant))
    }

    fn skill_url(&self, owner_tenant: &str, owner_namespace: &str, skillname: &str) -> String {
        format!(
            "{}/skills/{}/{}/{}/v1/chat/completions",
            self.gateway_base_url, owner_tenant, owner_namespace, skillname
        )
    }

    fn parse_text_response(body: &[u8]) -> Result<String, ErrorData> {
        let json: serde_json::Value = serde_json::from_slice(body)
            .map_err(|e| Self::internal_error(format!("invalid skill response JSON: {}", e)))?;
        json.get("choices")
            .and_then(|v| v.as_array())
            .and_then(|v| v.first())
            .and_then(|choice| choice.get("message"))
            .and_then(|message| message.get("content"))
            .and_then(|content| match content {
                serde_json::Value::String(s) => Some(s.clone()),
                serde_json::Value::Null => None,
                other => Some(other.to_string()),
            })
            .ok_or_else(|| Self::internal_error("skill response did not contain final text"))
    }
}

impl ServerHandler for McpStreamServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2025_03_26)
            .with_server_info(Implementation::new("mcp-stream-server", "1.0.0"))
            .with_instructions("InferX MCP server exposing tenant skills and subscriptions")
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let (authorization, tenant) = self.resolve_tenant(&context).await?;

        let own_skills = self
            .secret
            .GetOwnSkillsForTenant(&tenant)
            .await
            .map_err(|e| Self::internal_error(format!("failed to load own skills: {:?}", e)))?;
        let own_tool_names = compute_own_skill_tool_names(&own_skills);

        let requested_name = request.name.to_string();
        let own_match = own_skills.iter().find(|skill| {
            own_tool_names.get(&(
                skill.owner_tenant.clone(),
                skill.owner_namespace.clone(),
                skill.skillname.clone(),
            )) == Some(&requested_name)
        });

        let route = if let Some(skill) = own_match {
            (
                skill.owner_tenant.clone(),
                skill.owner_namespace.clone(),
                skill.skillname.clone(),
            )
        } else {
            let same_name = self
                .secret
                .ListOwnSkillsByName(&tenant, &requested_name)
                .await
                .map_err(|e| {
                    Self::internal_error(format!(
                        "failed to check own-skill name ambiguity: {:?}",
                        e
                    ))
                })?;
            if same_name.len() > 1 {
                return Err(Self::internal_error(format!(
                    "ambiguous tool name '{}'; re-run list_tools and use the qualified name",
                    requested_name
                )));
            }

            let subscription = self
                .secret
                .GetSubscribedSkillRoute(&tenant, &requested_name)
                .await
                .map_err(|e| {
                    Self::internal_error(format!("failed to load subscribed skill route: {:?}", e))
                })?;
            let Some(subscription) = subscription else {
                return Err(Self::internal_error("unknown tool or not subscribed"));
            };
            (
                subscription.owner_tenant,
                subscription.owner_namespace,
                subscription.skillname,
            )
        };

        let query = request
            .arguments
            .as_ref()
            .and_then(|args| args.get("query"))
            .and_then(|q| q.as_str())
            .unwrap_or("");

        let endpoint = self.skill_url(&route.0, &route.1, &route.2);
        context
            .peer
            .notify_progress(ProgressNotificationParam {
                progress_token: ProgressToken(NumberOrString::Number(0)),
                progress: 0.0,
                total: None,
                message: Some(format!("Forwarding to {}", endpoint)),
            })
            .await
            .map_err(|e| Self::internal_error(format!("failed to notify progress: {}", e)))?;

        let body = json!({
            "messages": [{"role": "user", "content": query}],
            "max_tokens": 5000,
            "temperature": 0,
            "stream": false
        });

        let client = reqwest::Client::new();
        let response = client
            .post(&endpoint)
            .header("Authorization", authorization)
            .json(&body)
            .send()
            .await
            .map_err(|e| Self::internal_error(format!("failed to connect to endpoint: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let detail = response.text().await.unwrap_or_default();
            let message = if detail.trim().is_empty() {
                format!("endpoint returned error: {}", status)
            } else {
                format!("endpoint returned error: {}: {}", status, detail.trim())
            };
            return Err(Self::internal_error(message));
        }

        let response_bytes = response.bytes().await.map_err(|e| {
            Self::internal_error(format!("failed to read endpoint response: {}", e))
        })?;
        let full_response = Self::parse_text_response(&response_bytes)?;

        Ok(CallToolResult::success(vec![Content::text(full_response)]))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> AResult<rmcp::model::ListToolsResult, ErrorData> {
        let (_, tenant) = self.resolve_tenant(&context).await?;
        let tools = self
            .secret
            .GetToolsForTenant(&tenant)
            .await
            .map_err(|e| Self::internal_error(format!("failed to load tenant tools: {:?}", e)))?
            .into_iter()
            .map(|tool| {
                Tool::new(
                    tool.tool_name,
                    tool.description.unwrap_or_default(),
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

#[cfg(test)]
mod tests {
    use super::{compute_own_skill_tool_names, McpStreamServer};
    use crate::gateway::secret::OwnSkillToolRoute;
    use serde_json::json;

    #[test]
    fn own_skill_unique_name_stays_plain() {
        let tools = vec![OwnSkillToolRoute {
            owner_tenant: "acme".to_string(),
            owner_namespace: "default".to_string(),
            skillname: "pricing".to_string(),
            description: None,
        }];
        let names = compute_own_skill_tool_names(&tools);
        assert_eq!(
            names.get(&(
                "acme".to_string(),
                "default".to_string(),
                "pricing".to_string()
            )),
            Some(&"pricing".to_string())
        );
    }

    #[test]
    fn dotted_or_duplicate_own_skill_name_gets_qualified() {
        let tools = vec![
            OwnSkillToolRoute {
                owner_tenant: "acme".to_string(),
                owner_namespace: "default".to_string(),
                skillname: "my.model".to_string(),
                description: None,
            },
            OwnSkillToolRoute {
                owner_tenant: "acme".to_string(),
                owner_namespace: "other".to_string(),
                skillname: "pricing".to_string(),
                description: None,
            },
            OwnSkillToolRoute {
                owner_tenant: "acme".to_string(),
                owner_namespace: "default".to_string(),
                skillname: "pricing".to_string(),
                description: None,
            },
        ];
        let names = compute_own_skill_tool_names(&tools);
        assert_eq!(
            names.get(&(
                "acme".to_string(),
                "default".to_string(),
                "my.model".to_string()
            )),
            Some(&"my%2Emodel.default.acme".to_string())
        );
        assert_eq!(
            names.get(&(
                "acme".to_string(),
                "default".to_string(),
                "pricing".to_string()
            )),
            Some(&"pricing.default.acme".to_string())
        );
        assert_eq!(
            names.get(&(
                "acme".to_string(),
                "other".to_string(),
                "pricing".to_string()
            )),
            Some(&"pricing.other.acme".to_string())
        );
    }

    #[test]
    fn parse_text_response_extracts_message_content() {
        let body = json!({
            "choices": [{
                "message": {"content": "final answer"}
            }]
        });
        assert_eq!(
            McpStreamServer::parse_text_response(&serde_json::to_vec(&body).unwrap()).unwrap(),
            "final answer"
        );
    }
}
