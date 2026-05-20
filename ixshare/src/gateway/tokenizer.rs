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
// limitations under the License

use axum::body::Body;
use axum::extract::{Json, Path as AxumPath, Request, State};
use axum::http::{StatusCode, Uri};
use axum::response::Response;
use axum::Extension;
use dashmap::DashMap;
use hyper::header::HeaderValue;
use inferxlib::obj_mgr::func_mgr::ApiType;
use minijinja::{context, Environment};
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::result::Result as SResult;
use std::sync::Arc;
use tokenizers::Tokenizer;

use crate::gateway::auth_layer::AccessToken;
use crate::gateway::http_gateway::FuncCall1;
use crate::gateway::http_gateway::HttpGateway;
use crate::node_config::KB_DIR;
use super::func_agent_mgr::FuncRouteTarget;

const FUNCCALL_MAX_BODY_BYTES: usize = 20 * 1024 * 1024;

// ==================================================================
// Types (OpenAI-compatible request shapes)
// ==================================================================

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ChatRequest {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(rename = "messages")]
    pub messages: Vec<ChatMessage>,
    #[serde(
        rename = "max_tokens",
        alias = "maxTokens",
        default,
        deserialize_with = "deserialize_optional_u32_from_any"
    )]
    pub maxTokens: Option<u32>,
    #[serde(default = "DefaultTemperature", deserialize_with = "deserialize_f32_from_any")]
    pub temperature: f32,
    #[serde(rename = "stream", deserialize_with = "deserialize_bool_from_any")]
    pub stream: bool,
    #[serde(
        rename = "top_p",
        alias = "topP",
        default = "DefaultTopP",
        deserialize_with = "deserialize_f32_from_any"
    )]
    pub topP: f32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChatMessage {
    #[serde(rename = "role")]
    pub role: String,
    #[serde(rename = "content", deserialize_with = "deserialize_chat_content")]
    pub content: String,
}

fn DefaultTemperature() -> f32 {
    1.0
}

fn DefaultTopP() -> f32 {
    1.0
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum JsonNumberOrString {
    Number(serde_json::Number),
    String(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum JsonBoolOrString {
    Bool(bool),
    String(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum ChatContentValue {
    Text(String),
    Parts(Vec<ChatContentPart>),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum ChatContentPart {
    Text(ChatTextPart),
    BareString(String),
    Json(serde_json::Value),
}

#[derive(Debug, Clone, Deserialize)]
struct ChatTextPart {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

fn deserialize_optional_u32_from_any<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<JsonNumberOrString>::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(JsonNumberOrString::Number(n)) => n
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .map(Some)
            .ok_or_else(|| de::Error::custom("expected a non-negative integer for max_tokens")),
        Some(JsonNumberOrString::String(s)) => s
            .trim()
            .parse::<u32>()
            .map(Some)
            .map_err(|_| de::Error::custom("expected max_tokens as integer or integer string")),
    }
}

fn deserialize_f32_from_any<'de, D>(deserializer: D) -> Result<f32, D::Error>
where
    D: Deserializer<'de>,
{
    match JsonNumberOrString::deserialize(deserializer)? {
        JsonNumberOrString::Number(n) => n
            .to_string()
            .parse::<f32>()
            .map_err(|_| de::Error::custom("expected numeric value")),
        JsonNumberOrString::String(s) => s
            .trim()
            .parse::<f32>()
            .map_err(|_| de::Error::custom("expected numeric string")),
    }
}

fn deserialize_bool_from_any<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    match JsonBoolOrString::deserialize(deserializer)? {
        JsonBoolOrString::Bool(v) => Ok(v),
        JsonBoolOrString::String(s) => s
            .trim()
            .parse::<bool>()
            .map_err(|_| de::Error::custom("expected bool or bool string")),
    }
}

fn deserialize_chat_content<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    match ChatContentValue::deserialize(deserializer)? {
        ChatContentValue::Text(text) => Ok(text),
        ChatContentValue::Parts(parts) => {
            let text = parts
                .into_iter()
                .filter_map(|part| match part {
                    ChatContentPart::Text(item) if item.kind == "text" => {
                        let text = item.text.trim().to_string();
                        if text.is_empty() {
                            None
                        } else {
                            Some(text)
                        }
                    }
                    ChatContentPart::BareString(text) => {
                        let text = text.trim().to_string();
                        if text.is_empty() {
                            None
                        } else {
                            Some(text)
                        }
                    }
                    ChatContentPart::Json(_) | ChatContentPart::Text(_) => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            Ok(text)
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CompletionRequest {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(rename = "prompt")]
    pub prompt: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "max_tokens"
    )]
    pub maxTokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "top_p")]
    pub topP: Option<f32>,
    #[serde(rename = "stream")]
    pub stream: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct VllmCompletionRequest {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(rename = "prompt")]
    pub prompt: Vec<u32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "max_tokens"
    )]
    pub maxTokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "top_p")]
    pub topP: Option<f32>,
    pub stream: bool,
    #[serde(rename = "skip_special_tokens")]
    pub skipSpecialTokens: bool,
}

/// Completion request that uses raw text prompt instead of tokenized IDs.
#[derive(Debug, Clone, Serialize)]
pub struct RawCompletionRequest {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(rename = "prompt")]
    pub prompt: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "max_tokens"
    )]
    pub maxTokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "top_p")]
    pub topP: Option<f32>,
    pub stream: bool,
    #[serde(rename = "skip_special_tokens")]
    pub skipSpecialTokens: bool,
}

// ==================================================================
// ChatTemplate helper
// ==================================================================

/// Read the chat_template, bos_token, eos_token from tokenizer_config.json.
fn LoadTokenizerConfig(modelPath: &str) -> (Option<String>, String, String) {
    let configPath = Path::new(modelPath).join("tokenizer_config.json");
    if let Ok(configStr) = std::fs::read_to_string(&configPath) {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&configStr) {
            let chatTemplate = ExtractStr(&val, "chat_template");
            let bosToken = ExtractStrOrDefault(&val, "bos_token", "<|begin_of_text|>");
            let eosToken = ExtractStrOrDefault(&val, "eos_token", "<|end_of_text|>");
            return (chatTemplate, bosToken, eosToken);
        }
    }
    (
        None,
        "<|begin_of_text|>".to_string(),
        "<|end_of_text|>".to_string(),
    )
}

fn ExtractStr(val: &serde_json::Value, key: &str) -> Option<String> {
    match val.get(key).and_then(|v| v.as_str()) {
        Some(s) => Some(s.to_string()),
        None => None,
    }
}

fn ExtractStrOrDefault(val: &serde_json::Value, key: &str, default: &str) -> String {
    match val.get(key).and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => default.to_string(),
    }
}

/// Apply the Jinja chat_template from tokenizer_config.json to convert
/// `/v1/chat/completions` messages into a prompt string for `/v1/completions`.
///
/// Uses minijinja to render the template. Falls back to a built-in Llama 3
/// format when no chat_template is available.
fn ApplyChatTemplate(
    messages: &[ChatMessage],
    chatTemplate: Option<&str>,
    bosToken: &str,
) -> String {
    if let Some(templateStr) = chatTemplate {
        let mut env = Environment::new();
        env.add_template_owned("chat_template", templateStr.to_string())
            .unwrap();

        let addGenerationPrompt =
            !messages.is_empty() && messages.last().map_or(true, |m| m.role != "assistant");

        let messagesJson: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| serde_json::json!({ "role": m.role, "content": m.content }))
            .collect();

        return env
            .get_template("chat_template")
            .unwrap()
            .render(context!(
                bos_token => bosToken.to_string(),
                eos_token => String::new(),
                messages => messagesJson,
                add_generation_prompt => addGenerationPrompt
            ))
            .unwrap_or_else(|_| DefaultLlama3Template(messages, bosToken));
    }

    DefaultLlama3Template(messages, bosToken)
}

/// Built-in Llama 3 chat template fallback (matches the standard
/// `<|begin_of_text|><|start_header_id|>...` format).
fn DefaultLlama3Template(messages: &[ChatMessage], bosToken: &str) -> String {
    let mut text = String::new();
    text.push_str(bosToken);

    for msg in messages {
        // Convert "bot" -> "assistant" for compatibility
        let role = if msg.role == "bot" {
            "assistant"
        } else {
            &msg.role
        };
        let eos = if role == "assistant" { "</s>" } else { "" };
        text.push_str(&format!(
            "<|start_header_id|>{role}<|end_header_id|>\n\n{content}{eos}",
            role = role,
            content = msg.content
        ));
    }

    text.push_str("<|start_header_id|>assistant<|end_header_id|>\n\n");
    text
}

#[cfg(test)]
mod tests {
    use super::ParseRequestFields;

    #[test]
    fn parse_chat_request_accepts_stringified_scalars() {
        let body = br#"{
            "max_tokens": "1000",
            "messages": [{"content": "what is the internal codename?", "role": "user"}],
            "model": "Qwen/Qwen2.5-Coder-7B-Instruct-GPTQ-Int4",
            "stream": "true",
            "temperature": "0",
            "top_p": "0.9"
        }"#;

        let fields = ParseRequestFields(body, "/v1/chat/completions", None, "").unwrap();
        assert_eq!(fields.model, "Qwen/Qwen2.5-Coder-7B-Instruct-GPTQ-Int4");
        assert_eq!(fields.maxTokens, Some(1000));
        assert_eq!(fields.temperature, Some(0.0));
        assert_eq!(fields.topP, Some(0.9));
        assert!(fields.stream);
        assert!(fields.promptText.contains("what is the internal codename?"));
    }

    #[test]
    fn parse_chat_request_accepts_content_arrays() {
        let body = br#"{
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "Describe this image"},
                        {"type": "image_url", "image_url": {"url": "http://example.com/cat.jpg"}}
                    ]
                }
            ],
            "stream": false
        }"#;

        let fields = ParseRequestFields(body, "/v1/chat/completions", None, "").unwrap();
        assert!(fields.promptText.contains("Describe this image"));
    }
}

// ==================================================================
// Shared Tokenizer State
// ==================================================================

pub struct TokenizerState {
    pub tokenizer: Arc<Tokenizer>,
    /// Jinja chatTemplate string from tokenizer_config.json
    pub chatTemplate: Option<String>,
    /// bosToken from tokenizer_config.json
    pub bosToken: String,
    /// eosToken from tokenizer_config.json
    pub eosToken: String,
}

lazy_static::lazy_static! {
    pub static ref SHARED_TOKENIZERS: DashMap<String, Arc<TokenizerState>> = DashMap::new();
}

async fn GetOrLoadTokenizer(modelPath: &str) -> SResult<Arc<TokenizerState>, StatusCode> {
    if let Some(state) = SHARED_TOKENIZERS.get(modelPath) {
        return Ok(state.value().clone());
    }

    // Load tokenizer.json (local or HF hub)
    let tokenizer = if Path::new(modelPath).exists() {
        let tokenizerJson = Path::new(modelPath).join("tokenizer.json");
        Tokenizer::from_file(tokenizerJson).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    } else {
        let api = hf_hub::api::sync::Api::new().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let repo = api.model(modelPath.to_string());
        let file = repo
            .get("tokenizer.json")
            .map_err(|_| StatusCode::NOT_FOUND)?;
        Tokenizer::from_file(file).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    };

    // Load tokenizer_config.json for chatTemplate, bosToken, etc.
    let (chatTemplate, bosToken, eosToken): (Option<String>, String, String) =
        if Path::new(modelPath).exists() {
            LoadTokenizerConfig(modelPath)
        } else {
            // Try loading from HF hub
            let config_str = (|| -> Option<String> {
                let api = hf_hub::api::sync::Api::new().ok()?;
                let repo = api.model(modelPath.to_string());
                let file = repo.get("tokenizer_config.json").ok()?;
                std::fs::read_to_string(file).ok()
            })();
            if let Some(cs) = config_str {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&cs) {
                    let ct = ExtractStr(&val, "chat_template");
                    let bos = ExtractStrOrDefault(&val, "bos_token", "<|begin_of_text|>");
                    let eos = ExtractStrOrDefault(&val, "eos_token", "<|end_of_text|>");
                    (ct, bos, eos)
                } else {
                    (
                        None,
                        "<|begin_of_text|>".to_string(),
                        "<|end_of_text|>".to_string(),
                    )
                }
            } else {
                (
                    None,
                    "<|begin_of_text|>".to_string(),
                    "<|end_of_text|>".to_string(),
                )
            }
        };

    let state = Arc::new(TokenizerState {
        tokenizer: Arc::new(tokenizer),
        chatTemplate,
        bosToken,
        eosToken,
    });

    SHARED_TOKENIZERS.insert(modelPath.to_string(), state.clone());
    Ok(state)
}

// ==================================================================
// Shared setup
// ==================================================================

pub struct RouteContext {
    pub tenant: String,
    pub namespace: String,
    pub funcName: String,
    pub remainPath: String,
    pub bodyBytes: Vec<u8>,
    pub modelPath: String,
    pub tokenizerState: Option<Arc<TokenizerState>>,
    pub kbPrompt: Option<String>,
}

async fn ResolveRouteContext(
    token: &Arc<AccessToken>,
    gw: &HttpGateway,
    req: axum::http::Request<axum::body::Body>,
    always_load_tokenizer: bool,
) -> SResult<RouteContext, StatusCode> {
    let (reqParts, body) = req.into_parts();
    let pathParts: Vec<&str> = reqParts.uri.path().split('/').collect();
    if pathParts.len() < 5 {
        return Err(StatusCode::BAD_REQUEST);
    }

    let tenant = pathParts[2].to_owned();
    let namespace = pathParts[3].to_owned();
    let funcName = pathParts[4].to_owned();

    if !token.CheckScope("inference") {
        return Err(StatusCode::UNAUTHORIZED);
    }

    if !token.IsNamespaceInferenceUser(&tenant, &namespace) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let mut remainPath = "".to_string();
    for i in 5..pathParts.len() {
        remainPath = remainPath + "/" + pathParts[i];
    }

    let func = match gw
        .funcAgentMgr
        .objRepo
        .GetFunc(&tenant, &namespace, &funcName)
    {
        Ok(f) => f,
        Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };

    let apiType = func.object.spec.SampleCallType();
    let kbPrompt = match apiType {
        ApiType::KnowledageBase => {
            let promptfilename = format!(
                "{}/{}.{}.{}/{}/kb.data",
                KB_DIR,
                func.tenant,
                func.namespace,
                func.name,
                func.Version()
            );
            match std::fs::read_to_string(&promptfilename) {
                Ok(p) => Some(p),
                Err(e) => {
                    error!(
                        "can't open prompt file {} with error {:?}",
                        &promptfilename, e
                    );
                    None
                }
            }
        }
        _ => None,
    };

    let bodyBytes = match axum::body::to_bytes(body, FUNCCALL_MAX_BODY_BYTES).await {
        Ok(b) => b.to_vec(),
        Err(_) => return Err(StatusCode::BAD_REQUEST),
    };

    // ModelPath and tokenizer are only needed for chat-completions (chat template rendering),
    // unless the caller (e.g. TokenizerRoute) always requires tokenization.
    let (model_path, tokenizerState) = if always_load_tokenizer || remainPath.starts_with("/v1/chat/completions") {
        let mp = match func.object.spec.ModelPath() {
            Some(p) => p,
            None => return Err(StatusCode::INTERNAL_SERVER_ERROR),
        };
        let tok = match GetOrLoadTokenizer(&mp).await {
            Ok(st) => Some(st),
            Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
        };
        (mp, tok)
    } else {
        (String::new(), None)
    };

    Ok(RouteContext {
        tenant,
        namespace,
        funcName,
        remainPath,
        bodyBytes,
        modelPath: model_path,
        tokenizerState,
        kbPrompt,
    })
}

async fn ForwardToFuncCall(
    token: &Arc<AccessToken>,
    gw: &HttpGateway,
    ctx: &RouteContext,
    targetPath: &str,
    body: &serde_json::Value,
) -> SResult<Response, StatusCode> {
    let new_body = serde_json::to_vec(body).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let new_req = Request::builder()
        .method(axum::http::Method::POST)
        .uri(format!(
            "/funccall/{}/{}/{}{}",
            ctx.tenant, ctx.namespace, ctx.funcName, targetPath
        ))
        .header("content-type", "application/json")
        .body(Body::from(new_body))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    FuncCall1(token, gw, new_req).await
}

#[derive(Debug, Clone, Deserialize)]
pub struct PromptReq {
    pub tenant: String,
    pub namespace: String,
    pub funcName: String,
    pub prompt: String,
    pub image: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct KbTokenCountRequest {
    pub model: String,
}

#[derive(Debug, Serialize)]
pub struct KbTokenCountResponse {
    pub kb_token_count: usize,
}

fn JsonResp<T: Serialize>(status: StatusCode, body: &T) -> SResult<Response<Body>, StatusCode> {
    let bytes = serde_json::to_vec(body).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(bytes))
        .unwrap())
}

fn PlainResp(status: StatusCode, body: &str) -> Response<Body> {
    Response::builder()
        .status(status)
        .body(Body::from(body.to_string()))
        .unwrap()
}

pub async fn CountKnowledgeBaseTokens(
    Extension(token): Extension<Arc<AccessToken>>,
    AxumPath((tenant, namespace, name)): AxumPath<(String, String, String)>,
    Json(req): Json<KbTokenCountRequest>,
) -> SResult<Response<Body>, StatusCode> {
    if !token.IsNamespaceAdmin(&tenant, &namespace) {
        return Ok(PlainResp(StatusCode::FORBIDDEN, "service failure: No permission"));
    }

    let model = req.model.trim();
    if model.is_empty() {
        return Ok(PlainResp(
            StatusCode::BAD_REQUEST,
            "service failure: missing model",
        ));
    }

    let kb_file = format!("{}/{}.{}.{}/kb.data", KB_DIR, tenant, namespace, name);
    let kb_prompt = match std::fs::read_to_string(&kb_file) {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PlainResp(
                StatusCode::NOT_FOUND,
                "service failure: kb.data not found",
            ));
        }
        Err(_) => {
            return Ok(PlainResp(
                StatusCode::INTERNAL_SERVER_ERROR,
                "service failure: failed to read kb.data",
            ));
        }
    };

    let tok = match GetOrLoadTokenizer(model).await {
        Ok(tok) => tok,
        Err(StatusCode::NOT_FOUND) => {
            return Ok(PlainResp(
                StatusCode::BAD_REQUEST,
                "service failure: model resolution failed",
            ));
        }
        Err(StatusCode::INTERNAL_SERVER_ERROR) => {
            return Ok(PlainResp(
                StatusCode::INTERNAL_SERVER_ERROR,
                "service failure: tokenizer loading failed",
            ));
        }
        Err(status) => return Ok(PlainResp(status, "service failure: tokenizer failed")),
    };

    let count = match tok.tokenizer.encode(&*kb_prompt, true) {
        Ok(encoded) => encoded.len(),
        Err(_) => {
            return Ok(PlainResp(
                StatusCode::INTERNAL_SERVER_ERROR,
                "service failure: tokenization failed",
            ));
        }
    };

    JsonResp(
        StatusCode::OK,
        &KbTokenCountResponse {
            kb_token_count: count,
        },
    )
}

pub async fn ModelsFuncCall(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    req: Request,
) -> SResult<Response, StatusCode> {
    let path = req.uri().path().to_string();
    let parts = path.split('/').collect::<Vec<&str>>();
    if parts.len() < 4 {
        return Ok(Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Body::from("Invalid path"))
            .unwrap());
    }

    let tenant = parts[2].to_owned();
    let namespace = parts[3].to_owned();

    let mut remainPath = "".to_string();
    if parts.len() > 4 {
        for i in 4..parts.len() {
            remainPath = remainPath + "/" + parts[i];
        }
    }

    if remainPath == "/v1/models" && req.method() == axum::http::Method::GET {
        let functions = match gw.objRepo.ListFunc(&tenant, &namespace) {
            Ok(funcs) => funcs,
            Err(e) => {
                return Ok(Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Body::from(format!(
                        "service failure: list models failed {:?}",
                        e
                    )))
                    .unwrap());
            }
        };

        let modelData: Vec<serde_json::Value> = functions
            .into_iter()
            .map(|f| {
                serde_json::json!({
                    "id": f.func.name,
                    "object": "model",
                })
            })
            .collect();

        let responseBody = serde_json::json!({
            "object": "list",
            "data": modelData
        });

        let bytes =
            serde_json::to_vec(&responseBody).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Body::from(bytes))
            .unwrap());
    } else if remainPath == "/health" && req.method() == axum::http::Method::GET {
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .body(Body::from("OK"))
            .unwrap());
    }

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

    let finalPath = format!(
        "/funccall/{}/{}/{}{}",
        tenant, namespace, modelName, remainPath
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

pub struct ParsedRequestFields {
    pub model: String,
    pub promptText: String,
    pub maxTokens: Option<u32>,
    pub temperature: Option<f32>,
    pub topP: Option<f32>,
    pub stream: bool,
}

fn ParseRequestFields(
    bodyBytes: &[u8],
    remainPath: &str,
    chatTemplate: Option<&str>,
    bosToken: &str,
) -> SResult<ParsedRequestFields, StatusCode> {
    if remainPath.starts_with("/v1/chat/completions") {
        let chatReq: ChatRequest =
            serde_json::from_slice(bodyBytes).map_err(|_| StatusCode::BAD_REQUEST)?;
        let promptText = if chatReq.messages.is_empty() {
            "".to_string()
        } else {
            ApplyChatTemplate(&chatReq.messages, chatTemplate, bosToken)
        };
        Ok(ParsedRequestFields {
            model: chatReq.model,
            promptText,
            maxTokens: chatReq.maxTokens,
            temperature: Some(chatReq.temperature),
            topP: Some(chatReq.topP),
            stream: chatReq.stream,
        })
    } else {
        let compReq: CompletionRequest =
            serde_json::from_slice(bodyBytes).map_err(|_| StatusCode::BAD_REQUEST)?;
        Ok(ParsedRequestFields {
            model: compReq.model,
            promptText: compReq.prompt,
            maxTokens: compReq.maxTokens,
            temperature: compReq.temperature,
            topP: compReq.topP,
            stream: compReq.stream,
        })
    }
}

fn BadResp(body: String) -> Response<Body> {
    Response::builder()
        .status(StatusCode::BAD_REQUEST)
        .body(Body::from(body))
        .unwrap()
}

/// Forward non-POST requests directly to the function call route without transformation.
async fn ForwardNonPostToFunc(
    token: &Arc<AccessToken>,
    gw: &HttpGateway,
    req: Request<Body>,
) -> SResult<Response<Body>, StatusCode> {
    let (mut reqParts, body) = req.into_parts();
    if !token.CheckScope("inference") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let pathParts: Vec<&str> = reqParts.uri.path().split('/').collect();
    if pathParts.len() < 5 {
        return Err(StatusCode::BAD_REQUEST);
    }
    let tenant = pathParts[2];
    let namespace = pathParts[3];
    let funcName = pathParts[4];
    if !token.IsNamespaceInferenceUser(tenant, namespace) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let _ = gw
        .funcAgentMgr
        .objRepo
        .GetFunc(tenant, namespace, funcName)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut remainPath = String::new();
    for i in 5..pathParts.len() {
        remainPath.push('/');
        remainPath.push_str(pathParts[i]);
    }

    let newUri = format!(
        "/funccall/{}/{}/{}{}",
        tenant, namespace, funcName, remainPath
    );
    reqParts.uri = Uri::try_from(newUri).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(FuncCall1(token, &gw, Request::from_parts(reqParts, body)).await?)
}

// ==================================================================
// TokenizerRoute handler
// ==================================================================

pub async fn TokenizerRoute(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    req: axum::http::Request<axum::body::Body>,
) -> SResult<Response<Body>, StatusCode> {
    if req.method() != axum::http::Method::POST {
        return ForwardNonPostToFunc(&token, &gw, req).await;
    }

    let ctx = match ResolveRouteContext(&token, &gw, req, true).await {
        Ok(c) => c,
        Err(status) => {
            return Ok(Response::builder()
                .status(status)
                .body(Body::from("service failure: validation failed"))
                .unwrap());
        }
    };

    let tok = match ctx.tokenizerState.clone() {
        Some(s) => s,
        None => match GetOrLoadTokenizer(&ctx.modelPath).await {
            Ok(s) => s,
            Err(_) => return Ok(BadResp("service failure: tokenizer unavailable".into())),
        },
    };

    let fields = match ParseRequestFields(
        &ctx.bodyBytes,
        &ctx.remainPath,
        tok.chatTemplate.as_deref(),
        &tok.bosToken,
    ) {
        Ok(f) => f,
        Err(_) => return Ok(BadResp("service failure: invalid JSON body".into())),
    };

    let promptIds = match tok.tokenizer.encode(&*fields.promptText, true) {
        Ok(encoded) => encoded.get_ids().to_vec(),
        Err(e) => {
            return Ok(BadResp(format!("tokenization error: {}", e)));
        }
    };

    let vllmReq = VllmCompletionRequest {
        model: fields.model,
        prompt: promptIds,
        maxTokens: fields.maxTokens,
        temperature: fields.temperature,
        topP: fields.topP,
        stream: fields.stream,
        skipSpecialTokens: true,
    };

    ForwardToFuncCall(
        &token,
        &gw,
        &ctx,
        "/v1/completions",
        &serde_json::to_value(&vllmReq).unwrap(),
    )
    .await
}

// ==================================================================
// Request normalization layer
// ==================================================================

pub struct NormalizedFuncRequest {
    pub target_path: String,
    pub body_bytes: Vec<u8>,
    pub content_type: &'static str,
}

pub async fn NormalizeFuncRequest(
    _gw: &HttpGateway,
    route: &FuncRouteTarget,
    remain_path: &str,
    body_bytes: &[u8],
) -> SResult<Option<NormalizedFuncRequest>, StatusCode> {
    if route.func.object.spec.SampleCallType() != ApiType::KnowledageBase {
        return Ok(None);
    }

    // Only load tokenizer when chat-template rendering is actually needed.
    // For /v1/completions the prompt is plain text; no tokenizer required.
    let tok_state = if remain_path.starts_with("/v1/chat/completions") {
        let model_path = match route.func.object.spec.ModelPath() {
            Some(p) => p,
            None => return Err(StatusCode::INTERNAL_SERVER_ERROR),
        };
        Some(GetOrLoadTokenizer(&model_path).await?)
    } else {
        None
    };

    let kb_file = format!(
        "{}/{}.{}.{}/{}/kb.data",
        KB_DIR,
        route.physical.tenant,
        route.physical.namespace,
        route.physical.funcname,
        route.physical.version
    );
    let kb_prompt = match std::fs::read_to_string(&kb_file) {
        Ok(p) => p,
        Err(e) => {
            error!("NormalizeFuncRequest: cannot read kb.data {}: {:?}", kb_file, e);
            String::new()
        }
    };

    let fields = ParseRequestFields(
        body_bytes,
        remain_path,
        tok_state.as_ref().and_then(|s| s.chatTemplate.as_deref()),
        tok_state.as_ref().map(|s| s.bosToken.as_str()).unwrap_or(""),
    )?;

    let raw_req = RawCompletionRequest {
        model: fields.model,
        prompt: kb_prompt + &fields.promptText,
        maxTokens: fields.maxTokens,
        temperature: fields.temperature,
        topP: fields.topP,
        stream: fields.stream,
        skipSpecialTokens: true,
    };

    let body_bytes = serde_json::to_vec(&raw_req).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    error!(
        "NormalizeFuncRequest forwarding KB request tenant={}/{} func={} source_path={} target_path=/v1/completions body={}",
        route.physical.tenant,
        route.physical.namespace,
        route.physical.funcname,
        remain_path,
        String::from_utf8_lossy(&body_bytes)
    );

    Ok(Some(NormalizedFuncRequest {
        target_path: "/v1/completions".to_string(),
        body_bytes,
        content_type: "application/json",
    }))
}

// ==================================================================
// KnowledgeBaseRoute handler - converts chat to completion without tokenization
// ==================================================================

pub async fn KnowledgeBaseRoute(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    req: axum::http::Request<axum::body::Body>,
) -> SResult<Response<Body>, StatusCode> {
    if req.method() != axum::http::Method::POST {
        return ForwardNonPostToFunc(&token, &gw, req).await;
    }

    let ctx = match ResolveRouteContext(&token, &gw, req, false).await {
        Ok(c) => c,
        Err(status) => {
            return Ok(Response::builder()
                .status(status)
                .body(Body::from("service failure: validation failed"))
                .unwrap());
        }
    };

    let fields = match ParseRequestFields(
        &ctx.bodyBytes,
        &ctx.remainPath,
        ctx.tokenizerState.as_ref().and_then(|s| s.chatTemplate.as_deref()),
        ctx.tokenizerState.as_ref().map(|s| s.bosToken.as_str()).unwrap_or(""),
    ) {
        Ok(f) => f,
        Err(_) => return Ok(BadResp("service failure: invalid JSON body".into())),
    };

    let promptText = match &ctx.kbPrompt {
        None => fields.promptText.clone(),
        Some(kbPrompt) => kbPrompt.clone() + &fields.promptText,
    };

    let rawReq = RawCompletionRequest {
        model: fields.model,
        prompt: promptText,
        maxTokens: fields.maxTokens,
        temperature: fields.temperature,
        topP: fields.topP,
        stream: fields.stream,
        skipSpecialTokens: true,
    };

    let kb_debug_body =
        serde_json::to_vec(&rawReq).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    error!(
        "KnowledgeBaseRoute forwarding KB request tenant={}/{} func={} source_path={} target_path=/v1/completions body={}",
        ctx.tenant,
        ctx.namespace,
        ctx.funcName,
        ctx.remainPath,
        String::from_utf8_lossy(&kb_debug_body)
    );

    ForwardToFuncCall(
        &token,
        &gw,
        &ctx,
        "/v1/completions",
        &serde_json::to_value(&rawReq).unwrap(),
    )
    .await
}
