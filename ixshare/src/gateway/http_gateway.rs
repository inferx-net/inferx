// Copyright (c) 2025 InferX Authors /
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under

use core::str;
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::result::Result as SResult;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicI64;
use std::sync::Arc;

use inferxlib::obj_mgr::funcpolicy_mgr::{FuncPolicy, FuncPolicySpec};
use opentelemetry::global::ObjectSafeSpan;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::trace::Tracer;
use opentelemetry::KeyValue;

use axum::extract::{Query, Request, State};
use axum::http::HeaderValue;
use axum::middleware::{from_fn_with_state, Next};
use axum::response::{IntoResponse, Response};
use axum::Json;
use axum::{
    body::Body, extract::Path, routing::delete, routing::get, routing::head, routing::post,
    routing::put, Extension, Router,
};

use super::session::SessionStore;
use chrono::{DateTime, Timelike, Utc};
use hyper::header::CONTENT_TYPE;
use inferxlib::obj_mgr::namespace_mgr::Namespace;
use inferxlib::obj_mgr::pod_mgr::PodState;
use inferxlib::obj_mgr::tenant_mgr::{Tenant, SYSTEM_NAMESPACE, SYSTEM_TENANT};
use opentelemetry::Context;
use prometheus_client::encoding::text::encode;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::StreamableHttpServerConfig;
use rmcp::transport::StreamableHttpService;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tower_http::cors::{Any, CorsLayer};

use axum_server::tls_rustls::RustlsConfig;
use http_body_util::BodyExt;
use hyper::body::Bytes;
use hyper::{StatusCode, Uri};

use tokio::sync::mpsc;

use crate::audit::{
    BillingCreditHistoryRecord, BillingRateHistoryRecord, ReqAudit, SqlAudit,
    TenantCreditHistoryRecord, REQ_AUDIT_AGENT,
};
use crate::common::*;
use crate::gateway::auth_layer::auth_transform_keycloaktoken;
use crate::gateway::func_worker::QHttpCallClientDirect;
use crate::gateway::mcp_stream_server::{McpCancelRegistry, McpStreamServer};
use crate::gateway::tokenizer::{CountKnowledgeBaseTokens, ModelsFuncCall};
use crate::ixmeta::req_watching_service_client::ReqWatchingServiceClient;
use crate::ixmeta::ReqWatchRequest;
use crate::metastore::cacher_client::CacherClient;
use crate::metastore::unique_id::UID;
use crate::na;
use crate::node_config::{GatewayConfig, NODE_CONFIG};
use crate::peer_mgr::IxTcpClient;
use crate::print::{set_trace_logging, trace_logging_enabled, verbose_category};
use inferxlib::data_obj::DataObject;
use inferxlib::obj_mgr::func_mgr::{ApiType, FuncState, Function};

use super::auth_layer::Grant;
use super::auth_layer::ObjectType;
use super::auth_layer::PermissionType;
use super::auth_layer::{AccessToken, ApikeyCreateRequest, ApikeyDeleteRequest, GetTokenCache};
use super::func_agent_mgr::IxTimestamp;
use super::func_agent_mgr::GW_OBJREPO;
use super::func_agent_mgr::{FuncAgentMgr, FuncIdentity, FuncRouteTarget};
use super::func_worker::QHttpCallClient;
use super::func_worker::RETRYABLE_HTTP_STATUS;
use super::gw_obj_repo::FuncDetail;
use super::gw_obj_repo::{GwObjRepo, NamespaceStore};
use super::log_admin::{
    DisableVerboseCategory, EnableVerboseCategory, GetVerboseCategories, PutVerboseCategories,
};
use super::metrics::FunccallLabels;
use super::metrics::Status;
use super::metrics::GATEWAY_METRICS;
use super::metrics::METRICS_REGISTRY;
use super::scheduler_client::SCHEDULER_CLIENT;
use super::http_gw::BuildOpenRouterModelEntry;
use super::secret::{EndpointMetadata, EndpointOpenRouterMetadata, SkillDetail, SqlSecret};
use super::skill_chain::{
    extract_is_child_request, handle_skill_call_chain, SkillChatCompletionsWireRequest,
    SkillInvocationContext, SKILL_CHAIN_DEPTH_HEADER,
};
// use super::tokenizer::KnowledgeBaseRoute;
use super::tokenizer::NormalizeFuncRequest;
use super::tokenizer::TokenizerRoute;
pub static GATEWAY_ID: AtomicI64 = AtomicI64::new(-1);
const FUNCCALL_MAX_BODY_BYTES: usize = 20 * 1024 * 1024;
const VIRTUAL_ENDPOINTS_NAMESPACE: &str = "endpoints";
const PLATFORM_TENANT: &str = "inferx";
const PLATFORM_SHARED_NAMESPACE: &str = "endpoint";
const SKILLS_NAMESPACE: &str = "skills";
const SKILLS_ROOT_DIR: &str = "/opt/inferx/skills";
const SKILL_FILE_NAME: &str = "skill.data";

#[derive(Debug, Deserialize)]
struct SkillCreateRequest {
    template_id: i64,
    prefix: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    serving_mode: Option<String>,
    #[serde(default)]
    has_cache: Option<bool>,
    #[serde(default)]
    gpu_billing_target: Option<String>,
    #[serde(default)]
    earning_type: Option<String>,
    #[serde(default)]
    allowed_child_skilleps: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct SkillTemplateCreateRequest {
    tenant: String,
    namespace: String,
    display_name: String,
    #[serde(default)]
    description: Option<String>,
    func_tenant: String,
    func_namespace: String,
    normal_funcname: String,
    #[serde(default = "default_true")]
    is_active: bool,
}

#[derive(Debug, Deserialize)]
struct SkillTemplateUpdateRequest {
    tenant: String,
    namespace: String,
    display_name: String,
    #[serde(default)]
    description: Option<String>,
    func_tenant: String,
    func_namespace: String,
    normal_funcname: String,
    #[serde(default = "default_true")]
    is_active: bool,
}

#[derive(Debug, Deserialize)]
struct SkillMarketplaceQuery {
    #[serde(default = "default_marketplace_page")]
    page: i64,
    #[serde(default = "default_marketplace_page_size")]
    page_size: i64,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    keyword: Option<String>,
    #[serde(default)]
    include_unpublished: bool,
}

#[derive(Debug, Deserialize)]
struct SkillSubscriptionCreateRequest {
    owner_tenant: String,
    owner_namespace: String,
    skillname: String,
    #[serde(default)]
    tool_alias: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SkillSubscriptionUpdateRequest {
    tool_alias: String,
}

fn default_true() -> bool {
    true
}

fn default_marketplace_page() -> i64 {
    1
}

fn default_marketplace_page_size() -> i64 {
    50
}

fn skill_version_dir_path(
    owner_tenant: &str,
    owner_namespace: &str,
    skillname: &str,
    version: i32,
) -> PathBuf {
    PathBuf::from(SKILLS_ROOT_DIR)
        .join(format!(
            "{}.{}.{}",
            owner_tenant, owner_namespace, skillname
        ))
        .join(version.to_string())
}

fn skill_file_path(
    owner_tenant: &str,
    owner_namespace: &str,
    skillname: &str,
    version: i32,
) -> PathBuf {
    skill_version_dir_path(owner_tenant, owner_namespace, skillname, version).join(SKILL_FILE_NAME)
}

fn skill_root_path(owner_tenant: &str, owner_namespace: &str, skillname: &str) -> PathBuf {
    PathBuf::from(SKILLS_ROOT_DIR).join(format!(
        "{}.{}.{}",
        owner_tenant, owner_namespace, skillname
    ))
}

fn write_skill_prefix(
    owner_tenant: &str,
    owner_namespace: &str,
    skillname: &str,
    version: i32,
    prefix: &str,
) -> Result<()> {
    let dir = skill_version_dir_path(owner_tenant, owner_namespace, skillname, version);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        skill_file_path(owner_tenant, owner_namespace, skillname, version),
        prefix,
    )?;
    Ok(())
}

fn remove_skill_storage(owner_tenant: &str, owner_namespace: &str, skillname: &str) -> Result<()> {
    let root = skill_root_path(owner_tenant, owner_namespace, skillname);
    if root.exists() {
        std::fs::remove_dir_all(root)?;
    }
    Ok(())
}

fn load_skill_prefix(skill: &SkillDetail) -> Result<String> {
    Ok(std::fs::read_to_string(skill_file_path(
        &skill.owner_tenant,
        &skill.owner_namespace,
        &skill.skillname,
        skill.version,
    ))?)
}

fn json_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

fn normalize_skill_request(
    remain_path: &str,
    body_bytes: &[u8],
    prefix: &str,
    model_path: Option<&str>,
) -> SResult<Option<super::tokenizer::NormalizedFuncRequest>, StatusCode> {
    if remain_path.starts_with("/v1/chat/completions") {
        let mut body: Value =
            serde_json::from_slice(body_bytes).map_err(|_| StatusCode::BAD_REQUEST)?;
        let obj = body.as_object_mut().ok_or(StatusCode::BAD_REQUEST)?;
        let messages = obj
            .remove("messages")
            .and_then(|v| v.as_array().cloned())
            .ok_or(StatusCode::BAD_REQUEST)?;

        let mut system_parts = Vec::new();
        if !prefix.trim().is_empty() {
            system_parts.push(prefix.trim().to_string());
        }

        let mut non_system = Vec::new();
        for msg in messages {
            let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
            if role == "system" {
                if let Some(content) = msg.get("content").and_then(json_text) {
                    if !content.trim().is_empty() {
                        system_parts.push(content);
                    }
                }
            } else {
                non_system.push(msg);
            }
        }

        let mut rewritten = Vec::new();
        if !system_parts.is_empty() {
            rewritten.push(serde_json::json!({
                "role": "system",
                "content": system_parts.join("\n\n"),
            }));
        }
        rewritten.extend(non_system);
        obj.insert("messages".to_string(), Value::Array(rewritten));
        if let Some(mp) = model_path {
            obj.insert("model".to_string(), Value::String(mp.to_string()));
        }

        return Ok(Some(super::tokenizer::NormalizedFuncRequest {
            target_path: remain_path.to_string(),
            body_bytes: serde_json::to_vec(&body).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            content_type: "application/json",
        }));
    }

    if remain_path.starts_with("/v1/completions") {
        let mut body: Value =
            serde_json::from_slice(body_bytes).map_err(|_| StatusCode::BAD_REQUEST)?;
        let obj = body.as_object_mut().ok_or(StatusCode::BAD_REQUEST)?;
        let prompt = obj
            .remove("prompt")
            .and_then(|v| json_text(&v))
            .ok_or(StatusCode::BAD_REQUEST)?;
        let combined = if prefix.trim().is_empty() {
            prompt
        } else if prompt.trim().is_empty() {
            prefix.to_string()
        } else {
            format!("{}\n\n{}", prefix, prompt)
        };
        obj.insert("prompt".to_string(), Value::String(combined));
        if let Some(mp) = model_path {
            obj.insert("model".to_string(), Value::String(mp.to_string()));
        }

        return Ok(Some(super::tokenizer::NormalizedFuncRequest {
            target_path: remain_path.to_string(),
            body_bytes: serde_json::to_vec(&body).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            content_type: "application/json",
        }));
    }

    Ok(None)
}

fn resolve_skill_calling_tenant(
    token: &AccessToken,
    tenant_exists: &dyn Fn(&str) -> bool,
) -> Result<String> {
    if let Some(tenant) = token.restrictTenant.as_ref() {
        return Ok(tenant.clone());
    }
    if let Some(dt) = token.defaultTenant.as_ref() {
        if tenant_exists(dt) && token.IsTenantUser(dt) {
            return Ok(dt.clone());
        }
    }
    Err(Error::CommonError(
        "no tenant context: set a default tenant or use a tenant-restricted API key".to_string(),
    ))
}

fn resolve_subscription_tenant(
    token: &AccessToken,
    tenant_exists: &dyn Fn(&str) -> bool,
) -> Result<String> {
    if let Some(tenant) = token.restrictTenant.as_ref() {
        return Ok(tenant.clone());
    }
    if let Some(dt) = token.defaultTenant.as_ref() {
        if tenant_exists(dt) && token.IsTenantUser(dt) {
            return Ok(dt.clone());
        }
    }
    Err(Error::CommonError(
        "no tenant context: set a default tenant or use a tenant-restricted API key".to_string(),
    ))
}

fn normalize_subscription_alias(alias: &str) -> Result<String> {
    let trimmed = alias.trim();
    if trimmed.is_empty()
        || !trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(Error::CommonError(
            "tool_alias may contain only letters, numbers, hyphens, underscores, and periods"
                .to_string(),
        ));
    }

    Ok(trimmed.to_string())
}

fn is_skill_subscription_conflict_error(err: &Error) -> bool {
    match err {
        Error::Exist(_) => true,
        Error::SqlxError(sqlx::Error::Database(db_err)) => {
            db_err.code().as_deref() == Some("23505")
                || db_err.code().as_deref() == Some("23514")
                || db_err
                    .message()
                    .contains("duplicate key value violates unique constraint")
        }
        _ => false,
    }
}

lazy_static::lazy_static! {
    #[derive(Debug)]
    pub static ref GATEWAY_CONFIG: GatewayConfig = GatewayConfig::New(&NODE_CONFIG);
}

pub fn GatewayId() -> i64 {
    return GATEWAY_ID.load(std::sync::atomic::Ordering::Relaxed);
}

fn summarize_headers_for_log(headers: &http::HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| {
            let header_name = name.as_str().to_string();
            let header_value = if header_name.eq_ignore_ascii_case("authorization")
                || header_name.eq_ignore_ascii_case("cookie")
                || header_name.eq_ignore_ascii_case("proxy-authorization")
                || header_name.eq_ignore_ascii_case("x-api-key")
            {
                "[redacted]".to_string()
            } else {
                value.to_str().unwrap_or("<non-utf8>").to_string()
            };
            (header_name, header_value)
        })
        .collect()
}

fn summarize_funccall_body_for_log(bytes: &Bytes, trace_enabled: bool) -> Value {
    if bytes.is_empty() {
        return Value::Null;
    }

    if !trace_enabled {
        return Value::String(format!(
            "[omitted request body; {} bytes; trace logging disabled]",
            bytes.len()
        ));
    }

    match serde_json::from_slice::<Value>(bytes) {
        Ok(value) => redact_inline_media_in_json(&value),
        Err(_) => Value::String(format!("[non-json request body; {} bytes]", bytes.len())),
    }
}

fn redact_inline_media_in_json(value: &Value) -> Value {
    match value {
        Value::Array(items) => {
            Value::Array(items.iter().map(redact_inline_media_in_json).collect())
        }
        Value::Object(map) => {
            let mut redacted = serde_json::Map::with_capacity(map.len());
            for (key, item) in map {
                redacted.insert(key.clone(), redact_inline_media_in_json(item));
            }
            Value::Object(redacted)
        }
        Value::String(text) if text.starts_with("data:") => {
            Value::String(format!("[redacted data URL; {} chars]", text.len()))
        }
        _ => value.clone(),
    }
}

fn funccall_route_error_response(namespace: &str, err: &Error) -> (StatusCode, &'static str) {
    if namespace == VIRTUAL_ENDPOINTS_NAMESPACE {
        return match err {
            Error::NotExist(_) => (StatusCode::NOT_FOUND, "service failure: endpoint not found"),
            Error::CommonError(msg) if msg.contains("is unpublished") => {
                (StatusCode::NOT_FOUND, "service failure: endpoint not found")
            }
            _ => (
                StatusCode::SERVICE_UNAVAILABLE,
                "service failure: endpoint unavailable",
            ),
        };
    }

    match err {
        Error::NotExist(_) => (StatusCode::NOT_FOUND, "service failure: not found"),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "service failure: internal error",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        funccall_route_error_response, is_blocked_public_endpoint_inference,
        resolve_skill_calling_tenant, resolve_subscription_tenant, summarize_funccall_body_for_log,
    };
    use crate::common::Error;
    use crate::gateway::auth_layer::AccessToken;
    use crate::gateway::log_admin::verbose_categories_mask_from_request;
    use axum::http::StatusCode;
    use hyper::body::Bytes;
    use serde_json::Value;
    use std::collections::BTreeSet;
    use std::time::SystemTime;

    fn restricted_apikey(scope: &str, tenant: &str) -> AccessToken {
        AccessToken {
            subject: String::new(),
            username: "user".to_owned(),
            display_name: None,
            email: String::new(),
            email_verified: false,
            roles: BTreeSet::new(),
            apiKeys: Vec::new(),
            scope: scope.to_owned(),
            sourceIsApikey: true,
            restrictTenant: Some(tenant.to_owned()),
            restrictNamespace: None,
            defaultTenant: None,
            updatetime: SystemTime::now(),
        }
    }

    #[test]
    fn summarize_funccall_body_for_log_skips_empty_body() {
        assert_eq!(
            summarize_funccall_body_for_log(&Bytes::new(), false),
            Value::Null
        );
    }

    #[test]
    fn summarize_funccall_body_for_log_omits_non_debug_payloads() {
        let summary = summarize_funccall_body_for_log(&Bytes::from_static(br#"{"a":1}"#), false);
        assert_eq!(
            summary,
            Value::String("[omitted request body; 7 bytes; trace logging disabled]".to_owned())
        );
    }

    #[test]
    fn summarize_funccall_body_for_log_falls_back_for_non_json_debug_payloads() {
        let summary = summarize_funccall_body_for_log(&Bytes::from_static(b"not-json"), true);
        assert_eq!(
            summary,
            Value::String("[non-json request body; 8 bytes]".to_owned())
        );
    }

    #[test]
    fn funccall_route_error_maps_missing_regular_func_to_not_found() {
        let (status, message) =
            funccall_route_error_response("Qwen", &Error::NotExist("missing func".to_owned()));
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(message, "service failure: function not found");
    }

    #[test]
    fn blocks_public_endpoint_inference_namespace() {
        assert!(is_blocked_public_endpoint_inference(
            "public",
            super::VIRTUAL_ENDPOINTS_NAMESPACE
        ));
        assert!(!is_blocked_public_endpoint_inference("public", "models"));
        assert!(!is_blocked_public_endpoint_inference(
            "tenant-a",
            super::VIRTUAL_ENDPOINTS_NAMESPACE
        ));
    }

    #[test]
    fn resolve_skill_calling_tenant_uses_restrict_tenant() {
        let token = restricted_apikey("inference", "tenant-a");
        let tenant = resolve_skill_calling_tenant(&token, &|_| true).unwrap();
        assert_eq!(tenant, "tenant-a");
    }

    #[test]
    fn verbose_category_admin_request_rejects_unknown_categories() {
        let categories = vec![
            "mcp".to_owned(),
            "schedulre".to_owned(),
            "routing".to_owned(),
            "schedulre".to_owned(),
        ];
        let err = verbose_categories_mask_from_request(&categories).unwrap_err();
        assert_eq!(err, vec!["schedulre".to_owned()]);
    }

    #[test]
    fn verbose_category_admin_request_rejects_whitespace_only_categories() {
        let categories = vec!["mcp".to_owned(), "   ".to_owned()];
        let err = verbose_categories_mask_from_request(&categories).unwrap_err();
        assert_eq!(err, vec![String::new()]);
    }

    fn token_with_tenant_role(tenant: &str, default_tenant: Option<&str>) -> AccessToken {
        let role = AccessToken::TenantUserRole(tenant);
        let mut roles = BTreeSet::new();
        roles.insert(role);
        AccessToken {
            subject: String::new(),
            username: "user".to_owned(),
            display_name: None,
            email: String::new(),
            email_verified: false,
            roles,
            apiKeys: Vec::new(),
            scope: "full".to_owned(),
            sourceIsApikey: false,
            restrictTenant: None,
            restrictNamespace: None,
            defaultTenant: default_tenant.map(str::to_owned),
            updatetime: SystemTime::now(),
        }
    }

    fn token_with_default_tenant_only(default_tenant: &str) -> AccessToken {
        AccessToken {
            subject: String::new(),
            username: "user".to_owned(),
            display_name: None,
            email: String::new(),
            email_verified: false,
            roles: BTreeSet::new(),
            apiKeys: Vec::new(),
            scope: "full".to_owned(),
            sourceIsApikey: false,
            restrictTenant: None,
            restrictNamespace: None,
            defaultTenant: Some(default_tenant.to_owned()),
            updatetime: SystemTime::now(),
        }
    }

    #[test]
    fn resolve_skill_calling_tenant_uses_default_tenant_when_authorized() {
        let token = token_with_tenant_role("tenant-a", Some("tenant-a"));
        let tenant = resolve_skill_calling_tenant(&token, &|_| true).unwrap();
        assert_eq!(tenant, "tenant-a");
    }

    #[test]
    fn resolve_skill_calling_tenant_restrict_tenant_takes_priority_over_default_tenant() {
        let mut token = token_with_tenant_role("tenant-a", Some("tenant-b"));
        token.restrictTenant = Some("tenant-a".to_owned());
        let tenant = resolve_skill_calling_tenant(&token, &|_| true).unwrap();
        assert_eq!(tenant, "tenant-a");
    }

    #[test]
    fn resolve_skill_calling_tenant_no_default_tenant_errors() {
        let token = token_with_tenant_role("tenant-a", None);
        let result = resolve_skill_calling_tenant(&token, &|_| true);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_skill_calling_tenant_unauthorized_default_tenant_errors() {
        let token = token_with_default_tenant_only("tenant-x");
        let result = resolve_skill_calling_tenant(&token, &|_| true);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_skill_calling_tenant_deleted_default_tenant_errors() {
        let mut token = token_with_tenant_role("tenant-a", Some("deleted-tenant"));
        token
            .roles
            .insert(AccessToken::TenantUserRole("deleted-tenant"));
        let result = resolve_skill_calling_tenant(&token, &|t| t != "deleted-tenant");
        assert!(result.is_err());
    }

    #[test]
    fn resolve_subscription_tenant_uses_default_tenant_when_authorized() {
        let token = token_with_tenant_role("tenant-a", Some("tenant-a"));
        let tenant = resolve_subscription_tenant(&token, &|_| true).unwrap();
        assert_eq!(tenant, "tenant-a");
    }

    #[test]
    fn resolve_subscription_tenant_restrict_tenant_takes_priority() {
        let mut token = token_with_tenant_role("tenant-a", Some("tenant-b"));
        token.restrictTenant = Some("tenant-a".to_owned());
        let tenant = resolve_subscription_tenant(&token, &|_| true).unwrap();
        assert_eq!(tenant, "tenant-a");
    }

    #[test]
    fn resolve_subscription_tenant_no_default_tenant_errors() {
        let token = token_with_tenant_role("tenant-a", None);
        let result = resolve_subscription_tenant(&token, &|_| true);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_subscription_tenant_unauthorized_default_tenant_errors() {
        let token = token_with_default_tenant_only("tenant-x");
        let result = resolve_subscription_tenant(&token, &|_| true);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_subscription_tenant_deleted_default_tenant_errors() {
        let mut token = token_with_tenant_role("tenant-a", Some("deleted-tenant"));
        token
            .roles
            .insert(AccessToken::TenantUserRole("deleted-tenant"));
        let result = resolve_subscription_tenant(&token, &|t| t != "deleted-tenant");
        assert!(result.is_err());
    }
}

fn tenant_from_path(path: &str) -> Option<&str> {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() < 2 {
        return None;
    }

    match parts[1] {
        "rbac" => match parts.get(2) {
            Some(&"tenantusers") => parts.get(4).copied(),
            Some(&"namespaceusers") => parts.get(4).copied(),
            _ => None,
        },
        "object" => match parts.get(2) {
            Some(&"tenant") => parts.get(5).copied(),
            _ => parts.get(3).copied(),
        },
        "objects" => match parts.get(2) {
            Some(&"tenant") => None,
            _ => parts.get(3).copied(),
        },
        "readypods" => parts.get(2).copied(),
        "directfunccall" => parts.get(2).copied(),
        "funccall" => parts.get(2).copied(),
        "sampleccall" => parts.get(2).copied(),
        "podlog" => parts.get(2).copied(),
        "podauditlog" => parts.get(2).copied(),
        "SnapshotSchedule" => parts.get(2).copied(),
        "faillogs" => parts.get(2).copied(),
        "faillog" => parts.get(2).copied(),
        "getreqs" => parts.get(2).copied(),
        "pods" => parts.get(2).copied(),
        "pod" => parts.get(2).copied(),
        "functions" => parts.get(2).copied(),
        "function" => parts.get(2).copied(),
        "tokenizer" => parts.get(2).copied(),
        "snapshot" => parts.get(2).copied(),
        "snapshots" => parts.get(2).copied(),
        "tenant" => parts.get(2).copied(),
        _ => None,
    }
}

fn tenant_quota_state(gw: &HttpGateway, tenant: &str) -> Result<(bool, bool)> {
    if tenant.is_empty() {
        return Ok((false, false));
    }

    match gw
        .objRepo
        .tenantMgr
        .Get(SYSTEM_TENANT, SYSTEM_NAMESPACE, tenant)
    {
        Ok(t) => Ok((t.object.spec.quota_exempt, t.object.status.quota_exceeded)),
        Err(e) => Err(e.into()),
    }
}

fn quota_exceeded_response(tenant: &str) -> Response<Body> {
    let body = Body::from(format!("service failure: tenant {} quota exceeded", tenant));
    Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .body(body)
        .unwrap()
}

fn quota_lookup_failed_response(tenant: &str) -> Response<Body> {
    let body = Body::from(format!(
        "service failure: tenant {} quota lookup failed",
        tenant
    ));
    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .body(body)
        .unwrap()
}

fn is_blocked_public_endpoint_inference(tenant: &str, namespace: &str) -> bool {
    tenant == "public" && namespace == VIRTUAL_ENDPOINTS_NAMESPACE
}

fn endpoint_policy_for_request(gw: &HttpGateway, tenant: &str, slug: &str) -> FuncPolicySpec {
    gw.objRepo.EndpointRoutePolicy(tenant, slug)
}

fn resolve_funccall_target(
    gw: &HttpGateway,
    tenant: &str,
    namespace: &str,
    funcname: &str,
) -> Result<FuncRouteTarget> {
    if namespace != VIRTUAL_ENDPOINTS_NAMESPACE {
        let func = gw.objRepo.GetFunc(tenant, namespace, funcname)?;
        let version = func.Version();
        let identity = FuncIdentity {
            tenant: tenant.to_owned(),
            namespace: namespace.to_owned(),
            funcname: funcname.to_owned(),
            version,
        };

        return Ok(FuncRouteTarget {
            logical: identity.clone(),
            physical: identity,
            policy: GW_OBJREPO.get().unwrap().FuncPolicy(&func),
            func,
            caller_tenant: None,
        });
    }

    let func = gw
        .objRepo
        .GetFunc(PLATFORM_TENANT, PLATFORM_SHARED_NAMESPACE, funcname)?;
    let funcstatus = gw
        .objRepo
        .funcstatusMgr
        .Get(PLATFORM_TENANT, PLATFORM_SHARED_NAMESPACE, funcname)
        .map_err(|_| {
            Error::NotExist(format!(
                "endpoint funcstatus {}/{}/{} does not exist",
                PLATFORM_TENANT, PLATFORM_SHARED_NAMESPACE, funcname
            ))
        })?;

    if !funcstatus.object.published {
        return Err(Error::CommonError(format!(
            "endpoint {}/{} is unpublished",
            namespace, funcname
        )));
    }

    if funcstatus.object.version != func.Version() {
        return Err(Error::CommonError(format!(
            "endpoint funcstatus version {} does not match function version {} for endpoint {}",
            funcstatus.object.version,
            func.Version(),
            funcname
        )));
    }

    Ok(FuncRouteTarget {
        logical: FuncIdentity {
            tenant: tenant.to_owned(),
            namespace: namespace.to_owned(),
            funcname: funcname.to_owned(),
            version: func.Version(),
        },
        physical: FuncIdentity {
            tenant: PLATFORM_TENANT.to_owned(),
            namespace: PLATFORM_SHARED_NAMESPACE.to_owned(),
            funcname: funcname.to_owned(),
            version: func.Version(),
        },
        policy: endpoint_policy_for_request(gw, tenant, funcname),
        func,
        caller_tenant: None,
    })
}

/// Validate that `slug` names a published endpoint accessible to `tenant`,
/// using the same authoritative function-status check the routing path uses
/// (not `Endpoints` row presence). `slug` is the bare endpoint funcname; a
/// leading `endpoints/` is tolerated. Returns the backend error string on
/// failure (e.g. `"endpoint ... is unpublished"`).
pub fn validate_agent_endpoint_published(
    gw: &HttpGateway,
    tenant: &str,
    slug: &str,
) -> std::result::Result<(), String> {
    let slug = slug.trim().trim_start_matches('/');
    let slug = slug.strip_prefix("endpoints/").unwrap_or(slug);
    if slug.is_empty() {
        return Err("empty endpoint slug".to_string());
    }
    resolve_funccall_target(gw, tenant, VIRTUAL_ENDPOINTS_NAMESPACE, slug)
        .map(|_| ())
        .map_err(|e| format!("{:?}", e))
}

fn enforce_tenant_quota_for_write(
    token: &Arc<AccessToken>,
    gw: &HttpGateway,
    tenant: &str,
) -> Option<Response<Body>> {
    if token.IsInferxAdmin() {
        return None;
    }

    match tenant_quota_state(gw, tenant) {
        Ok((true, _)) => return None,
        Ok((false, true)) => return Some(quota_exceeded_response(tenant)),
        Ok((false, false)) => {}
        Err(e) => {
            error!("tenant quota lookup failed for {}: {:?}", tenant, e);
            return Some(quota_lookup_failed_response(tenant));
        }
    };

    None
}

fn is_funccall_path(path: &str) -> bool {
    path.starts_with("/funccall/")
        || path.starts_with("/directfunccall/")
        || path.starts_with("/sampleccall/")
}

fn enforce_tenant_quota_for_request(
    token: &Arc<AccessToken>,
    gw: &HttpGateway,
    tenant: &str,
    method: &axum::http::Method,
    path: &str,
) -> Option<Response<Body>> {
    if token.IsInferxAdmin() {
        return None;
    }

    match tenant_quota_state(gw, tenant) {
        Ok((true, _)) => return None,
        Ok((false, true)) => {
            if is_funccall_path(path) {
                return Some(quota_exceeded_response(tenant));
            }

            if *method == axum::http::Method::GET
                || *method == axum::http::Method::HEAD
                || *method == axum::http::Method::OPTIONS
            {
                return None;
            }

            return Some(quota_exceeded_response(tenant));
        }
        Ok((false, false)) => {}
        Err(e) => {
            error!("tenant quota lookup failed for {}: {:?}", tenant, e);
            return Some(quota_lookup_failed_response(tenant));
        }
    };

    None
}

async fn TenantQuotaGuard(
    State(gw): State<HttpGateway>,
    req: Request,
    next: Next,
) -> SResult<Response, StatusCode> {
    if !GATEWAY_CONFIG.enforceBilling {
        return Ok(next.run(req).await);
    }

    let tenant = match tenant_from_path(req.uri().path()) {
        Some(t) if !t.is_empty() => t,
        _ => {
            ctrace!(
                verbose_category::ROUTING,
                "TenantQuotaGuard skip: no tenant in path {}",
                req.uri().path()
            );
            return Ok(next.run(req).await);
        }
    };

    let token = match req.extensions().get::<Arc<AccessToken>>() {
        Some(t) => t,
        None => {
            ctrace!(
                verbose_category::ROUTING,
                "TenantQuotaGuard skip: no token for tenant {} path {}",
                tenant,
                req.uri().path()
            );
            return Ok(next.run(req).await);
        }
    };

    if token.IsInferxAdmin() {
        ctrace!(
            verbose_category::ROUTING,
            "TenantQuotaGuard skip: inferx admin tenant {} path {}",
            tenant,
            req.uri().path()
        );
        return Ok(next.run(req).await);
    }

    if let Some(resp) =
        enforce_tenant_quota_for_request(token, &gw, tenant, req.method(), req.uri().path())
    {
        ctrace!(
            verbose_category::ROUTING,
            "TenantQuotaGuard block: tenant {} path {}",
            tenant,
            req.uri().path()
        );
        return Ok(resp);
    }

    ctrace!(
        verbose_category::ROUTING,
        "TenantQuotaGuard allow: tenant {} path {}",
        tenant,
        req.uri().path()
    );
    Ok(next.run(req).await)
}

#[derive(Debug, Clone)]
pub struct HttpGateway {
    pub objRepo: GwObjRepo,
    pub funcAgentMgr: FuncAgentMgr,
    pub namespaceStore: NamespaceStore,
    pub sqlAudit: SqlAudit,
    pub sqlBilling: SqlAudit,
    pub sqlSecret: SqlSecret,
    pub client: CacherClient,
    pub sessions: SessionStore,
}

impl HttpGateway {
    pub async fn HttpServe(&self) -> Result<()> {
        let gatewayId = UID
            .get()
            .unwrap()
            .Get()
            .await
            .expect("HttpGateway: fail to get gateway id");
        GATEWAY_ID.store(gatewayId, std::sync::atomic::Ordering::SeqCst);

        let cors = CorsLayer::new()
            .allow_origin(Any) // Allow requests from any origin
            .allow_methods(Any)
            .allow_headers(Any)
            .expose_headers(Any);
        let _ = rustls::crypto::ring::default_provider().install_default();

        let auth_layer = GATEWAY_CONFIG.keycloakconfig.AuthLayer();

        //////////////////////////////////////////////////////////////////////////////////////
        // have to put the mpc code in this area for compiliation issue
        let config = StreamableHttpServerConfig::default()
            .disable_allowed_hosts()
            .with_sse_keep_alive(Some(std::time::Duration::from_secs(30)));

        let session_manager = LocalSessionManager::default();
        let mcp_secret = Arc::new(self.sqlSecret.clone());
        let mcp_gateway_base_url = format!("http://127.0.0.1:{}", GATEWAY_CONFIG.gatewayPort);
        let mpcservice = StreamableHttpService::new(
            move || {
                Ok(McpStreamServer::new(
                    mcp_secret.clone(),
                    mcp_gateway_base_url.clone(),
                ))
            },
            Arc::new(session_manager),
            config,
        );

        //////////////////////////////////////////////////////////////

        let app = Router::new()
            .nest_service("/mcp", mpcservice)
            .route("/sessions", post(super::session::CreateSession))
            .route("/sessions/current", get(super::session::GetCurrentSession))
            .route(
                "/sessions/:id",
                get(super::session::GetSession).delete(super::session::DeleteSession),
            )
            .route(
                "/sessions/:id/messages",
                get(super::session::GetSessionMessages),
            )
            .route("/sessions/:id/prompt", post(super::session::Prompt))
            .route(
                "/sessions/:id/prompt_stream",
                post(super::session::PromptStream),
            )
            .route(
                "/sessions/:id/interrupt",
                post(super::session::InterruptSession),
            )
            .route("/rbac/", post(RbacGrant))
            .route("/rbac/", delete(RbacRevoke))
            .route("/rbac/roles/", get(RbacRoleBindingGet))
            .route("/rbac/tenantusers/:role/:tenant/", get(RbacTenantUsers))
            .route(
                "/rbac/namespaceusers/:role/:tenant/:namespace/",
                get(RbacNamespaceUsers),
            )
            .route("/apikey/", get(GetApikeys))
            .route("/apikey/", put(CreateApikey))
            .route("/apikey/", delete(DeleteApikey))
            .route("/onboard", post(Onboard))
            .route("/admin/tenants", get(GetAdminTenants))
            .route("/admin/endpoints/:slug/metadata", put(SaveEndpointMetadata))
            .route("/admin/endpoints/:slug/publish", post(PublishEndpoint))
            .route("/admin/endpoints/:slug/unpublish", post(UnpublishEndpoint))
            .route(
                "/admin/endpoints/:slug/openrouter",
                put(SaveEndpointOpenRouterMetadata),
            )
            .route(
                "/admin/endpoints/:slug/openrouter/suggest",
                post(SuggestEndpointOpenRouterSlug),
            )
            .route(
                "/admin/endpoints/:slug/openrouter/list",
                post(ListOnOpenRouter),
            )
            .route(
                "/admin/endpoints/:slug/openrouter/offline",
                post(TakeOfflineOnOpenRouter),
            )
            .route(
                "/admin/endpoints/:slug/openrouter/unlist",
                post(UnlistFromOpenRouter),
            )
            .route("/v1/models", get(OpenRouterModels))
            .route("/v1/chat/completions", post(OpenRouterChatCompletions))
            .route("/v1/completions", post(OpenRouterChatCompletions))
            .route("/object/", put(CreateObj))
            .route("/object/:type/:tenant/:namespace/:name/", delete(DeleteObj))
            .route(
                "/readypods/:tenant/:namespace/:funcname/",
                get(ListReadyPods),
            )
            .route("/directfunccall/*rest", post(DirectFuncCall))
            .route("/directfunccall/*rest", get(DirectFuncCall))
            .route("/directfunccall/*rest", head(DirectFuncCall))
            .route("/models/*rest", post(ModelsFuncCall))
            .route("/models/*rest", get(ModelsFuncCall))
            .route("/models/*rest", head(ModelsFuncCall))
            .route(
                "/kb/token-count/:tenant/:namespace/:name",
                post(CountKnowledgeBaseTokens),
            )
            .route("/funccall/*rest", post(FuncCall))
            .route("/funccall/*rest", get(FuncCall))
            .route("/funccall/*rest", head(FuncCall))
            .route("/skilltemplates", get(ListSkillTemplates))
            .route("/skills", get(ListPublishedSkills))
            .route("/skills/:owner_tenant", get(ListSkillsByTenant))
            .route(
                "/skills/:owner_tenant/:namespace",
                get(ListSkillsByNamespace),
            )
            .route("/api/v1/skills/marketplace", get(ListSkillMarketplace))
            .route(
                "/api/v1/skills/subscriptions",
                get(ListSkillSubscriptions).post(CreateSkillSubscription),
            )
            .route(
                "/api/v1/skills/subscriptions/:owner_tenant/:owner_namespace/:skillname",
                delete(DeleteSkillSubscription).patch(UpdateSkillSubscription),
            )
            .route(
                "/admin/skilltemplates",
                get(ListAdminSkillTemplates).post(CreateSkillTemplate),
            )
            .route(
                "/admin/skilltemplates/:template_id",
                put(UpdateSkillTemplate).delete(DeleteSkillTemplate),
            )
            .route(
                "/admin/skilltemplates/:template_id/activate",
                post(ActivateSkillTemplate),
            )
            .route(
                "/admin/skilltemplates/:template_id/deactivate",
                post(DeactivateSkillTemplate),
            )
            .route(
                "/skills/:owner_tenant/:namespace/:skillname",
                post(CreateSkill).get(GetSkill).delete(DeleteSkill),
            )
            .route(
                "/skills/:owner_tenant/:namespace/:skillname/publish",
                post(PublishSkill),
            )
            .route(
                "/skills/:owner_tenant/:namespace/:skillname/unpublish",
                post(UnpublishSkill),
            )
            .route(
                "/skills/:owner_tenant/:namespace/:skillname/*subpath",
                post(SkillCall).get(SkillCall).head(SkillCall),
            )
            .route("/tokenizer/*rest", post(TokenizerRoute))
            .route("/tokenizer/*rest", get(TokenizerRoute))
            .route("/tokenizer/*rest", head(TokenizerRoute))
            // Experimental KB route family. Disabled for now to avoid
            // overlapping with `/kb/token-count/...` in the router.
            // .route("/kb/*rest", post(KnowledgeBaseRoute))
            // .route("/kb/*rest", get(KnowledgeBaseRoute))
            // .route("/kb/*rest", head(KnowledgeBaseRoute))
            .route("/prompt/", post(PostPrompt))
            .route("/debug/func_agents", get(GetFuncAgentsState))
            .route(
                "/sampleccall/:tenant/:namespace/:name/",
                get(GetSampleRestCall),
            )
            .route(
                "/podlog/:tenant/:namespace/:name/:revision/:id/",
                get(ReadLog),
            )
            .route(
                "/podauditlog/:tenant/:namespace/:name/:revision/:id/",
                get(ReadPodAuditLog),
            )
            .route(
                "/SnapshotSchedule/:tenant/:namespace/:name/:revision/",
                get(ReadSnapshotScheduleRecords),
            )
            .route(
                "/faillogs/:tenant/:namespace/:name/:revision",
                get(ReadPodFaillogs),
            )
            .route(
                "/faillog/:tenant/:namespace/:name/:revision/:id",
                get(ReadPodFaillog),
            )
            .route("/getreqs/:tenant/:namespace/:name/", get(GetReqs))
            .route("/", get(root))
            .route("/object/", post(UpdateObj))
            .route("/object/:type/:tenant/:namespace/:name/", get(GetObj))
            .route("/objects/:type/:tenant/:namespace/", get(ListObj))
            .route("/nodes/", get(GetNodes))
            .route("/node/:nodename/", get(GetNode))
            .route("/pods/:tenant/:namespace/:funcname/", get(GetFuncPods))
            .route(
                "/pod/:tenant/:namespace/:funcname/:version/:id/",
                get(GetFuncPod).delete(KillPod),
            )
            .route("/functions/:tenant/:namespace/", get(ListFuncBrief))
            .route(
                "/function/:tenant/:namespace/:funcname/",
                get(GetFuncDetail),
            )
            .route(
                "/published-endpoint/:tenant/:slug/",
                get(GetPublishedEndpointDetail),
            )
            .route(
                "/snapshot/:tenant/:namespace/:snapshotname/",
                get(GetSnapshot),
            )
            .route("/snapshots/:tenant/:namespace/", get(GetSnapshots))
            .route("/tenant/:tenant/credits", post(AddTenantCredits))
            .route(
                "/tenant/:tenant/quota-exceeded",
                post(SetTenantQuotaExceeded),
            )
            .route("/billing/rates", post(AddBillingRate))
            .route("/billing/rates", get(GetBillingRateHistory))
            .route("/billing/credits/history", get(GetBillingCreditHistory))
            .route("/tenant/:tenant/credits", get(GetTenantCredits))
            .route(
                "/tenant/:tenant/credits/history",
                get(GetTenantCreditHistory),
            )
            .route(
                "/tenant/:tenant/billing-summary",
                get(GetTenantBillingSummary),
            )
            .route("/tenant/:tenant/usage/hourly", get(GetTenantHourlyUsage))
            .route(
                "/tenant/:tenant/usage/hourly-by-model",
                get(GetTenantHourlyUsageByModel),
            )
            .route(
                "/tenant/:tenant/usage/hourly-by-namespace",
                get(GetTenantHourlyUsageByNamespace),
            )
            .route("/tenant/:tenant/usage/by-model", get(GetTenantUsageByModel))
            .route(
                "/tenant/:tenant/usage/by-namespace",
                get(GetTenantUsageByNamespace),
            )
            .route("/tenant/:tenant/usage/summary", get(GetTenantUsageSummary))
            .route("/admin/usage/endpoints", get(GetAdminEndpointUsage))
            .route(
                "/admin/usage/endpoints/:tenant/:endpoint_slug",
                get(GetAdminEndpointTenantUsageByPeriod),
            )
            .route(
                "/admin/log/verbose-categories",
                get(GetVerboseCategories).put(PutVerboseCategories),
            )
            .route(
                "/admin/log/verbose-categories/:category/enable",
                post(EnableVerboseCategory),
            )
            .route(
                "/admin/log/verbose-categories/:category/disable",
                post(DisableVerboseCategory),
            )
            .route("/metrics", get(GetMetrics))
            .route("/debug/trace_logging/:state", post(SetTraceLogging))
            .with_state(self.clone())
            .layer(cors)
            .layer(from_fn_with_state(self.clone(), TenantQuotaGuard))
            .layer(axum::middleware::from_fn(auth_transform_keycloaktoken))
            .layer(auth_layer);

        let gw_clone = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(
                super::session::SessionCleanupIntervalSecs,
            ));
            loop {
                interval.tick().await;
                let cleaned = gw_clone.sessions.CleanupTimedOutSessions().await;
                if cleaned > 0 {
                    log::info!("Cleaned up {} timed-out sessions", cleaned);
                }
            }
        });

        let tlsconfig = NODE_CONFIG.tlsconfig.clone();

        println!("tls config is {:#?}", &tlsconfig);
        if tlsconfig.enable {
            // configure certificate and private key used by https
            let config = RustlsConfig::from_pem_file(
                PathBuf::from(tlsconfig.certpath),
                PathBuf::from(tlsconfig.keypath),
            )
            .await
            .unwrap();

            let addr = SocketAddr::from(([0, 0, 0, 0], GATEWAY_CONFIG.gatewayPort));
            println!("listening on tls {}", &addr);
            axum_server::bind_rustls(addr, config)
                .serve(app.into_make_service())
                .await
                .unwrap();
        } else {
            let gatewayUrl = format!("0.0.0.0:{}", GATEWAY_CONFIG.gatewayPort);
            let listener = tokio::net::TcpListener::bind(gatewayUrl).await.unwrap();
            println!("listening on {}", listener.local_addr().unwrap());
            axum::serve(listener, app).await.unwrap();
        }

        return Ok(());
    }
}

async fn root() -> &'static str {
    "InferX Gateway!"
}

async fn SetTraceLogging(
    Extension(_token): Extension<Arc<AccessToken>>,
    Path(state): Path<String>,
) -> SResult<Response, StatusCode> {
    let lower = state.to_ascii_lowercase();
    let enable = match lower.as_str() {
        "on" | "enable" | "enabled" | "true" | "1" => Some(true),
        "off" | "disable" | "disabled" | "false" | "0" => Some(false),
        _ => None,
    };

    let enable = match enable {
        Some(v) => v,
        None => {
            let body = Body::from(format!("invalid state '{}', use on/off", state));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    };

    set_trace_logging(enable);
    error!("TRACE_GATEWAY_LOG: {}", trace_logging_enabled());

    let body = Body::from(if enable {
        "trace logging enabled"
    } else {
        "trace logging disabled"
    });

    let resp = Response::builder()
        .status(StatusCode::OK)
        .body(body)
        .unwrap();
    return Ok(resp);
}

async fn GetMetrics() -> SResult<Response, StatusCode> {
    let mut buffer = String::new();
    let registery = METRICS_REGISTRY.lock().await;
    encode(&mut buffer, &*registery).unwrap();
    return Ok(Response::builder()
        .status(StatusCode::OK)
        .header(
            CONTENT_TYPE,
            "application/openmetrics-text; version=1.0.0; charset=utf-8",
        )
        .body(Body::from(buffer))
        .unwrap());
}

async fn GetFuncAgentsState(
    Extension(_token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
) -> SResult<Response, StatusCode> {
    let data = gw.funcAgentMgr.DebugInfo().await;
    let resp = Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(data.to_string()))
        .unwrap();
    Ok(resp)
}

async fn GetReqs(
    Path((_tenant, _namespace, _name)): Path<(String, String, String)>,
) -> SResult<Response, StatusCode> {
    let mut client = ReqWatchingServiceClient::connect("http://127.0.0.1:1237")
        .await
        .unwrap();

    let req = ReqWatchRequest::default();
    let response = client.watch(tonic::Request::new(req)).await.unwrap();
    let mut ws = response.into_inner();

    let (tx, rx) = mpsc::channel::<SResult<String, Infallible>>(128);
    tokio::spawn(async move {
        loop {
            let event = ws.message().await;
            let req = match event {
                Err(_e) => {
                    return;
                }
                Ok(b) => match b {
                    Some(e) => e,
                    None => {
                        return;
                    }
                },
            };

            match tx.send(Ok(req.value.clone())).await {
                Err(_) => {
                    // error!("PostCall sendbytes fail with channel unexpected closed");
                    return;
                }
                Ok(()) => (),
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let body: http_body_util::StreamBody<
        tokio_stream::wrappers::ReceiverStream<SResult<String, Infallible>>,
    > = http_body_util::StreamBody::new(stream);

    let body = axum::body::Body::from_stream(body);

    return Ok(Response::new(body));
}

async fn GetSampleRestCall(
    State(gw): State<HttpGateway>,
    Path((tenant, namespace, funcname)): Path<(String, String, String)>,
) -> SResult<String, StatusCode> {
    if is_blocked_public_endpoint_inference(&tenant, &namespace) {
        return Ok("service failure: unsupported".to_owned());
    }

    let func = match gw.objRepo.GetFunc(&tenant, &namespace, &funcname) {
        Err(e) => {
            return Ok(format!("service failure {:?}", e));
        }
        Ok(f) => f,
    };

    let sampleRestCall = func.SampleRestCall();

    return Ok(sampleRestCall);
}

// test func, remove later
async fn PostPrompt(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Json(req): Json<PromptReq>,
) -> SResult<Response, StatusCode> {
    error!("PostPrompt req is {:?}", &req);
    if GATEWAY_CONFIG.enforceBilling {
        if let Some(resp) = enforce_tenant_quota_for_write(&token, &gw, &req.tenant) {
            return Ok(resp);
        }
    }
    let client = reqwest::Client::new();

    let tenant = req.tenant.clone();
    let namespace = req.namespace.clone();
    let funcname = req.funcname.clone();

    let func = match gw.objRepo.GetFunc(&tenant, &namespace, &funcname) {
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .body(body)
                .unwrap();

            return Ok(resp);
        }
        Ok(f) => f,
    };

    let samplecall = &func.object.spec.sampleCall;
    let mut map = samplecall.body.clone();

    if let Some(obj) = map.as_object_mut() {
        obj.insert("prompt".to_owned(), Value::String(req.prompt.clone()));

        if samplecall.apiType == ApiType::Image2Text {
            let image = req.image.clone();
            obj.insert("image".to_owned(), Value::String(image));
        }
    } else {
        map = serde_json::json!({
            "prompt": req.prompt.clone()
        });
    }

    // 4. OpenAI logic remains the same
    let isOpenAi = match samplecall.apiType {
        ApiType::Text2Text => true,
        _ => false,
    };

    let url = format!(
        "http://localhost:4000/funccall/{}/{}/{}/{}",
        &req.tenant, &req.namespace, &req.funcname, &samplecall.path
    );

    let mut resp = match client.post(url).json(&map).send().await {
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .body(body)
                .unwrap();

            return Ok(resp);
        }
        Ok(resp) => resp,
    };

    let mut kvs = Vec::new();
    for (k, v) in resp.headers() {
        let key = k.to_string();
        if let Ok(val) = v.to_str() {
            kvs.push((key, val.to_owned()));
        }
    }

    if resp.status().as_u16() != StatusCode::OK.as_u16() {
        let body = axum::body::Body::from(resp.text().await.unwrap());

        let mut resp = Response::new(body);
        *resp.status_mut() = resp.status();

        return Ok(resp);
    }

    let (tx, rx) = mpsc::channel::<SResult<Bytes, Infallible>>(128);
    tokio::spawn(async move {
        loop {
            let chunk = resp.chunk().await;
            let bytes = match chunk {
                Err(e) => {
                    error!("PostPrompt 1 get error {:?}", e);
                    return;
                }
                Ok(b) => match b {
                    Some(b) => b,
                    None => return,
                },
            };

            if isOpenAi {
                let str = match str::from_utf8(bytes.as_ref()) {
                    Err(e) => {
                        error!("PostPrompt 2 get error {:?}", e);
                        return;
                    }
                    Ok(s) => s,
                };

                let lines = str.split("data:");
                let mut parselines = Vec::new();

                for l1 in lines {
                    if l1.len() == 0 || l1.contains("[DONE]") {
                        continue;
                    }

                    let v: serde_json::Value = match serde_json::from_str(l1) {
                        Err(e) => {
                            error!("PostPrompt 3 get error {:?} line is {:?}", e, l1);
                            return;
                        }
                        Ok(v) => v,
                    };

                    parselines.push(v);
                }

                for l in &parselines {
                    let delta = &l["choices"][0];
                    let content = match delta["text"].as_str() {
                        None => {
                            format!("PostPrompt fail with lines {:#?}", &parselines)
                        }
                        Some(c) => c.to_owned(),
                    };
                    let bytes = Bytes::from(content.as_bytes().to_vec());
                    match tx.send(Ok(bytes)).await {
                        Err(_) => {
                            return;
                        }
                        Ok(()) => (),
                    }
                }
            } else {
                match tx.send(Ok(bytes)).await {
                    Err(_) => {
                        return;
                    }
                    Ok(()) => (),
                }
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let body = http_body_util::StreamBody::new(stream);

    let body = axum::body::Body::from_stream(body);

    let mut response = Response::new(body);
    for (key, value) in kvs {
        if let (Ok(header_name), Ok(header_value)) = (
            hyper::header::HeaderName::from_bytes(key.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            response.headers_mut().insert(header_name, header_value);
        }
    }
    return Ok(response);
}

async fn ListReadyPods(
    State(gw): State<HttpGateway>,
    Path((tenant, namespace, funcname)): Path<(String, String, String)>,
) -> SResult<Response, StatusCode> {
    error!("ListReadyPods 1 {}/{}/{}", &tenant, &namespace, &funcname);
    match gw.objRepo.ListReadyPods(&tenant, &namespace, &funcname) {
        Ok(pods) => {
            let data = serde_json::to_string(&pods).unwrap();
            let body = Body::from(format!("{}", data));
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn DirectFuncCallProc(gw: &HttpGateway, mut req: Request) -> Result<Response> {
    let path = req.uri().path();
    let parts = path.split("/").collect::<Vec<&str>>();

    let partsCount = parts.len();
    let tenant = parts[2].to_owned();
    let namespace = parts[3].to_owned();
    let funcname = parts[4].to_owned();
    let version = parts[5].to_owned();
    let id = parts[6].to_owned();

    let podname = format!(
        "{}/{}/{}/{}/{}",
        &tenant, &namespace, &funcname, &version, &id
    );

    error!("DirectFuncCallProc 2 {}", &podname);
    let pod = gw.objRepo.GetFuncPod(&tenant, &namespace, &podname)?;

    error!("DirectFuncCallProc 2.0 {}", &pod.object.spec.host_ip);
    let hostip = IpAddress::FromString(&pod.object.spec.host_ip)?;
    let hostport = pod.object.spec.host_port;
    let dstPort = pod.object.spec.funcspec.endpoint.port;
    let dstIp = pod.object.spec.ipAddr;

    let tcpclient = IxTcpClient {
        hostIp: hostip.0,
        hostPort: hostport,
        tenant: pod.tenant.clone(),
        namespace: pod.namespace.clone(),
        dstIp: dstIp,
        dstPort: dstPort,
        srcIp: 0x01020305,
        srcPort: 123,
    };

    error!("DirectFuncCallProc 2.1 {:?}", &tcpclient);

    let stream = tcpclient.Connect().await?;

    let mut remainPath = "".to_string();
    for i in 7..partsCount {
        remainPath = remainPath + "/" + parts[i];
    }

    error!("DirectFuncCallProc 3 {}", &remainPath);
    let uri = format!("http://127.0.0.1{}", remainPath); // &func.object.spec.endpoint.path);
    *req.uri_mut() = Uri::try_from(uri).unwrap();

    let mut client = QHttpCallClientDirect::New(stream).await?;

    let mut res = client.Send(req).await?;

    let mut kvs = Vec::new();
    for (k, v) in res.headers() {
        kvs.push((k.clone(), v.clone()));
    }

    error!("DirectFuncCallProc 4 {}", &remainPath);
    let (tx, rx) = mpsc::channel::<SResult<Bytes, Infallible>>(128);
    tokio::spawn(async move {
        defer!(drop(client));
        loop {
            let frame = res.frame().await;
            let bytes = match frame {
                None => {
                    return;
                }
                Some(b) => match b {
                    Ok(b) => b,
                    Err(e) => {
                        error!(
                            "PostCall for path {}/{}/{} get error {:?}",
                            tenant, namespace, funcname, e
                        );
                        return;
                    }
                },
            };
            let bytes: Bytes = bytes.into_data().unwrap();

            match tx.send(Ok(bytes)).await {
                Err(_) => {
                    // error!("PostCall sendbytes fail with channel unexpected closed");
                    return;
                }
                Ok(()) => (),
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let body = http_body_util::StreamBody::new(stream);

    let body = axum::body::Body::from_stream(body);

    let mut resp = Response::new(body);

    for (k, v) in kvs {
        resp.headers_mut().insert(k, v);
    }

    return Ok(resp);
}

async fn DirectFuncCall(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    req: Request,
) -> SResult<Response, StatusCode> {
    let path = req.uri().path();
    let parts = path.split("/").collect::<Vec<&str>>();

    let partsCount = parts.len();
    if partsCount < 7 {
        let body = Body::from(format!("service failure: Invalid input"));
        let resp = Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(body)
            .unwrap();

        return Ok(resp);
    }
    let tenant = parts[2].to_owned();
    let namespace = parts[3].to_owned();

    if is_blocked_public_endpoint_inference(&tenant, &namespace) {
        let body = Body::from("service failure: unsupported");
        let resp = Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    if !token.CheckScope("inference") {
        let body = Body::from(format!("service failure: insufficient scope"));
        let resp = Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    if !token.IsNamespaceInferenceUser(&tenant, &namespace) {
        let body = Body::from(format!("service failure: No permission"));
        let resp = Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(body)
            .unwrap();

        return Ok(resp);
    }
    match DirectFuncCallProc(&gw, req).await {
        Ok(resp) => return Ok(resp),
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn RetryGetClient(
    gw: &HttpGateway,
    route: &FuncRouteTarget,
    timeout: u64,
    timestamp: IxTimestamp,
) -> Result<(QHttpCallClient, bool)> {
    let mut _retry = 0;
    loop {
        match gw.funcAgentMgr.GetClient(route, timeout, timestamp).await {
            Err(e) => {
                _retry += 1;
                if timestamp.Elapsed() < timeout {
                    // trace!(
                    //     "RetryGetClient retry {} {}/{}/{} timeout {}",
                    //     retry,
                    //     route.logical.tenant,
                    //     route.logical.namespace,
                    //     route.logical.funcname,
                    //     timestamp.Elapsed()
                    // );
                    continue;
                }
                error!("RetryGetClient, e: {:?}", e);
                return Err(e);
            }
            Ok(client) => {
                if _retry > 0 {
                    // trace!("RetryGetClient retry success {} ", retry);
                }

                return Ok(client);
            }
        };
    }
}

async fn FailureResponse(e: Error, labels: &mut FunccallLabels, _status: Status) -> Response<Body> {
    // error!("Http call fail with error {:?}", &e);
    let errcode: StatusCode = match &e {
        Error::Timeout(_timeout) => {
            error!("Http start fail with timeout {:?}", _timeout);
            StatusCode::GATEWAY_TIMEOUT
        }
        Error::QueueFull => {
            error!("Http start fail with QueueFull");
            StatusCode::SERVICE_UNAVAILABLE
        }
        Error::ServiceUnavailable => {
            error!("Http start fail with ServiceUnavailable");
            StatusCode::SERVICE_UNAVAILABLE
        }
        Error::BAD_REQUEST(code) => {
            error!("Http start fail with bad request");
            *code
        }
        e => {
            error!("Http start fail with error {:?}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };

    labels.status = errcode.as_u16();
    GATEWAY_METRICS
        .lock()
        .await
        .funccallcnt
        .get_or_create(labels)
        .inc();

    let body = Body::from(format!("service failure {:?}", &e));
    let resp = Response::builder().status(errcode).body(body).unwrap();

    return resp;
}

pub struct Disconnect {
    pub start: std::time::Instant,
    pub cancel: AtomicBool,
    pub req: serde_json::Value,
    pub headers: Vec<(String, String)>,
    pub labels: FunccallLabels,
}

impl Disconnect {
    pub fn New(
        req: serde_json::Value,
        headers: Vec<(String, String)>,
        labels: &FunccallLabels,
    ) -> Self {
        return Self {
            start: std::time::Instant::now(),
            cancel: AtomicBool::new(false),
            req: req,
            headers: headers,
            labels: labels.clone(),
        };
    }

    pub fn Cancel(&self) {
        self.cancel
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

impl Drop for Disconnect {
    fn drop(&mut self) {
        if !self.cancel.load(std::sync::atomic::Ordering::Acquire) {
            let mut labels = self.labels.clone();
            tokio::spawn(async move {
                labels.status = 499; // Client close req
                GATEWAY_METRICS
                    .lock()
                    .await
                    .funccallcnt
                    .get_or_create(&labels)
                    .inc();
            });

            error!(
                "Funccall ********* Fail: Client disconnect before vllm return header {} ms, req {:#?}, headers {:#?}",
                self.start.elapsed().as_millis(),
                &self.req,
                &self.headers
            );
        }
    }
}

async fn FuncCall(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    req: Request,
) -> SResult<Response, StatusCode> {
    return FuncCall1(&token, &gw, req).await;
}

pub async fn FuncCall1(
    token: &Arc<AccessToken>,
    gw: &HttpGateway,
    req: Request,
) -> SResult<Response, StatusCode> {
    let path = req.uri().path();
    let parts = path.split("/").collect::<Vec<&str>>();
    let partsCount = parts.len();
    if partsCount < 5 {
        let body = Body::from(format!("service failure: Invalid input"));
        let resp = Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(body)
            .unwrap();

        return Ok(resp);
    }

    if partsCount > 5 {
        let p5 = parts[5].to_owned();
        if p5.starts_with("sleep") || p5.starts_with("wake_up") {
            let body = Body::from(format!("service failure: unsupported"));
            let resp = Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(body)
                .unwrap();

            return Ok(resp);
        }
    }

    let tenant = parts[2].to_owned();
    let namespace = parts[3].to_owned();
    let funcname = parts[4].to_owned();
    if is_blocked_public_endpoint_inference(&tenant, &namespace) {
        let body = Body::from("service failure: unsupported");
        let resp = Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    if !token.CheckScope("inference") {
        let body = Body::from(format!("service failure: insufficient scope"));
        let resp = Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    if !token.IsNamespaceInferenceUser(&tenant, &namespace) {
        let body = Body::from(format!("service failure: No permission"));
        let resp = Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(body)
            .unwrap();

        return Ok(resp);
    }

    let mut remainPath = "".to_string();
    for i in 5..partsCount {
        remainPath = remainPath + "/" + parts[i];
    }

    let route = match resolve_funccall_target(&gw, &tenant, &namespace, &funcname) {
        Ok(route) => route,
        Err(e) => {
            let (errcode, message) = funccall_route_error_response(&namespace, &e);
            ctrace!(
                verbose_category::ROUTING,
                "FuncCall route resolution failed for {tenant}/{namespace}/{funcname}: {e:?}"
            );
            let body = Body::from(message);
            let resp = Response::builder().status(errcode).body(body).unwrap();
            return Ok(resp);
        }
    };
    dispatch_func_call(
        gw, req, route, tenant, namespace, funcname, remainPath, None,
    )
    .await
}

pub const TCPCONN_LATENCY_HEADER: &'static str = "X-TcpConn-Latency";
pub const TTFT_LATENCY_HEADER: &'static str = "X-Ttft-Latency";

pub(super) async fn dispatch_func_call(
    gw: &HttpGateway,
    mut req: Request,
    route: FuncRouteTarget,
    tenant: String,
    namespace: String,
    funcname: String,
    remainPath: String,
    skill_prefix: Option<&str>,
) -> SResult<Response, StatusCode> {
    let reqStart = std::time::Instant::now();
    let headers = req.headers().clone();
    let path = req.uri().path();

    let tracer = opentelemetry::global::tracer("gateway");
    let mut ttftSpan = tracer.start("TTFT");
    ttftSpan.set_attribute(KeyValue::new("req", path.to_owned()));
    let ttftCtx = Context::current_with_span(ttftSpan);
    let now = std::time::Instant::now();

    let mut labels = FunccallLabels {
        tenant: tenant.clone(),
        namespace: namespace.clone(),
        funcname: funcname.clone(),
        status: StatusCode::OK.as_u16(),
    };

    let policy = route.policy.clone();
    let timestamp = IxTimestamp::default();
    let timeout_header = req
        .headers()
        .get("X-Inferx-Timeout")
        .and_then(|v| v.to_str().ok());
    let timeoutSec = match &timeout_header {
        None => policy.queueTimeout,
        Some(s) => match s.parse() {
            Err(_) => policy.queueTimeout,
            Ok(t) => policy.queueTimeout.min(t),
        },
    };
    let timeout = (timeoutSec * 1000.0) as u64;

    *req.uri_mut() = Uri::try_from(remainPath.clone()).unwrap();

    let mut res;
    let (mut parts, body) = req.into_parts();
    let mut bytes = match axum::body::to_bytes(body, FUNCCALL_MAX_BODY_BYTES).await {
        Err(_) => {
            let resp = FailureResponse(
                Error::BAD_REQUEST(StatusCode::BAD_REQUEST),
                &mut labels,
                Status::InvalidRequest,
            )
            .await;
            return Ok(resp);
        }
        Ok(b) => b,
    };

    let skill_model_path = skill_prefix
        .map(|_| route.func.object.spec.ModelPath())
        .flatten();
    if parts.method == axum::http::Method::POST {
        if let Some(prefix) = skill_prefix {
            match normalize_skill_request(&remainPath, &bytes, prefix, skill_model_path.as_deref())?
            {
                None => {}
                Some(norm) => {
                    parts.uri = Uri::try_from(norm.target_path.as_str()).unwrap_or(parts.uri);
                    bytes = Bytes::from(norm.body_bytes);
                    parts.headers.remove(hyper::header::CONTENT_LENGTH);
                    parts
                        .headers
                        .insert(CONTENT_TYPE, HeaderValue::from_static(norm.content_type));
                }
            }
        }

        match NormalizeFuncRequest(&gw, &route, &remainPath, &bytes).await {
            Err(status) => {
                error!(
                    "dispatch_func_call normalization failed tenant={}/{} func={} path={} method={} status={}",
                    route.physical.tenant,
                    route.physical.namespace,
                    route.physical.funcname,
                    remainPath,
                    parts.method,
                    status
                );
                let resp = Response::builder()
                    .status(status)
                    .body(Body::from("service failure: normalization failed"))
                    .unwrap();
                return Ok(resp);
            }
            Ok(None) => {}
            Ok(Some(norm)) => {
                parts.uri = Uri::try_from(norm.target_path.as_str()).unwrap_or(parts.uri);
                bytes = Bytes::from(norm.body_bytes);
                parts.headers.remove(hyper::header::CONTENT_LENGTH);
                parts
                    .headers
                    .insert(CONTENT_TYPE, HeaderValue::from_static(norm.content_type));
            }
        }
    }

    let trace_enabled = trace_logging_enabled();
    let redacted_json_req = summarize_funccall_body_for_log(&bytes, trace_enabled);
    let redacted_headers = summarize_headers_for_log(&headers);
    {
        let (max_tokens, input_chars) = serde_json::from_slice::<Value>(&bytes)
            .map(|v| {
                let max_tokens = v
                    .get("max_tokens")
                    .map(|t| t.to_string())
                    .unwrap_or_default();
                let input_chars: usize = v
                    .get("messages")
                    .and_then(Value::as_array)
                    .map(|msgs| {
                        msgs.iter()
                            .map(|m| m.get("content").map(|c| c.to_string().len()).unwrap_or(0))
                            .sum()
                    })
                    .unwrap_or_else(|| {
                        v.get("prompt")
                            .and_then(Value::as_str)
                            .map(|s| s.len())
                            .unwrap_or(0)
                    });
                (max_tokens, input_chars)
            })
            .unwrap_or_default();
        ctrace!(verbose_category::ROUTING, "dispatch_func_call sending to pod pod={} path={} input_chars={} approx_input_tokens={} max_tokens={}",
            route.physical.funcname, remainPath, input_chars, input_chars / 4, max_tokens);
    }
    if trace_enabled {
        trace!("FuncCall get req {:#?}", redacted_json_req);
    }
    let disconnect = Disconnect::New(redacted_json_req.clone(), redacted_headers.clone(), &labels);

    let mut retry = 0;
    let mut error = Error::Timeout(timeout);
    let client;
    let keepalive;
    let mut tcpConnLatency;
    let mut start;

    loop {
        retry += 1;
        if timestamp.Elapsed() > timeout {
            let resp = FailureResponse(error, &mut labels, Status::RequestFailure).await;
            ttftCtx.span().end();
            return Ok(resp);
        }

        let (mut tclient, tkeepalive) = match RetryGetClient(&gw, &route, timeout, timestamp).await
        {
            Err(e) => {
                error!("dispatch_func_call RetryGetClient failed logical={}/{}/{} physical={}/{}/{}: {:?}",
                    route.logical.tenant, route.logical.namespace, route.logical.funcname,
                    route.physical.tenant, route.physical.namespace, route.physical.funcname, e);
                let resp = FailureResponse(e, &mut labels, Status::ConnectFailure).await;
                return Ok(resp);
            }
            Ok(client) => client,
        };

        tcpConnLatency = now.elapsed().as_millis() as u64;
        if !tkeepalive {
            GATEWAY_METRICS
                .lock()
                .await
                .funccallCsCnt
                .get_or_create(&labels)
                .inc();
        }

        start = std::time::Instant::now();
        let req = Request::from_parts(parts.clone(), axum::body::Body::from(bytes.clone()));
        res = match tclient.Send(req).await {
            Err(e) => {
                error = e;
                continue;
            }
            Ok(r) => {
                if retry > 1 {
                    error!(
                        "FuncCall retry success {} with try round {}",
                        route.AgentKey(),
                        retry
                    );
                }
                r
            }
        };

        let status = res.status();
        if status != StatusCode::OK {
            if RETRYABLE_HTTP_STATUS.contains(&(status.as_u16())) {
                error!(
                    "Http call get fail status {:?} for pod {}",
                    status,
                    tclient.PodName()
                );
                continue;
            }
            let err_body = res
                .into_body()
                .collect()
                .await
                .map(|b| String::from_utf8_lossy(&b.to_bytes()).to_string())
                .unwrap_or_default();
            error!("dispatch_func_call pod returned non-200 status={} pod={} logical={}/{}/{} physical={}/{}/{} path={} body={}",
                status, tclient.PodName(),
                labels.tenant, labels.namespace, labels.funcname,
                route.physical.tenant, route.physical.namespace, route.physical.funcname,
                remainPath, err_body);
            let resp = FailureResponse(
                Error::BAD_REQUEST(status),
                &mut labels,
                Status::InvalidRequest,
            )
            .await;
            ttftCtx.span().end();
            return Ok(resp);
        }

        client = tclient;
        keepalive = tkeepalive;
        break;
    }

    labels.status = StatusCode::OK.as_u16();
    GATEWAY_METRICS
        .lock()
        .await
        .funccallcnt
        .get_or_create(&labels)
        .inc();

    let kvs: Vec<_> = res
        .headers()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let (tx, rx) = mpsc::channel::<SResult<Bytes, Infallible>>(4096);
    let (ttftTx, mut ttftRx) = mpsc::channel::<u64>(1);
    disconnect.Cancel();
    tokio::spawn(async move {
        let _client = client;
        let mut first = true;
        let mut ttft = 0;
        let mut total = 0;
        let mut bytecnt = 0;
        let mut framecount = 0;
        loop {
            let frame = res.frame().await;
            framecount += 1;
            if first {
                ttft = start.elapsed().as_millis() as u64;
                ttftTx.send(ttft).await.ok();
                ttftCtx.span().end();
                first = false;
                total = ttft + tcpConnLatency;
                if !keepalive {
                    GATEWAY_METRICS
                        .lock()
                        .await
                        .funccallCsTtft
                        .get_or_create(&labels)
                        .observe(total as f64 / 1000.0);
                } else {
                    GATEWAY_METRICS
                        .lock()
                        .await
                        .funccallTtft
                        .get_or_create(&labels)
                        .observe(total as f64);
                }
            }

            let bytes = match frame {
                None => {
                    let latency = start.elapsed();
                    REQ_AUDIT_AGENT.Audit(ReqAudit {
                        tenant: tenant.clone(),
                        namespace: namespace.clone(),
                        fpname: funcname.clone(),
                        keepalive,
                        ttft: ttft as i32,
                        latency: latency.as_millis() as i32,
                    });
                    return;
                }
                Some(Ok(b)) => b,
                Some(Err(e)) => {
                    error!(
                        "PostCall for path {}/{}/{} len {} get error {:?}",
                        tenant, namespace, funcname, bytecnt, e
                    );
                    return;
                }
            };
            let bytes: Bytes = bytes.into_data().unwrap();
            bytecnt += bytes.len();
            if tx.send(Ok(bytes)).await.is_err() {
                error!(
                    "Funccall ********* Fail: sendbytes fail with client disconnect after return header {} ms ttft {} ms framecount {} retry count {} total send {} bytes headers {:#?} req {:#?}",
                    reqStart.elapsed().as_millis(),
                    total,
                    framecount,
                    retry - 1,
                    bytecnt,
                    &redacted_headers,
                    redacted_json_req
                );
                return;
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let body = axum::body::Body::from_stream(http_body_util::StreamBody::new(stream));
    let mut resp = Response::new(body);
    for (k, v) in kvs {
        resp.headers_mut().insert(k, v);
    }
    resp.headers_mut().insert(
        TCPCONN_LATENCY_HEADER,
        HeaderValue::from_str(&format!("{}", tcpConnLatency)).unwrap(),
    );
    if let Some(ttft) = ttftRx.recv().await {
        resp.headers_mut().insert(
            TTFT_LATENCY_HEADER,
            HeaderValue::from_str(&format!("{}", ttft)).unwrap(),
        );
    }
    Ok(resp)
}

fn skill_admin_response(err: Error) -> Response<Body> {
    let status = match err {
        Error::NoPermission => StatusCode::FORBIDDEN,
        Error::SqlxError(sqlx::Error::RowNotFound) | Error::NotExist(_) => StatusCode::NOT_FOUND,
        Error::Exist(_) => StatusCode::CONFLICT,
        Error::SqlxError(sqlx::Error::Database(ref db_err))
            if db_err.code().as_deref() == Some("23505")
                || db_err.code().as_deref() == Some("23514") =>
        {
            StatusCode::CONFLICT
        }
        _ => StatusCode::BAD_REQUEST,
    };
    Response::builder()
        .status(status)
        .body(Body::from(format!("service failure {:?}", err)))
        .unwrap()
}

fn is_skill_template_duplicate_name_error(err: &Error) -> bool {
    match err {
        Error::SqlxError(sqlx::Error::Database(db_err)) => {
            if db_err.code().as_deref() != Some("23505") {
                return false;
            }

            if matches!(
                db_err.constraint(),
                Some("skilltemplate_tenant_namespace_display_name_key")
            ) {
                return true;
            }

            db_err
                .message()
                .contains("duplicate key value violates unique constraint")
        }
        _ => false,
    }
}

fn normalize_required_skill_template_field(name: &str, value: &str) -> Result<String> {
    let normalized = value.trim();
    if normalized.is_empty() {
        return Err(Error::CommonError(format!("{} is required", name)));
    }

    Ok(normalized.to_string())
}

async fn ListSkillTemplates(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
) -> SResult<Response, StatusCode> {
    if !token.CheckScope("read") {
        return Ok(Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(Body::from("service failure: insufficient scope"))
            .unwrap());
    }
    match gw.sqlSecret.ListActiveSkillTemplates().await {
        Ok(templates) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&templates).unwrap()))
            .unwrap()),
        Err(e) => Ok(skill_admin_response(e)),
    }
}

async fn ListPublishedSkills(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
) -> SResult<Response, StatusCode> {
    if !token.CheckScope("read") {
        return Ok(Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(Body::from("service failure: insufficient scope"))
            .unwrap());
    }

    let result = if token.IsInferxAdmin() {
        gw.sqlSecret.ListAllSkills().await
    } else {
        gw.sqlSecret.ListPublishedSkills().await
    };
    match result {
        Ok(skills) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&skills).unwrap()))
            .unwrap()),
        Err(e) => Ok(skill_admin_response(e)),
    }
}

async fn ListSkillsByTenant(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path(owner_tenant): Path<String>,
) -> SResult<Response, StatusCode> {
    if !token.CheckScope("read") {
        return Ok(Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(Body::from("service failure: insufficient scope"))
            .unwrap());
    }

    let show_all = token.IsTenantAdmin(&owner_tenant) || token.IsInferxAdmin();
    match gw
        .sqlSecret
        .ListSkillsByTenant(&owner_tenant, show_all)
        .await
    {
        Ok(skills) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&skills).unwrap()))
            .unwrap()),
        Err(e) => Ok(skill_admin_response(e)),
    }
}

async fn ListSkillsByNamespace(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path((owner_tenant, namespace)): Path<(String, String)>,
) -> SResult<Response, StatusCode> {
    if !token.CheckScope("read") {
        return Ok(Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(Body::from("service failure: insufficient scope"))
            .unwrap());
    }

    let show_all = token.IsNamespaceAdmin(&owner_tenant, &namespace) || token.IsInferxAdmin();
    match gw
        .sqlSecret
        .ListSkillsByNamespace(&owner_tenant, &namespace, show_all)
        .await
    {
        Ok(skills) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&skills).unwrap()))
            .unwrap()),
        Err(e) => Ok(skill_admin_response(e)),
    }
}

async fn ListSkillMarketplace(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Query(params): Query<SkillMarketplaceQuery>,
) -> SResult<Response, StatusCode> {
    if !token.CheckScope("read") {
        return Ok(Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(Body::from("service failure: insufficient scope"))
            .unwrap());
    }

    if params.page < 1 {
        return Ok(skill_admin_response(Error::CommonError(
            "page must be >= 1".to_string(),
        )));
    }

    if params.page_size < 1 || params.page_size > 200 {
        return Ok(skill_admin_response(Error::CommonError(
            "page_size must be between 1 and 200".to_string(),
        )));
    }

    if let Some(tag) = params
        .tag
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Ok(skill_admin_response(Error::CommonError(format!(
            "tag filter '{}' is not supported: skills do not have tag metadata",
            tag
        ))));
    }

    let tenant_exists = |t: &str| gw.objRepo.tenantMgr.Get("system", "system", t).is_ok();
    let subscriber_tenant = match resolve_subscription_tenant(&token, &tenant_exists) {
        Ok(tenant) => tenant,
        Err(e) => return Ok(skill_admin_response(e)),
    };
    let include_unpublished = params.include_unpublished && token.IsInferxAdmin();
    match gw
        .sqlSecret
        .ListMarketplaceSkills(
            Some(subscriber_tenant.as_str()),
            params.keyword.as_deref(),
            params.page,
            params.page_size,
            include_unpublished,
        )
        .await
    {
        Ok(skills) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&skills).unwrap()))
            .unwrap()),
        Err(e) => Ok(skill_admin_response(e)),
    }
}

async fn CreateSkillSubscription(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Json(req): Json<SkillSubscriptionCreateRequest>,
) -> SResult<Response, StatusCode> {
    let tenant_exists = |t: &str| gw.objRepo.tenantMgr.Get("system", "system", t).is_ok();
    let subscriber_tenant = match resolve_subscription_tenant(&token, &tenant_exists) {
        Ok(tenant) => tenant,
        Err(e) => return Ok(skill_admin_response(e)),
    };
    if !token.IsTenantAdmin(&subscriber_tenant) && !token.IsInferxAdmin() {
        return Ok(skill_admin_response(Error::NoPermission));
    }

    let tool_alias = match req.tool_alias.as_deref() {
        Some(alias) => match normalize_subscription_alias(alias) {
            Ok(alias) => alias,
            Err(e) => return Ok(skill_admin_response(e)),
        },
        None => match normalize_subscription_alias(&req.skillname) {
            Ok(alias) => alias,
            Err(_) => {
                return Ok(skill_admin_response(Error::CommonError(format!(
                    "skillname '{}' cannot be used as the default tool_alias; provide an explicit alias using only letters, numbers, hyphens, underscores, and periods",
                    req.skillname.trim()
                ))))
            }
        },
    };

    match gw
        .sqlSecret
        .CreateSkillSubscription(
            &subscriber_tenant,
            req.owner_tenant.trim(),
            req.owner_namespace.trim(),
            req.skillname.trim(),
            &tool_alias,
            &token.username,
            token.IsInferxAdmin(),
        )
        .await
    {
        Ok(sub) => Ok(Response::builder()
            .status(StatusCode::CREATED)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&sub).unwrap()))
            .unwrap()),
        Err(e) if is_skill_subscription_conflict_error(&e) => Ok(skill_admin_response(
            Error::Exist("subscription or tool_alias already exists".to_string()),
        )),
        Err(e) => Ok(skill_admin_response(e)),
    }
}

async fn ListSkillSubscriptions(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
) -> SResult<Response, StatusCode> {
    if !token.CheckScope("read") {
        return Ok(Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(Body::from("service failure: insufficient scope"))
            .unwrap());
    }

    let tenant_exists = |t: &str| gw.objRepo.tenantMgr.Get("system", "system", t).is_ok();
    let subscriber_tenant = match resolve_subscription_tenant(&token, &tenant_exists) {
        Ok(tenant) => tenant,
        Err(e) => return Ok(skill_admin_response(e)),
    };

    match gw
        .sqlSecret
        .ListSkillSubscriptions(&subscriber_tenant)
        .await
    {
        Ok(subs) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&subs).unwrap()))
            .unwrap()),
        Err(e) => Ok(skill_admin_response(e)),
    }
}

async fn DeleteSkillSubscription(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path((owner_tenant, owner_namespace, skillname)): Path<(String, String, String)>,
) -> SResult<Response, StatusCode> {
    let tenant_exists = |t: &str| gw.objRepo.tenantMgr.Get("system", "system", t).is_ok();
    let subscriber_tenant = match resolve_subscription_tenant(&token, &tenant_exists) {
        Ok(tenant) => tenant,
        Err(e) => return Ok(skill_admin_response(e)),
    };
    if !token.IsTenantAdmin(&subscriber_tenant) {
        return Ok(skill_admin_response(Error::NoPermission));
    }

    match gw
        .sqlSecret
        .DeleteSkillSubscription(
            &subscriber_tenant,
            &owner_tenant,
            &owner_namespace,
            &skillname,
        )
        .await
    {
        Ok(()) => Ok(Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Body::empty())
            .unwrap()),
        Err(e) => Ok(skill_admin_response(e)),
    }
}

async fn UpdateSkillSubscription(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path((owner_tenant, owner_namespace, skillname)): Path<(String, String, String)>,
    Json(req): Json<SkillSubscriptionUpdateRequest>,
) -> SResult<Response, StatusCode> {
    let tenant_exists = |t: &str| gw.objRepo.tenantMgr.Get("system", "system", t).is_ok();
    let subscriber_tenant = match resolve_subscription_tenant(&token, &tenant_exists) {
        Ok(tenant) => tenant,
        Err(e) => return Ok(skill_admin_response(e)),
    };
    if !token.IsTenantAdmin(&subscriber_tenant) {
        return Ok(skill_admin_response(Error::NoPermission));
    }

    let tool_alias = match normalize_subscription_alias(&req.tool_alias) {
        Ok(alias) => alias,
        Err(e) => return Ok(skill_admin_response(e)),
    };

    match gw
        .sqlSecret
        .UpdateSkillSubscriptionAlias(
            &subscriber_tenant,
            &owner_tenant,
            &owner_namespace,
            &skillname,
            &tool_alias,
        )
        .await
    {
        Ok(sub) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&sub).unwrap()))
            .unwrap()),
        Err(e) if is_skill_subscription_conflict_error(&e) => Ok(skill_admin_response(
            Error::Exist("tool_alias already exists".to_string()),
        )),
        Err(e) => Ok(skill_admin_response(e)),
    }
}

async fn ListAdminSkillTemplates(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
) -> SResult<Response, StatusCode> {
    if !token.IsInferxAdmin() {
        return Ok(skill_admin_response(Error::NoPermission));
    }

    match gw.sqlSecret.ListSkillTemplates().await {
        Ok(templates) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&templates).unwrap()))
            .unwrap()),
        Err(e) => Ok(skill_admin_response(e)),
    }
}

async fn CreateSkillTemplate(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Json(req): Json<SkillTemplateCreateRequest>,
) -> SResult<Response, StatusCode> {
    if !token.IsInferxAdmin() {
        return Ok(skill_admin_response(Error::NoPermission));
    }

    let tenant = match normalize_required_skill_template_field("tenant", &req.tenant) {
        Ok(v) => v,
        Err(e) => return Ok(skill_admin_response(e)),
    };
    let namespace = match normalize_required_skill_template_field("namespace", &req.namespace) {
        Ok(v) => v,
        Err(e) => return Ok(skill_admin_response(e)),
    };
    let display_name =
        match normalize_required_skill_template_field("display_name", &req.display_name) {
            Ok(v) => v,
            Err(e) => return Ok(skill_admin_response(e)),
        };
    let func_tenant = match normalize_required_skill_template_field("func_tenant", &req.func_tenant)
    {
        Ok(v) => v,
        Err(e) => return Ok(skill_admin_response(e)),
    };
    let func_namespace =
        match normalize_required_skill_template_field("func_namespace", &req.func_namespace) {
            Ok(v) => v,
            Err(e) => return Ok(skill_admin_response(e)),
        };
    let normal_funcname =
        match normalize_required_skill_template_field("normal_funcname", &req.normal_funcname) {
            Ok(v) => v,
            Err(e) => return Ok(skill_admin_response(e)),
        };
    let description = req
        .description
        .as_ref()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());

    if let Err(e) = gw
        .objRepo
        .GetFunc(&func_tenant, &func_namespace, &normal_funcname)
    {
        return Ok(Response::builder()
            .status(StatusCode::UNPROCESSABLE_ENTITY)
            .body(Body::from(format!("function not found: {:?}", e)))
            .unwrap());
    }

    match gw
        .sqlSecret
        .CreateSkillTemplate(
            &tenant,
            &namespace,
            &display_name,
            description.as_deref(),
            &func_tenant,
            &func_namespace,
            &normal_funcname,
            req.is_active,
        )
        .await
    {
        Ok(template) => Ok(Response::builder()
            .status(StatusCode::CREATED)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&template).unwrap()))
            .unwrap()),
        Err(e) => {
            if is_skill_template_duplicate_name_error(&e) {
                return Ok(Response::builder()
                    .status(StatusCode::CONFLICT)
                    .body(Body::from("skill template name is taken"))
                    .unwrap());
            }

            Ok(skill_admin_response(e))
        }
    }
}

async fn DeactivateSkillTemplate(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path(template_id): Path<i64>,
) -> SResult<Response, StatusCode> {
    if !token.IsInferxAdmin() {
        return Ok(skill_admin_response(Error::NoPermission));
    }

    match gw.sqlSecret.DeactivateSkillTemplate(template_id).await {
        Ok(()) => Ok(Response::builder()
            .status(StatusCode::OK)
            .body(Body::from("deactivated"))
            .unwrap()),
        Err(e) => Ok(skill_admin_response(e)),
    }
}

async fn ActivateSkillTemplate(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path(template_id): Path<i64>,
) -> SResult<Response, StatusCode> {
    if !token.IsInferxAdmin() {
        return Ok(skill_admin_response(Error::NoPermission));
    }

    match gw.sqlSecret.ActivateSkillTemplate(template_id).await {
        Ok(()) => Ok(Response::builder()
            .status(StatusCode::OK)
            .body(Body::from("activated"))
            .unwrap()),
        Err(e) => Ok(skill_admin_response(e)),
    }
}

async fn UpdateSkillTemplate(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path(template_id): Path<i64>,
    Json(req): Json<SkillTemplateUpdateRequest>,
) -> SResult<Response, StatusCode> {
    if !token.IsInferxAdmin() {
        return Ok(skill_admin_response(Error::NoPermission));
    }

    let tenant = match normalize_required_skill_template_field("tenant", &req.tenant) {
        Ok(v) => v,
        Err(e) => return Ok(skill_admin_response(e)),
    };
    let namespace = match normalize_required_skill_template_field("namespace", &req.namespace) {
        Ok(v) => v,
        Err(e) => return Ok(skill_admin_response(e)),
    };
    let display_name =
        match normalize_required_skill_template_field("display_name", &req.display_name) {
            Ok(v) => v,
            Err(e) => return Ok(skill_admin_response(e)),
        };
    let func_tenant = match normalize_required_skill_template_field("func_tenant", &req.func_tenant)
    {
        Ok(v) => v,
        Err(e) => return Ok(skill_admin_response(e)),
    };
    let func_namespace =
        match normalize_required_skill_template_field("func_namespace", &req.func_namespace) {
            Ok(v) => v,
            Err(e) => return Ok(skill_admin_response(e)),
        };
    let normal_funcname =
        match normalize_required_skill_template_field("normal_funcname", &req.normal_funcname) {
            Ok(v) => v,
            Err(e) => return Ok(skill_admin_response(e)),
        };
    let description = req
        .description
        .as_ref()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());

    let current = match gw.sqlSecret.GetSkillTemplate(template_id).await {
        Ok(t) => t,
        Err(e) => return Ok(skill_admin_response(e)),
    };

    let func_fields_changed = current.func_tenant != func_tenant
        || current.func_namespace != func_namespace
        || current.normal_funcname != normal_funcname;

    if func_fields_changed {
        if let Err(e) = gw
            .objRepo
            .GetFunc(&func_tenant, &func_namespace, &normal_funcname)
        {
            return Ok(Response::builder()
                .status(StatusCode::UNPROCESSABLE_ENTITY)
                .body(Body::from(format!("function not found: {:?}", e)))
                .unwrap());
        }

        match gw.sqlSecret.IsSkillTemplateReferenced(template_id).await {
            Ok(true) => {
                return Ok(Response::builder()
                    .status(StatusCode::CONFLICT)
                    .body(Body::from(
                        "function fields cannot be changed: template is in use by existing skills",
                    ))
                    .unwrap());
            }
            Ok(false) => {}
            Err(e) => return Ok(skill_admin_response(e)),
        }
    }

    match gw
        .sqlSecret
        .UpdateSkillTemplate(
            template_id,
            &tenant,
            &namespace,
            &display_name,
            description.as_deref(),
            &func_tenant,
            &func_namespace,
            &normal_funcname,
            req.is_active,
        )
        .await
    {
        Ok(template) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&template).unwrap()))
            .unwrap()),
        Err(e) => {
            if is_skill_template_duplicate_name_error(&e) {
                return Ok(Response::builder()
                    .status(StatusCode::CONFLICT)
                    .body(Body::from("skill template name is taken"))
                    .unwrap());
            }

            Ok(skill_admin_response(e))
        }
    }
}

async fn DeleteSkillTemplate(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path(template_id): Path<i64>,
) -> SResult<Response, StatusCode> {
    if !token.IsInferxAdmin() {
        return Ok(skill_admin_response(Error::NoPermission));
    }

    match gw.sqlSecret.IsSkillTemplateReferenced(template_id).await {
        Ok(true) => {
            return Ok(Response::builder()
                .status(StatusCode::CONFLICT)
                .body(Body::from(
                    "template is in use by existing skills and cannot be deleted",
                ))
                .unwrap());
        }
        Ok(false) => {}
        Err(e) => return Ok(skill_admin_response(e)),
    }

    match gw.sqlSecret.DeleteSkillTemplate(template_id).await {
        Ok(()) => Ok(Response::builder()
            .status(StatusCode::OK)
            .body(Body::from("deleted"))
            .unwrap()),
        Err(e) => Ok(skill_admin_response(e)),
    }
}

async fn CreateSkill(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path((owner_tenant, namespace, skillname)): Path<(String, String, String)>,
    Json(req): Json<SkillCreateRequest>,
) -> SResult<Response, StatusCode> {
    if !token.IsNamespaceAdmin(&owner_tenant, &namespace) {
        return Ok(skill_admin_response(Error::NoPermission));
    }
    if req.earning_type.is_some() {
        return Ok(skill_admin_response(Error::CommonError(
            "earning_type is not supported in R1".to_string(),
        )));
    }
    let serving_mode = req.serving_mode.unwrap_or_else(|| "dedicated".to_string());
    if serving_mode != "dedicated" {
        return Ok(skill_admin_response(Error::CommonError(
            "serving_mode must be dedicated in R1".to_string(),
        )));
    }
    if req.has_cache.unwrap_or(false) {
        return Ok(skill_admin_response(Error::CommonError(
            "has_cache=true is not supported in R1".to_string(),
        )));
    }
    let gpu_billing_target = req
        .gpu_billing_target
        .clone()
        .unwrap_or_else(|| "caller".to_string());
    if gpu_billing_target != "caller" && gpu_billing_target != "owner" {
        return Ok(skill_admin_response(Error::CommonError(
            "gpu_billing_target must be caller or owner".to_string(),
        )));
    }

    let template = match gw.sqlSecret.GetSkillTemplate(req.template_id).await {
        Ok(template) => template,
        Err(e) => return Ok(skill_admin_response(e)),
    };
    if !template.is_active {
        return Ok(skill_admin_response(Error::CommonError(
            "template is inactive".to_string(),
        )));
    }

    match gw
        .sqlSecret
        .CreateSkill(
            &owner_tenant,
            &namespace,
            &skillname,
            req.description.as_deref(),
            &serving_mode,
            "free",
            None,
            &gpu_billing_target,
            req.template_id,
            false,
            req.allowed_child_skilleps.as_deref(),
            &token.username,
        )
        .await
    {
        Ok(skill) => match write_skill_prefix(
            &owner_tenant,
            &namespace,
            &skillname,
            skill.version,
            &req.prefix,
        ) {
            Ok(()) => Ok(Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&skill).unwrap()))
                .unwrap()),
            Err(e) => {
                let _ = gw
                    .sqlSecret
                    .DeleteSkill(&owner_tenant, &namespace, &skillname)
                    .await;
                Ok(skill_admin_response(Error::from(e)))
            }
        },
        Err(e) => Ok(skill_admin_response(e)),
    }
}

async fn GetSkill(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path((owner_tenant, namespace, skillname)): Path<(String, String, String)>,
) -> SResult<Response, StatusCode> {
    let skill = match gw
        .sqlSecret
        .GetSkill(&owner_tenant, &namespace, &skillname)
        .await
    {
        Ok(skill) => skill,
        Err(e) => return Ok(skill_admin_response(e)),
    };
    if !skill.is_published
        && !token.IsNamespaceAdmin(&owner_tenant, &namespace)
        && !token.IsInferxAdmin()
    {
        return Ok(skill_admin_response(Error::NoPermission));
    }
    let mut body = serde_json::to_value(&skill).unwrap();
    if !token.IsNamespaceAdmin(&owner_tenant, &namespace) && !token.IsInferxAdmin() {
        if let Some(obj) = body.as_object_mut() {
            obj.remove("allowed_child_skilleps");
        }
    }
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap())
}

async fn DeleteSkill(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path((owner_tenant, namespace, skillname)): Path<(String, String, String)>,
) -> SResult<Response, StatusCode> {
    if !token.IsNamespaceAdmin(&owner_tenant, &namespace) {
        return Ok(skill_admin_response(Error::NoPermission));
    }
    match gw
        .sqlSecret
        .DeleteSkill(&owner_tenant, &namespace, &skillname)
        .await
    {
        Ok(()) => match remove_skill_storage(&owner_tenant, &namespace, &skillname) {
            Ok(()) => Ok(Response::builder()
                .status(StatusCode::OK)
                .body(Body::from("deleted"))
                .unwrap()),
            Err(e) => Ok(skill_admin_response(Error::from(e))),
        },
        Err(e) => Ok(skill_admin_response(e)),
    }
}

async fn PublishSkill(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path((owner_tenant, namespace, skillname)): Path<(String, String, String)>,
) -> SResult<Response, StatusCode> {
    if !token.IsInferxAdmin() {
        return Ok(skill_admin_response(Error::NoPermission));
    }
    match gw
        .sqlSecret
        .PublishSkill(&owner_tenant, &namespace, &skillname, &token.username)
        .await
    {
        Ok(()) => Ok(Response::builder()
            .status(StatusCode::OK)
            .body(Body::from("published"))
            .unwrap()),
        Err(e) => Ok(skill_admin_response(e)),
    }
}

async fn UnpublishSkill(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path((owner_tenant, namespace, skillname)): Path<(String, String, String)>,
) -> SResult<Response, StatusCode> {
    if !token.IsInferxAdmin() {
        return Ok(skill_admin_response(Error::NoPermission));
    }
    match gw
        .sqlSecret
        .UnpublishSkill(&owner_tenant, &namespace, &skillname)
        .await
    {
        Ok(()) => Ok(Response::builder()
            .status(StatusCode::OK)
            .body(Body::from("unpublished"))
            .unwrap()),
        Err(e) => Ok(skill_admin_response(e)),
    }
}

async fn SkillCall(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path((owner_tenant, namespace, skillname, subpath)): Path<(String, String, String, String)>,
    req: Request,
) -> SResult<Response, StatusCode> {
    if !token.CheckScope("inference") {
        return Ok(Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(Body::from("service failure: insufficient scope"))
            .unwrap());
    }

    let remainPath = format!("/{}", subpath);

    let skill = match gw
        .sqlSecret
        .GetSkill(&owner_tenant, &namespace, &skillname)
        .await
    {
        Ok(skill) => skill,
        Err(Error::SqlxError(sqlx::Error::RowNotFound)) => {
            return Ok(Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::from("service failure: skill not found"))
                .unwrap())
        }
        Err(e) => return Ok(skill_admin_response(e)),
    };

    // TODO: Revisit this decision in the future based on user experience considerations.
    // if !skill.is_published
    //     && !token.IsNamespaceInferenceUser(&owner_tenant, &namespace)
    //     && !token.IsNamespaceAdmin(&owner_tenant, &namespace)
    // {
    //     return Ok(Response::builder()
    //         .status(StatusCode::NOT_FOUND)
    //         .body(Body::from("service failure: not found"))
    //         .unwrap());
    // }
    if skill.has_cache && skill.cache_status != "ready" {
        return Ok(Response::builder()
            .status(StatusCode::SERVICE_UNAVAILABLE)
            .body(Body::from("service failure: skill cache not ready"))
            .unwrap());
    }

    let tenant_exists = |t: &str| gw.objRepo.tenantMgr.Get("system", "system", t).is_ok();
    let calling_tenant = match resolve_skill_calling_tenant(&token, &tenant_exists) {
        Ok(tenant) => tenant,
        Err(e) => return Ok(skill_admin_response(e)),
    };

    if !token.IsInferxAdmin() {
        let payer = if skill.gpu_billing_target == "owner" {
            skill.owner_tenant.as_str()
        } else {
            calling_tenant.as_str()
        };
        match tenant_quota_state(&gw, payer) {
            Ok((true, _)) => {}
            Ok((false, true)) => return Ok(quota_exceeded_response(payer)),
            Ok((false, false)) => {}
            Err(e) => {
                error!("skill quota lookup failed for {}: {:?}", payer, e);
                return Ok(quota_lookup_failed_response(payer));
            }
        }
    }

    let prefix = match load_skill_prefix(&skill) {
        Ok(prefix) => prefix,
        Err(e) => {
            error!(
                "SkillCall load_skill_prefix failed skill={}/{}/{} version={}: {:?}",
                owner_tenant, namespace, skillname, skill.version, e
            );
            return Ok(skill_admin_response(e));
        }
    };
    ctrace!(
        verbose_category::SKILL,
        "SkillCall prefix loaded skill={}/{}/{} prefix_len={}",
        owner_tenant,
        namespace,
        skillname,
        prefix.len()
    );

    let physical_funcname = if skill.has_cache {
        skill.consumer_funcname.clone().ok_or_else(|| {
            Error::CommonError("consumer_funcname missing for cached skill".to_string())
        })
    } else {
        Ok(skill.normal_funcname.clone())
    };
    let physical_funcname = match physical_funcname {
        Ok(name) => name,
        Err(e) => return Ok(skill_admin_response(e)),
    };

    let func = match gw.objRepo.GetFunc(
        &skill.func_tenant,
        &skill.func_namespace,
        &physical_funcname,
    ) {
        Ok(func) => func,
        Err(e) => {
            error!(
                "SkillCall GetFunc failed func={}/{}/{}: {:?}",
                skill.func_tenant, skill.func_namespace, physical_funcname, e
            );
            return Ok(skill_admin_response(e));
        }
    };
    ctrace!(
        verbose_category::SKILL,
        "SkillCall GetFunc ok func={}/{}/{} version={}",
        skill.func_tenant,
        skill.func_namespace,
        physical_funcname,
        func.Version()
    );

    let owner_billed = skill.gpu_billing_target == "owner";
    let base_funcname = format!(
        "{}.{}.{}",
        skill.func_tenant, skill.func_namespace, physical_funcname
    );
    // Collapse a caller-billed skill backed by the platform endpoint model onto
    // the caller's own endpoint logical identity (`{calling_tenant}/endpoints/
    // {physical_funcname}`), so it shares the same instance pool / policy as a
    // direct `/funccall/{calling_tenant}/endpoints/{slug}` call. No same-tenant
    // guard: cross-tenant callers collapse too, each keyed on its own
    // `calling_tenant`, so subscribers never converge onto the owner's pod.
    let collapse_onto_endpoint = !owner_billed
        && skill.func_tenant == PLATFORM_TENANT
        && skill.func_namespace == PLATFORM_SHARED_NAMESPACE;
    let (logical_tenant, logical_namespace, logical_funcname, caller_tenant) =
        if collapse_onto_endpoint {
            (
                calling_tenant.clone(),
                VIRTUAL_ENDPOINTS_NAMESPACE.to_string(),
                physical_funcname.clone(),
                None,
            )
        } else if owner_billed {
            (
                skill.owner_tenant.clone(),
                SKILLS_NAMESPACE.to_string(),
                format!("{}.{}", base_funcname, calling_tenant),
                Some(calling_tenant.clone()),
            )
        } else {
            (
                calling_tenant.clone(),
                SKILLS_NAMESPACE.to_string(),
                base_funcname,
                None,
            )
        };
    ctrace!(
        verbose_category::SKILL,
        "SkillCall dispatch logical={}/{}/{} physical={}/{}/{} remain={}",
        logical_tenant,
        logical_namespace,
        logical_funcname,
        skill.func_tenant,
        skill.func_namespace,
        physical_funcname,
        remainPath
    );
    let route = FuncRouteTarget {
        logical: FuncIdentity {
            tenant: logical_tenant,
            namespace: logical_namespace.clone(),
            funcname: logical_funcname.clone(),
            version: func.Version(),
        },
        physical: FuncIdentity {
            tenant: skill.func_tenant.clone(),
            namespace: skill.func_namespace.clone(),
            funcname: physical_funcname,
            version: func.Version(),
        },
        policy: GW_OBJREPO.get().unwrap().FuncPolicy(&func),
        func,
        caller_tenant,
    };

    if req.method() != axum::http::Method::POST || !remainPath.starts_with("/v1/chat/completions") {
        return dispatch_func_call(
            &gw,
            req,
            route,
            calling_tenant,
            logical_namespace.clone(),
            logical_funcname,
            remainPath,
            Some(prefix.as_str()),
        )
        .await;
    }

    let cancel_token = req
        .headers()
        .get("X-Mcp-Cancel-Id")
        .and_then(|v| v.to_str().ok())
        .and_then(|id| McpCancelRegistry::global().lookup(id))
        .unwrap_or_else(CancellationToken::new);

    let is_debug_authorized =
        token.IsNamespaceAdmin(&owner_tenant, &namespace) || token.IsInferxAdmin();

    let (req_parts, req_body) = req.into_parts();
    let req_headers = req_parts.headers;

    let is_child_request = extract_is_child_request(&req_headers);
    let child_chain_depth = if is_child_request {
        req_headers
            .get(SKILL_CHAIN_DEPTH_HEADER)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u32>().ok())
    } else {
        None
    };

    let body_bytes = match axum::body::to_bytes(req_body, FUNCCALL_MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(_) => {
            return Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from("service failure: invalid request body"))
                .unwrap())
        }
    };
    let wire: SkillChatCompletionsWireRequest = match serde_json::from_slice(&body_bytes) {
        Ok(w) => w,
        Err(_) => {
            return Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from("service failure: invalid request body"))
                .unwrap())
        }
    };

    let allowed_child_skilleps = skill.allowed_child_skilleps.map(|ids| {
        std::sync::Arc::new(
            ids.into_iter()
                .collect::<std::collections::HashSet<String>>(),
        )
    });

    let ctx = SkillInvocationContext {
        route,
        calling_tenant,
        display_skill_id: format!("{}/{}/{}", owner_tenant, namespace, skillname),
        logical_funcname,
        remain_path: remainPath,
        prefix,
        skills_namespace: logical_namespace,
        is_child_request,
        child_chain_depth,
        is_debug_authorized,
        allowed_child_skilleps,
        cancel_token,
        request_headers: req_headers,
    };

    handle_skill_call_chain(&gw, wire, ctx).await
}

async fn ReadPodFaillogs(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path((tenant, namespace, name, revision)): Path<(String, String, String, i64)>,
) -> SResult<Response, StatusCode> {
    let logs = gw
        .ReadPodFailLogs(&token, &tenant, &namespace, &name, revision)
        .await;
    let logs = match logs {
        Ok(d) => d,
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    };
    let data = serde_json::to_string(&logs).unwrap();
    let body = Body::from(data);
    let resp = Response::builder()
        .status(StatusCode::OK)
        .body(body)
        .unwrap();
    return Ok(resp);
}

async fn ReadPodFaillog(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path((tenant, namespace, name, revision, id)): Path<(String, String, String, i64, String)>,
) -> SResult<Response, StatusCode> {
    let log = gw
        .ReadPodFaillog(&token, &tenant, &namespace, &name, revision, &id)
        .await;
    let logs = match log {
        Ok(d) => d,
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    };
    let data = serde_json::to_string(&logs).unwrap();
    let body = Body::from(data);
    let resp = Response::builder()
        .status(StatusCode::OK)
        .body(body)
        .unwrap();
    return Ok(resp);
}

async fn CreateApikey(
    Extension(token): Extension<Arc<AccessToken>>,
    Json(obj): Json<ApikeyCreateRequest>,
) -> SResult<Response, StatusCode> {
    match GetTokenCache().await.CreateApikey(&token, &obj).await {
        Ok(apikey) => {
            let body = Body::from(serde_json::to_string(&apikey).unwrap());
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            if is_apikey_duplicate_keyname_error(&e) {
                let body = Body::from(format!(
                    "API key name '{}' already exists. Please choose a different key name.",
                    obj.keyname
                ));
                let resp = Response::builder()
                    .status(StatusCode::CONFLICT)
                    .body(body)
                    .unwrap();
                return Ok(resp);
            }

            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct OnboardResponse {
    tenant_name: String,
    role: String,
    created: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    apikey: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    apikey_name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct AdminTenantProfileRow {
    tenant_name: String,
    sub: Option<String>,
    display_name: Option<String>,
    email: Option<String>,
    normal_model_count: u64,
    normal_catalog_model_count: u64,
    total_catalog_model_count: u64,
    total_model_count: u64,
    used_cents: i64,
    balance_cents: i64,
    created_at: Option<chrono::NaiveDateTime>,
}

async fn Onboard(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
) -> SResult<Response, StatusCode> {
    match gw.Onboard(&token).await {
        Ok((tenant_name, created, apikey, apikey_name)) => {
            let apikey = if apikey.is_empty() {
                None
            } else {
                Some(apikey)
            };
            let apikey_name = if apikey_name.is_empty() {
                None
            } else {
                Some(apikey_name)
            };
            let body = Body::from(
                serde_json::to_string(&OnboardResponse {
                    tenant_name: tenant_name,
                    role: "admin".to_owned(),
                    created: created,
                    apikey,
                    apikey_name,
                })
                .unwrap(),
            );
            let resp = Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, "application/json")
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(Error::NoPermission) => {
            let body = Body::from("service failure: No permission");
            let resp = Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn GetAdminTenants(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
) -> SResult<Response, StatusCode> {
    if !token.IsInferxAdmin() {
        let body = Body::from("service failure: No permission");
        let resp = Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    let tenant_objs = match gw.objRepo.tenantMgr.GetObjects("", "") {
        Ok(v) => v,
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    };

    let mut tenant_names = Vec::new();
    for t in tenant_objs {
        tenant_names.push(t.Name());
    }
    tenant_names.sort();
    tenant_names.dedup();

    let mut model_counts: HashMap<String, (u64, u64, u64, u64)> = HashMap::new();
    match gw.objRepo.funcMgr.GetObjects("", "") {
        Ok(funcs) => {
            for func in funcs {
                let is_normal =
                    match gw
                        .objRepo
                        .funcstatusMgr
                        .Get(&func.tenant, &func.namespace, &func.name)
                    {
                        Ok(funcstatus) => matches!(funcstatus.object.state, FuncState::Normal),
                        Err(_) => matches!(func.object.status.state, FuncState::Normal),
                    };
                let is_catalog_backed = func.object.spec.catalogSource.is_some();

                let entry = model_counts
                    .entry(func.tenant.clone())
                    .or_insert((0, 0, 0, 0));
                entry.3 += 1;
                if is_normal {
                    entry.0 += 1;
                }
                if is_catalog_backed {
                    entry.2 += 1;
                    if is_normal {
                        entry.1 += 1;
                    }
                }
            }
        }
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }

    match GetTokenCache()
        .await
        .sqlstore
        .GetTenantProfilesByTenantNames(&tenant_names)
        .await
    {
        Ok(mut profiles) => {
            let mut billing = match gw
                .sqlBilling
                .GetTenantBillingSummariesByTenantNames(&tenant_names)
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    let body = Body::from(format!("service failure {:?}", e));
                    let resp = Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(body)
                        .unwrap();
                    return Ok(resp);
                }
            };

            let mut rows = Vec::with_capacity(tenant_names.len());
            for tenant_name in tenant_names {
                let (
                    normal_model_count,
                    normal_catalog_model_count,
                    total_catalog_model_count,
                    total_model_count,
                ) = match model_counts.remove(&tenant_name) {
                    Some((normal, normal_catalog, total_catalog, total)) => {
                        (normal, normal_catalog, total_catalog, total)
                    }
                    None => (0, 0, 0, 0),
                };
                let (used_cents, balance_cents) = match billing.remove(&tenant_name) {
                    Some(summary) => (summary.used_cents, summary.balance_cents),
                    None => (0, 0),
                };
                match profiles.remove(&tenant_name) {
                    Some(profile) => rows.push(AdminTenantProfileRow {
                        tenant_name,
                        sub: Some(profile.sub),
                        display_name: profile.display_name,
                        email: Some(profile.email),
                        normal_model_count,
                        normal_catalog_model_count,
                        total_catalog_model_count,
                        total_model_count,
                        used_cents,
                        balance_cents,
                        created_at: profile.created_at,
                    }),
                    None => rows.push(AdminTenantProfileRow {
                        tenant_name,
                        sub: None,
                        display_name: None,
                        email: None,
                        normal_model_count,
                        normal_catalog_model_count,
                        total_catalog_model_count,
                        total_model_count,
                        used_cents,
                        balance_cents,
                        created_at: None,
                    }),
                }
            }

            let body = Body::from(serde_json::to_string(&rows).unwrap());
            let resp = Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, "application/json")
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

fn is_apikey_duplicate_keyname_error(err: &Error) -> bool {
    match err {
        Error::SqlxError(sqlx::Error::Database(db_err)) => {
            let is_unique_violation = db_err.code().as_deref() == Some("23505");
            if !is_unique_violation {
                return false;
            }

            if matches!(
                db_err.constraint(),
                Some("apikey_idx_username_keyname") | Some("apikey_idx_realm_username")
            ) {
                return true;
            }

            // Fallback for migrated environments where index names may differ.
            db_err
                .message()
                .contains("duplicate key value violates unique constraint")
        }
        _ => false,
    }
}

async fn GetApikeys(
    Extension(token): Extension<Arc<AccessToken>>,
) -> SResult<Response, StatusCode> {
    let username = token.username.clone();
    match GetTokenCache().await.GetApikeys(&username).await {
        Ok(keys) => {
            let data = serde_json::to_string(&keys).unwrap();
            let body = Body::from(format!("{}", data));
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn DeleteApikey(
    Extension(token): Extension<Arc<AccessToken>>,
    Json(apikey): Json<ApikeyDeleteRequest>,
) -> SResult<Response, StatusCode> {
    error!("DeleteApikey *** {:?}", &apikey);
    match GetTokenCache().await.DeleteApiKey(&token, &apikey).await {
        Ok(exist) => {
            if exist {
                let body = Body::from(format!("deleted key '{}'", apikey.keyname));
                let resp = Response::builder()
                    .status(StatusCode::OK)
                    .body(body)
                    .unwrap();
                return Ok(resp);
            } else {
                let body = Body::from(format!("apikey {:?} not exist ", apikey.keyname));
                let resp = Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body(body)
                    .unwrap();
                return Ok(resp);
            }
        }
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn CreateObj(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Json(obj): Json<DataObject<Value>>,
) -> SResult<Response, StatusCode> {
    let dataobj = obj;

    error!("CreateObj obj is {:#?}", &dataobj);
    let tenant_target = if dataobj.objType.as_str() == Tenant::KEY {
        dataobj.name.as_str()
    } else {
        dataobj.tenant.as_str()
    };
    if GATEWAY_CONFIG.enforceBilling {
        if let Some(resp) = enforce_tenant_quota_for_write(&token, &gw, tenant_target) {
            return Ok(resp);
        }
    }
    if dataobj.objType.as_str() == Tenant::KEY && !token.IsInferxAdmin() {
        let body = Body::from("service failure: No permission");
        let resp = Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    let res = match dataobj.objType.as_str() {
        Tenant::KEY => gw.CreateTenant(&token, dataobj).await,
        Namespace::KEY => gw.CreateNamespace(&token, dataobj).await,
        Function::KEY => gw.CreateFunc(&token, dataobj).await,
        FuncPolicy::KEY => gw.CreateFuncPolicy(&token, dataobj).await,
        _ => gw.client.Create(&dataobj).await,
    };

    match res {
        Ok(version) => {
            let body = Body::from(format!("{}", version));
            let resp = Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, "text/plain")
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn SaveEndpointMetadata(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path(slug): Path<String>,
    Json(metadata): Json<EndpointMetadata>,
) -> SResult<Response, StatusCode> {
    match gw.SaveEndpointMetadata(&token, &slug, &metadata).await {
        Ok(version) => {
            let body = Body::from(format!("{}", version));
            let resp = Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, "text/plain")
                .body(body)
                .unwrap();
            Ok(resp)
        }
        Err(e) => Ok(error_response_for_endpoint_admin_action(e)),
    }
}

async fn PublishEndpoint(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path(slug): Path<String>,
    Json(metadata): Json<EndpointMetadata>,
) -> SResult<Response, StatusCode> {
    match gw.PublishEndpoint(&token, &slug, &metadata).await {
        Ok(version) => {
            let body = Body::from(format!("{}", version));
            let resp = Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, "text/plain")
                .body(body)
                .unwrap();
            Ok(resp)
        }
        Err(e) => Ok(error_response_for_endpoint_admin_action(e)),
    }
}

async fn UnpublishEndpoint(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path(slug): Path<String>,
) -> SResult<Response, StatusCode> {
    match gw.UnpublishEndpoint(&token, &slug).await {
        Ok(version) => {
            let body = Body::from(format!("{}", version));
            let resp = Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, "text/plain")
                .body(body)
                .unwrap();
            Ok(resp)
        }
        Err(e) => Ok(error_response_for_endpoint_admin_action(e)),
    }
}

fn endpoint_admin_ok_response() -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/plain")
        .body(Body::from("ok"))
        .unwrap()
}

async fn SaveEndpointOpenRouterMetadata(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path(slug): Path<String>,
    Json(metadata): Json<EndpointOpenRouterMetadata>,
) -> SResult<Response, StatusCode> {
    match gw
        .SaveEndpointOpenRouterMetadata(&token, &slug, &metadata)
        .await
    {
        // Non-blocking slug warnings (HF-mismatch / `:free`) ride back in the body so
        // the caller can surface them and gate the auto-list on an explicit acknowledge.
        Ok(warnings) => {
            let body = serde_json::json!({ "saved": true, "warnings": warnings });
            Ok(Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap_or_default()))
                .unwrap())
        }
        Err(e) => Ok(error_response_for_endpoint_admin_action(e)),
    }
}

#[derive(serde::Deserialize, Default)]
struct SuggestSlugRequest {
    #[serde(default)]
    hugging_face_id: Option<String>,
}

/// Suggestion/auto path exposed for the editor's "Suggest from HF ID" action. Takes
/// the HF id as a request parameter (from the live form, NOT the DB row — a first
/// listing has no stored HF id). Best-effort: returns the resolved slug or null; never
/// errors, so the dialog degrades to manual entry on no-match / ambiguity / fetch fail.
async fn SuggestEndpointOpenRouterSlug(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path(slug): Path<String>,
    Json(req): Json<SuggestSlugRequest>,
) -> SResult<Response, StatusCode> {
    let _ = slug;
    if let Err(e) = gw.EnsurePlatformEndpointAdmin(&token) {
        return Ok(error_response_for_endpoint_admin_action(e));
    }
    let hf_id = req.hugging_face_id.unwrap_or_default();
    let suggestion = gw.suggest_openrouter_slug(&hf_id).await;
    let body = serde_json::json!({ "openrouter_slug": suggestion });
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap_or_default()))
        .unwrap())
}

async fn ListOnOpenRouter(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path(slug): Path<String>,
) -> SResult<Response, StatusCode> {
    match gw.ListOnOpenRouter(&token, &slug).await {
        Ok(()) => Ok(endpoint_admin_ok_response()),
        Err(e) => Ok(error_response_for_endpoint_admin_action(e)),
    }
}

#[derive(serde::Deserialize, Default)]
struct TakeOfflineRequest {
    #[serde(default)]
    deprecation_date: Option<String>,
}

async fn TakeOfflineOnOpenRouter(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path(slug): Path<String>,
    Json(req): Json<TakeOfflineRequest>,
) -> SResult<Response, StatusCode> {
    let deprecation_date = match req.deprecation_date.as_deref().map(str::trim) {
        Some(s) if !s.is_empty() => {
            match chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
                Ok(d) => Some(d),
                Err(_) => {
                    return Ok(error_response_for_endpoint_admin_action(Error::CommonError(
                        format!("invalid deprecation_date {:?}; expected YYYY-MM-DD", s),
                    )));
                }
            }
        }
        _ => None,
    };
    match gw
        .TakeOfflineOnOpenRouter(&token, &slug, deprecation_date)
        .await
    {
        Ok(()) => Ok(endpoint_admin_ok_response()),
        Err(e) => Ok(error_response_for_endpoint_admin_action(e)),
    }
}

async fn UnlistFromOpenRouter(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path(slug): Path<String>,
) -> SResult<Response, StatusCode> {
    match gw.UnlistFromOpenRouter(&token, &slug).await {
        Ok(()) => Ok(endpoint_admin_ok_response()),
        Err(e) => Ok(error_response_for_endpoint_admin_action(e)),
    }
}

/// Flat provider catalog. Emits the OpenRouter-required schema for every endpoint
/// with `or_listed = true`, read from Postgres (not etcd). Requires a key with
/// inference access to `inferx/endpoint`. `or_listed` is the source of truth: rows
/// are emitted verbatim and only leave the catalog through the offline/delist
/// lifecycle — `ListOnOpenRouter` and the save guard keep a
/// listed row valid, so the read path does not silently drop entries.
async fn OpenRouterModels(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
) -> SResult<Response, StatusCode> {
    if !token.IsNamespaceInferenceUser(PLATFORM_TENANT, PLATFORM_SHARED_NAMESPACE) {
        return Ok(Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(Body::from("unauthorized"))
            .unwrap());
    }

    let rows = match gw.sqlSecret.ListListedEndpoints().await {
        Ok(rows) => rows,
        Err(e) => {
            return Ok(Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from(format!("service failure: list models failed {:?}", e)))
                .unwrap());
        }
    };

    let data: Vec<Value> = rows.iter().map(BuildOpenRouterModelEntry).collect();

    let response_body = serde_json::json!({ "data": data });
    let bytes = match serde_json::to_vec(&response_body) {
        Ok(b) => b,
        Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(bytes))
        .unwrap())
}

/// Root OpenRouter inference entrypoint. Pulls `model` from the JSON body, gates on
/// `or_listed`, and rewrites to `/funccall/inferx/endpoint/{model}{remainPath}` before
/// delegating to `FuncCall1`.
async fn OpenRouterChatCompletions(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    req: Request,
) -> SResult<Response, StatusCode> {
    let remainPath = req.uri().path().to_string();

    let (mut reqParts, body) = req.into_parts();
    reqParts.headers.remove(hyper::header::CONTENT_LENGTH);
    let bytes = match axum::body::to_bytes(body, FUNCCALL_MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(_) => return Err(StatusCode::BAD_REQUEST),
    };

    let mut jsonReq: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| StatusCode::BAD_REQUEST)?;
    let modelName = jsonReq
        .as_object_mut()
        .and_then(|obj| obj.remove("model"))
        .and_then(|val| val.as_str().map(|s| s.to_string()))
        .ok_or(StatusCode::BAD_REQUEST)?;

    let bytes = serde_json::to_vec(&jsonReq).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Serve only `or_listed` endpoints (regardless of `published`).
    let or_listed = match gw.sqlSecret.GetEndpointForListing(&modelName).await {
        Ok(Some(row)) => row.or_listed,
        _ => false,
    };
    if !or_listed {
        let body = Body::from("service failure: endpoint not found");
        let resp = Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    // Route to the physical `inferx/endpoint` namespace so RBAC matches a provider key
    // restricted to it.
    let finalPath = format!(
        "/funccall/{}/{}/{}{}",
        PLATFORM_TENANT, PLATFORM_SHARED_NAMESPACE, modelName, remainPath
    );

    let mut newReq = Request::from_parts(reqParts, Body::from(bytes));
    *newReq.uri_mut() = Uri::try_from(finalPath).unwrap();
    newReq
        .headers_mut()
        .insert("X-Inferx-Model", HeaderValue::from_str(&modelName).unwrap());
    newReq
        .headers_mut()
        .insert("X-Inferx-Model-Call", HeaderValue::from_static("true"));

    FuncCall1(&token, &gw, newReq).await
}

fn error_response_for_endpoint_admin_action(err: Error) -> Response {
    let status = match &err {
        Error::NoPermission => StatusCode::UNAUTHORIZED,
        _ => StatusCode::BAD_REQUEST,
    };

    Response::builder()
        .status(status)
        .body(Body::from(format!("service failure {:?}", err)))
        .unwrap()
}

async fn UpdateObj(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Json(obj): Json<DataObject<Value>>,
) -> SResult<Response, StatusCode> {
    let dataobj = obj;
    let tenant_target = if dataobj.objType.as_str() == Tenant::KEY {
        dataobj.name.as_str()
    } else {
        dataobj.tenant.as_str()
    };
    if GATEWAY_CONFIG.enforceBilling {
        if let Some(resp) = enforce_tenant_quota_for_write(&token, &gw, tenant_target) {
            return Ok(resp);
        }
    }

    let res = match dataobj.objType.as_str() {
        Tenant::KEY => gw.UpdateTenant(&token, dataobj).await,
        Namespace::KEY => gw.UpdateNamespace(&token, dataobj).await,
        Function::KEY => gw.UpdateFunc(&token, dataobj).await,
        FuncPolicy::KEY => gw.UpdateFuncPolicy(&token, dataobj).await,
        _ => gw.client.Update(&dataobj, 0).await,
    };

    match res {
        Ok(version) => {
            let body = Body::from(format!("{}", version));
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn DeleteObj(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path((objType, tenant, namespace, name)): Path<(String, String, String, String)>,
) -> SResult<Response, StatusCode> {
    let res = match objType.as_str() {
        Tenant::KEY => gw.DeleteTenant(&token, &tenant, &namespace, &name).await,
        Namespace::KEY => gw.DeleteNamespace(&token, &tenant, &namespace, &name).await,
        Function::KEY => gw.DeleteFunc(&token, &tenant, &namespace, &name).await,
        FuncPolicy::KEY => {
            gw.DeleteFuncPolicy(&token, &tenant, &namespace, &name)
                .await
        }
        _ => {
            gw.client
                .Delete(&objType, &tenant, &namespace, &name, 0)
                .await
        }
    };

    match res {
        Ok(version) => {
            let body = Body::from(format!("{}", version));
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn GetObj(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path((objType, tenant, namespace, name)): Path<(String, String, String, String)>,
) -> SResult<Response, StatusCode> {
    let has_permission = match objType.as_str() {
        // Tenant objects are stored as /object/tenant/system/system/<tenant-name>/.
        Tenant::KEY => token.IsInferxAdmin() || token.IsTenantAdmin(&name),
        // Namespace objects are stored as /object/namespace/<tenant>/system/<namespace-name>/.
        Namespace::KEY => token.IsNamespaceUser(&tenant, &name),
        // Most other objects are scoped by <tenant>/<namespace>.
        _ => token.IsNamespaceUser(&tenant, &namespace),
    };

    if !has_permission {
        let body = Body::from("service failure: No permission");
        let resp = Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    match gw.client.Get(&objType, &tenant, &namespace, &name, 0).await {
        Ok(obj) => match obj {
            None => {
                let body = Body::from(format!("NOT_FOUND"));
                let resp = Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(body)
                    .unwrap();
                return Ok(resp);
            }
            Some(obj) => {
                let data = serde_json::to_string(&obj).unwrap();
                let body = Body::from(format!("{}", data));
                let resp = Response::builder()
                    .status(StatusCode::OK)
                    .body(body)
                    .unwrap();
                return Ok(resp);
            }
        },
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn ListObj(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path((objType, tenant, namespace)): Path<(String, String, String)>,
) -> SResult<Response, StatusCode> {
    match gw.ListObj(&token, &objType, &tenant, &namespace).await {
        Ok(list) => {
            let data = serde_json::to_string(&list).unwrap();
            let body = Body::from(format!("{}", data));
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn GetSnapshots(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path((tenant, namespace)): Path<(String, String)>,
) -> SResult<Response, StatusCode> {
    match gw.GetSnapshots(&token, &tenant, &namespace) {
        Ok(list) => {
            let data = serde_json::to_string_pretty(&list).unwrap();
            let body = Body::from(format!("{}", data));
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn GetSnapshot(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path((tenant, namespace, name)): Path<(String, String, String)>,
) -> SResult<Response, StatusCode> {
    match gw.GetSnapshot(&token, &tenant, &namespace, &name) {
        Ok(list) => {
            let data = serde_json::to_string_pretty(&list).unwrap();
            let body = Body::from(format!("{}", data));
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

// Request body for adding tenant credits
#[derive(Deserialize)]
struct AddCreditsRequest {
    amount_cents: i64,
    currency: Option<String>,
    note: Option<String>,
    payment_ref: Option<String>,
}

#[derive(Deserialize)]
struct AddBillingRateRequest {
    usage_type: String,
    rate_cents_per_hour: i32,
    effective_from: Option<String>,
    effective_to: Option<String>,
    tenant: Option<String>,
}

#[derive(Deserialize)]
struct SetTenantQuotaExceededRequest {
    quota_exceeded: bool,
}

#[derive(Deserialize)]
struct CreditHistoryQuery {
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Deserialize)]
struct BillingRateHistoryQuery {
    limit: Option<i64>,
    offset: Option<i64>,
    scope: Option<String>,  // all | global | tenant
    tenant: Option<String>, // used when scope=tenant
}

#[derive(Deserialize)]
struct BillingCreditHistoryQuery {
    limit: Option<i64>,
    offset: Option<i64>,
    tenant: Option<String>,
}

// Response for credit operations
#[derive(Serialize)]
struct CreditResponse {
    balance_cents: i64,
    used_cents: i64,
    currency: String,
    quota_exceeded: bool,
}

#[derive(Serialize)]
struct CreditHistoryResponse {
    records: Vec<TenantCreditHistoryRecord>,
    total: i64,
    balance_cents: i64,
}

#[derive(Serialize)]
struct AddCreditsResponse {
    success: bool,
    credit_id: i64,
    new_balance_cents: i64,
}

#[derive(Serialize)]
struct AddBillingRateResponse {
    success: bool,
    rate_id: i64,
    usage_type: String,
    rate_cents_per_hour: i32,
    effective_from: String,
    effective_to: Option<String>,
    tenant: Option<String>,
}

#[derive(Serialize)]
struct BillingRateHistoryResponse {
    records: Vec<BillingRateHistoryRecord>,
    total: i64,
}

#[derive(Serialize)]
struct BillingCreditHistoryResponse {
    records: Vec<BillingCreditHistoryRecord>,
    total: i64,
}

#[derive(Serialize)]
struct BillingSummaryPeriod {
    inference_cents: i64,
    standby_cents: i64,
    inference_hours: f64,
    standby_hours: f64,
}

#[derive(Serialize)]
struct BillingSummaryResponse {
    balance_cents: i64,
    used_cents: i64,
    threshold_cents: i64,
    quota_exceeded: bool,
    quota_exempt: bool,
    total_credits_cents: i64,
    currency: String,
    period: BillingSummaryPeriod,
}

#[derive(Serialize)]
struct HourlyUsageRecord {
    hour: String,
    charge_cents: i64,
    inference_cents: i64,
    standby_cents: i64,
    inference_ms: i64,
    standby_ms: i64,
    inference_numer: i64,
    standby_numer: i64,
}

#[derive(Serialize)]
struct HourlyUsageResponse {
    usage: Vec<HourlyUsageRecord>,
    inference_ms: i64,
    standby_ms: i64,
    total_ms: i64,
    inference_cents: i64,
    standby_cents: i64,
    total_cents: i64,
    timezone: String,
}

#[derive(Deserialize)]
struct HourlyUsageQuery {
    hours: Option<i32>,
    start: Option<String>,
    end: Option<String>,
}

#[derive(Deserialize)]
struct UsageByGroupQuery {
    hours: Option<i32>,
    start: Option<String>,
    end: Option<String>,
    limit: Option<i32>,
}

#[derive(Deserialize)]
struct HourlyUsageByNamespaceQuery {
    hours: Option<i32>,
    start: Option<String>,
    end: Option<String>,
    namespace: Option<String>,
}

#[derive(Deserialize)]
struct HourlyUsageByModelQuery {
    hours: Option<i32>,
    start: Option<String>,
    end: Option<String>,
    namespace: Option<String>,
    funcname: Option<String>,
}

#[derive(Deserialize)]
struct AdminEndpointUsageQuery {
    hours: Option<i32>,
    start: Option<String>,
    end: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
    group_by: Option<String>,
    tenant: Option<String>,
}

#[derive(Deserialize)]
struct AdminEndpointUsageByPeriodQuery {
    hours: Option<i32>,
    start: Option<String>,
    end: Option<String>,
    granularity: Option<String>,
}

fn BadRequest(msg: impl Into<String>) -> Response {
    Response::builder()
        .status(StatusCode::BAD_REQUEST)
        .body(Body::from(msg.into()))
        .unwrap()
}

fn ParseAdminEndpointRange(
    hours: Option<i32>,
    start: Option<String>,
    end: Option<String>,
) -> std::result::Result<(DateTime<Utc>, DateTime<Utc>, i32), Response> {
    if start.is_some() || end.is_some() {
        let start_str = match start {
            Some(s) => s,
            None => return Err(BadRequest("Missing start parameter")),
        };
        let end_str = match end {
            Some(s) => s,
            None => return Err(BadRequest("Missing end parameter")),
        };

        let start = match DateTime::parse_from_rfc3339(&start_str) {
            Ok(v) => v.with_timezone(&Utc),
            Err(_) => return Err(BadRequest("Invalid start format (use RFC3339)")),
        };
        let end = match DateTime::parse_from_rfc3339(&end_str) {
            Ok(v) => v.with_timezone(&Utc),
            Err(_) => return Err(BadRequest("Invalid end format (use RFC3339)")),
        };

        if end <= start {
            return Err(BadRequest("Invalid range: end must be after start"));
        }

        let max_range = chrono::Duration::hours(720);
        if end - start > max_range {
            return Err(BadRequest("Range too large (max 720 hours / 30 days)"));
        }

        let start_hour_naive = start.date_naive().and_hms_opt(start.hour(), 0, 0).unwrap();
        let end_hour_naive = end.date_naive().and_hms_opt(end.hour(), 0, 0).unwrap();
        let start_hour = DateTime::<Utc>::from_naive_utc_and_offset(start_hour_naive, Utc);
        let end_hour = DateTime::<Utc>::from_naive_utc_and_offset(end_hour_naive, Utc);
        let total_hours = ((end_hour - start_hour).num_hours() + 1).max(1) as i32;
        return Ok((start_hour, end_hour, total_hours));
    }

    let hours = hours.unwrap_or(24).min(720).max(1);
    let now = Utc::now();
    let end_hour_naive = now.date_naive().and_hms_opt(now.hour(), 0, 0).unwrap();
    let end_hour = DateTime::<Utc>::from_naive_utc_and_offset(end_hour_naive, Utc);
    let start_hour = end_hour - chrono::Duration::hours((hours - 1) as i64);
    Ok((start_hour, end_hour, hours))
}

fn ParseHourlyRange(
    hours: Option<i32>,
    start: Option<String>,
    end: Option<String>,
) -> std::result::Result<(DateTime<Utc>, DateTime<Utc>, i32), Response> {
    if start.is_some() || end.is_some() {
        let start_str = match start {
            Some(s) => s,
            None => return Err(BadRequest("Missing start parameter")),
        };
        let end_str = match end {
            Some(s) => s,
            None => return Err(BadRequest("Missing end parameter")),
        };

        let start = match DateTime::parse_from_rfc3339(&start_str) {
            Ok(v) => v.with_timezone(&Utc),
            Err(_) => return Err(BadRequest("Invalid start format (use RFC3339)")),
        };
        let end = match DateTime::parse_from_rfc3339(&end_str) {
            Ok(v) => v.with_timezone(&Utc),
            Err(_) => return Err(BadRequest("Invalid end format (use RFC3339)")),
        };

        if end <= start {
            return Err(BadRequest("Invalid range: end must be after start"));
        }

        let max_range = chrono::Duration::days(90);
        if end - start > max_range {
            return Err(BadRequest("Range too large (max 90 days)"));
        }

        let start_hour_naive = start.date_naive().and_hms_opt(start.hour(), 0, 0).unwrap();
        let end_hour_naive = end.date_naive().and_hms_opt(end.hour(), 0, 0).unwrap();
        let start_hour = DateTime::<Utc>::from_naive_utc_and_offset(start_hour_naive, Utc);
        let end_hour = DateTime::<Utc>::from_naive_utc_and_offset(end_hour_naive, Utc);
        let total_hours = ((end_hour - start_hour).num_hours() + 1).max(1) as i32;
        return Ok((start_hour, end_hour, total_hours));
    }

    let hours = hours.unwrap_or(24).min(720).max(1); // 1 to 720 hours (30 days max)
    let now = Utc::now();
    let end_hour_naive = now.date_naive().and_hms_opt(now.hour(), 0, 0).unwrap();
    let end_hour = DateTime::<Utc>::from_naive_utc_and_offset(end_hour_naive, Utc);
    let start_hour = end_hour - chrono::Duration::hours((hours - 1) as i64);
    Ok((start_hour, end_hour, hours))
}

fn BuildHourlyUsage(
    usage_records: Vec<(DateTime<Utc>, i64, i64, i64, i64, i64, i64, i64)>,
    start_hour: DateTime<Utc>,
    total_hours: i32,
) -> HourlyUsageResponse {
    let mut usage_map: std::collections::HashMap<String, HourlyUsageRecord> =
        std::collections::HashMap::new();
    let mut total_inference_ms: i64 = 0;
    let mut total_standby_ms: i64 = 0;
    let mut total_inference_numer: i64 = 0;
    let mut total_standby_numer: i64 = 0;

    for (
        hour,
        charge_cents,
        inference_cents,
        standby_cents,
        inference_ms,
        standby_ms,
        inference_numer,
        standby_numer,
    ) in usage_records
    {
        let hour_key = hour.format("%Y-%m-%dT%H:00:00Z").to_string();
        usage_map.insert(
            hour_key.clone(),
            HourlyUsageRecord {
                hour: hour_key,
                charge_cents,
                inference_cents,
                standby_cents,
                inference_ms,
                standby_ms,
                inference_numer,
                standby_numer,
            },
        );
        total_inference_ms += inference_ms;
        total_standby_ms += standby_ms;
        total_inference_numer += inference_numer;
        total_standby_numer += standby_numer;
    }

    let mut usage: Vec<HourlyUsageRecord> = Vec::with_capacity(total_hours as usize);
    for i in 0..total_hours {
        let hour = start_hour + chrono::Duration::hours(i as i64);
        let hour_key = hour.format("%Y-%m-%dT%H:00:00Z").to_string();
        usage.push(usage_map.remove(&hour_key).unwrap_or(HourlyUsageRecord {
            hour: hour_key,
            charge_cents: 0,
            inference_cents: 0,
            standby_cents: 0,
            inference_ms: 0,
            standby_ms: 0,
            inference_numer: 0,
            standby_numer: 0,
        }));
    }

    let total_ms = total_inference_ms + total_standby_ms;
    HourlyUsageResponse {
        usage,
        inference_ms: total_inference_ms,
        standby_ms: total_standby_ms,
        total_ms,
        inference_cents: total_inference_numer / 3600000,
        standby_cents: total_standby_numer / 3600000,
        total_cents: (total_inference_numer + total_standby_numer) / 3600000,
        timezone: "UTC".to_string(),
    }
}

#[derive(Serialize)]
struct ModelUsageItem {
    funcname: String,
    namespace: String,
    inference_ms: i64,
    standby_ms: i64,
    usage_ms: i64,
    inference_gpu_hours: f64,
    standby_gpu_hours: f64,
    gpu_hours: f64,
    inference_cents: i64,
    standby_cents: i64,
    charge_cents: i64,
}

#[derive(Serialize)]
struct NamespaceUsageItem {
    namespace: String,
    inference_ms: i64,
    standby_ms: i64,
    usage_ms: i64,
    inference_gpu_hours: f64,
    standby_gpu_hours: f64,
    gpu_hours: f64,
    inference_cents: i64,
    standby_cents: i64,
    charge_cents: i64,
}

#[derive(Serialize)]
struct OtherBucket {
    count: i64,
    inference_ms: i64,
    standby_ms: i64,
    usage_ms: i64,
    inference_gpu_hours: f64,
    standby_gpu_hours: f64,
    gpu_hours: f64,
    inference_cents: i64,
    standby_cents: i64,
    charge_cents: i64,
}

#[derive(Serialize)]
struct UsageByModelResponse {
    usage: Vec<ModelUsageItem>,
    other: OtherBucket,
    inference_ms: i64,
    standby_ms: i64,
    total_ms: i64,
    inference_gpu_hours: f64,
    standby_gpu_hours: f64,
    total_gpu_hours: f64,
    inference_cents: i64,
    standby_cents: i64,
    total_cents: i64,
    timezone: String,
}

#[derive(Serialize)]
struct UsageByNamespaceResponse {
    usage: Vec<NamespaceUsageItem>,
    other: OtherBucket,
    inference_ms: i64,
    standby_ms: i64,
    total_ms: i64,
    inference_gpu_hours: f64,
    standby_gpu_hours: f64,
    total_gpu_hours: f64,
    inference_cents: i64,
    standby_cents: i64,
    total_cents: i64,
    timezone: String,
}

#[derive(Serialize)]
struct TopModelInfo {
    funcname: String,
    gpu_hours: f64,
}

#[derive(Serialize)]
struct TopNamespaceInfo {
    namespace: String,
    gpu_hours: f64,
}

#[derive(Serialize)]
struct PeakHourInfo {
    hour: String,
    gpu_hours: f64,
}

#[derive(Serialize)]
struct UsageSummaryResponse {
    total_gpu_hours: f64,
    total_cents: i64,
    top_model: Option<TopModelInfo>,
    top_namespace: Option<TopNamespaceInfo>,
    peak_hour: Option<PeakHourInfo>,
    timezone: String,
}

#[derive(Serialize)]
struct SetTenantQuotaExceededResponse {
    success: bool,
    tenant: String,
    quota_exceeded: bool,
    revision: i64,
}

const TENANT_QUOTA_UPDATE_MAX_RETRIES: usize = 5;

async fn SetTenantQuotaExceeded(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path(tenant): Path<String>,
    Json(req): Json<SetTenantQuotaExceededRequest>,
) -> SResult<Response, StatusCode> {
    if !token.IsInferxAdmin() {
        let body =
            Body::from("Permission denied: Only InferxAdmin can update tenant quota_exceeded");
        let resp = Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    for attempt in 0..TENANT_QUOTA_UPDATE_MAX_RETRIES {
        let tenant_obj = match gw
            .client
            .Get(Tenant::KEY, SYSTEM_TENANT, SYSTEM_NAMESPACE, &tenant, 0)
            .await
        {
            Ok(Some(obj)) => obj,
            Ok(None) => {
                let body = Body::from(format!("Tenant {} not found", tenant));
                let resp = Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(body)
                    .unwrap();
                return Ok(resp);
            }
            Err(e) => {
                let body = Body::from(format!("Failed to read tenant object: {:?}", e));
                let resp = Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body(body)
                    .unwrap();
                return Ok(resp);
            }
        };

        let mut tenant_obj: Tenant = match Tenant::FromDataObject(tenant_obj) {
            Ok(obj) => obj,
            Err(e) => {
                let body = Body::from(format!("Failed to parse tenant object: {:?}", e));
                let resp = Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body(body)
                    .unwrap();
                return Ok(resp);
            }
        };

        if tenant_obj.object.status.quota_exceeded == req.quota_exceeded {
            let resp_body = SetTenantQuotaExceededResponse {
                success: true,
                tenant: tenant.clone(),
                quota_exceeded: req.quota_exceeded,
                revision: tenant_obj.revision,
            };
            let data = serde_json::to_string(&resp_body).unwrap();
            let body = Body::from(data);
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }

        let expect_rev = tenant_obj.revision;
        if expect_rev <= 0 {
            let body = Body::from(format!(
                "invalid tenant revision {} for {}",
                expect_rev, tenant
            ));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }

        tenant_obj.object.status.quota_exceeded = req.quota_exceeded;
        match gw.client.Update(&tenant_obj.DataObject(), expect_rev).await {
            Ok(version) => {
                let resp_body = SetTenantQuotaExceededResponse {
                    success: true,
                    tenant: tenant.clone(),
                    quota_exceeded: req.quota_exceeded,
                    revision: version,
                };
                let data = serde_json::to_string(&resp_body).unwrap();
                let body = Body::from(data);
                let resp = Response::builder()
                    .status(StatusCode::OK)
                    .body(body)
                    .unwrap();
                return Ok(resp);
            }
            Err(Error::UpdateRevNotMatchErr(e)) => {
                if attempt + 1 == TENANT_QUOTA_UPDATE_MAX_RETRIES {
                    let body = Body::from(format!(
                        "tenant {} update conflicted after {} retries (expected_rev={}, actual_rev={})",
                        tenant, TENANT_QUOTA_UPDATE_MAX_RETRIES, e.expectRv, e.actualRv
                    ));
                    let resp = Response::builder()
                        .status(StatusCode::CONFLICT)
                        .body(body)
                        .unwrap();
                    return Ok(resp);
                }
            }
            Err(e) => {
                let body = Body::from(format!("Failed to update tenant quota_exceeded: {:?}", e));
                let resp = Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body(body)
                    .unwrap();
                return Ok(resp);
            }
        }
    }

    let body = Body::from("tenant update retried but was not applied");
    let resp = Response::builder()
        .status(StatusCode::CONFLICT)
        .body(body)
        .unwrap();
    Ok(resp)
}

async fn AddBillingRate(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Json(req): Json<AddBillingRateRequest>,
) -> SResult<Response, StatusCode> {
    if !token.IsInferxAdmin() {
        let body = Body::from("Permission denied: Only InferxAdmin can add billing rates");
        let resp = Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    let usage_type = match req.usage_type.trim().to_ascii_lowercase().as_str() {
        "standby" => "standby",
        "inference" | "request" | "snapshot" => "inference",
        other => {
            let body = Body::from(format!(
                "Unsupported usage_type: {}. Expected one of: inference, standby",
                other
            ));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    };

    if req.rate_cents_per_hour < 0 {
        let body = Body::from("rate_cents_per_hour must be >= 0");
        let resp = Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    let effective_from = match req.effective_from.as_deref() {
        Some(v) => match DateTime::parse_from_rfc3339(v) {
            Ok(dt) => dt.with_timezone(&Utc),
            Err(_) => {
                let body = Body::from("Invalid effective_from format (use RFC3339)");
                let resp = Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body(body)
                    .unwrap();
                return Ok(resp);
            }
        },
        None => Utc::now(),
    };

    let effective_to = match req.effective_to.as_deref() {
        Some(v) => match DateTime::parse_from_rfc3339(v) {
            Ok(dt) => Some(dt.with_timezone(&Utc)),
            Err(_) => {
                let body = Body::from("Invalid effective_to format (use RFC3339)");
                let resp = Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body(body)
                    .unwrap();
                return Ok(resp);
            }
        },
        None => None,
    };

    if let Some(to) = effective_to.as_ref() {
        if *to <= effective_from {
            let body = Body::from("Invalid range: effective_to must be after effective_from");
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }

    let tenant = req
        .tenant
        .as_ref()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    let added_by = Some(token.username.as_str());

    let effective_to_resp = effective_to.as_ref().map(|v| v.to_rfc3339());

    error!(
        "AddBillingRate: usage_type={}, rate_cents_per_hour={}, effective_from={}, effective_to={:?}, tenant={:?}, added_by={:?}",
        usage_type,
        req.rate_cents_per_hour,
        effective_from.to_rfc3339(),
        effective_to_resp,
        tenant,
        added_by
    );

    match gw
        .sqlBilling
        .AddBillingRate(
            usage_type,
            req.rate_cents_per_hour,
            effective_from,
            effective_to,
            tenant.as_deref(),
            added_by,
        )
        .await
    {
        Ok(id) => {
            let resp_body = AddBillingRateResponse {
                success: true,
                rate_id: id,
                usage_type: usage_type.to_string(),
                rate_cents_per_hour: req.rate_cents_per_hour,
                effective_from: effective_from.to_rfc3339(),
                effective_to: effective_to_resp,
                tenant,
            };
            let data = serde_json::to_string(&resp_body).unwrap();
            let body = Body::from(data);
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("Failed to add billing rate: {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn AddTenantCredits(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path(tenant): Path<String>,
    Json(req): Json<AddCreditsRequest>,
) -> SResult<Response, StatusCode> {
    if !token.IsInferxAdmin() {
        let body = Body::from("Permission denied: Only InferxAdmin can add credits");
        let resp = Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    // Validate currency (only USD supported for now)
    let currency = req.currency.as_deref().unwrap_or("USD");
    if currency != "USD" {
        let body = Body::from(format!(
            "Unsupported currency: {}. Only USD is supported.",
            currency
        ));
        let resp = Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    if req.amount_cents == 0 {
        let body = Body::from("amount_cents must be non-zero");
        let resp = Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    // Add credits to tenant
    let added_by = Some(token.username.as_str());
    error!(
        "AddTenantCredits: tenant={}, amount_cents={}, currency={}, note={:?}, payment_ref={:?}, added_by={:?}",
        &tenant, req.amount_cents, currency, req.note, req.payment_ref, added_by
    );
    match gw
        .sqlBilling
        .AddTenantCredit(
            &tenant,
            req.amount_cents,
            currency,
            req.note.as_deref(),
            req.payment_ref.as_deref(),
            added_by,
        )
        .await
    {
        Ok(id) => {
            error!("AddTenantCredits: credit added with id={}", id);
            let quota_exceeded = match gw.sqlBilling.RecalculateTenantQuota(&tenant).await {
                Ok(v) => v,
                Err(e) => {
                    let body = Body::from(format!("Failed to recalc tenant quota: {:?}", e));
                    let resp = Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(body)
                        .unwrap();
                    return Ok(resp);
                }
            };

            let tenant_obj = match gw
                .client
                .Get(Tenant::KEY, SYSTEM_TENANT, SYSTEM_NAMESPACE, &tenant, 0)
                .await
            {
                Ok(obj) => obj,
                Err(e) => {
                    let body = Body::from(format!("Failed to read tenant object: {:?}", e));
                    let resp = Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(body)
                        .unwrap();
                    return Ok(resp);
                }
            };

            let tenant_obj = match tenant_obj {
                Some(obj) => obj,
                None => {
                    let body = Body::from(format!("Tenant {} not found", tenant));
                    let resp = Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(body)
                        .unwrap();
                    return Ok(resp);
                }
            };

            let mut tenant_obj: Tenant = match Tenant::FromDataObject(tenant_obj) {
                Ok(obj) => obj,
                Err(e) => {
                    let body = Body::from(format!("Failed to parse tenant object: {:?}", e));
                    let resp = Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(body)
                        .unwrap();
                    return Ok(resp);
                }
            };

            error!(
                "AddTenantCredits: tenant={}, old_quota_exceeded={}, new_quota_exceeded={}",
                &tenant, tenant_obj.object.status.quota_exceeded, quota_exceeded
            );
            if tenant_obj.object.status.quota_exceeded != quota_exceeded {
                tenant_obj.object.status.quota_exceeded = quota_exceeded;
                error!(
                    "AddTenantCredits: updating tenant quota_exceeded to {}",
                    quota_exceeded
                );
                if let Err(e) = gw.client.Update(&tenant_obj.DataObject(), 0).await {
                    let body = Body::from(format!("Failed to update tenant quota status: {:?}", e));
                    let resp = Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(body)
                        .unwrap();
                    return Ok(resp);
                }
            }

            // Get updated balance
            let balance = match gw.sqlBilling.GetTenantCreditBalance(&tenant).await {
                Ok(b) => b,
                Err(e) => {
                    error!("AddTenantCredits: GetTenantCreditBalance failed: {:?}", e);
                    0
                }
            };
            error!(
                "AddTenantCredits: returning id={}, balance_cents={}",
                id, balance
            );
            let resp_body = AddCreditsResponse {
                success: true,
                credit_id: id,
                new_balance_cents: balance,
            };
            let data = serde_json::to_string(&resp_body).unwrap();
            let body = Body::from(data);
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("Failed to add credits: {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn GetBillingRateHistory(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Query(params): Query<BillingRateHistoryQuery>,
) -> SResult<Response, StatusCode> {
    if !token.IsInferxAdmin() {
        let body = Body::from("Permission denied: Only InferxAdmin can read billing rate history");
        let resp = Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    let mut limit = params.limit.unwrap_or(20);
    let mut offset = params.offset.unwrap_or(0);
    if limit < 0 {
        limit = 0;
    }
    if limit > 200 {
        limit = 200;
    }
    if offset < 0 {
        offset = 0;
    }

    let scope = match params
        .scope
        .as_deref()
        .unwrap_or("all")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "all" => "all",
        "global" => "global",
        "tenant" => "tenant",
        other => {
            let body = Body::from(format!(
                "Invalid scope '{}'. Expected one of: all, global, tenant",
                other
            ));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    };

    let tenant = params
        .tenant
        .as_ref()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());

    match gw
        .sqlBilling
        .ListBillingRateHistory(scope, tenant.as_deref(), limit, offset)
        .await
    {
        Ok((records, total)) => {
            let resp_body = BillingRateHistoryResponse { records, total };
            let data = serde_json::to_string(&resp_body).unwrap();
            let body = Body::from(data);
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("Failed to get billing rate history: {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn GetTenantCredits(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path(tenant): Path<String>,
) -> SResult<Response, StatusCode> {
    if !token.IsTenantUser(&tenant) {
        let body = Body::from("No permission to access this tenant");
        let resp = Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    match gw.sqlBilling.GetTenantBillingSummary(&tenant).await {
        Ok((
            balance_cents,
            used_cents,
            _threshold,
            quota_exceeded,
            _total_credits,
            currency,
            _,
            _,
            _,
            _,
        )) => {
            let resp_body = CreditResponse {
                balance_cents,
                used_cents,
                currency,
                quota_exceeded,
            };
            let data = serde_json::to_string(&resp_body).unwrap();
            let body = Body::from(data);
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("Failed to get credits: {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn GetBillingCreditHistory(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Query(params): Query<BillingCreditHistoryQuery>,
) -> SResult<Response, StatusCode> {
    if !token.IsInferxAdmin() {
        let body =
            Body::from("Permission denied: Only InferxAdmin can read billing credit history");
        let resp = Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    let mut limit = params.limit.unwrap_or(20);
    let mut offset = params.offset.unwrap_or(0);
    if limit < 0 {
        limit = 0;
    }
    if limit > 200 {
        limit = 200;
    }
    if offset < 0 {
        offset = 0;
    }

    let tenant = params
        .tenant
        .as_ref()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .filter(|t| t != "all" && t != "__all__");

    match gw
        .sqlBilling
        .ListBillingCreditHistory(tenant.as_deref(), limit, offset)
        .await
    {
        Ok((records, total)) => {
            let resp_body = BillingCreditHistoryResponse { records, total };
            let data = serde_json::to_string(&resp_body).unwrap();
            let body = Body::from(data);
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("Failed to get billing credit history: {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn GetTenantCreditHistory(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path(tenant): Path<String>,
    Query(params): Query<CreditHistoryQuery>,
) -> SResult<Response, StatusCode> {
    if !token.IsTenantUser(&tenant) {
        let body = Body::from("No permission to access this tenant");
        let resp = Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    let mut limit = params.limit.unwrap_or(20);
    let mut offset = params.offset.unwrap_or(0);
    if limit < 0 {
        limit = 0;
    }
    if offset < 0 {
        offset = 0;
    }

    match gw
        .sqlBilling
        .ListTenantCreditHistory(&tenant, limit, offset)
        .await
    {
        Ok((records, total)) => match gw.sqlBilling.GetTenantCreditBalance(&tenant).await {
            Ok(balance) => {
                let resp_body = CreditHistoryResponse {
                    records,
                    total,
                    balance_cents: balance,
                };
                let data = serde_json::to_string(&resp_body).unwrap();
                let body = Body::from(data);
                let resp = Response::builder()
                    .status(StatusCode::OK)
                    .body(body)
                    .unwrap();
                return Ok(resp);
            }
            Err(e) => {
                let body = Body::from(format!("Failed to get credits balance: {:?}", e));
                let resp = Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body(body)
                    .unwrap();
                return Ok(resp);
            }
        },
        Err(e) => {
            let body = Body::from(format!("Failed to get credit history: {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn GetTenantBillingSummary(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path(tenant): Path<String>,
) -> SResult<Response, StatusCode> {
    if !token.IsTenantUser(&tenant) {
        let body = Body::from("No permission to access this tenant");
        let resp = Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    match gw.sqlBilling.GetTenantBillingSummary(&tenant).await {
        Ok((
            balance_cents,
            used_cents,
            threshold_cents,
            quota_exceeded,
            total_credits_cents,
            currency,
            inference_cents,
            standby_cents,
            inference_ms,
            standby_ms,
        )) => {
            let quota_exempt = gw
                .objRepo
                .tenantMgr
                .Get(SYSTEM_TENANT, SYSTEM_NAMESPACE, &tenant)
                .map(|t| t.object.spec.quota_exempt)
                .unwrap_or(false);
            let resp_body = BillingSummaryResponse {
                balance_cents,
                used_cents,
                threshold_cents,
                quota_exceeded,
                quota_exempt,
                total_credits_cents,
                currency,
                period: BillingSummaryPeriod {
                    inference_cents,
                    standby_cents,
                    inference_hours: inference_ms as f64 / 3600000.0,
                    standby_hours: standby_ms as f64 / 3600000.0,
                },
            };
            let data = serde_json::to_string(&resp_body).unwrap();
            let body = Body::from(data);
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("Failed to get billing summary: {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn GetTenantHourlyUsage(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path(tenant): Path<String>,
    Query(params): Query<HourlyUsageQuery>,
) -> SResult<Response, StatusCode> {
    if !token.IsTenantUser(&tenant) {
        let body = Body::from("No permission to access this tenant");
        let resp = Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    let (start_hour, end_hour, total_hours) =
        match ParseHourlyRange(params.hours, params.start.clone(), params.end.clone()) {
            Ok(v) => v,
            Err(resp) => return Ok(resp),
        };

    match gw
        .sqlBilling
        .GetTenantHourlyUsageRange(&tenant, start_hour, end_hour)
        .await
    {
        Ok(records) => {
            let resp_body = BuildHourlyUsage(records, start_hour, total_hours);
            let data = serde_json::to_string(&resp_body).unwrap();
            let body = Body::from(data);
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("Failed to get hourly usage: {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn GetTenantUsageByModel(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path(tenant): Path<String>,
    Query(params): Query<UsageByGroupQuery>,
) -> SResult<Response, StatusCode> {
    if !token.IsTenantUser(&tenant) {
        let body = Body::from("No permission to access this tenant");
        let resp = Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    let (start_hour, end_hour, _total_hours) =
        match ParseHourlyRange(params.hours, params.start.clone(), params.end.clone()) {
            Ok(v) => v,
            Err(resp) => return Ok(resp),
        };
    let _deprecated_limit = params.limit;

    match gw
        .sqlBilling
        .GetTenantUsageByModel(&tenant, start_hour, end_hour)
        .await
    {
        Ok((items, total)) => {
            let usage: Vec<ModelUsageItem> = items
                .into_iter()
                .map(
                    |(
                        funcname,
                        namespace,
                        inference_ms,
                        standby_ms,
                        inference_cents,
                        standby_cents,
                        charge_cents,
                    )| {
                        let usage_ms = inference_ms + standby_ms;
                        ModelUsageItem {
                            funcname,
                            namespace,
                            inference_ms,
                            standby_ms,
                            usage_ms,
                            inference_gpu_hours: inference_ms as f64 / 3600000.0,
                            standby_gpu_hours: standby_ms as f64 / 3600000.0,
                            gpu_hours: usage_ms as f64 / 3600000.0,
                            inference_cents,
                            standby_cents,
                            charge_cents,
                        }
                    },
                )
                .collect();

            let (
                total_inference_ms,
                total_standby_ms,
                total_inference_cents,
                total_standby_cents,
                total_cents,
            ) = total;
            let total_ms = total_inference_ms + total_standby_ms;

            let resp_body = UsageByModelResponse {
                usage,
                other: OtherBucket {
                    count: 0,
                    inference_ms: 0,
                    standby_ms: 0,
                    usage_ms: 0,
                    inference_gpu_hours: 0.0,
                    standby_gpu_hours: 0.0,
                    gpu_hours: 0.0,
                    inference_cents: 0,
                    standby_cents: 0,
                    charge_cents: 0,
                },
                inference_ms: total_inference_ms,
                standby_ms: total_standby_ms,
                total_ms,
                inference_gpu_hours: total_inference_ms as f64 / 3600000.0,
                standby_gpu_hours: total_standby_ms as f64 / 3600000.0,
                total_gpu_hours: total_ms as f64 / 3600000.0,
                inference_cents: total_inference_cents,
                standby_cents: total_standby_cents,
                total_cents,
                timezone: "UTC".to_string(),
            };
            let data = serde_json::to_string(&resp_body).unwrap();
            let body = Body::from(data);
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("Failed to get usage by model: {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn GetTenantHourlyUsageByModel(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path(tenant): Path<String>,
    Query(params): Query<HourlyUsageByModelQuery>,
) -> SResult<Response, StatusCode> {
    if !token.IsTenantUser(&tenant) {
        let body = Body::from("No permission to access this tenant");
        let resp = Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    let namespace = match params
        .namespace
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        Some(v) => v,
        None => return Ok(BadRequest("Missing namespace parameter")),
    };
    let funcname = match params
        .funcname
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        Some(v) => v,
        None => return Ok(BadRequest("Missing funcname parameter")),
    };

    let (start_hour, end_hour, total_hours) =
        match ParseHourlyRange(params.hours, params.start.clone(), params.end.clone()) {
            Ok(v) => v,
            Err(resp) => return Ok(resp),
        };

    match gw
        .sqlBilling
        .GetFuncHourlyUsageRange(&tenant, &namespace, &funcname, start_hour, end_hour)
        .await
    {
        Ok(records) => {
            let resp_body = BuildHourlyUsage(records, start_hour, total_hours);
            let data = serde_json::to_string(&resp_body).unwrap();
            let body = Body::from(data);
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("Failed to get hourly usage by model: {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn GetTenantHourlyUsageByNamespace(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path(tenant): Path<String>,
    Query(params): Query<HourlyUsageByNamespaceQuery>,
) -> SResult<Response, StatusCode> {
    if !token.IsTenantUser(&tenant) {
        let body = Body::from("No permission to access this tenant");
        let resp = Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    let namespace = match params
        .namespace
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        Some(v) => v,
        None => return Ok(BadRequest("Missing namespace parameter")),
    };

    let (start_hour, end_hour, total_hours) =
        match ParseHourlyRange(params.hours, params.start.clone(), params.end.clone()) {
            Ok(v) => v,
            Err(resp) => return Ok(resp),
        };

    match gw
        .sqlBilling
        .GetNamespaceHourlyUsageRange(&tenant, &namespace, start_hour, end_hour)
        .await
    {
        Ok(records) => {
            let resp_body = BuildHourlyUsage(records, start_hour, total_hours);
            let data = serde_json::to_string(&resp_body).unwrap();
            let body = Body::from(data);
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("Failed to get hourly usage by namespace: {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn GetTenantUsageByNamespace(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path(tenant): Path<String>,
    Query(params): Query<UsageByGroupQuery>,
) -> SResult<Response, StatusCode> {
    if !token.IsTenantUser(&tenant) {
        let body = Body::from("No permission to access this tenant");
        let resp = Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    let (start_hour, end_hour, _total_hours) =
        match ParseHourlyRange(params.hours, params.start.clone(), params.end.clone()) {
            Ok(v) => v,
            Err(resp) => return Ok(resp),
        };
    let _deprecated_limit = params.limit;

    match gw
        .sqlBilling
        .GetTenantUsageByNamespace(&tenant, start_hour, end_hour)
        .await
    {
        Ok((items, total)) => {
            let usage: Vec<NamespaceUsageItem> = items
                .into_iter()
                .map(
                    |(
                        namespace,
                        inference_ms,
                        standby_ms,
                        inference_cents,
                        standby_cents,
                        charge_cents,
                    )| {
                        let usage_ms = inference_ms + standby_ms;
                        NamespaceUsageItem {
                            namespace,
                            inference_ms,
                            standby_ms,
                            usage_ms,
                            inference_gpu_hours: inference_ms as f64 / 3600000.0,
                            standby_gpu_hours: standby_ms as f64 / 3600000.0,
                            gpu_hours: usage_ms as f64 / 3600000.0,
                            inference_cents,
                            standby_cents,
                            charge_cents,
                        }
                    },
                )
                .collect();

            let (
                total_inference_ms,
                total_standby_ms,
                total_inference_cents,
                total_standby_cents,
                total_cents,
            ) = total;
            let total_ms = total_inference_ms + total_standby_ms;

            let resp_body = UsageByNamespaceResponse {
                usage,
                other: OtherBucket {
                    count: 0,
                    inference_ms: 0,
                    standby_ms: 0,
                    usage_ms: 0,
                    inference_gpu_hours: 0.0,
                    standby_gpu_hours: 0.0,
                    gpu_hours: 0.0,
                    inference_cents: 0,
                    standby_cents: 0,
                    charge_cents: 0,
                },
                inference_ms: total_inference_ms,
                standby_ms: total_standby_ms,
                total_ms,
                inference_gpu_hours: total_inference_ms as f64 / 3600000.0,
                standby_gpu_hours: total_standby_ms as f64 / 3600000.0,
                total_gpu_hours: total_ms as f64 / 3600000.0,
                inference_cents: total_inference_cents,
                standby_cents: total_standby_cents,
                total_cents,
                timezone: "UTC".to_string(),
            };
            let data = serde_json::to_string(&resp_body).unwrap();
            let body = Body::from(data);
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("Failed to get usage by namespace: {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn GetTenantUsageSummary(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path(tenant): Path<String>,
    Query(params): Query<HourlyUsageQuery>,
) -> SResult<Response, StatusCode> {
    if !token.IsTenantUser(&tenant) {
        let body = Body::from("No permission to access this tenant");
        let resp = Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    let hours = params.hours.unwrap_or(24).min(720).max(1);

    match gw
        .sqlBilling
        .GetTenantAnalyticsSummary(&tenant, hours)
        .await
    {
        Ok((
            total_ms,
            total_cents,
            top_model_name,
            top_model_ms,
            top_namespace_name,
            top_namespace_ms,
            peak_hour,
            peak_hour_ms,
        )) => {
            let top_model = top_model_name.map(|name| TopModelInfo {
                funcname: name,
                gpu_hours: top_model_ms as f64 / 3600000.0,
            });

            let top_namespace = top_namespace_name.map(|name| TopNamespaceInfo {
                namespace: name,
                gpu_hours: top_namespace_ms as f64 / 3600000.0,
            });

            let peak_hour_info = peak_hour.map(|hour| PeakHourInfo {
                hour: hour.format("%Y-%m-%dT%H:00:00Z").to_string(),
                gpu_hours: peak_hour_ms as f64 / 3600000.0,
            });

            let resp_body = UsageSummaryResponse {
                total_gpu_hours: total_ms as f64 / 3600000.0,
                total_cents,
                top_model,
                top_namespace: top_namespace,
                peak_hour: peak_hour_info,
                timezone: "UTC".to_string(),
            };
            let data = serde_json::to_string(&resp_body).unwrap();
            let body = Body::from(data);
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("Failed to get usage summary: {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn GetAdminEndpointUsage(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Query(params): Query<AdminEndpointUsageQuery>,
) -> SResult<Response, StatusCode> {
    if !token.IsInferxAdmin() {
        let body = Body::from("Admin access required");
        let resp = Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    let (start_hour, end_hour, _total_hours) =
        match ParseAdminEndpointRange(params.hours, params.start.clone(), params.end.clone()) {
            Ok(v) => v,
            Err(resp) => return Ok(resp),
        };

    let limit = params.limit.unwrap_or(100);
    let offset = params.offset.unwrap_or(0);
    let group_by = match params.group_by.as_deref() {
        Some("tenant") => "tenant",
        _ => "endpoint",
    };

    match gw
        .sqlBilling
        .GetAdminEndpointUsage(
            start_hour,
            end_hour,
            limit,
            offset,
            group_by,
            params.tenant.as_deref(),
        )
        .await
    {
        Ok(rows) => {
            let total = rows.first().map(|r| r.total_count).unwrap_or(0);
            let usage: Vec<serde_json::Value> = rows
                .into_iter()
                .map(|r| {
                    serde_json::json!({
                        "tenant": r.tenant,
                        "endpoint_slug": r.endpoint_slug,
                        "endpoint_count": r.endpoint_count,
                        "inference_ms": r.inference_ms,
                        "inference_cents": r.inference_cents,
                        "total_cents": r.total_cents,
                        "last_seen_at": r.last_seen_at.format("%Y-%m-%dT%H:%M:%SZ").to_string()
                    })
                })
                .collect();

            let resp_body = serde_json::json!({
                "usage": usage,
                "total": total,
                "group_by": group_by,
                "start": start_hour.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                "end": end_hour.format("%Y-%m-%dT%H:%M:%SZ").to_string()
            });

            let data = serde_json::to_string(&resp_body).unwrap();
            let body = Body::from(data);
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("Failed to get admin endpoint usage: {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn GetAdminEndpointTenantUsageByPeriod(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path((tenant, endpoint_slug)): Path<(String, String)>,
    Query(params): Query<AdminEndpointUsageByPeriodQuery>,
) -> SResult<Response, StatusCode> {
    if !token.IsInferxAdmin() {
        let body = Body::from("Admin access required");
        let resp = Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    let (start_hour, end_hour, total_hours) =
        match ParseAdminEndpointRange(params.hours, params.start.clone(), params.end.clone()) {
            Ok(v) => v,
            Err(resp) => return Ok(resp),
        };

    let granularity = params.granularity.unwrap_or_else(|| {
        if total_hours <= 24 {
            "hourly".to_string()
        } else if total_hours <= 168 {
            "daily".to_string()
        } else {
            "weekly".to_string()
        }
    });

    match gw
        .sqlBilling
        .GetAdminEndpointTenantUsageByPeriod(
            &tenant,
            &endpoint_slug,
            start_hour,
            end_hour,
            &granularity,
        )
        .await
    {
        Ok(rows) => {
            let usage: Vec<serde_json::Value> = rows
                .into_iter()
                .map(|r| {
                    serde_json::json!({
                        "period_start": r.period_start.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                        "period_end": r.period_end.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                        "inference_ms": r.inference_ms,
                        "inference_cents": r.inference_cents,
                        "total_cents": r.total_cents
                    })
                })
                .collect();

            let resp_body = serde_json::json!({
                "tenant": tenant,
                "endpoint_slug": endpoint_slug,
                "granularity": granularity,
                "usage": usage,
                "start": start_hour.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                "end": end_hour.format("%Y-%m-%dT%H:%M:%SZ").to_string()
            });

            let data = serde_json::to_string(&resp_body).unwrap();
            let body = Body::from(data);
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("Failed to get endpoint usage by period: {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn ListFuncBrief(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path((tenant, namespace)): Path<(String, String)>,
) -> SResult<Response, StatusCode> {
    match gw.ListFuncBrief(&token, &tenant, &namespace) {
        Ok(list) => {
            let data = serde_json::to_string(&list).unwrap();
            let body = Body::from(format!("{}", data));
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn GetFuncDetail(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path((tenant, namespace, funcname)): Path<(String, String, String)>,
) -> SResult<Response, StatusCode> {
    match gw.GetFuncDetail(&token, &tenant, &namespace, &funcname) {
        Ok(detail) => {
            let data = serde_json::to_string(&detail).unwrap();
            let body = Body::from(format!("{}", data));
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn GetPublishedEndpointDetail(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path((tenant, slug)): Path<(String, String)>,
) -> SResult<Response, StatusCode> {
    if !token.CheckScope("read") {
        let body = Body::from("service failure NoPermission");
        let resp = Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    if !token.IsTenantUser(&tenant) {
        let body = Body::from("service failure NoPermission");
        let resp = Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    match resolve_funccall_target(&gw, &tenant, VIRTUAL_ENDPOINTS_NAMESPACE, &slug) {
        Ok(target) => {
            let detail = FuncDetail {
                sampleRestCall: target.func.SampleRestCall(),
                func: target.func,
                snapshots: Vec::new(),
                pods: Vec::new(),
                isAdmin: false,
                policy: target.policy,
            };
            let data = serde_json::to_string(&detail).unwrap();
            let body = Body::from(format!("{}", data));
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn GetNodes(State(gw): State<HttpGateway>) -> SResult<Response, StatusCode> {
    match gw.objRepo.GetNodes() {
        Ok(list) => {
            let data = serde_json::to_string(&list).unwrap();
            let body = Body::from(format!("{}", data));
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn GetNode(
    State(gw): State<HttpGateway>,
    Path(nodename): Path<String>,
) -> SResult<Response, StatusCode> {
    match gw.objRepo.GetNode(&nodename) {
        Ok(list) => {
            let data = serde_json::to_string_pretty(&list).unwrap();
            let body = Body::from(format!("{}", data));
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn RbacGrant(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Json(grant): Json<Grant>,
) -> SResult<Response, StatusCode> {
    let tenant_target = match grant.objType {
        ObjectType::Tenant => grant.name.as_str(),
        ObjectType::Namespace => grant.tenant.as_str(),
    };
    if GATEWAY_CONFIG.enforceBilling {
        if let Some(resp) = enforce_tenant_quota_for_write(&token, &gw, tenant_target) {
            return Ok(resp);
        }
    }
    match gw
        .Rbac(
            &token,
            &PermissionType::Grant,
            &grant.objType,
            &grant.tenant,
            &grant.namespace,
            &grant.name,
            grant.role,
            &grant.username,
        )
        .await
    {
        Ok(()) => {
            let body = Body::from(format!("{}", "rbac successfully"));
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn RbacRevoke(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Json(grant): Json<Grant>,
) -> SResult<Response, StatusCode> {
    let tenant_target = match grant.objType {
        ObjectType::Tenant => grant.name.as_str(),
        ObjectType::Namespace => grant.tenant.as_str(),
    };
    if GATEWAY_CONFIG.enforceBilling {
        if let Some(resp) = enforce_tenant_quota_for_write(&token, &gw, tenant_target) {
            return Ok(resp);
        }
    }
    match gw
        .Rbac(
            &token,
            &PermissionType::Revoke,
            &grant.objType,
            &grant.tenant,
            &grant.namespace,
            &grant.name,
            grant.role,
            &grant.username,
        )
        .await
    {
        Ok(()) => {
            let body = Body::from(format!("{}", "rbac successfully"));
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn RbacTenantUsers(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path((role, tenant)): Path<(String, String)>,
) -> SResult<Response, StatusCode> {
    match gw.RbacTenantUsers(&token, &role, &tenant).await {
        Ok(users) => {
            let data = serde_json::to_string(&users).unwrap();
            let body = Body::from(format!("{}", data));
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn RbacNamespaceUsers(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path((role, tenant, namespace)): Path<(String, String, String)>,
) -> SResult<Response, StatusCode> {
    match gw
        .RbacNamespaceUsers(&token, &role, &tenant, &namespace)
        .await
    {
        Ok(users) => {
            let data = serde_json::to_string(&users).unwrap();
            let body = Body::from(format!("{}", data));
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn RbacRoleBindingGet(
    Extension(token): Extension<Arc<AccessToken>>,
) -> SResult<Response, StatusCode> {
    match token.RoleBindings() {
        Ok(bindings) => {
            let data = serde_json::to_string(&bindings).unwrap();
            let body = Body::from(format!("{}", data));
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn GetFuncPods(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path((tenant, namespace, funcname)): Path<(String, String, String)>,
) -> SResult<Response, StatusCode> {
    match gw.GetFuncPods(&token, &tenant, &namespace, &funcname) {
        Ok(list) => {
            let lease_state = gw.funcAgentMgr.PodLeaseState();
            let mut rows = Vec::with_capacity(list.len());

            for pod in list {
                let pod_key = format!(
                    "{}/{}/{}/{}/{}",
                    &pod.tenant,
                    &pod.namespace,
                    &pod.object.spec.funcname,
                    pod.object.spec.fprevision,
                    &pod.object.spec.id
                );
                let mut pod_value = serde_json::to_value(&pod).unwrap();

                if pod.object.status.state == PodState::Ready {
                    let leased = lease_state
                        .get(&pod_key)
                        .map(|info| info.leased)
                        .unwrap_or(false);
                    if let Some(obj) = pod_value.as_object_mut() {
                        obj.insert("leased".to_owned(), Value::Bool(leased));
                        if leased {
                            if let Some(consumer_tenant) = lease_state
                                .get(&pod_key)
                                .and_then(|info| info.endpoint_consumer_tenant.clone())
                            {
                                obj.insert(
                                    "endpoint_consumer_tenant".to_owned(),
                                    Value::String(consumer_tenant),
                                );
                            }
                        }
                    }
                }

                rows.push(pod_value);
            }

            let data = serde_json::to_string(&rows).unwrap();
            let body = Body::from(format!("{}", data));
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn GetFuncPod(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path((tenant, namespace, funcname, version, id)): Path<(
        String,
        String,
        String,
        String,
        String,
    )>,
) -> SResult<Response, StatusCode> {
    let podname = format!(
        "{}/{}/{}/{}/{}",
        &tenant, &namespace, &funcname, &version, &id
    );
    match gw.GetFuncPod(&token, &tenant, &namespace, &podname) {
        Ok(list) => {
            let data = serde_json::to_string(&list).unwrap();
            let body = Body::from(format!("{}", data));
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn KillPod(
    State(_gw): State<HttpGateway>,
    Extension(token): Extension<Arc<AccessToken>>,
    Path((tenant, namespace, funcname, version, id)): Path<(String, String, String, i64, String)>,
) -> impl IntoResponse {
    if !token.IsNamespaceAdmin(&tenant, &namespace) {
        let body = Body::from("service failure: No permission");
        return Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(body)
            .unwrap()
            .into_response();
    }

    let req = na::KillPodReq {
        tenant,
        namespace,
        funcname,
        fprevision: version,
        id,
    };

    let mut client = match SCHEDULER_CLIENT.GetClient().await {
        Ok(client) => client,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

    match client.kill_pod(req).await {
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, e.to_string()).into_response(),
        Ok(resp) => {
            let resp = resp.into_inner();
            let status =
                na::KillPodStatus::from_i32(resp.status).unwrap_or(na::KillPodStatus::Internal);

            match status {
                na::KillPodStatus::Ok if resp.error.is_empty() => {
                    StatusCode::ACCEPTED.into_response()
                }
                na::KillPodStatus::Ok => {
                    (StatusCode::INTERNAL_SERVER_ERROR, resp.error).into_response()
                }
                na::KillPodStatus::NotFound => (StatusCode::NOT_FOUND, resp.error).into_response(),
                na::KillPodStatus::InvalidState => {
                    (StatusCode::CONFLICT, resp.error).into_response()
                }
                na::KillPodStatus::Internal => {
                    (StatusCode::INTERNAL_SERVER_ERROR, resp.error).into_response()
                }
            }
        }
    }
}

async fn ReadLog(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path((tenant, namespace, funcname, version, id)): Path<(String, String, String, i64, String)>,
) -> SResult<Response, StatusCode> {
    match gw
        .ReadLog(&token, &tenant, &namespace, &funcname, version, &id)
        .await
    {
        Ok(log) => {
            let body = Body::from(log);
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn ReadPodAuditLog(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path((tenant, namespace, funcname, version, id)): Path<(String, String, String, i64, String)>,
) -> SResult<Response, StatusCode> {
    match gw
        .ReadPodAuditLog(&token, &tenant, &namespace, &funcname, version, &id)
        .await
    {
        Ok(logs) => {
            let data = serde_json::to_string(&logs).unwrap();
            let body = Body::from(data);
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            error!("ReadPodAuditLog error {:?}", &e);
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

async fn ReadSnapshotScheduleRecords(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path((tenant, namespace, funcname, version)): Path<(String, String, String, i64)>,
) -> SResult<Response, StatusCode> {
    match gw
        .ReadSnapshotScheduleRecords(&token, &tenant, &namespace, &funcname, version)
        .await
    {
        Ok(recs) => {
            let data = serde_json::to_string(&recs).unwrap();
            let body = Body::from(data);
            let resp = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
        Err(e) => {
            error!("ReadSnapshotScheduleRecords error {:?}", &e);
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(body)
                .unwrap();
            return Ok(resp);
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct PromptReq {
    pub tenant: String,
    pub namespace: String,
    pub funcname: String,
    pub prompt: String,
    #[serde(default)]
    pub image: String,
}

#[derive(Serialize)]
pub struct OpenAIReq {
    pub prompt: String,
    pub model: String,
    pub max_tokens: usize,
    pub temperature: usize,
    pub stream: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LlavaReq {
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub image: String,
}

impl Default for LlavaReq {
    fn default() -> Self {
        return Self {
            prompt: "What is shown in this image?".to_owned(),
            image: "https://www.ilankelman.org/stopsigns/australia.jpg".to_owned(),
        };
    }
}
