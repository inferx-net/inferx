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
use std::sync::Arc;
use tokio::sync::Notify;

use crate::common::*;
use crate::etcd::etcd_store::EtcdStore;
use crate::metastore::informer::EventHandler;
use crate::metastore::informer_factory::InformerFactory;
use crate::metastore::selection_predicate::ListOption;
use crate::metastore::store::ThreadSafeStore;
use inferxlib::data_obj::*;
use inferxlib::obj_mgr::func_mgr::*;
use inferxlib::obj_mgr::funcsnapshot_mgr::ContainerSnapshot;
use inferxlib::obj_mgr::namespace_mgr::*;
use inferxlib::obj_mgr::node_mgr::Node;
use inferxlib::obj_mgr::pod_mgr::FuncPod;
use inferxlib::obj_mgr::pod_mgr::PodMgr;

use super::scheduler::SCHEDULER;

#[derive(Debug)]
pub struct SchedObjRepoInner {
    pub notify: Arc<Notify>,

    pub funcMgr: FuncMgr,
    pub podMgr: PodMgr,
    pub funcpolicyMgr: FuncPolicyMgr,

    pub factory: InformerFactory,
    pub listDone: bool,
}

#[derive(Debug, Clone)]
pub struct SchedObjRepo(Arc<SchedObjRepoInner>);

impl Deref for SchedObjRepo {
    type Target = Arc<SchedObjRepoInner>;

    fn deref(&self) -> &Arc<SchedObjRepoInner> {
        &self.0
    }
}

impl SchedObjRepo {
    pub async fn New(addresses: Vec<String>) -> Result<Self> {
        let factory = InformerFactory::New(addresses, "", "").await?;

        // funcSpec
        factory.AddInformer(Function::KEY, &ListOption::default())?;

        // pod
        factory.AddInformer(FuncPod::KEY, &ListOption::default())?;

        // node
        factory.AddInformer(Node::KEY, &ListOption::default())?;

        // snapshot
        factory.AddInformer(ContainerSnapshot::KEY, &ListOption::default())?;

        // funcpolicy
        factory.AddInformer(FuncPolicy::KEY, &ListOption::default())?;

        let notify = Arc::new(Notify::new());

        let inner = SchedObjRepoInner {
            notify: notify,
            funcMgr: FuncMgr::default(),
            podMgr: PodMgr::default(),
            funcpolicyMgr: FuncPolicyMgr::default(),
            factory: factory,
            listDone: false,
        };

        let mgr = Self(Arc::new(inner));
        mgr.factory.AddEventHandler(Arc::new(mgr.clone())).await?;

        return Ok(mgr);
    }

    pub async fn Process(&self) -> Result<()> {
        return self.factory.Process(self.notify.clone()).await;
    }

    pub fn ContainsFunc(&self, tenant: &str, namespace: &str, name: &str) -> Result<bool> {
        return Ok(self.funcMgr.Contains(tenant, namespace, name));
    }

    pub fn GetFunc(&self, tenant: &str, namespace: &str, name: &str) -> Result<Function> {
        let ret = self.funcMgr.Get(tenant, namespace, name)?;
        return Ok(ret);
    }

    pub fn GetFuncs(&self, tenant: &str, namespace: &str) -> Result<Vec<String>> {
        let ret = self.funcMgr.GetObjectKeys(tenant, namespace)?;
        return Ok(ret);
    }

    pub fn AddFunc(&self, func: Function) -> Result<()> {
        self.funcMgr.Add(func)?;

        return Ok(());
    }

    pub fn UpdateFunc(&self, func: Function) -> Result<()> {
        self.funcMgr.Update(func)?;

        return Ok(());
    }

    pub fn RemoveFunc(&self, func: Function) -> Result<()> {
        self.funcMgr.Remove(func)?;
        return Ok(());
    }

    pub fn GetFuncPods(
        &self,
        tenant: &str,
        namespace: &str,
        funcName: &str,
        revision: i64,
    ) -> Result<Vec<FuncPod>> {
        let pods = self
            .podMgr
            .GetObjectsByPrefix(tenant, namespace, funcName)?;
        let mut ret = Vec::new();
        for p in pods {
            if p.object.spec.funcspec.version == revision {
                ret.push(p);
            }
        }

        return Ok(ret);
    }

    pub fn ProcessDeltaEvent(&self, event: &DeltaEvent) -> Result<()> {
        let obj = event.obj.clone();
        match &event.type_ {
            EventType::Added => match &obj.objType as &str {
                Function::KEY => {
                    let spec = Function::FromDataObject(obj)?;
                    self.AddFunc(spec)?;
                }
                FuncPod::KEY => {
                    let podDef = FuncPod::FromDataObject(obj)?;
                    self.podMgr.Add(podDef)?;
                }
                FuncPolicy::KEY => {
                    let p = FuncPolicy::FromDataObject(obj)?;
                    self.funcpolicyMgr.Add(p)?;
                }
                Node::KEY => {}
                ContainerSnapshot::KEY => {
                    // let snapshot = FuncSnapshot::FromDataObject(obj)?;
                    // error!("ProcessDeltaEvent get snapshot {:?}", &snapshot);
                }
                _ => {
                    return Err(Error::CommonError(format!(
                        "NamespaceMgr::ProcessDeltaEvent {:?}",
                        event
                    )));
                }
            },
            EventType::Modified => match &obj.objType as &str {
                Function::KEY => {
                    let spec = Function::FromDataObject(obj)?;
                    self.UpdateFunc(spec)?;
                }
                FuncPod::KEY => {
                    let podDef = FuncPod::FromDataObject(obj)?;
                    self.podMgr.Update(podDef)?;
                }
                FuncPolicy::KEY => {
                    let p = FuncPolicy::FromDataObject(obj)?;
                    self.funcpolicyMgr.Update(p)?;
                }
                Node::KEY => {}
                _ => {
                    return Err(Error::CommonError(format!(
                        "NamespaceMgr::ProcessDeltaEvent {:?}",
                        event
                    )));
                }
            },
            EventType::Deleted => match &obj.objType as &str {
                Function::KEY => {
                    let spec = Function::FromDataObject(obj)?;
                    self.RemoveFunc(spec)?;
                }
                FuncPod::KEY => {
                    let podDef = FuncPod::FromDataObject(obj)?;
                    self.podMgr.Remove(podDef)?;
                }
                FuncPolicy::KEY => {
                    let p = FuncPolicy::FromDataObject(obj)?;
                    self.funcpolicyMgr.Remove(p)?;
                }
                Node::KEY => {}
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

impl EventHandler for SchedObjRepo {
    fn handle(&self, _store: &ThreadSafeStore, event: &DeltaEvent) {
        match SCHEDULER.ProcessDeltaEvent(event) {
            Err(e) => {
                error!("SchedObjRepo::handler fail {:?}", e);
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

    pub async fn DropFunc(&self, namespace: &str, name: &str, revision: i64) -> Result<()> {
        let key = format!("{}/{}/{}", Function::KEY, namespace, name);
        self.store.Delete(&key, revision).await?;
        return Ok(());
    }
}
