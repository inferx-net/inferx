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
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::result::Result as SResult;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicI64;
use std::sync::Arc;

use inferxlib::obj_mgr::funcpolicy_mgr::FuncPolicy;
use opentelemetry::global::ObjectSafeSpan;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::trace::Tracer;
use opentelemetry::KeyValue;

use axum::extract::{Query, Request, State};
use axum::http::HeaderValue;
use axum::middleware::{from_fn_with_state, Next};
use axum::response::Response;
use axum::Json;
use axum::{
    body::Body, extract::Path, routing::delete, routing::get, routing::head, routing::post,
    routing::put, Extension, Router,
};

use hyper::header::CONTENT_TYPE;
use inferxlib::obj_mgr::namespace_mgr::Namespace;
use inferxlib::obj_mgr::tenant_mgr::{Tenant, SYSTEM_NAMESPACE, SYSTEM_TENANT};
use opentelemetry::Context;
use prometheus_client::encoding::text::encode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tower_http::cors::{Any, CorsLayer};
use chrono::{DateTime, Timelike, Utc};

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
use crate::ixmeta::req_watching_service_client::ReqWatchingServiceClient;
use crate::ixmeta::ReqWatchRequest;
use crate::metastore::cacher_client::CacherClient;
use crate::metastore::unique_id::UID;
use crate::node_config::{GatewayConfig, NODE_CONFIG};
use crate::peer_mgr::IxTcpClient;
use crate::print::{set_trace_logging, trace_logging_enabled};
use inferxlib::data_obj::DataObject;
use inferxlib::obj_mgr::func_mgr::{ApiType, Function};

use super::auth_layer::Grant;
use super::auth_layer::ObjectType;
use super::auth_layer::PermissionType;
use super::auth_layer::{AccessToken, GetTokenCache};
use super::func_agent_mgr::FuncAgentMgr;
use super::func_agent_mgr::IxTimestamp;
use super::func_agent_mgr::GW_OBJREPO;
use super::func_worker::QHttpCallClient;
use super::func_worker::RETRYABLE_HTTP_STATUS;
use super::gw_obj_repo::{GwObjRepo, NamespaceStore};
use super::metrics::FunccallLabels;
use super::metrics::Status;
use super::metrics::GATEWAY_METRICS;
use super::metrics::METRICS_REGISTRY;
use super::secret::Apikey;

pub static GATEWAY_ID: AtomicI64 = AtomicI64::new(-1);

lazy_static::lazy_static! {
    #[derive(Debug)]
    pub static ref GATEWAY_CONFIG: GatewayConfig = GatewayConfig::New(&NODE_CONFIG);
}

pub fn GatewayId() -> i64 {
    return GATEWAY_ID.load(std::sync::atomic::Ordering::Relaxed);
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
        "snapshot" => parts.get(2).copied(),
        "snapshots" => parts.get(2).copied(),
        "tenant" => parts.get(2).copied(),
        _ => None,
    }
}

fn tenant_quota_exceeded(gw: &HttpGateway, tenant: &str) -> Result<bool> {
    if tenant.is_empty() {
        return Ok(false);
    }

    match gw
        .objRepo
        .tenantMgr
        .Get(SYSTEM_TENANT, SYSTEM_NAMESPACE, tenant)
    {
        Ok(t) => Ok(t.object.status.quota_exceeded),
        Err(e) => Err(e.into()),
    }
}

fn quota_exceeded_response(tenant: &str) -> Response<Body> {
    let body = Body::from(format!(
        "service failure: tenant {} quota exceeded",
        tenant
    ));
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

fn enforce_tenant_quota_for_write(
    token: &Arc<AccessToken>,
    gw: &HttpGateway,
    tenant: &str,
) -> Option<Response<Body>> {
    if token.IsInferxAdmin() {
        return None;
    }

    match tenant_quota_exceeded(gw, tenant) {
        Ok(true) => return Some(quota_exceeded_response(tenant)),
        Ok(false) => {}
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

    match tenant_quota_exceeded(gw, tenant) {
        Ok(true) => {
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
        Ok(false) => {}
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
    let tenant = match tenant_from_path(req.uri().path()) {
        Some(t) if !t.is_empty() => t,
        _ => {
            trace!(
                "TenantQuotaGuard skip: no tenant in path {}",
                req.uri().path()
            );
            return Ok(next.run(req).await);
        }
    };

    let token = match req.extensions().get::<Arc<AccessToken>>() {
        Some(t) => t,
        None => {
            trace!(
                "TenantQuotaGuard skip: no token for tenant {} path {}",
                tenant,
                req.uri().path()
            );
            return Ok(next.run(req).await);
        }
    };

    if token.IsInferxAdmin() {
        trace!(
            "TenantQuotaGuard skip: inferx admin tenant {} path {}",
            tenant,
            req.uri().path()
        );
        return Ok(next.run(req).await);
    }

    if let Some(resp) = enforce_tenant_quota_for_request(
        token,
        &gw,
        tenant,
        req.method(),
        req.uri().path(),
    ) {
        trace!(
            "TenantQuotaGuard block: tenant {} path {}",
            tenant,
            req.uri().path()
        );
        return Ok(resp);
    }

    trace!(
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
    pub client: CacherClient,
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

        let auth_layer = NODE_CONFIG.keycloakconfig.AuthLayer();

        let app = Router::new()
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
            .route("/object/", put(CreateObj))
            .route("/object/:type/:tenant/:namespace/:name/", delete(DeleteObj))
            .route(
                "/readypods/:tenant/:namespace/:funcname/",
                get(ListReadyPods),
            )
            .route("/directfunccall/*rest", post(DirectFuncCall))
            .route("/directfunccall/*rest", get(DirectFuncCall))
            .route("/directfunccall/*rest", head(DirectFuncCall))
            .route("/funccall/*rest", post(FuncCall))
            .route("/funccall/*rest", get(FuncCall))
            .route("/funccall/*rest", head(FuncCall))
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
                get(GetFuncPod),
            )
            .route("/functions/:tenant/:namespace/", get(ListFuncBrief))
            .route(
                "/function/:tenant/:namespace/:funcname/",
                get(GetFuncDetail),
            )
            .route(
                "/snapshot/:tenant/:namespace/:snapshotname/",
                get(GetSnapshot),
            )
            .route("/snapshots/:tenant/:namespace/", get(GetSnapshots))
            .route("/tenant/:tenant/credits", post(AddTenantCredits))
            .route("/billing/rates", post(AddBillingRate))
            .route("/billing/rates", get(GetBillingRateHistory))
            .route("/billing/credits/history", get(GetBillingCreditHistory))
            .route("/tenant/:tenant/credits", get(GetTenantCredits))
            .route("/tenant/:tenant/credits/history", get(GetTenantCreditHistory))
            .route("/tenant/:tenant/billing-summary", get(GetTenantBillingSummary))
            .route("/tenant/:tenant/usage/hourly", get(GetTenantHourlyUsage))
            .route("/tenant/:tenant/usage/hourly-by-model", get(GetTenantHourlyUsageByModel))
            .route(
                "/tenant/:tenant/usage/hourly-by-namespace",
                get(GetTenantHourlyUsageByNamespace),
            )
            .route("/tenant/:tenant/usage/by-model", get(GetTenantUsageByModel))
            .route("/tenant/:tenant/usage/by-namespace", get(GetTenantUsageByNamespace))
            .route("/tenant/:tenant/usage/summary", get(GetTenantUsageSummary))
            .route("/metrics", get(GetMetrics))
            .route("/debug/trace_logging/:state", post(SetTraceLogging))
            .with_state(self.clone())
            .layer(cors)
            .layer(from_fn_with_state(self.clone(), TenantQuotaGuard))
            .layer(axum::middleware::from_fn(auth_transform_keycloaktoken))
            .layer(auth_layer);

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
    if let Some(resp) = enforce_tenant_quota_for_write(&token, &gw, &req.tenant) {
        return Ok(resp);
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
    map.insert("prompt".to_owned(), req.prompt.clone());

    if samplecall.apiType == ApiType::Image2Text {
        let image = req.image.clone();
        map.insert("image".to_owned(), image);
    }
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

    if !token.IsNamespaceUser(&tenant, &namespace) {
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
    tenant: &str,
    namespace: &str,
    funcname: &str,
    func: &Function,
    timeout: u64,
    timestamp: IxTimestamp,
) -> Result<(QHttpCallClient, bool)> {
    let mut _retry = 0;
    loop {
        match gw
            .funcAgentMgr
            .GetClient(&tenant, &namespace, &funcname, &func, timeout, timestamp)
            .await
        {
            Err(e) => {
                _retry += 1;
                if timestamp.Elapsed() < timeout {
                    // trace!(
                    //     "RetryGetClient retry {} {}/{}/{} timeout {}",
                    //     retry,
                    //     tenant,
                    //     namespace,
                    //     funcname,
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
    pub headers: http::HeaderMap,
    pub labels: FunccallLabels,
}

impl Disconnect {
    pub fn New(req: serde_json::Value, headers: http::HeaderMap, labels: &FunccallLabels) -> Self {
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
    mut req: Request,
) -> SResult<Response, StatusCode> {
    let reqStart = std::time::Instant::now();
    let headers = req.headers().clone();
    let path = req.uri().path();

    let tracer = opentelemetry::global::tracer("gateway");
    let mut ttftSpan = tracer.start("TTFT");
    ttftSpan.set_attribute(KeyValue::new("req", path.to_owned()));
    let ttftCtx = Context::current_with_span(ttftSpan);

    let now = std::time::Instant::now();

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
    let tenant = parts[2].to_owned();
    let namespace = parts[3].to_owned();
    let funcname = parts[4].to_owned();

    if !token.IsNamespaceUser(&tenant, &namespace) {
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

    let mut labels = FunccallLabels {
        tenant: tenant.clone(),
        namespace: namespace.clone(),
        funcname: funcname.clone(),
        status: StatusCode::OK.as_u16(),
    };

    let timestamp = IxTimestamp::default();
    let func = match gw
        .funcAgentMgr
        .objRepo
        .GetFunc(&tenant, &namespace, &funcname)
    {
        Ok(f) => f,
        Err(e) => {
            let errcode = StatusCode::INTERNAL_SERVER_ERROR;
            let body = Body::from(format!("service failure {:?}", &e));
            let resp = Response::builder().status(errcode).body(body).unwrap();
            return Ok(resp);
        }
    };

    let policy = GW_OBJREPO.get().unwrap().FuncPolicy(&func);

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

    let uri = format!("http://127.0.0.1{}", remainPath); // &func.object.spec.endpoint.path);
    *req.uri_mut() = Uri::try_from(uri).unwrap();

    let mut res;

    let (parts, body) = req.into_parts();

    // Collect the body bytes
    let bytes = match axum::body::to_bytes(body, 1024 * 1024).await {
        Err(_e) => {
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

    let json_req: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| StatusCode::BAD_REQUEST)?;
    // error!("FuncCall get req {:#?}", json_req);
    let disconnect = Disconnect::New(json_req.clone(), headers.clone(), &labels);

    let mut retry = 0;

    let mut error = Error::Timeout(timeout);
    let client;
    let keepalive;
    let mut tcpConnLatency;
    let mut start;
    loop {
        retry += 1;
        if timestamp.Elapsed() > timeout {
            error!("FuncCall 1");
            let resp = FailureResponse(error, &mut labels, Status::RequestFailure).await;
            ttftCtx.span().end();
            return Ok(resp);
        }

        // let mut startupSpan = tracer.start_with_context("startup", &ttftCtx);

        let (mut tclient, tkeepalive) = match RetryGetClient(
            &gw, &tenant, &namespace, &funcname, &func, timeout, timestamp,
        )
        .await
        {
            Err(e) => {
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

        // startupSpan.end();

        start = std::time::Instant::now();
        let body = axum::body::Body::from(bytes.clone());
        let req = Request::from_parts(parts.clone(), body);
        res = match tclient.Send(req).await {
            Err(e) => {
                // error!(
                //     "FuncCall fail {} retry {} with error {:?}",
                //     tclient.PodName(),
                //     retry,
                //     &e
                // );
                error = e;
                continue;
            }
            Ok(r) => {
                if retry > 1 {
                    error!(
                        "FuncCall retry success {} with try round {}",
                        func.Id(),
                        retry
                    );
                }
                r
            }
        };

        let status = res.status();

        if status != StatusCode::OK {
            let needRetry = RETRYABLE_HTTP_STATUS.contains(&(status.as_u16()));

            if needRetry {
                error!(
                    "Http call get fail status {:?} for pod {}",
                    status,
                    tclient.PodName()
                );
                continue;
            } else {
                // let text = String::from_utf8(bytes.to_vec()).ok();
                let resp = FailureResponse(
                    Error::BAD_REQUEST(status),
                    &mut labels,
                    Status::InvalidRequest,
                )
                .await;
                ttftCtx.span().end();
                return Ok(resp);
            }
        }

        client = tclient;
        keepalive = tkeepalive;
        break;
    }

    let mut first = true;

    labels.status = StatusCode::OK.as_u16();
    GATEWAY_METRICS
        .lock()
        .await
        .funccallcnt
        .get_or_create(&labels)
        .inc();

    let mut kvs = Vec::new();
    for (k, v) in res.headers() {
        kvs.push((k.clone(), v.clone()));
    }

    let mut bytecnt = 0;
    let mut framecount = 0;

    let (tx, rx) = mpsc::channel::<SResult<Bytes, Infallible>>(4096);
    let (ttftTx, mut ttftRx) = mpsc::channel::<u64>(1);
    disconnect.Cancel();
    tokio::spawn(async move {
        let _client = client;
        let mut ttft = 0;
        let mut total = 0;
        loop {
            let frame = res.frame().await;
            framecount += 1;
            if first {
                ttft = start.elapsed().as_millis() as u64;
                ttftTx.send(ttft).await.ok();

                ttftCtx.span().end();
                first = false;

                total = ttft + tcpConnLatency;
                // error!(
                //     "ttft is {} ms /total {} ms keepalive {}",
                //     ttft, total, keepalive
                // );
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
                        keepalive: keepalive,
                        ttft: ttft as i32,
                        latency: latency.as_millis() as i32,
                    });
                    // error!(
                    //     "Funccall ********* Pass: sendbytes finish with client connection takes {} ms ttft {} ms framecount {} retry count {} total send {} bytes headers {:#?} req {:#?}",
                    //     reqStart.elapsed().as_millis(),
                    //     total,
                    //     framecount,
                    //     retry-1,
                    //     bytecnt,
                    //     &headers,
                    //     json_req
                    // );
                    return;
                }
                Some(b) => match b {
                    Ok(b) => b,
                    Err(e) => {
                        error!(
                            "PostCall for path {}/{}/{} len {} get error {:?}",
                            tenant, namespace, funcname, bytecnt, e
                        );
                        return;
                    }
                },
            };
            let bytes: Bytes = bytes.into_data().unwrap();
            bytecnt += bytes.len();

            match tx.send(Ok(bytes)).await {
                Err(_) => {
                    error!(
                        "Funccall ********* Fail: sendbytes fail with client disconnect after return header {} ms ttft {} ms framecount {} retry count {} total send {} bytes headers {:#?} req {:#?}",
                        reqStart.elapsed().as_millis(),
                        total,
                        framecount,
                        retry-1,
                        bytecnt,
                        &headers,
                        json_req
                    );
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

    let val = HeaderValue::from_str(&format!("{}", tcpConnLatency)).unwrap();
    resp.headers_mut().insert("TCPCONN_LATENCY_HEADER", val);

    match ttftRx.recv().await {
        Some(ttft) => {
            let val = HeaderValue::from_str(&format!("{}", ttft)).unwrap();
            resp.headers_mut().insert("TTFT_LATENCY_HEADER", val);
        }
        None => (),
    };

    return Ok(resp);
}

pub const TCPCONN_LATENCY_HEADER: &'static str = "X-TcpConn-Latency";
pub const TTFT_LATENCY_HEADER: &'static str = "X-Ttft-Latency";

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
    Json(obj): Json<Apikey>,
) -> SResult<Response, StatusCode> {
    match GetTokenCache()
        .await
        .CreateApikey(&token, &obj.username, &obj.keyname)
        .await
    {
        Ok(apikey) => {
            let body = Body::from(format!("{:?}", apikey));
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
    Json(apikey): Json<Apikey>,
) -> SResult<Response, StatusCode> {
    error!("DeleteApikey *** {:?}", &apikey);
    match GetTokenCache()
        .await
        .DeleteApiKey(&token, &apikey.keyname, &apikey.username)
        .await
    {
        Ok(exist) => {
            if exist {
                let body = Body::from(format!("{:?}", apikey));
                let resp = Response::builder()
                    .status(StatusCode::OK)
                    .body(body)
                    .unwrap();
                return Ok(resp);
            } else {
                let body = Body::from(format!("apikey {:?} not exist ", apikey));
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
    if let Some(resp) = enforce_tenant_quota_for_write(&token, &gw, tenant_target) {
        return Ok(resp);
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
    if let Some(resp) = enforce_tenant_quota_for_write(&token, &gw, tenant_target) {
        return Ok(resp);
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
}

#[derive(Serialize)]
struct HourlyUsageResponse {
    usage: Vec<HourlyUsageRecord>,
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

fn BadRequest(msg: impl Into<String>) -> Response {
    Response::builder()
        .status(StatusCode::BAD_REQUEST)
        .body(Body::from(msg.into()))
        .unwrap()
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

fn BuildHourlyUsage(usage_records: Vec<(DateTime<Utc>, i64, i64, i64, i64, i64)>, start_hour: DateTime<Utc>, total_hours: i32) -> Vec<HourlyUsageRecord> {
    let mut usage_map: std::collections::HashMap<String, (i64, i64, i64, i64, i64)> = std::collections::HashMap::new();
    for (hour, charge_cents, inference_cents, standby_cents, inference_ms, standby_ms) in usage_records {
        let hour_key = hour.format("%Y-%m-%dT%H:00:00Z").to_string();
        usage_map.insert(hour_key, (charge_cents, inference_cents, standby_cents, inference_ms, standby_ms));
    }

    let mut usage: Vec<HourlyUsageRecord> = Vec::with_capacity(total_hours as usize);
    for i in 0..total_hours {
        let hour = start_hour + chrono::Duration::hours(i as i64);
        let hour_key = hour.format("%Y-%m-%dT%H:00:00Z").to_string();
        let (charge_cents, inference_cents, standby_cents, inference_ms, standby_ms) =
            usage_map.get(&hour_key).copied().unwrap_or((0, 0, 0, 0, 0));
        usage.push(HourlyUsageRecord {
            hour: hour_key,
            charge_cents,
            inference_cents,
            standby_cents,
            inference_ms,
            standby_ms,
        });
    }

    usage
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
        .sqlAudit
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
        let body = Body::from(format!("Unsupported currency: {}. Only USD is supported.", currency));
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
    match gw.sqlAudit.AddTenantCredit(
        &tenant,
        req.amount_cents,
        currency,
        req.note.as_deref(),
        req.payment_ref.as_deref(),
        added_by,
    ).await {
        Ok(id) => {
            error!("AddTenantCredits: credit added with id={}", id);
            let quota_exceeded = match gw.sqlAudit.RecalculateTenantQuota(&tenant).await {
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
                error!("AddTenantCredits: updating tenant quota_exceeded to {}", quota_exceeded);
                if let Err(e) = gw.client.Update(&tenant_obj.DataObject(), 0).await {
                    let body =
                        Body::from(format!("Failed to update tenant quota status: {:?}", e));
                    let resp = Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(body)
                        .unwrap();
                    return Ok(resp);
                }
            }

            // Get updated balance
            let balance = match gw.sqlAudit.GetTenantCreditBalance(&tenant).await {
                Ok(b) => b,
                Err(e) => {
                    error!("AddTenantCredits: GetTenantCreditBalance failed: {:?}", e);
                    0
                }
            };
            error!("AddTenantCredits: returning id={}, balance_cents={}", id, balance);
            let resp_body = AddCreditsResponse { success: true, credit_id: id, new_balance_cents: balance };
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
        .sqlAudit
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

    match gw.sqlAudit.GetTenantBillingSummary(&tenant).await {
        Ok((balance_cents, used_cents, _threshold, quota_exceeded, _total_credits, currency, _, _, _, _)) => {
            let resp_body = CreditResponse { balance_cents, used_cents, currency, quota_exceeded };
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
        let body = Body::from("Permission denied: Only InferxAdmin can read billing credit history");
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
        .sqlAudit
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
        .sqlAudit
        .ListTenantCreditHistory(&tenant, limit, offset)
        .await
    {
        Ok((records, total)) => match gw.sqlAudit.GetTenantCreditBalance(&tenant).await {
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

    match gw.sqlAudit.GetTenantBillingSummary(&tenant).await {
        Ok((balance_cents, used_cents, threshold_cents, quota_exceeded, total_credits_cents, currency,
            inference_cents, standby_cents, inference_ms, standby_ms)) => {
            let resp_body = BillingSummaryResponse {
                balance_cents,
                used_cents,
                threshold_cents,
                quota_exceeded,
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
        .sqlAudit
        .GetTenantHourlyUsageRange(&tenant, start_hour, end_hour)
        .await
    {
        Ok(records) => {
            let usage = BuildHourlyUsage(records, start_hour, total_hours);
            let resp_body = HourlyUsageResponse {
                usage,
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
        .sqlAudit
        .GetTenantUsageByModel(&tenant, start_hour, end_hour)
        .await
    {
        Ok((items, total)) => {
            let usage: Vec<ModelUsageItem> = items
                .into_iter()
                .map(|(funcname, namespace, inference_ms, standby_ms, inference_cents, standby_cents, charge_cents)| {
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
                })
                .collect();

            let (total_inference_ms, total_standby_ms, total_inference_cents, total_standby_cents, total_cents) = total;
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
        .sqlAudit
        .GetFuncHourlyUsageRange(&tenant, &namespace, &funcname, start_hour, end_hour)
        .await
    {
        Ok(records) => {
            let usage = BuildHourlyUsage(records, start_hour, total_hours);
            let resp_body = HourlyUsageResponse {
                usage,
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
        .sqlAudit
        .GetNamespaceHourlyUsageRange(&tenant, &namespace, start_hour, end_hour)
        .await
    {
        Ok(records) => {
            let usage = BuildHourlyUsage(records, start_hour, total_hours);
            let resp_body = HourlyUsageResponse {
                usage,
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
            let body = Body::from(format!(
                "Failed to get hourly usage by namespace: {:?}",
                e
            ));
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
        .sqlAudit
        .GetTenantUsageByNamespace(&tenant, start_hour, end_hour)
        .await
    {
        Ok((items, total)) => {
            let usage: Vec<NamespaceUsageItem> = items
                .into_iter()
                .map(|(namespace, inference_ms, standby_ms, inference_cents, standby_cents, charge_cents)| {
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
                })
                .collect();

            let (total_inference_ms, total_standby_ms, total_inference_cents, total_standby_cents, total_cents) = total;
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

    match gw.sqlAudit.GetTenantAnalyticsSummary(&tenant, hours).await {
        Ok((total_ms, total_cents, top_model_name, top_model_ms, top_namespace_name, top_namespace_ms, peak_hour, peak_hour_ms)) => {
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
    if let Some(resp) = enforce_tenant_quota_for_write(&token, &gw, tenant_target) {
        return Ok(resp);
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
    if let Some(resp) = enforce_tenant_quota_for_write(&token, &gw, tenant_target) {
        return Ok(resp);
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
