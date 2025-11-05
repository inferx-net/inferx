// Copyright (c) 2025 InferX Authors
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
// limitations under the License.

use std::result::Result as SResult;
use std::sync::Arc;

use inferxlib::obj_mgr::funcpolicy_mgr::FuncPolicy;
use inferxlib::selector::Selector;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::sync::Notify;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

use super::IxAggrStore::IxAggrStore;
use crate::common::*;
use crate::etcd::etcd_store::EtcdStore;
use crate::ixmeta;
use crate::ixmeta::req_watching_service_server::ReqWatchingServiceServer;
use crate::metastore::cache_store::CacheStore;
use crate::metastore::informer::EventHandler;
use crate::metastore::informer_factory::InformerFactory;
use crate::metastore::obj::*;
use crate::metastore::selection_predicate::*;
use crate::metastore::store::ThreadSafeStore;
use crate::metastore::svc_dir::SvcDir;
use crate::metastore::unique_id::Uid;
use crate::node_config::StateSvcConfig;
use crate::node_config::NODE_CONFIG;
use crate::pgsql::listener::Listener;
use crate::scheduler::scheduler_register::SchedulerInfo;
use crate::state_svc::statesvc_register::StateSvcRegister;
use inferxlib::data_obj::*;
use inferxlib::obj_mgr::func_mgr::FuncMgr;
use inferxlib::obj_mgr::func_mgr::FuncObject;
use inferxlib::obj_mgr::func_mgr::Function;
use inferxlib::obj_mgr::namespace_mgr::Namespace;
use inferxlib::obj_mgr::namespace_mgr::NamespaceMgr;
use inferxlib::obj_mgr::node_mgr::Node;
use inferxlib::obj_mgr::tenant_mgr::Tenant;
use inferxlib::obj_mgr::tenant_mgr::TenantMgr;
use inferxlib::obj_mgr::tenant_mgr::TenantObject;

lazy_static::lazy_static! {
    #[derive(Debug)]
    pub static ref STATESVC_CONFIG: StateSvcConfig = StateSvcConfig::New(&NODE_CONFIG);
    pub static ref ETCD_OBJECTS: Vec<&'static str> = vec![
        Node::KEY,
        Namespace::KEY,
        Function::KEY,
        Tenant::KEY,
        FuncPolicy::KEY,
        SchedulerInfo::KEY,
    ];
}

pub const VERSION: &str = "0.1";

#[derive(Debug, Clone)]
pub struct StateSvc {
    pub svcDir: SvcDir,
    pub store: EtcdStore,

    pub factory: InformerFactory,

    pub tenantMgr: TenantMgr,
    pub funcMgr: FuncMgr,
    pub namespaceMgr: NamespaceMgr,

    pub reqListener: Listener,
}

impl StateSvc {
    pub async fn New(etcdAddrs: &[String], auditdbAddr: &str) -> Result<Self> {
        let svcDir = SvcDir::default();
        let store = EtcdStore::NewWithEndpoints(etcdAddrs, true).await?;
        let channelRev = svcDir.read().unwrap().channelRev.clone();
        for i in 0..ETCD_OBJECTS.len() {
            let t = ETCD_OBJECTS[i];
            let c = CacheStore::New(Some(Arc::new(store.clone())), t, 0, &channelRev).await?;
            svcDir.write().unwrap().map.insert(t.to_string(), c);
        }

        let statsvcPort = STATESVC_CONFIG.stateSvcPort;

        let statsvcAddr = format!("http://localhost:{}", statsvcPort);
        let factory = InformerFactory::New(vec![statsvcAddr], "", "").await?;

        // namespaceSpec
        factory.AddInformer(Tenant::KEY, &ListOption::default())?;

        // namespaceSpec
        factory.AddInformer(Namespace::KEY, &ListOption::default())?;

        // funcSpec
        factory.AddInformer(Function::KEY, &ListOption::default())?;

        let reqListener = Listener::New(auditdbAddr, "ReqAudit_insert").await?;

        let svc = Self {
            svcDir: svcDir,
            store: store,

            factory: factory,

            tenantMgr: TenantMgr::default(),
            funcMgr: FuncMgr::default(),
            namespaceMgr: NamespaceMgr::default(),
            reqListener: reqListener,
        };

        svc.factory.AddEventHandler(Arc::new(svc.clone())).await?;
        error!("StateSvc new 7");
        return Ok(svc);
    }

    pub async fn Uid(&self) -> Result<i64> {
        return self.store.GetRev(&Uid::DataObject()).await;
    }

    pub async fn Process(&self) -> Result<()> {
        let notify = Arc::new(Notify::new());
        return self.factory.Process(notify.clone()).await;
    }

    pub fn GetFuncs(&self, tenant: &str, namespace: &str) -> Result<Vec<String>> {
        let ret = self.funcMgr.GetObjectKeys(tenant, namespace)?;
        return Ok(ret);
    }

    pub fn ProcessDeltaEvent(&self, event: &DeltaEvent) -> Result<()> {
        let obj = event.obj.clone();
        // error!(
        //     "ProcessDeltaEvent type {:?} obj is {:#?}",
        //     &event.type_, &obj
        // );
        match &event.type_ {
            EventType::Added => match &obj.objType as &str {
                Tenant::KEY => {
                    let tenant = obj.To::<TenantObject>()?;
                    self.tenantMgr.Add(tenant)?;
                }
                Function::KEY => {
                    let spec = obj.To::<FuncObject>()?;
                    self.funcMgr.Add(spec)?;
                }
                Namespace::KEY => {
                    let spec: Namespace = Namespace::FromDataObject(obj)?;
                    self.namespaceMgr.Add(spec)?;
                }
                _ => {
                    return Err(Error::CommonError(format!(
                        "NamespaceMgr::ProcessDeltaEvent {:?}",
                        event
                    )));
                }
            },
            EventType::Modified => {
                match &obj.objType as &str {
                    Function::KEY => {
                        let oldspec = event.oldObj.clone().unwrap().To::<FuncObject>()?;
                        // Function::FromDataObject(event.oldObj.clone().unwrap())?;
                        self.funcMgr.Remove(oldspec)?;
                        let spec = obj.To::<FuncObject>()?;
                        self.funcMgr.Add(spec)?;
                    }
                    Namespace::KEY => {
                        let spec: Namespace = Namespace::FromDataObject(obj)?;
                        self.namespaceMgr.Update(spec)?;
                    }
                    _ => {
                        return Err(Error::CommonError(format!(
                            "NamespaceMgr::ProcessDeltaEvent {:?}",
                            event
                        )));
                    }
                }
            }
            EventType::Deleted => match &obj.objType as &str {
                Function::KEY => {
                    let obj = event.oldObj.clone().unwrap();
                    let spec = obj.To::<FuncObject>()?;
                    self.funcMgr.Remove(spec)?;
                }
                Namespace::KEY => {
                    let obj = event.oldObj.clone().unwrap();
                    let namespace = Namespace::FromDataObject(obj)?;
                    self.namespaceMgr.Remove(namespace)?;
                }
                Tenant::KEY => {
                    let obj = event.oldObj.clone().unwrap();
                    let tenant = Tenant::FromDataObject(obj)?;
                    self.tenantMgr.Remove(tenant)?;
                }
                _ => {
                    return Err(Error::CommonError(format!(
                        "NamespaceMgr::ProcessDeltaEvent {:?}",
                        event
                    )));
                }
            },
            EventType::InitDone => {}
            _o => {
                return Err(Error::CommonError(format!(
                    "NamespaceMgr::ProcessDeltaEvent {:?}",
                    event
                )));
            }
        }

        return Ok(());
    }
}

use async_trait::async_trait;
#[async_trait]
impl EventHandler for StateSvc {
    async fn handle(&self, _store: &ThreadSafeStore, event: &DeltaEvent) {
        match self.ProcessDeltaEvent(event) {
            Err(e) => {
                error!(
                    "StateSvc::ProcessDeltaEvent fail {:?} for event {:#?}",
                    e, event
                );
            }
            Ok(()) => (),
        }
    }
}

#[tonic::async_trait]
impl ixmeta::req_watching_service_server::ReqWatchingService for StateSvc {
    type WatchStream =
        std::pin::Pin<Box<dyn futures::Stream<Item = SResult<ixmeta::ReqEvent, Status>> + Send>>;

    async fn watch(
        &self,
        _request: Request<ixmeta::ReqWatchRequest>,
    ) -> SResult<Response<Self::WatchStream>, Status> {
        let (tx, rx) = mpsc::channel(200);
        let stream = ReceiverStream::new(rx);

        fn predicate(_v: &serde_json::value::Value) -> bool {
            return true;
        }
        let mut watcher = self.reqListener.Watch(predicate).await.unwrap();
        tokio::spawn(async move {
            loop {
                let v = watcher.Recv().await.unwrap();
                let event = ixmeta::ReqEvent {
                    value: v.to_string(),
                };
                tx.send(Ok(event)).await.ok();
            }
        });
        return Ok(Response::new(Box::pin(stream) as Self::WatchStream));
    }
}

#[tonic::async_trait]
impl ixmeta::ix_meta_service_server::IxMetaService for StateSvc {
    async fn version(
        &self,
        request: Request<ixmeta::VersionRequestMessage>,
    ) -> SResult<Response<ixmeta::VersionResponseMessage>, Status> {
        error!("Request from {:?}", request.remote_addr());

        let response = ixmeta::VersionResponseMessage {
            version: VERSION.to_string(),
        };
        Ok(Response::new(response))
    }

    async fn create(
        &self,
        request: Request<ixmeta::CreateRequestMessage>,
    ) -> SResult<Response<ixmeta::CreateResponseMessage>, Status> {
        // todo: check namespace and authz
        let req = request.get_ref();

        let dataobj: DataObject<Value> = match &req.obj {
            None => {
                return Ok(Response::new(ixmeta::CreateResponseMessage {
                    error: format!("create error: invalid request"),
                    revision: 0,
                }))
            }
            Some(o) => o.into(),
        };

        match self.CreateObjCheck(&dataobj) {
            Err(e) => {
                return Ok(Response::new(ixmeta::CreateResponseMessage {
                    error: format!("create error: {:?}", e),
                    revision: 0,
                }))
            }
            Ok(()) => (),
        }

        let o = match self.store.Create(&dataobj, 0).await {
            Err(e) => {
                return Ok(Response::new(ixmeta::CreateResponseMessage {
                    error: format!("create error: {:?}", e),
                    revision: 0,
                }))
            }
            Ok(o) => o,
        };
        return Ok(Response::new(ixmeta::CreateResponseMessage {
            error: "".into(),
            revision: o.revision,
        }));
    }

    async fn update(
        &self,
        request: Request<ixmeta::UpdateRequestMessage>,
    ) -> SResult<Response<ixmeta::UpdateResponseMessage>, Status> {
        let req = request.get_ref();
        let dataobj: DataObject<Value> = match &req.obj {
            None => {
                return Ok(Response::new(ixmeta::UpdateResponseMessage {
                    error: format!("update error: invalid request"),
                    revision: 0,
                }))
            }
            Some(o) => o.into(),
        };

        match self.UpdateObjCheck(&dataobj) {
            Err(e) => {
                return Ok(Response::new(ixmeta::UpdateResponseMessage {
                    error: format!("update error: {:?}", e),
                    revision: 0,
                }))
            }
            Ok(()) => (),
        }

        let o = match self.store.Update(req.expect_rev, &dataobj, 0).await {
            Err(e) => {
                return Ok(Response::new(ixmeta::UpdateResponseMessage {
                    error: format!("create error: {:?}", e),
                    revision: 0,
                }))
            }
            Ok(o) => o,
        };
        return Ok(Response::new(ixmeta::UpdateResponseMessage {
            error: "".into(),
            revision: o.revision,
        }));
    }

    async fn delete(
        &self,
        request: Request<ixmeta::DeleteRequestMessage>,
    ) -> SResult<Response<ixmeta::DeleteResponseMessage>, Status> {
        let req = request.get_ref();

        let cacher = match self.svcDir.GetCacher(&req.obj_type) {
            None => {
                return Ok(Response::new(ixmeta::DeleteResponseMessage {
                    error: format!("statesvc doesn't support obj type {}", &req.obj_type),
                    revision: 0,
                }))
            }
            Some(c) => c,
        };

        let key = format!(
            "{}/{}/{}/{}",
            &req.obj_type, &req.tenant, &req.namespace, &req.name
        );

        match cacher
            .Get(&req.tenant, &req.namespace, &req.name, req.expect_rev)
            .await
        {
            Err(e) => {
                return Ok(Response::new(ixmeta::DeleteResponseMessage {
                    error: format!("Object delete Fail: {:?}", e),
                    revision: 0,
                }));
            }
            Ok(o) => {
                if o.is_none() {
                    return Ok(Response::new(ixmeta::DeleteResponseMessage {
                        error: format!("delete object {:?} Fail not exist", &key),
                        revision: 0,
                    }));
                }
            }
        }

        match self.store.Delete(&key, req.expect_rev).await {
            Err(e) => {
                return Ok(Response::new(ixmeta::DeleteResponseMessage {
                    error: format!("delete error: {:?}", e),
                    revision: 0,
                }));
            }
            Ok(rev) => {
                return Ok(Response::new(ixmeta::DeleteResponseMessage {
                    error: "".into(),
                    revision: rev,
                }));
            }
        };
    }

    async fn uid(
        &self,
        _request: Request<ixmeta::UidRequestMessage>,
    ) -> SResult<Response<ixmeta::UidReponseMessage>, Status> {
        match self.Uid().await {
            Err(e) => {
                return Ok(Response::new(ixmeta::UidReponseMessage {
                    error: format!("Fail: {:?}", e),
                    uid: 0,
                }));
            }
            Ok(uid) => {
                return Ok(Response::new(ixmeta::UidReponseMessage {
                    error: "".to_owned(),
                    uid: uid,
                }));
            }
        }
    }

    async fn get(
        &self,
        request: Request<ixmeta::GetRequestMessage>,
    ) -> SResult<Response<ixmeta::GetResponseMessage>, Status> {
        let req = request.get_ref();
        let cacher = match self.svcDir.GetCacher(&req.obj_type) {
            None => {
                return Ok(Response::new(ixmeta::GetResponseMessage {
                    error: format!("statesvc doesn't support obj type {}", &req.obj_type),
                    obj: None,
                }))
            }
            Some(c) => c,
        };

        match cacher
            .Get(&req.tenant, &req.namespace, &req.name, req.revision)
            .await
        {
            Err(e) => {
                return Ok(Response::new(ixmeta::GetResponseMessage {
                    error: format!("Fail: {:?}", e),
                    obj: None,
                }));
            }
            Ok(o) => {
                return Ok(Response::new(ixmeta::GetResponseMessage {
                    error: "".into(),
                    obj: match o {
                        None => None,
                        Some(o) => Some(o.Obj()),
                    },
                }));
            }
        }
    }

    async fn list(
        &self,
        request: Request<ixmeta::ListRequestMessage>,
    ) -> SResult<Response<ixmeta::ListResponseMessage>, Status> {
        let req = request.get_ref();
        let cacher = match self.svcDir.GetCacher(&req.obj_type) {
            None => {
                return Ok(Response::new(ixmeta::ListResponseMessage {
                    error: format!(
                        "statesvc list doesn't support obj type {}, supports {:?}",
                        &req.obj_type,
                        self.svcDir.ObjectTypes()
                    ),
                    revision: 0,
                    objs: Vec::new(),
                }))
            }
            Some(c) => c,
        };
        let labelSelector = match Selector::Parse(&req.label_selector) {
            Err(e) => {
                return Ok(Response::new(ixmeta::ListResponseMessage {
                    error: format!("Fail: {:?}", e),
                    ..Default::default()
                }))
            }
            Ok(s) => s,
        };
        let fieldSelector = match Selector::Parse(&req.field_selector) {
            Err(e) => {
                return Ok(Response::new(ixmeta::ListResponseMessage {
                    error: format!("Fail: {:?}", e),
                    ..Default::default()
                }))
            }
            Ok(s) => s,
        };

        let opts = ListOption {
            revision: req.revision,
            revisionMatch: RevisionMatch::Exact,
            predicate: SelectionPredicate {
                label: labelSelector,
                field: fieldSelector,
                limit: 00,
                continue_: None,
            },
        };

        match cacher.List(&req.tenant, &req.namespace, &opts).await {
            Err(e) => {
                return Ok(Response::new(ixmeta::ListResponseMessage {
                    error: format!("Fail: {:?}", e),
                    ..Default::default()
                }))
            }
            Ok(resp) => {
                let mut objs = Vec::new();
                for o in resp.objs {
                    objs.push(o.Obj());
                }
                return Ok(Response::new(ixmeta::ListResponseMessage {
                    error: "".into(),
                    revision: resp.revision,
                    objs: objs,
                }));
            }
        }
    }

    type WatchStream =
        std::pin::Pin<Box<dyn futures::Stream<Item = SResult<ixmeta::WEvent, Status>> + Send>>;

    async fn watch(
        &self,
        request: Request<ixmeta::WatchRequestMessage>,
    ) -> SResult<Response<Self::WatchStream>, Status> {
        let (tx, rx) = mpsc::channel(200);
        let stream = ReceiverStream::new(rx);

        let svcDir = self.svcDir.clone();
        tokio::spawn(async move {
            let req = request.get_ref();
            let cacher = match svcDir.GetCacher(&req.obj_type) {
                None => {
                    tx.send(Err(Status::invalid_argument(&format!(
                        "svcdir watch doesn't support obj type {}",
                        &req.obj_type
                    ))))
                    .await
                    .ok();
                    return;
                }
                Some(c) => c,
            };

            let labelSelector = match Selector::Parse(&req.label_selector) {
                Err(e) => {
                    tx.send(Err(Status::invalid_argument(&format!("Fail: {:?}", e))))
                        .await
                        .ok();

                    return;
                }
                Ok(s) => s,
            };
            let fieldSelector = match Selector::Parse(&req.field_selector) {
                Err(e) => {
                    tx.send(Err(Status::invalid_argument(&format!("Fail: {:?}", e))))
                        .await
                        .ok();
                    return;
                }
                Ok(s) => s,
            };

            let predicate = SelectionPredicate {
                label: labelSelector,
                field: fieldSelector,
                limit: 00,
                continue_: None,
            };

            match cacher.Watch(&req.namespace, req.revision, predicate) {
                Err(e) => {
                    tx.send(Err(Status::invalid_argument(&format!("Fail: {:?}", e))))
                        .await
                        .ok();
                    return;
                }
                Ok(mut w) => loop {
                    let event = w.stream.recv().await;
                    match event {
                        None => return,
                        Some(event) => {
                            let eventType = match event.type_ {
                                EventType::None => 0,
                                EventType::Added => 1,
                                EventType::Modified => 2,
                                EventType::Deleted => 3,
                                EventType::InitDone => 4,
                                EventType::Error(s) => {
                                    tx.send(Err(Status::invalid_argument(&format!(
                                        "Fail: {:?}",
                                        s
                                    ))))
                                    .await
                                    .ok();
                                    return;
                                }
                            };

                            let we = ixmeta::WEvent {
                                event_type: eventType,
                                obj: Some(event.obj.Obj()),
                            };
                            match tx.send(Ok(we)).await {
                                Ok(()) => (),
                                Err(e) => {
                                    tx.send(Err(Status::invalid_argument(&format!(
                                        "Fail: {:?}",
                                        e
                                    ))))
                                    .await
                                    .ok();
                                    return;
                                }
                            }
                        }
                    }
                },
            }
        });

        return Ok(Response::new(Box::pin(stream) as Self::WatchStream));
    }
}

pub async fn StateService(notify: Option<Arc<Notify>>) -> Result<()> {
    use crate::ixmeta::ix_meta_service_server::IxMetaServiceServer;
    use tonic::transport::Server;

    info!("StateService config {:#?}", *STATESVC_CONFIG);
    let stateSvc = StateSvc::New(&STATESVC_CONFIG.etcdAddrs, &STATESVC_CONFIG.auditdbAddr).await?;

    let stateSvcRegister =
        StateSvcRegister::New(&STATESVC_CONFIG.etcdAddrs, "ss", "0.0.0.0", 8890).await?;

    error!("StateService 1");
    let nodeagentAggrStore = IxAggrStore::New(&stateSvc.svcDir.ChannelRev()).await?;
    stateSvc.svcDir.AddCacher(nodeagentAggrStore.NodeStore());
    stateSvc.svcDir.AddCacher(nodeagentAggrStore.PodStore());
    stateSvc
        .svcDir
        .AddCacher(nodeagentAggrStore.SnapshotStore());
    let nodeagentAggrStoreFuture = nodeagentAggrStore.Process();
    let stateSvcAddr = format!("0.0.0.0:{}", STATESVC_CONFIG.stateSvcPort);
    error!("StateService 2");
    let stateSvcFuture = Server::builder()
        .add_service(IxMetaServiceServer::new(stateSvc.clone()))
        .add_service(ReqWatchingServiceServer::new(stateSvc.clone()))
        .serve(stateSvcAddr.parse().unwrap());
    error!("StateService 3");
    match notify {
        Some(n) => {
            n.notify_waiters();
        }
        None => (),
    }

    info!("StateService started ...");
    tokio::select! {
        _ = stateSvcFuture => {
            info!("stateSvcFuture finish...");
        }
        ret = nodeagentAggrStoreFuture => {
            info!("nodeagentAggrStoreFuture finish... {:?}", ret);
        }
        ret = stateSvcRegister.Process(&nodeagentAggrStore) => {
            info!("stateSvcRegister finish... {:?}", ret);
        }
        ret = stateSvc.Process() => {
            info!("stateSvc process finish... {:?}", ret);
        }
    }

    Ok(())
}
