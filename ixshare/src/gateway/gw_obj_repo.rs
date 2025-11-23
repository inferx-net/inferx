// Copyright (c) 2025 InferX Authors / 2014 The Kubernetes Authors
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

use core::ops::Deref;
use inferxlib::obj_mgr::funcpolicy_mgr::FuncPolicy;
use inferxlib::obj_mgr::funcpolicy_mgr::FuncPolicyMgr;
use inferxlib::obj_mgr::funcpolicy_mgr::FuncPolicySpec;
use inferxlib::obj_mgr::pod_mgr::PodState;
use inferxlib::obj_mgr::tenant_mgr::Tenant;
use inferxlib::obj_mgr::tenant_mgr::TenantMgr;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::sync::Notify;

use crate::gateway::scheduler_client::SCHEDULER_CLIENT;
use crate::metastore::informer::EventHandler;
use crate::metastore::informer_factory::InformerFactory;
use crate::metastore::selection_predicate::ListOption;
use crate::metastore::store::ThreadSafeStore;
use crate::na;
use inferxlib::data_obj::*;
use inferxlib::obj_mgr::funcsnapshot_mgr::ContainerSnapshot;
use inferxlib::obj_mgr::funcsnapshot_mgr::FuncSnapshot;
use inferxlib::obj_mgr::funcsnapshot_mgr::FuncSnapshotMgr;
use inferxlib::obj_mgr::node_mgr::Node;
use inferxlib::obj_mgr::node_mgr::NodeMgr;
use inferxlib::obj_mgr::pod_mgr::FuncPod;

use crate::common::*;
use crate::etcd::etcd_store::EtcdStore;
use crate::scheduler::scheduler_register::SchedulerInfo;
use inferxlib::obj_mgr::func_mgr::*;
use inferxlib::obj_mgr::namespace_mgr::*;
use inferxlib::obj_mgr::pod_mgr::PodMgr;

use super::func_agent_mgr::FuncAgentMgr;
use super::http_gateway::GATEWAY_CONFIG;

lazy_static::lazy_static! {
    pub static ref SCHEDULER_URL : Mutex<Option<String>> = Mutex::new(None);
}

#[derive(Serialize, Deserialize, Debug)]
pub struct GatewayInfo {
    pub name: String,
}

impl GatewayInfo {
    pub const KEY: &'static str = "gateway";
    pub fn New(name: &str) -> Self {
        return Self {
            name: name.to_owned(),
        };
    }

    pub fn DataObject(&self) -> DataObject<Value> {
        let s = serde_json::to_string_pretty(&self).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();

        let inner = DataObject {
            objType: Self::KEY.to_owned(),
            namespace: "system".to_owned(),
            name: Self::KEY.to_owned(),
            object: v,
            ..Default::default()
        };

        return inner.into();
    }

    pub fn FromDataObject(obj: DataObject<Value>) -> Result<Self> {
        let info = match serde_json::from_value::<Self>(obj.object.clone()) {
            Err(e) => {
                return Err(Error::CommonError(format!(
                    "SchedulerInfo::FromDataObject {:?}",
                    e
                )))
            }
            Ok(s) => s,
        };
        return Ok(info);
    }
}

#[derive(Debug)]
pub struct GwObjRepoInner {
    pub nodeMgr: NodeMgr,
    pub funcMgr: FuncMgr,
    pub tenantMgr: TenantMgr,
    pub namespaceMgr: NamespaceMgr,
    pub podMgr: PodMgr,
    pub snapshotMgr: FuncSnapshotMgr,
    pub funcpolicyMgr: FuncPolicyMgr,

    pub factory: InformerFactory,

    pub nodeListDone: AtomicBool,
    pub tenantListDone: AtomicBool,
    pub namespaceListDone: AtomicBool,
    pub funcListDone: AtomicBool,
    pub podListDone: AtomicBool,
    pub snapshotListDone: AtomicBool,
    pub schedulerListDone: AtomicBool,
    pub funcpolicyListDone: AtomicBool,
    pub listDone: AtomicBool,

    pub funcAgentMgr: Mutex<Option<FuncAgentMgr>>,
}

enum ListType {
    node,
    tenant,
    namespace,
    func,
    pod,
    // node,
    scheduler,
    snapshot,
    funcpolicy,
}

#[derive(Debug, Clone)]
pub struct GwObjRepo(Arc<GwObjRepoInner>);

impl Deref for GwObjRepo {
    type Target = Arc<GwObjRepoInner>;

    fn deref(&self) -> &Arc<GwObjRepoInner> {
        &self.0
    }
}

impl GwObjRepo {
    pub async fn New(addresses: Vec<String>) -> Result<Self> {
        let factory = InformerFactory::New(addresses, "", "").await?;

        // namespaceSpec
        factory.AddInformer(Node::KEY, &ListOption::default())?;

        // TenantSpec
        factory.AddInformer(Tenant::KEY, &ListOption::default())?;

        // namespaceSpec
        factory.AddInformer(Namespace::KEY, &ListOption::default())?;

        // funcSpec
        factory.AddInformer(Function::KEY, &ListOption::default())?;

        // pod
        factory.AddInformer(FuncPod::KEY, &ListOption::default())?;

        // funcsnapshot
        factory.AddInformer(ContainerSnapshot::KEY, &ListOption::default())?;

        // scheduler
        factory.AddInformer(SchedulerInfo::KEY, &ListOption::default())?;

        // funcpolicy
        factory.AddInformer(FuncPolicy::KEY, &ListOption::default())?;

        let inner = GwObjRepoInner {
            nodeMgr: NodeMgr::default(),
            tenantMgr: TenantMgr::default(),
            funcMgr: FuncMgr::default(),
            namespaceMgr: NamespaceMgr::default(),
            podMgr: PodMgr::default(),
            snapshotMgr: FuncSnapshotMgr::default(),
            funcpolicyMgr: FuncPolicyMgr::default(),
            factory: factory,

            tenantListDone: AtomicBool::new(false),
            namespaceListDone: AtomicBool::new(false),
            funcListDone: AtomicBool::new(false),
            podListDone: AtomicBool::new(false),
            nodeListDone: AtomicBool::new(false),
            snapshotListDone: AtomicBool::new(false),
            schedulerListDone: AtomicBool::new(false),
            funcpolicyListDone: AtomicBool::new(false),
            listDone: AtomicBool::new(false),
            funcAgentMgr: Mutex::new(None),
        };

        let mgr = Self(Arc::new(inner));
        mgr.factory.AddEventHandler(Arc::new(mgr.clone())).await?;

        return Ok(mgr);
    }

    pub fn FuncPolicy(&self, func: &Function) -> FuncPolicySpec {
        let p = &func.object.spec.policy;
        match p {
            ObjRef::Obj(p) => return p.clone(),
            ObjRef::Link(l) => {
                if l.objType != FuncPolicy::KEY {
                    return FuncPolicySpec::default();
                    // return Err(Error::CommonError(format!(
                    //     "FuncStatus::FuncPolicy for policy {} fail invalic link type {}",
                    //     l.Key(),
                    //     l.objType
                    // )));
                }

                match self.funcpolicyMgr.Get(&func.tenant, &l.namespace, &l.name) {
                    Err(_) => {
                        return FuncPolicySpec::default();
                    }
                    Ok(p) => return p.object,
                }
            }
        }
    }

    pub const LEASE_TTL: i64 = 1; // seconds

    pub async fn Process(&self) -> Result<()> {
        let store = EtcdStore::NewWithEndpoints(&GATEWAY_CONFIG.etcdAddrs, false).await?;

        let leaseId = store.LeaseGrant(Self::LEASE_TTL).await?;

        Self::GetLeader(&store, leaseId).await?;

        let notify = Arc::new(Notify::new());
        return self.factory.Process(notify.clone()).await;
    }

    pub async fn GetLeader(store: &EtcdStore, leaseId: i64) -> Result<()> {
        let info = GatewayInfo::New(&GATEWAY_CONFIG.nodeName);
        loop {
            match store.Create(&info.DataObject(), leaseId).await {
                Ok(_) => break,
                Err(_) => (),
            }

            store.LeaseKeepalive(leaseId).await?;
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }

        return Ok(());
    }

    pub async fn KeepLeader(store: &EtcdStore, leaseId: i64) -> Result<()> {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {
                    // keepalive for each 500 ms
                    store.LeaseKeepalive(leaseId).await?;
                }
            }
        }
    }

    pub fn ListDone(&self) -> bool {
        return self.listDone.load(std::sync::atomic::Ordering::Relaxed);
    }

    fn SetListDone(&self, type_: ListType) {
        match type_ {
            ListType::node => {
                self.nodeListDone
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            ListType::tenant => {
                self.tenantListDone
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            ListType::namespace => {
                self.namespaceListDone
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            ListType::func => {
                self.funcListDone
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            ListType::pod => {
                self.podListDone
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            ListType::scheduler => {
                self.schedulerListDone
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            ListType::snapshot => {
                self.snapshotListDone
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            ListType::funcpolicy => {
                self.funcpolicyListDone
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            } // ListType::node => {
              //     self.nodeListDone
              //         .store(true, std::sync::atomic::Ordering::SeqCst);
              // }
        }

        if self
            .namespaceListDone
            .load(std::sync::atomic::Ordering::Relaxed)
            && self
                .tenantListDone
                .load(std::sync::atomic::Ordering::Relaxed)
            && self.nodeListDone.load(std::sync::atomic::Ordering::Relaxed)
            && self.funcListDone.load(std::sync::atomic::Ordering::Relaxed)
            && self.podListDone.load(std::sync::atomic::Ordering::Relaxed)
            && self
                .snapshotListDone
                .load(std::sync::atomic::Ordering::Relaxed)
            && self
                .schedulerListDone
                .load(std::sync::atomic::Ordering::Relaxed)
            && self
                .funcpolicyListDone
                .load(std::sync::atomic::Ordering::Relaxed)
        // && self.nodeListDone.load(std::sync::atomic::Ordering::Relaxed)
        {
            self.listDone.store(true, Ordering::SeqCst);
        }
    }

    // pub fn ContainsNamespace(&self, tenant: &str, namespace: &str) -> bool {
    //     return self.namespaceMgr.Contains(tenant, namespace);
    // }

    pub fn SetFuncAgentMgr(&self, funcAgentMgr: &FuncAgentMgr) {
        *self.funcAgentMgr.lock().unwrap() = Some(funcAgentMgr.clone());
    }

    pub fn FuncAgentMgr(&self) -> FuncAgentMgr {
        return self.funcAgentMgr.lock().unwrap().clone().unwrap();
    }

    pub fn ContainsFunc(&self, tenant: &str, namespace: &str, name: &str) -> Result<bool> {
        // if !self.ContainsNamespace(tenant, namespace) {
        //     return Err(Error::NotExist(format!(
        //         "ContainersFunc has no namespace {}/{}",
        //         tenant, namespace
        //     )));
        // }

        match self.GetFunc(tenant, namespace, name) {
            Err(_) => return Ok(false),
            Ok(_) => return Ok(true),
        }
    }

    pub fn GetFunc(&self, tenant: &str, namespace: &str, name: &str) -> Result<Function> {
        let ret = self.funcMgr.Get(tenant, namespace, name)?;
        return Ok(ret);
    }

    pub fn GetFuncs(&self, tenant: &str, namespace: &str) -> Result<Vec<Function>> {
        let ret = self.funcMgr.GetObjects(tenant, namespace)?;
        return Ok(ret);
    }

    pub fn GetSnapshot(&self, tenant: &str, namespace: &str, name: &str) -> Result<FuncSnapshot> {
        let ret = self.snapshotMgr.Get(tenant, namespace, name)?;
        return Ok(ret);
    }

    pub fn GetSnapshots(&self, tenant: &str, namespace: &str) -> Result<Vec<FuncSnapshot>> {
        let ret = self.snapshotMgr.GetObjects(tenant, namespace)?;
        return Ok(ret);
    }

    pub fn AddFunc(&self, spec: Function) -> Result<()> {
        self.funcMgr.Add(spec)?;

        return Ok(());
    }

    pub fn UpdateFunc(&self, spec: Function) -> Result<()> {
        self.funcMgr.Update(spec)?;

        return Ok(());
    }

    pub fn RemoveFunc(&self, spec: Function) -> Result<()> {
        self.funcMgr.Remove(spec)?;
        return Ok(());
    }

    pub fn GetFuncPods(
        &self,
        tenant: &str,
        namespace: &str,
        funcName: &str,
    ) -> Result<Vec<FuncPod>> {
        let ret = self
            .podMgr
            .GetObjectsByPrefix(tenant, namespace, funcName)?;
        return Ok(ret);
    }

    pub fn GetFuncPod(&self, tenant: &str, namespace: &str, podname: &str) -> Result<FuncPod> {
        let ret = self.podMgr.Get(tenant, namespace, podname)?;
        return Ok(ret);
    }

    pub fn GetNodes(&self) -> Result<Vec<Node>> {
        let ret = self.nodeMgr.GetObjects("", "")?;
        return Ok(ret);
    }

    pub fn GetNode(&self, nodename: &str) -> Result<Node> {
        let ret = self.nodeMgr.Get("system", "system", nodename)?;
        return Ok(ret);
    }
    // after ListDone, process all objs in a batch
    pub async fn InitState(&self) -> Result<()> {
        for s in self.factory.GetInformer(SchedulerInfo::KEY)?.store.List() {
            let SchedulerInfo = match SchedulerInfo::FromDataObject(s.clone()) {
                Err(e) => {
                    error!("can't deserilize obj {:?} with error {:?}", s, e);
                    continue;
                }
                Ok(s) => s,
            };

            info!(
                "********************EventType::InitState scheduler set url {}...************",
                SchedulerInfo.SchedulerUrl()
            );

            match SCHEDULER_CLIENT
                .Connect(&SchedulerInfo.SchedulerUrl())
                .await
            {
                Err(e) => {
                    info!(
                        "EventType::InitState scheduler set url {}, connect fail with error {:?}",
                        SchedulerInfo.SchedulerUrl(),
                        e
                    );
                }
                Ok(_) => (),
            }
        }

        for ns in self.factory.GetInformer(Node::KEY)?.store.List() {
            let spec: Node = match Node::FromDataObject(ns.clone()) {
                Err(e) => {
                    error!("can't deserilize obj {:?} with error {:?}", ns, e);
                    continue;
                }
                Ok(s) => s,
            };
            self.nodeMgr.Add(spec)?;
        }

        for ns in self.factory.GetInformer(Tenant::KEY)?.store.List() {
            let spec = match Tenant::FromDataObject(ns.clone()) {
                Err(e) => {
                    error!("can't deserilize obj {:?} with error {:?}", ns, e);
                    continue;
                }
                Ok(s) => s,
            };
            self.tenantMgr.Add(spec)?;
        }

        for ns in self.factory.GetInformer(Namespace::KEY)?.store.List() {
            let spec = match Namespace::FromDataObject(ns.clone()) {
                Err(e) => {
                    error!("can't deserilize obj {:?} with error {:?}", ns, e);
                    continue;
                }
                Ok(s) => s,
            };
            self.namespaceMgr.Add(spec)?;
        }

        for fp in self.factory.GetInformer(Function::KEY)?.store.List() {
            let func = match fp.To::<FuncObject>() {
                Err(e) => {
                    error!("can't deserilize obj {:?} with error {:?}", fp, e);
                    continue;
                }
                Ok(s) => s,
            };
            self.AddFunc(func)?;
        }

        for pod in self.factory.GetInformer(FuncPod::KEY)?.store.List() {
            let podDef = match FuncPod::FromDataObject(pod.clone()) {
                Err(e) => {
                    error!("can't deserilize obj {:?} with error {:?}", pod, e);
                    continue;
                }
                Ok(s) => s,
            };
            self.podMgr.Add(podDef)?;
        }

        for s in self.factory.GetInformer(FuncPolicy::KEY)?.store.List() {
            let policy = match FuncPolicy::FromDataObject(s.clone()) {
                Err(e) => {
                    error!("can't deserilize obj {:?} with error {:?}", s, e);
                    continue;
                }
                Ok(s) => s,
            };
            self.funcpolicyMgr.Add(policy)?;
        }

        for snapshot in self
            .factory
            .GetInformer(ContainerSnapshot::KEY)?
            .store
            .List()
        {
            let snapshtdef = FuncSnapshot::FromDataObject(snapshot)?;
            self.snapshotMgr.Add(snapshtdef)?;
        }

        return Ok(());
    }

    pub async fn ProcessDeltaEvent(&self, event: &DeltaEvent) -> Result<()> {
        let obj = event.obj.clone();
        match &event.type_ {
            EventType::Added => {
                if self.ListDone() {
                    match &obj.objType as &str {
                        Function::KEY => {
                            let func = obj.To::<FuncObject>()?;
                            self.AddFunc(func)?;
                        }
                        Node::KEY => {
                            let spec = Node::FromDataObject(obj)?;

                            self.nodeMgr.Add(spec)?;
                        }
                        Tenant::KEY => {
                            let spec: Tenant = Tenant::FromDataObject(obj)?;
                            self.tenantMgr.Add(spec)?;
                        }
                        Namespace::KEY => {
                            let spec: Namespace = Namespace::FromDataObject(obj)?;
                            self.namespaceMgr.Add(spec)?;
                        }
                        FuncPod::KEY => {
                            let podDef = FuncPod::FromDataObject(obj)?;
                            self.podMgr.Add(podDef)?;
                            // self.FuncAgentMgr().FuncPodEventHandler(event.clone())?;
                        }
                        ContainerSnapshot::KEY => {
                            let snapshot = FuncSnapshot::FromDataObject(obj)?;
                            self.snapshotMgr.Add(snapshot)?;
                        }
                        SchedulerInfo::KEY => {
                            let SchedulerInfo = SchedulerInfo::FromDataObject(obj)?;
                            info!("********************EventType::Added scheduler set url {}...************", SchedulerInfo.SchedulerUrl());
                            SCHEDULER_CLIENT
                                .Connect(&SchedulerInfo.SchedulerUrl())
                                .await?;
                        }
                        FuncPolicy::KEY => {
                            let p = FuncPolicy::FromDataObject(obj)?;
                            self.funcpolicyMgr.Add(p)?;
                        }
                        _ => {
                            return Err(Error::CommonError(format!(
                                "NamespaceMgr::ProcessDeltaEvent {:?}",
                                event
                            )));
                        }
                    }
                }
            }
            EventType::Modified => {
                if self.ListDone() {
                    match &obj.objType as &str {
                        Node::KEY => {
                            let spec = Node::FromDataObject(obj)?;
                            self.nodeMgr.Update(spec)?;
                        }
                        Function::KEY => {
                            let oldfunc = event.oldObj.clone().unwrap().To::<FuncObject>()?;
                            // Function::FromDataObject(event.oldObj.clone().unwrap())?;
                            self.RemoveFunc(oldfunc)?;
                            let func = obj.To::<FuncObject>()?;
                            self.AddFunc(func)?;
                        }
                        Tenant::KEY => {
                            let spec: Tenant = Tenant::FromDataObject(obj)?;
                            self.tenantMgr.Update(spec)?;
                        }
                        Namespace::KEY => {
                            let spec: Namespace = Namespace::FromDataObject(obj)?;
                            self.namespaceMgr.Update(spec)?;
                        }
                        FuncPod::KEY => {
                            let podDef = FuncPod::FromDataObject(obj)?;
                            self.podMgr.Update(podDef)?;
                            // self.FuncAgentMgr().FuncPodEventHandler(event.clone())?;
                        }
                        ContainerSnapshot::KEY => {
                            let snapshot = FuncSnapshot::FromDataObject(obj)?;
                            self.snapshotMgr.Update(snapshot)?;
                            // self.FuncAgentMgr().FuncPodEventHandler(event.clone())?;
                        }
                        FuncPolicy::KEY => {
                            let p = FuncPolicy::FromDataObject(obj)?;
                            self.funcpolicyMgr.Update(p)?;
                            // self.FuncAgentMgr().FuncPodEventHandler(event.clone())?;
                        }
                        _ => {
                            return Err(Error::CommonError(format!(
                                "NamespaceMgr::ProcessDeltaEvent {:?}",
                                event
                            )));
                        }
                    }
                }
            }
            EventType::Deleted => {
                if self.ListDone() {
                    match &obj.objType as &str {
                        Node::KEY => {
                            let spec = Node::FromDataObject(obj)?;
                            self.nodeMgr.Remove(spec)?;
                        }
                        Function::KEY => {
                            let obj = event.oldObj.clone().unwrap();
                            let func = obj.To::<FuncObject>()?;
                            self.RemoveFunc(func)?;
                        }
                        Tenant::KEY => {
                            let obj = event.oldObj.clone().unwrap();
                            let tenant = Tenant::FromDataObject(obj)?;
                            self.tenantMgr.Remove(tenant)?;
                        }
                        Namespace::KEY => {
                            let obj = event.oldObj.clone().unwrap();
                            let namespace = Namespace::FromDataObject(obj)?;
                            self.namespaceMgr.Remove(namespace)?;
                        }
                        FuncPod::KEY => {
                            let podDef = FuncPod::FromDataObject(obj)?;
                            self.podMgr.Remove(podDef)?;
                            // self.FuncAgentMgr().FuncPodEventHandler(event.clone())?;
                        }
                        ContainerSnapshot::KEY => {
                            let snapshot = FuncSnapshot::FromDataObject(obj)?;
                            self.snapshotMgr.Remove(snapshot)?;
                        }
                        SchedulerInfo::KEY => {
                            SCHEDULER_CLIENT.Disconnect().await;
                            info!("********************EventType::Deleted scheduler removed ...************3");
                        }
                        FuncPolicy::KEY => {
                            let p = FuncPolicy::FromDataObject(obj)?;
                            self.funcpolicyMgr.Remove(p)?;
                        }
                        _ => {
                            return Err(Error::CommonError(format!(
                                "NamespaceMgr::ProcessDeltaEvent {:?}",
                                event
                            )));
                        }
                    }
                }
            }
            EventType::InitDone => {
                match &obj.objType as &str {
                    Node::KEY => {
                        self.SetListDone(ListType::node);
                    }
                    Function::KEY => {
                        self.SetListDone(ListType::func);
                    }
                    Tenant::KEY => {
                        self.SetListDone(ListType::tenant);
                    }
                    Namespace::KEY => {
                        self.SetListDone(ListType::namespace);
                    }
                    FuncPod::KEY => {
                        self.SetListDone(ListType::pod);
                    }
                    SchedulerInfo::KEY => {
                        self.SetListDone(ListType::scheduler);
                    }
                    ContainerSnapshot::KEY => {
                        self.SetListDone(ListType::snapshot);
                    }
                    FuncPolicy::KEY => {
                        self.SetListDone(ListType::funcpolicy);
                    }
                    // NodeInfo::KEY => {
                    //     self.SetListDone(ListType::node);
                    // }
                    _ => {
                        return Err(Error::CommonError(format!(
                            "NamespaceMgr::InitDone {:?}",
                            event
                        )));
                    }
                };

                if self.ListDone() {
                    self.InitState().await?;
                }
            }
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

impl GwObjRepo {
    pub fn ListFunc(&self, tenant: &str, namespace: &str) -> Result<Vec<FuncBrief>> {
        let funcs = self.GetFuncs(tenant, namespace)?;
        let mut funcbriefs = Vec::new();
        for func in funcs {
            let funcname = func.name.clone() + "/";
            let snapshotPrefix = format!("{}/{}/{}", tenant, namespace, &funcname);
            let snapshots =
                self.snapshotMgr
                    .GetObjectsByPrefix(tenant, namespace, &snapshotPrefix)?;
            let mut nodes = Vec::new();
            for s in &snapshots {
                if s.object.funckey.contains(&funcname) {
                    nodes.push(s.object.nodename.clone());
                }
            }

            funcbriefs.push(FuncBrief {
                func: func,
                snapshotNodes: nodes,
            })
        }

        return Ok(funcbriefs);
    }

    pub fn ListReadyPods(
        &self,
        tenant: &str,
        namespace: &str,
        funcname: &str,
    ) -> Result<Vec<FuncReadyPod>> {
        let podPrefix = format!("{}/{}/{}", tenant, namespace, funcname);
        let pods = self
            .podMgr
            .GetObjectsByPrefix(tenant, namespace, &podPrefix)?;
        let mut filterPods = Vec::new();
        for p in pods {
            if p.object.status.state == PodState::Ready {
                let readyPod = FuncReadyPod {
                    tenant: p.tenant.clone(),
                    namespace: p.namespace.clone(),
                    funcname: p.name.clone(),
                    fprevision: p.revision,
                    id: p.object.spec.id,
                };

                filterPods.push(readyPod);
            }
        }

        return Ok(filterPods);
    }

    pub fn GetFuncDetail(
        &self,
        tenant: &str,
        namespace: &str,
        funcname: &str,
    ) -> Result<FuncDetail> {
        let func = self.GetFunc(tenant, namespace, funcname)?;

        let policy = match &func.object.spec.policy {
            ObjRef::Link(l) => match self.funcpolicyMgr.Get(tenant, &l.namespace, &l.name) {
                Ok(p) => p.object,
                _ => FuncPolicySpec::default(),
            },
            ObjRef::Obj(o) => o.clone(),
        };

        let snapshotPrefix = format!("{}/{}/{}", tenant, namespace, funcname);
        let snapshots = self
            .snapshotMgr
            .GetObjectsByPrefix(tenant, namespace, &snapshotPrefix)?;

        let mut containerSnapshots = Vec::new();
        for s in &snapshots {
            containerSnapshots.push(s.object.clone());
        }

        let sampleRestCall = func.SampleRestCall();
        let podPrefix = format!("{}/{}/{}", tenant, namespace, funcname);

        let pods = self
            .podMgr
            .GetObjectsByPrefix(tenant, namespace, &podPrefix)?;
        let mut filterPods = Vec::new();
        for p in pods {
            if p.object.spec.funcspec.version == func.Version() {
                filterPods.push(p);
            }
        }
        return Ok(FuncDetail {
            func: func,
            snapshots: containerSnapshots,
            sampleRestCall: sampleRestCall,
            pods: filterPods,
            isAdmin: false,
            policy: policy,
        });
    }

    pub async fn GetFuncPodLog(
        &self,
        tenant: &str,
        namespace: &str,
        funcname: &str,
        version: i64,
        id: &str,
    ) -> Result<String> {
        let podname = format!("{}/{}/{}/{}/{}", tenant, namespace, funcname, version, id);
        let pod = self.GetFuncPod(tenant, namespace, &podname)?;
        let nodename = pod.object.spec.nodename.clone();
        let node = self.GetNode(&nodename)?;
        let nodeagentUrl = node.NodeAgentUrl();
        let mut client =
            na::node_agent_service_client::NodeAgentServiceClient::connect(nodeagentUrl).await?;
        let request = tonic::Request::new(na::ReadPodLogReq {
            tenant: tenant.to_owned(),
            namespace: namespace.to_owned(),
            funcname: funcname.to_owned(),
            fprevision: version,
            id: id.to_owned(),
        });

        let response = client.read_pod_log(request).await?;
        let resp = response.into_inner();

        if resp.error.len() == 0 {
            return Ok(resp.log);
        }

        return Err(Error::CommonError(resp.error));
    }
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct FuncBrief {
    pub func: Function,
    // list of nodes contains the func snapshot
    pub snapshotNodes: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct FuncDetail {
    pub func: Function,
    pub snapshots: Vec<ContainerSnapshot>,
    pub sampleRestCall: String,
    pub pods: Vec<FuncPod>,
    pub isAdmin: bool,
    pub policy: FuncPolicySpec,
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct FuncReadyPod {
    pub tenant: String,
    pub namespace: String,
    pub funcname: String,
    pub fprevision: i64,
    pub id: String,
}

use async_trait::async_trait;
#[async_trait]
impl EventHandler for GwObjRepo {
    async fn handle(&self, _store: &ThreadSafeStore, event: &DeltaEvent) {
        match self.ProcessDeltaEvent(event).await {
            Err(e) => {
                error!(
                    "GwObjRepo::Process fail for error {:#?} event {:#?} ",
                    event, e
                );
            }
            Ok(()) => (),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NamespaceStore {
    pub store: EtcdStore,
}

impl NamespaceStore {
    pub async fn New(endpoints: &[String]) -> Result<Self> {
        let store = EtcdStore::NewWithEndpoints(endpoints, false).await?;

        return Ok(Self { store: store });
    }

    pub async fn CreateNamespace(&self, namespace: &Namespace) -> Result<()> {
        let namespaceObj = namespace.DataObject();
        self.store.Create(&namespaceObj, 0).await?;
        return Ok(());
    }

    pub async fn UpdateNamespace(&self, namespace: &Namespace) -> Result<()> {
        let namespaceObj = namespace.DataObject();
        self.store
            .Update(namespace.revision, &namespaceObj, 0)
            .await?;
        return Ok(());
    }

    pub async fn DisasbleNamespace(&self, namespace: &Namespace) -> Result<()> {
        let mut namespace = namespace.clone();
        namespace.object.status.disable = true;
        self.store
            .Update(namespace.revision, &namespace.DataObject(), 0)
            .await?;
        return Ok(());
    }

    pub async fn CreateFunc(&self, func: &Function) -> Result<()> {
        let obj = func.DataObject();
        self.store.Create(&obj, 0).await?;
        return Ok(());
    }

    pub async fn UpdateFunc(&self, func: &Function) -> Result<()> {
        let obj = func.DataObject();
        self.store.Update(func.revision, &obj, 0).await?;
        return Ok(());
    }

    pub async fn DropFunc(
        &self,
        tenant: &str,
        namespace: &str,
        name: &str,
        revision: i64,
    ) -> Result<()> {
        let key = format!("{}/{}/{}/{}", Function::KEY, tenant, namespace, name);
        self.store.Delete(&key, revision).await?;
        return Ok(());
    }
}
