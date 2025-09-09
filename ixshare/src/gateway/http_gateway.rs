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
use std::sync::atomic::AtomicI64;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::HeaderValue;
use axum::response::Response;
use axum::Json;
use axum::{
    body::Body, extract::Path, routing::delete, routing::get, routing::post, routing::put,
    Extension, Router,
};

use hyper::header::CONTENT_TYPE;
use inferxlib::obj_mgr::namespace_mgr::Namespace;
use inferxlib::obj_mgr::tenant_mgr::Tenant;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tower_http::cors::{Any, CorsLayer};

use axum_server::tls_rustls::RustlsConfig;
use http_body_util::BodyExt;
use hyper::body::Bytes;
use hyper::{StatusCode, Uri};

use tokio::sync::mpsc;

use crate::audit::{ReqAudit, SqlAudit, REQ_AUDIT_AGENT};
use crate::common::*;
use crate::gateway::auth_layer::auth_transform_keycloaktoken;
use crate::metastore::cacher_client::CacherClient;
use crate::metastore::unique_id::UID;
use crate::node_config::NODE_CONFIG;
use crate::peer_mgr::NA_CONFIG;
use crate::ixmeta::req_watching_service_client::ReqWatchingServiceClient;
use crate::ixmeta::ReqWatchRequest;
use inferxlib::data_obj::DataObject;
use inferxlib::obj_mgr::func_mgr::{ApiType, Function};

use super::auth_layer::{AccessToken, GetTokenCache};
use super::func_agent_mgr::FuncAgentMgr;
use super::gw_obj_repo::{GwObjRepo, NamespaceStore};
use super::secret::Apikey;

pub static GATEWAY_ID: AtomicI64 = AtomicI64::new(-1);

pub fn GatewayId() -> i64 {
    return GATEWAY_ID.load(std::sync::atomic::Ordering::Relaxed);
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
            .route("/apikey/", get(GetApikeys))
            .route("/apikey/", put(CreateApikey))
            .route("/apikey/", delete(DeleteApikey))
            .route("/object/", put(CreateObj))
            .route("/object/:type/:tenant/:namespace/:name/", delete(DeleteObj))
            .route("/funccall/*rest", post(PostCall))
            .route("/prompt/", post(PostPrompt))
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
            .with_state(self.clone())
            .layer(cors)
            .layer(axum::middleware::from_fn(auth_transform_keycloaktoken))
            .layer(auth_layer);

        let tlsconfig = NA_CONFIG.tlsconfig.clone();

        println!("tls config is {:#?}", &tlsconfig);
        if tlsconfig.enable {
            // configure certificate and private key used by https
            let config = RustlsConfig::from_pem_file(
                PathBuf::from(tlsconfig.certpath),
                PathBuf::from(tlsconfig.keypath),
            )
            .await
            .unwrap();

            let addr = SocketAddr::from(([0, 0, 0, 0], NA_CONFIG.gatewayPort));
            println!("listening on tls {}", &addr);
            axum_server::bind_rustls(addr, config)
                .serve(app.into_make_service())
                .await
                .unwrap();
        } else {
            let gatewayUrl = format!("0.0.0.0:{}", NA_CONFIG.gatewayPort);
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
    State(gw): State<HttpGateway>,
    Json(req): Json<PromptReq>,
) -> SResult<Response, StatusCode> {
    error!("PostPrompt req is {:?}", &req);
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

async fn PostCall(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    mut req: Request,
) -> SResult<Response, StatusCode> {
    let path = req.uri().path();

    let now = std::time::Instant::now();

    let parts = path.split("/").collect::<Vec<&str>>();

    let partsCount = parts.len();
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

    let (mut client, keepalive, _func) = match gw
        .funcAgentMgr
        .GetClient(&tenant, &namespace, &funcname)
        .await
    {
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .body(body)
                .unwrap();

            return Ok(resp);
        }
        Ok(client) => client,
    };

    let uri = format!("http://127.0.0.1{}", remainPath); // &func.object.spec.endpoint.path);
    *req.uri_mut() = Uri::try_from(uri).unwrap();

    let tcpConnLatency = now.elapsed().as_millis() as u64;
    let start = std::time::Instant::now();

    let mut first = true;

    let mut res = match client.Send(req).await {
        Err(e) => {
            let body = Body::from(format!("service failure {:?}", e));
            let resp = Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .body(body)
                .unwrap();

            return Ok(resp);
        }
        Ok(r) => r,
    };

    let mut kvs = Vec::new();
    for (k, v) in res.headers() {
        kvs.push((k.clone(), v.clone()));
    }

    let (tx, rx) = mpsc::channel::<SResult<Bytes, Infallible>>(128);
    let (ttftTx, mut ttftRx) = mpsc::channel::<u64>(1);
    tokio::spawn(async move {
        defer!(drop(client));
        loop {
            let frame = res.frame().await;
            let mut ttft = 0;
            if first {
                ttft = start.elapsed().as_millis() as u64;
                ttftTx.send(ttft).await.ok();

                first = false;
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
    let username = token.username.clone();
    error!("CreateApikey keyname {}", &obj.keyname);
    match GetTokenCache()
        .await
        .CreateApikey(&username, &obj.keyname)
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
    let username = token.username.clone();
    error!("DeleteApikey *** {:?}", &apikey);
    match GetTokenCache()
        .await
        .DeleteApiKey(&apikey.apikey, &username)
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
    let res = match dataobj.objType.as_str() {
        Tenant::KEY => gw.CreateTenant(&token, dataobj).await,
        Namespace::KEY => gw.CreateNamespace(&token, dataobj).await,
        Function::KEY => gw.CreateFunc(&token, dataobj).await,
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

    let res = match dataobj.objType.as_str() {
        Tenant::KEY => gw.UpdateTenant(&token, dataobj).await,
        Namespace::KEY => gw.UpdateNamespace(&token, dataobj).await,
        Function::KEY => gw.UpdateFunc(&token, dataobj).await,
        _ => gw.client.Create(&dataobj).await,
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
    State(gw): State<HttpGateway>,
    Path((objType, tenant, namespace, name)): Path<(String, String, String, String)>,
) -> SResult<Response, StatusCode> {
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
    // match gw
    //     .client
    //     .List("node_info", "system", "system", &ListOption::default())
    //     .await
    // {
    //     Ok(l) => {
    //         error!("GetNodes the nodes xxxx is {:#?}", &l);
    //     }
    //     Err(_) => (),
    // }

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
