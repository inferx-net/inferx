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

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::ops::Deref;
use std::str::FromStr;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Instant;
use std::time::SystemTime;

use inferxlib::node::WorkerPodState;
use inferxlib::obj_mgr::node_mgr::NAState;
use inferxlib::resource::StandbyType;
use tokio::sync::mpsc;
use tokio::sync::oneshot::Sender;
use tokio::sync::Notify;
use tokio::time::Interval;

use crate::common::*;
use crate::metastore::cacher_client::CacherClient;
use crate::metastore::unique_id::UID;
use crate::na::RemoveSnapshotReq;
use crate::na::TerminatePodReq;
use crate::peer_mgr::PeerMgr;
use crate::scheduler::scheduler::SCHEDULER_CONFIG;
use inferxlib::data_obj::DeltaEvent;
use inferxlib::data_obj::EventType;
use inferxlib::obj_mgr::func_mgr::*;
use inferxlib::obj_mgr::funcsnapshot_mgr::ContainerSnapshot;
use inferxlib::obj_mgr::funcsnapshot_mgr::FuncSnapshot;
use inferxlib::obj_mgr::funcsnapshot_mgr::SnapshotState;
use inferxlib::obj_mgr::pod_mgr::CreatePodType;
use inferxlib::obj_mgr::pod_mgr::FuncPod;
use inferxlib::obj_mgr::pod_mgr::PodState;
use inferxlib::resource::NodeResources;

use crate::na;
use crate::na::Kv;
use inferxlib::obj_mgr::node_mgr::Node;
use inferxlib::resource::Resources;

use super::scheduler::WorkerPod;

lazy_static::lazy_static! {
    pub static ref PEER_MGR: PeerMgr = {
        let cidrStr = "0.0.0.0"; // we don't need this for scheduler
        error!("PEER_MGR cidr {}", cidrStr);
        let ipv4 = ipnetwork::Ipv4Network::from_str(&cidrStr).unwrap();
        //let localIp = local_ip_address::local_ip().unwrap();
        let pm = PeerMgr::New(ipv4.prefix() as _, ipv4.network());
        pm
    };
}

#[derive(Debug)]
pub struct PendingPodInner {
    pub nodeName: String,
    pub podKey: String,
    pub funcId: String,
    pub allocResources: NodeResources,
    pub schedulingTime: SystemTime,
}

#[derive(Debug, Clone)]
pub struct PendingPod(Arc<PendingPodInner>);

impl Deref for PendingPod {
    type Target = Arc<PendingPodInner>;

    fn deref(&self) -> &Arc<PendingPodInner> {
        &self.0
    }
}

impl PendingPod {
    pub fn New(nodeName: &str, podKey: &str, funcId: &str, allocResources: &NodeResources) -> Self {
        let inner = PendingPodInner {
            nodeName: nodeName.to_owned(),
            podKey: podKey.to_owned(),
            funcId: funcId.to_owned(),
            allocResources: allocResources.clone(),
            schedulingTime: SystemTime::now(),
        };

        return Self(Arc::new(inner));
    }
}

pub type NodeName = String;

#[derive(Debug)]
pub struct NodeStatus {
    pub node: Node,
    pub total: NodeResources,
    pub available: NodeResources,
    // podKey --> PendingPod
    pub pendingPods: BTreeMap<String, PendingPod>,

    pub pods: BTreeMap<String, WorkerPod>,
    pub createTime: Instant,
    pub state: NAState,
}

impl NodeStatus {
    pub fn New(
        node: Node,
        total: NodeResources,
        pods: BTreeMap<String, WorkerPod>,
    ) -> Result<Self> {
        let mut available = total.Copy();
        for (_, pod) in &pods {
            available.Sub(&pod.pod.object.spec.allocResources)?;
        }

        // error!("NodeStatus total {:#?}, availabe {:#?}", &total, &available);

        let state = node.object.state;
        return Ok(Self {
            node: node,
            total: total,
            available: available,
            pendingPods: BTreeMap::new(),
            pods: pods,
            createTime: std::time::Instant::now(),
            state: state,
        });
    }

    pub fn AddPendingPod(&mut self, pendingPod: &PendingPod) -> Result<()> {
        //assert!(self.available.CanAlloc(&pendingPod.resources));
        self.pendingPods
            .insert(pendingPod.podKey.clone(), pendingPod.clone());
        return Ok(());
    }

    pub fn AddPod(&mut self, pod: &WorkerPod) -> Result<()> {
        let podKey = pod.pod.PodKey();

        match self.pendingPods.remove(&podKey) {
            None => {
                // this pod is not created by the scheduler
                error!(
                    "AddPod  pod {} available {:?} \n req is {:?}",
                    pod.pod.Name(),
                    &self.available,
                    &pod.pod.object.spec.allocResources
                );
                self.available.Sub(&pod.pod.object.spec.allocResources)?;
            }
            Some(_) => (),
        }
        self.pods.insert(podKey, pod.clone());
        return Ok(());
    }

    pub fn UpdatePod(&mut self, pod: &WorkerPod, oldPod: &WorkerPod) -> Result<()> {
        let podKey = pod.pod.PodKey();

        if oldPod.pod.object.status.state == PodState::Resuming
            && pod.pod.object.status.state == PodState::Ready
        {
            self.pendingPods.remove(&podKey);
        }

        // if pod.pod.object.status.state == PodState::Failed
        //     && oldPod.pod.object.status.state != PodState::Failed
        // {
        //     self.FreeResource(&pod.pod.object.spec.allocResources)?;
        // }

        assert!(self.pods.contains_key(&podKey));

        self.pods.insert(podKey, pod.clone());

        return Ok(());
    }

    pub fn RemovePod(
        &mut self,
        podKey: &str,
        resources: &NodeResources,
        stopping: bool,
    ) -> Result<()> {
        let pendingExist = self.pendingPods.remove(podKey).is_some();
        let exist = match self.pods.remove(podKey) {
            Some(_pod) => true,
            None => false,
        };

        // we don't free source for stopping pod as they has been reclaimed when stopping it.
        if (pendingExist || exist) && !stopping {
            self.FreeResource(resources, podKey)?;
        }

        return Ok(());
    }

    pub fn AllocResource(
        &mut self,
        req: &Resources,
        _action: &str,
        _owner: &str,
        createSnapshot: bool,
    ) -> Result<NodeResources> {
        let res = self.available.Alloc(req, createSnapshot)?;
        return Ok(res);
    }

    pub fn ResourceQuota(&self, req: &Resources) -> Result<NodeResources> {
        let res = self.available.ResourceQuota(req);
        return Ok(res);
    }

    pub fn FreeResource(&mut self, free: &NodeResources, _podkey: &str) -> Result<()> {
        // error!(
        //     "xxxxxxxxxxxxxxxxx  FreeResource pod {} resource {:#?}",
        //     _podkey, free
        // );
        self.available.Add(free)?;
        return Ok(());
    }
}

#[derive(Debug)]
pub struct FuncStatus {
    pub func: Function,

    pub pods: BTreeMap<String, WorkerPod>,
    // podname --> PendingPod
    pub pendingPods: BTreeMap<String, PendingPod>,

    pub leaseWorkerReqs: VecDeque<(na::LeaseWorkerReq, Sender<na::LeaseWorkerResp>)>,
}

impl FuncStatus {
    pub fn New(fp: Function, pods: BTreeMap<String, WorkerPod>) -> Result<Self> {
        return Ok(Self {
            func: fp,
            pods: pods,
            pendingPods: BTreeMap::new(),
            leaseWorkerReqs: VecDeque::new(),
        });
    }

    pub fn PushLeaseWorkerReq(&mut self, req: na::LeaseWorkerReq, tx: Sender<na::LeaseWorkerResp>) {
        self.leaseWorkerReqs.push_back((req, tx));
    }

    pub fn PopLeaseWorkerReq(
        &mut self,
    ) -> Option<(na::LeaseWorkerReq, Sender<na::LeaseWorkerResp>)> {
        return self.leaseWorkerReqs.pop_front();
    }

    pub fn AddPendingPod(&mut self, pendingPod: &PendingPod) -> Result<()> {
        self.pendingPods
            .insert(pendingPod.podKey.clone(), pendingPod.clone());
        return Ok(());
    }

    pub fn HasPendingPod(&self) -> bool {
        return self.pendingPods.len() > 0;
    }

    pub fn AddPod(&mut self, pod: &WorkerPod) -> Result<()> {
        let podKey = pod.pod.PodKey();
        self.pendingPods.remove(&podKey);
        self.pods.insert(podKey, pod.clone());
        return Ok(());
    }

    pub fn UpdatePod(&mut self, pod: &WorkerPod) -> Result<()> {
        let podKey = pod.pod.PodKey();
        if self.pods.insert(podKey.clone(), pod.clone()).is_none() {
            error!("podkey is {}", &podKey);
            panic!("podkey is {}", &podKey);
        }

        if pod.pod.object.status.state == PodState::Ready {
            loop {
                match self.PopLeaseWorkerReq() {
                    Some((req, tx)) => {
                        pod.SetWorking(req.gateway_id); // first time get ready, we don't need to remove it from idlePods in scheduler

                        let peer = match PEER_MGR.LookforPeer(pod.pod.object.spec.ipAddr) {
                            Ok(p) => p,
                            Err(e) => {
                                return Err(e);
                            }
                        };

                        let resp = na::LeaseWorkerResp {
                            error: "".to_owned(),
                            id: pod.pod.object.spec.id.clone(),
                            ipaddr: pod.pod.object.spec.ipAddr,
                            keepalive: false,
                            hostipaddr: peer.hostIp,
                            hostport: peer.port as u32,
                        };

                        match tx.send(resp) {
                            Ok(()) => {
                                break;
                            }
                            Err(_) => (), // if no gateway are waiting ...
                        }
                    }
                    None => {
                        pod.SetIdle();
                        break;
                    }
                }
            }
        }

        return Ok(());
    }

    pub fn RemovePod(&mut self, podKey: &str) -> Result<()> {
        self.pods.remove(podKey);
        self.pendingPods.remove(podKey);
        return Ok(());
    }
}

#[derive(Debug)]
pub enum WorkerHandlerMsg {
    StartWorker(na::CreateFuncPodReq),
    StopWorker(na::TerminatePodReq),
    LeaseWorker((na::LeaseWorkerReq, Sender<na::LeaseWorkerResp>)),
    ReturnWorker((na::ReturnWorkerReq, Sender<na::ReturnWorkerResp>)),
    RefreshGateway(na::RefreshGatewayReq),
}

#[derive(Debug, Default)]
pub struct SchedulerHandler {
    pub pods: BTreeMap<String, WorkerPod>,

    /*************** gateways ***************************** */
    // gatewayId -> refreshTimestamp
    pub gateways: BTreeMap<i64, std::time::Instant>,

    /*************** nodes ***************************** */
    pub nodes: BTreeMap<String, NodeStatus>,

    // temp pods storage when the node is not ready
    // nodename --> < podKey --> pod >
    pub nodePods: BTreeMap<String, BTreeMap<String, WorkerPod>>,

    /********************function ******************* */
    // funcname --> func
    pub funcs: BTreeMap<String, FuncStatus>,

    /*********************snapshot ******************* */
    // funcid -> [nodename->SnapshotState]
    pub snapshots: BTreeMap<String, BTreeMap<String, ContainerSnapshot>>,

    // funcid -> BTreeSet<nodename>
    pub pendingsnapshots: BTreeMap<String, BTreeSet<String>>,

    /********************idle pods ************************* */
    // returnId --> PodKey()
    pub idlePods: BTreeMap<u64, String>,

    /********************stopping pods ************************* */
    pub stoppingPods: BTreeSet<String>,

    // temp pods storage when the func is not ready
    // package name -> <Podid --> WorkerPod>
    pub packagePods: BTreeMap<String, BTreeMap<String, WorkerPod>>,

    pub nodeListDone: bool,
    pub funcListDone: bool,
    pub funcPodListDone: bool,
    pub snapshotListDone: bool,
    pub listDone: bool,

    pub nextWorkId: AtomicU64,
}

impl SchedulerHandler {
    pub fn New() -> Self {
        return Self {
            nextWorkId: AtomicU64::new(1),
            ..Default::default()
        };
    }

    pub async fn ProcessRefreshGateway(&mut self, req: na::RefreshGatewayReq) -> Result<()> {
        let gatewayId = req.gateway_id;
        let now = std::time::Instant::now();
        self.gateways.insert(gatewayId, now);
        return Ok(());
    }

    pub async fn ProcessGatewayTimeout(&mut self) -> Result<()> {
        let now = std::time::Instant::now();
        let mut timeoutGateways = HashSet::new();

        for (&gatewayId, &refresh) in &self.gateways {
            if now.duration_since(refresh) > std::time::Duration::from_millis(4000) {
                timeoutGateways.insert(gatewayId);
            }
        }

        if timeoutGateways.len() == 0 {
            return Ok(());
        }

        for &gatewayId in &timeoutGateways {
            self.gateways.remove(&gatewayId);
        }

        for (_podname, worker) in &mut self.pods {
            let state = worker.State();
            match state {
                WorkerPodState::Working(gatewayId) => {
                    if timeoutGateways.contains(&gatewayId) {
                        let returnId = worker.SetIdle();
                        // how to handle the recovered failure gateway?
                        self.idlePods.insert(returnId, worker.pod.PodKey());
                    }
                }
                _ => (),
            }
        }

        return Ok(());
    }

    pub async fn ProcessLeaseWorkerReq(
        &mut self,
        req: na::LeaseWorkerReq,
        tx: Sender<na::LeaseWorkerResp>,
    ) -> Result<()> {
        let pods = self.GetFuncPods(&req.tenant, &req.namespace, &req.funcname, req.fprevision)?;

        for worker in &pods {
            let pod = &worker.pod;
            if pod.object.status.state == PodState::Ready && worker.State().IsIdle() {
                let returnId = worker.SetWorking(req.gateway_id);
                self.idlePods.remove(&returnId);

                let peer = match PEER_MGR.LookforPeer(pod.object.spec.ipAddr) {
                    Ok(p) => p,
                    Err(e) => {
                        return Err(e);
                    }
                };

                let resp = na::LeaseWorkerResp {
                    error: String::new(),
                    id: pod.object.spec.id.clone(),
                    ipaddr: pod.object.spec.ipAddr,
                    keepalive: true,
                    hostipaddr: peer.hostIp,
                    hostport: peer.port as u32,
                };
                tx.send(resp).unwrap();
                return Ok(());
            }
        }

        let key = format!(
            "{}/{}/{}/{}",
            &req.tenant, &req.namespace, &req.funcname, req.fprevision
        );
        match self.ResumePod(&key).await {
            Err(e) => {
                let resp = na::LeaseWorkerResp {
                    error: format!("{:?}", e),
                    ..Default::default()
                };
                tx.send(resp).unwrap();
            }
            Ok(_) => {
                self.PushLeaseWorkerReq(&key, req, tx)?;
            }
        }

        return Ok(());
    }

    pub async fn ProcessReturnWorkerReq(
        &mut self,
        req: na::ReturnWorkerReq,
        tx: Sender<na::ReturnWorkerResp>,
    ) -> Result<()> {
        let worker = self.GetFuncPod(
            &req.tenant,
            &req.namespace,
            &req.funcname,
            req.fprevision,
            &req.id,
        )?;

        if worker.State().IsIdle() {
            error!(
                "ProcessReturnWorkerReq fail the {} state {:?}",
                worker.pod.PodKey(),
                worker.State()
            );
        }

        // in case the gateway dead and recover and try to return an out of date pod
        // assert!(
        //     !worker.State().IsIdle(),
        //     "the state is {:?}",
        //     worker.State()
        // );
        let returnId = worker.SetIdle();
        self.idlePods.insert(returnId, worker.pod.PodKey());
        let resp = na::ReturnWorkerResp {
            error: "".to_owned(),
        };

        tx.send(resp).unwrap();

        return Ok(());
    }

    pub fn GetFuncPod(
        &self,
        tenant: &str,
        namespace: &str,
        fpname: &str,
        revision: i64,
        id: &str,
    ) -> Result<WorkerPod> {
        let podKey = format!("{}/{}/{}/{}/{}", tenant, namespace, fpname, revision, id);
        match self.pods.get(&podKey) {
            None => return Err(Error::NotExist(format!("pod {:?} doesn't exist", podKey))),
            Some(pod) => return Ok(pod.clone()),
        };
    }

    pub fn GetFuncPodsByKey(&self, fpkey: &str) -> Result<Vec<WorkerPod>> {
        match self.funcs.get(fpkey) {
            None => {
                error!("get pods key is {} keys {:#?}", &fpkey, self.funcs.keys());
                return Ok(Vec::new());
            }
            Some(fpStatus) => {
                return Ok(fpStatus.pods.values().cloned().collect());
            }
        }
    }

    pub fn GetFuncPods(
        &self,
        tenant: &str,
        namespace: &str,
        fpname: &str,
        revision: i64,
    ) -> Result<Vec<WorkerPod>> {
        let fpkey = format!("{}/{}/{}/{}", tenant, namespace, fpname, revision);
        return self.GetFuncPodsByKey(&fpkey);
    }

    pub fn GetFunc(&self, tenant: &str, namespace: &str, name: &str) -> Result<Function> {
        let fpkey = format!("{}/{}/{}", tenant, namespace, name);
        match self.funcs.get(&fpkey) {
            None => return Err(Error::NotExist(format!("GetFunc {}", fpkey))),
            Some(fpStatus) => return Ok(fpStatus.func.clone()),
        }
    }

    pub fn AddPendingSnapshot(&mut self, funcid: &str, nodename: &str) {
        match self.pendingsnapshots.get_mut(funcid) {
            None => {
                let mut nodes = BTreeSet::new();
                nodes.insert(nodename.to_owned());
                self.pendingsnapshots.insert(funcid.to_owned(), nodes);
            }
            Some(nodes) => {
                nodes.insert(nodename.to_owned());
            }
        }
    }

    pub fn RemovePendingSnapshot(&mut self, funcid: &str, nodename: &str) {
        let needRemoveFunc;
        match self.pendingsnapshots.get_mut(funcid) {
            None => return,
            Some(nodes) => {
                nodes.remove(nodename);
                needRemoveFunc = nodes.len() == 0;
            }
        }

        if needRemoveFunc {
            self.pendingsnapshots.remove(funcid);
        }
    }

    pub fn HasPendingSnapshot(&self, funcid: &str) -> bool {
        return self.pendingsnapshots.contains_key(funcid);
    }

    pub fn AddSnapshot(&mut self, snapshot: &FuncSnapshot) -> Result<()> {
        let funckey = snapshot.object.funckey.clone();

        self.RemovePendingSnapshot(&funckey, &snapshot.object.nodename);

        if !self.snapshots.contains_key(&funckey) {
            self.snapshots.insert(funckey.clone(), BTreeMap::new());
        }

        self.snapshots
            .get_mut(&funckey)
            .unwrap()
            .insert(snapshot.object.nodename.clone(), snapshot.object.clone());

        return Ok(());
    }

    pub fn UpdateSnapshot(&mut self, snapshot: &FuncSnapshot) -> Result<()> {
        let funckey = snapshot.object.funckey.clone();
        assert!(self.snapshots.contains_key(&funckey));

        self.snapshots
            .get_mut(&funckey)
            .unwrap()
            .insert(snapshot.object.nodename.clone(), snapshot.object.clone());

        return Ok(());
    }

    pub async fn RemoveSnapshot(&mut self, snapshot: &FuncSnapshot) -> Result<()> {
        let funckey = snapshot.object.funckey.clone();
        if !self.snapshots.contains_key(&funckey) {
            return Ok(());
        }

        self.snapshots
            .get_mut(&funckey)
            .unwrap()
            .remove(&snapshot.object.nodename);

        // let nodename = snapshot.spec.nodename.clone();
        // self.RemoveSnapshotFromNode(&nodename, &funckey).await?;

        return Ok(());
    }

    pub async fn RemoveSnapshotByFunckey(&mut self, funckey: &str) -> Result<()> {
        let snapshot = self.snapshots.remove(funckey);
        if let Some(snapshot) = snapshot {
            for (nodename, _) in snapshot {
                match self.RemoveSnapshotFromNode(&nodename, &funckey).await {
                    Ok(()) => (),
                    Err(e) => {
                        error!("{:?}", e);
                    }
                }
            }
        }

        return Ok(());
    }

    pub async fn CleanSnapshots(&mut self) -> Result<()> {
        let mut cleanSnapshots = Vec::new();
        for (funckey, _) in &self.snapshots {
            if !self.funcs.contains_key(funckey) {
                cleanSnapshots.push(funckey.clone());
            }
        }

        for funckey in &cleanSnapshots {
            let snapshots = match self.snapshots.remove(funckey) {
                None => continue,
                Some(ns) => ns,
            };

            for (nodename, _state) in &snapshots {
                match self.RemoveSnapshotFromNode(nodename, &funckey).await {
                    Ok(()) => (),
                    Err(e) => {
                        error!("{:?}", e);
                    }
                }
            }
        }

        return Ok(());
    }

    pub async fn RemoveSnapshotFromNode(&self, nodename: &str, funckey: &str) -> Result<()> {
        let nodeStatus = self.nodes.get(nodename).unwrap();
        let nodeAgentUrl = nodeStatus.node.NodeAgentUrl();
        let mut client =
            na::node_agent_service_client::NodeAgentServiceClient::connect(nodeAgentUrl.to_owned())
                .await?;

        let request = tonic::Request::new(RemoveSnapshotReq {
            funckey: funckey.to_owned(),
        });
        let response = client.remove_snapshot(request).await?;
        let resp = response.into_inner();

        if resp.error.len() == 0 {
            return Ok(());
        }

        return Err(Error::CommonError(format!(
            "RemoveSnapshotFromNode fail with error {}",
            resp.error
        )));
    }

    pub async fn ProcessOnce(
        &mut self,
        closeNotfiy: &Arc<Notify>,
        eventRx: &mut mpsc::Receiver<DeltaEvent>,
        msgRx: &mut mpsc::Receiver<WorkerHandlerMsg>,
        interval: &mut Interval,
    ) -> Result<()> {
        tokio::select! {
            _ = closeNotfiy.notified() => {
                return Err(Error::ProcessDone);
            }
            _ = interval.tick() => {
                if self.listDone {
                    // retry scheduling to see wheter there is more resource avaiable
                    self.RefreshScheduling().await?;
                    self.CleanSnapshots().await?;
                    self.ProcessGatewayTimeout().await?;
                }
            }
            event = eventRx.recv() => {
                if let Some(event) = event {
                    let obj = event.obj.clone();
                    // defer!(error!("schedulerhandler end ..."));
                    match &event.type_ {
                        EventType::Added => {
                            match &obj.objType as &str {
                                Function::KEY => {
                                    let func = Function::FromDataObject(obj)?;
                                    let funcid = func.Id();
                                    self.AddFunc(func)?;
                                    if self.listDone {
                                        self.ProcessAddFunc(&funcid).await?;
                                    }
                                }
                                Node::KEY => {
                                    let node = Node::FromDataObject(obj)?;
                                    let peerIp = ipnetwork::Ipv4Network::from_str(&node.object.nodeIp)
                                        .unwrap()
                                        .ip()
                                        .into();
                                    let peerPort: u16 = node.object.tsotSvcPort;
                                    let cidr = ipnetwork::Ipv4Network::from_str(&node.object.cidr).unwrap();

                                    match PEER_MGR.AddPeer(peerIp, peerPort, cidr.ip().into()) {
                                        Err(e) => {
                                            error!(
                                                "NodeMgr::addpeer fail with peer {:x?}/{} cidr  {:?} error {:x?}",
                                                &node.object.nodeIp, peerPort, &node.object.cidr, e
                                            );
                                            panic!();
                                        }
                                        Ok(()) => (),
                                    };
                                    self.AddNode(node)?;

                                }
                                FuncPod::KEY => {
                                    let pod = FuncPod::FromDataObject(obj)?;
                                    self.AddPod(pod.clone())?;
                                }
                                ContainerSnapshot::KEY => {
                                    let snapshot = FuncSnapshot::FromDataObject(obj)?;
                                    self.AddSnapshot(&snapshot)?;
                                }
                                _ => {
                                }
                            }
                        }
                        EventType::Modified => {
                            match &obj.objType as &str {
                                Function::KEY => {
                                    let oldobj = event.oldObj.clone().unwrap();
                                    let oldspec = Function::FromDataObject(oldobj)?;
                                    self.ProcessRemoveFunc(&oldspec).await?;
                                    self.RemoveFunc(oldspec)?;

                                    let spec = Function::FromDataObject(obj)?;
                                    let fpId = spec.Id();
                                    self.AddFunc(spec)?;
                                    if self.listDone {
                                        self.ProcessAddFunc(&fpId).await?;
                                    }
                                }
                                Node::KEY => {
                                    let node = Node::FromDataObject(obj)?;
                                    error!("Update node {:?}", &node);
                                    self.UpdateNode(node)?;
                                }
                                FuncPod::KEY => {
                                    let pod = FuncPod::FromDataObject(obj)?;
                                    self.UpdatePod(pod.clone()).await?;
                                }
                                ContainerSnapshot::KEY => {
                                    let snapshot = FuncSnapshot::FromDataObject(obj)?;
                                    self.UpdateSnapshot(&snapshot)?;
                                }
                                _ => {
                                }
                            }
                        }
                        EventType::Deleted => {
                            match &obj.objType as &str {
                                Function::KEY => {
                                    let obj = event.oldObj.clone().unwrap();
                                    let spec = Function::FromDataObject(obj)?;
                                    self.ProcessRemoveFunc(&spec).await?;
                                    self.RemoveFunc(spec)?;

                                }
                                Node::KEY => {
                                    let node = Node::FromDataObject(obj)?;
                                    let cidr = ipnetwork::Ipv4Network::from_str(&node.object.cidr).unwrap();
                                    PEER_MGR.RemovePeer(cidr.ip().into()).unwrap();
                                    self.RemoveNode(node)?;
                                }
                                FuncPod::KEY => {
                                    let pod = FuncPod::FromDataObject(obj)?;
                                    self.RemovePod(&pod).await?;
                                }
                                ContainerSnapshot::KEY => {
                                    let snapshot = FuncSnapshot::FromDataObject(obj)?;
                                    self.RemoveSnapshot(&snapshot).await?;



                                }
                                _ => {
                                }
                            }
                        }
                        EventType::InitDone => {
                            match &obj.objType as &str {
                                Function::KEY => {
                                    self.ListDone(ListType::Func).await?;
                                }
                                Node::KEY => {
                                    self.ListDone(ListType::Node).await?;
                                }
                                FuncPod::KEY => {
                                    self.ListDone(ListType::FuncPod).await?;
                                }
                                ContainerSnapshot::KEY => {
                                    self.ListDone(ListType::Snapshot).await?;
                                }
                                _ => {
                                    error!("SchedulerHandler get unexpect list done {}", &obj.objType);
                                }
                            }

                        }
                        _o => {
                            return Err(Error::CommonError(format!(
                                "PodHandler::ProcessDeltaEvent {:?}",
                                event
                            )));
                        }
                    }
                } else {
                    error!("scheduler eventRx read fail...");
                    return Err(Error::ProcessDone);
                }
            }
            m = msgRx.recv() => {
                if let Some(msg) = m {
                    match msg {
                        WorkerHandlerMsg::LeaseWorker((m, tx)) => {
                            self.ProcessLeaseWorkerReq(m, tx).await?;
                        }
                        WorkerHandlerMsg::ReturnWorker((m, tx)) => {
                            self.ProcessReturnWorkerReq(m, tx).await.ok();
                        }
                        WorkerHandlerMsg::RefreshGateway(m) => {
                            self.ProcessRefreshGateway(m).await?;
                        }
                        _ => ()
                    }
                } else {
                    error!("scheduler msgRx read fail...");
                    return Err(Error::ProcessDone);
                }
            }
        }

        return Ok(());
    }

    pub async fn Process(
        &mut self,
        closeNotfiy: Arc<Notify>,
        eventRx: &mut mpsc::Receiver<DeltaEvent>,
        msgRx: &mut mpsc::Receiver<WorkerHandlerMsg>,
    ) -> Result<()> {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(4000));

        loop {
            match self
                .ProcessOnce(&closeNotfiy, eventRx, msgRx, &mut interval)
                .await
            {
                Ok(()) => (),
                Err(Error::ProcessDone) => break,
                Err(_e) => {
                    // error!("Scheduler get error {:?}", e);
                }
            }
        }

        return Ok(());
    }

    pub fn NextWorkerId(&self) -> u64 {
        return self
            .nextWorkId
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn StandyResource(&self, funcId: &str, nodename: &str) -> Resources {
        let snapshot = self
            .snapshots
            .get(funcId)
            .unwrap()
            .get(nodename)
            .unwrap()
            .clone();

        return snapshot.StandbyResource();
    }

    // when doing resume, the final resources needed for the pod
    pub fn ReadyResource(
        &self,
        funcResource: &Resources,
        funcId: &str,
        nodename: &str,
    ) -> Resources {
        let snapshot = self
            .snapshots
            .get(funcId)
            .unwrap()
            .get(nodename)
            .unwrap()
            .clone();

        let cacheMemory = snapshot.ReadyCacheMemory();

        let mut readyResource = funcResource.clone();

        readyResource.cacheMemory = cacheMemory;
        readyResource.memory -= cacheMemory;
        readyResource.gpu.contextCount = 1;
        return readyResource;
    }

    // when doing resume, the extra resource is required for node allocation
    pub fn ReqResumeResource(
        &self,
        funcResource: &Resources,
        funcId: &str,
        nodename: &str,
    ) -> Resources {
        let mut extraResource = self.ReadyResource(funcResource, funcId, nodename);
        let standbyResource = self.StandyResource(funcId, nodename);
        // error!("extraResource   is {:?}", &extraResource);
        // error!("standbyResource is {:?}", &standbyResource);
        if extraResource.memory > standbyResource.memory {
            extraResource.memory -= standbyResource.memory;
        } else {
            extraResource.memory = 0;
        }

        if extraResource.cacheMemory > standbyResource.cacheMemory {
            extraResource.cacheMemory -= standbyResource.cacheMemory;
        } else {
            extraResource.cacheMemory = 0;
        }
        // the standby resource only incldue memory and cache
        return extraResource;
    }

    // the cache memory usage when in ready state for the snapshot
    pub fn SnapshotReadyCacheMemory(&self, funcId: &str, nodename: &str) -> u64 {
        let snapshot = self
            .snapshots
            .get(funcId)
            .unwrap()
            .get(nodename)
            .unwrap()
            .clone();

        return snapshot.ReadyCacheMemory();
    }

    // find a node to run the pod (snapshot or standy resume), if there is no enough free resource, add list of evaction pods
    // ret ==> (node, evacation pods)
    pub async fn FindNode4Pod(
        &mut self,
        func: &Function,
        forStandby: bool,
        candidateNodes: &BTreeSet<String>,
        createSnapshot: bool,
    ) -> Result<(String, Vec<WorkerPod>)> {
        let mut nodeSnapshots = BTreeMap::new();

        // go through candidate list to look for node has enough free resource, if so return
        for nodename in candidateNodes {
            let node = self.nodes.get(nodename).unwrap();

            if Self::PRINT_SCHEDER_INFO {
                error!("FindNode4Pod 1 ns is {:#?}", &node.available);
            }

            if !createSnapshot {
                let mut standbyPod = 0;
                for (podname, pod) in &node.pods {
                    if pod.pod.object.status.state != PodState::Standby {
                        continue;
                    }

                    if pod.pod.FuncKey() != func.Id() {
                        continue;
                    }

                    if node.pendingPods.contains_key(podname) {
                        continue;
                    }

                    standbyPod += 1;
                }

                if standbyPod == 0 {
                    continue;
                }
            }

            let nr = node.available.clone();
            let contextCount = nr.maxContextCnt;
            let req = if !forStandby {
                // snapshot need to take whole gpu
                func.object.spec.SnapshotResource(contextCount)
            } else {
                self.ReqResumeResource(&func.object.spec.RunningResource(), &func.Id(), &nodename)
            };
            if nr.CanAlloc(&req, createSnapshot) {
                return Ok((nodename.clone(), Vec::new()));
            }
            nodeSnapshots.insert(nodename.clone(), (nr, Vec::new()));
        }

        let mut missWorkers = Vec::new();

        let mut findnodeName = None;
        let mut contextCount = 0;

        // try to simulate killing idle pods and see whether can find good node
        for (workid, podKey) in &self.idlePods {
            match self.pods.get(podKey) {
                None => {
                    missWorkers.push(*workid);
                    continue;
                }
                Some(pod) => match nodeSnapshots.get_mut(&pod.pod.object.spec.nodename) {
                    None => (),
                    Some((nr, workids)) => {
                        workids.push(*workid);
                        nr.Add(&pod.pod.object.spec.allocResources).unwrap();

                        let req = if !forStandby {
                            func.object.spec.SnapshotResource(nr.maxContextCnt)
                        } else {
                            self.ReqResumeResource(
                                &func.object.spec.RunningResource(),
                                &func.Id(),
                                &pod.pod.object.spec.nodename,
                            )
                        };
                        if nr.CanAlloc(&req, createSnapshot) {
                            findnodeName = Some(pod.pod.object.spec.nodename.clone());
                            if !forStandby {
                                contextCount = nr.maxContextCnt;
                            }
                        }
                    }
                },
            }
        }

        for workerid in &missWorkers {
            self.idlePods.remove(workerid);
        }

        if findnodeName.is_none() {
            return Err(Error::SchedulerNoEnoughResource(format!(
                "can find enough resource for {}",
                func.Key()
            )));
        }

        let nodename = findnodeName.unwrap();

        let req = if !forStandby {
            func.object.spec.SnapshotResource(contextCount)
        } else {
            // func.object.spec.AllResources()
            self.ReqResumeResource(&func.object.spec.RunningResource(), &func.Id(), &nodename)
        };

        let mut pods = Vec::new();
        let (_, workdis) = nodeSnapshots.get(&nodename).unwrap().clone();
        for workid in &workdis {
            match self.idlePods.remove(workid) {
                None => unreachable!(),
                Some(podkey) => match self.pods.get(&podkey) {
                    None => unreachable!(),
                    Some(pod) => {
                        let nodename = pod.pod.object.spec.nodename.clone();
                        let nodeStatus = self.nodes.get_mut(&nodename).unwrap();
                        self.stoppingPods.insert(podkey.clone());

                        nodeStatus
                            .FreeResource(&pod.pod.object.spec.allocResources, &pod.pod.PodKey())?;
                        pods.push(pod.clone());
                        if nodeStatus.available.CanAlloc(&req, createSnapshot) {
                            break;
                        }
                    }
                },
            }
        }

        return Ok((nodename, pods));
    }

    pub async fn GetBestResumeWorker(
        &mut self,
        fp: &Function,
    ) -> Result<(WorkerPod, Vec<WorkerPod>)> {
        let funcid = fp.Id();

        let pods = self.GetFuncPodsByKey(&funcid)?;

        if pods.len() == 0 {
            return Err(Error::SchedulerNoEnoughResource(format!(
                "no standby worker for function {:?}",
                &fp.Id()
            )));
        }

        let mut nodes = BTreeSet::new();
        for p in &pods {
            if p.pod.object.status.state != PodState::Standby {
                continue;
            }
            nodes.insert(p.pod.object.spec.nodename.clone());
        }

        if nodes.len() == 0 {
            return Err(Error::SchedulerErr(format!(
                "No standy pod for func {:?}, likely it is just restarted, please retry",
                &funcid
            )));
        }

        let (nodename, terminalpods) = self.FindNode4Pod(fp, true, &nodes, false).await?;

        for worker in &pods {
            if worker.pod.object.status.state != PodState::Standby {
                continue;
            }

            if &worker.pod.object.spec.nodename == &nodename {
                return Ok((worker.clone(), terminalpods));
            }
        }

        return Err(Error::SchedulerNoEnoughResource(format!(
            "no standby worker for function {:?}",
            &fp.Id()
        )));
    }

    pub fn IsNodeReady(&self, nodename: &str) -> bool {
        match self.nodes.get(nodename) {
            None => return false,
            Some(ns) => {
                return ns.state == NAState::NodeAgentAvaiable;
            }
        }
    }

    pub fn GetSnapshotNodes(&self, funcid: &str) -> BTreeSet<String> {
        let mut nodes = BTreeSet::new();
        match self.snapshots.get(funcid) {
            None => return nodes,
            Some(ns) => {
                for (nodename, _) in ns {
                    nodes.insert(nodename.to_owned());
                }
                return nodes;
            }
        };
    }

    pub fn HasPendingPod(&self, funcid: &str) -> bool {
        match self.funcs.get(funcid) {
            None => return false,
            Some(fs) => {
                return fs.HasPendingPod();
            }
        };
    }

    pub fn GetSnapshotCandidateNodes(&self, func: &Function) -> BTreeSet<String> {
        let funcid = &func.Id();
        let snapshotNodes = self.GetSnapshotNodes(funcid);
        let mut nodes = BTreeSet::new();

        for (_, ns) in &self.nodes {
            if Self::PRINT_SCHEDER_INFO {
                error!(
                    "GetSnapshotCandidateNodes 1 {:?}/{:?}",
                    ns.state, &ns.available
                );
            }

            if ns.state != NAState::NodeAgentAvaiable {
                continue;
            }

            // wait 5 sec to sync the snapshot information
            if std::time::Instant::now().duration_since(ns.createTime)
                < std::time::Duration::from_secs(5)
            {
                continue;
            }

            let spec = &func.object.spec;
            if !ns.node.object.blobStoreEnable {
                if spec.standby.gpuMem == StandbyType::Blob
                    || spec.standby.PageableMem() == StandbyType::Blob
                    || spec.standby.pinndMem == StandbyType::Blob
                {
                    continue;
                }
            }

            if !snapshotNodes.contains(&ns.node.name) {
                match self.nodes.get(&ns.node.name) {
                    None => (),
                    Some(ns) => {
                        if ns.total.CanAlloc(&func.object.spec.resources, true) {
                            nodes.insert(ns.node.name.clone());
                        }
                    }
                }
            }
        }

        return nodes;
    }

    pub async fn GetBestNodeToSnapshot(
        &mut self,
        func: &Function,
    ) -> Result<(String, Vec<WorkerPod>)> {
        let snapshotCandidateNodes = self.GetSnapshotCandidateNodes(&func);
        if snapshotCandidateNodes.len() == 0 {
            return Err(Error::SchedulerErr(format!(
                "GetBestNodeToSnapshot can't schedule {}, no enough resource",
                func.Id(),
            )));
        }

        let (nodename, pods) = self
            .FindNode4Pod(func, false, &snapshotCandidateNodes, true)
            .await?;

        return Ok((nodename, pods));
    }

    pub async fn GetBestNodeToRestore(&mut self, fp: &Function) -> Result<String> {
        use rand::prelude::SliceRandom;
        use rand::thread_rng;

        let funcid = fp.Id();
        let pods = self.GetFuncPodsByKey(&funcid)?;
        let mut existnodes = BTreeSet::new();
        for pod in &pods {
            let podstate = pod.pod.object.status.state;
            // error!("GetBestNodeToRestore id {} state {}", &funcid, podstate);
            // the pod reach ready state, create another standby pod
            if podstate != PodState::Ready {
                let nodename = pod.pod.object.spec.nodename.clone();
                if self.IsNodeReady(&nodename) {
                    existnodes.insert(pod.pod.object.spec.nodename.clone());
                }
            }
        }

        let mut res = Vec::new();
        match self.snapshots.get(&funcid) {
            None => (),
            Some(set) => {
                for (nodename, _) in set {
                    if !self.IsNodeReady(nodename) {
                        continue;
                    }
                    if !existnodes.contains(nodename) {
                        res.push(nodename.to_owned());
                    }
                }
            }
        }

        if res.len() == 0 {
            return Err(Error::CommonError(format!(
                "can't find snapshot to restore {}",
                funcid
            )));
        }

        let mut rng = thread_rng();
        res.shuffle(&mut rng);

        let nodename = res[0].clone();

        return Ok(nodename);
    }

    pub async fn RefreshScheduling(&mut self) -> Result<()> {
        let funcids: Vec<String> = self.funcs.keys().cloned().collect();
        for fpId in &funcids {
            self.ProcessAddFunc(fpId).await.ok();
        }

        return Ok(());
    }

    pub fn GetReadySnapshotNodes(&self, funcid: &str) -> Result<Vec<String>> {
        match self.snapshots.get(funcid) {
            None => return Ok(Vec::new()),
            Some(snapshots) => {
                let mut nodes = Vec::new();
                for (nodename, s) in snapshots {
                    if s.state == SnapshotState::Ready {
                        nodes.push(nodename.to_owned());
                    }
                }

                return Ok(nodes);
            }
        }
    }

    pub async fn ProcessAddFunc(&mut self, funcid: &str) -> Result<()> {
        let func = match self.funcs.get(funcid) {
            None => {
                return Err(Error::NotExist(format!(
                    "ProcessAddFunc can't find funcpcakge with id {}",
                    funcid
                )));
            }
            Some(fpStatus) => fpStatus.func.clone(),
        };

        let funcState = func.object.status.state;

        if funcState == FuncState::Fail {
            return Ok(());
        }

        self.TryCreateSnapshot(funcid, &func).await?;
        self.TryCreateStandbyPod(funcid).await?;
        return Ok(());
    }

    pub async fn TryCreateStandbyPod(&mut self, funcid: &str) -> Result<()> {
        let nodes = self.GetReadySnapshotNodes(funcid)?;

        if nodes.len() == 0 {
            // no checkpoint
            return Ok(());
        }

        let needRestore; // whether need to create new standby pod
        let function = match self.funcs.get(funcid) {
            None => {
                return Err(Error::NotExist(format!(
                    "ProcessAddFunc can't find funcpcakge with id {}",
                    funcid
                )))
            }
            Some(funcStatus) => {
                let pendingCnt = funcStatus.pendingPods.len();
                let podcnt = funcStatus.pendingPods.len() + funcStatus.pods.len();

                let keepaliveCnt = 8;
                needRestore = (pendingCnt == 0) && (podcnt < keepaliveCnt);

                // error!(
                //     "TryRetorePod pendingCnt {} podcnt {} needRestore {} keepaliveCnt {} for {}",
                //     pendingCnt, podcnt, needRestore, keepaliveCnt, fpKey
                // );
                funcStatus.func.clone()
            }
        };

        if needRestore {
            let nodename = self.GetBestNodeToRestore(&function).await?;
            let allocResources; // = NodeResources::default();
            let nodeAgentUrl;
            let resourceQuota;

            let standbyResource = self.StandyResource(funcid, &nodename);
            {
                let nodeStatus = match self.nodes.get_mut(&nodename) {
                    None => return Ok(()), // the node information is not synced
                    Some(ns) => ns,
                };

                allocResources =
                    nodeStatus.AllocResource(&standbyResource, "CreateStandby", funcid, false)?;
                resourceQuota = nodeStatus.ResourceQuota(&function.object.spec.resources)?;
                nodeAgentUrl = nodeStatus.node.NodeAgentUrl();
            }

            let id = match self
                .StartWorker(
                    &nodeAgentUrl,
                    &function,
                    &allocResources,
                    &resourceQuota,
                    na::CreatePodType::Restore, 
                    &Vec::new(),
                )
                .await
            {
                Err(e) => {
                    let nodeStatus = match self.nodes.get_mut(&nodename) {
                        None => return Ok(()), // the node information is not synced
                        Some(ns) => ns,
                    };
                    let resourceQuota = nodeStatus.ResourceQuota(&standbyResource)?;
                    nodeStatus.FreeResource(&resourceQuota, "")?;
                    return Err(e);
                }
                Ok(id) => id,
            };

            let podKey = FuncPod::FuncPodKey(
                &function.tenant,
                &function.namespace,
                &function.name,
                function.Version(),
                &format!("{id}"),
            );

            let pendingPod = PendingPod::New(&nodename, &podKey, &funcid, &allocResources);
            let nodeStatus = self.nodes.get_mut(&nodename).unwrap();
            nodeStatus.AddPendingPod(&pendingPod)?;

            self.funcs
                .get_mut(funcid)
                .unwrap()
                .AddPendingPod(&pendingPod)?;
        }

        return Ok(());
    }

    pub const MAX_SNAPSHOT_PER_FUNC: usize = 3;
    pub const PRINT_SCHEDER_INFO: bool = false;

    pub async fn TryCreateSnapshot(&mut self, funcid: &str, func: &Function) -> Result<()> {
        if Self::PRINT_SCHEDER_INFO {
            error!("TryCreateSnapshot 1 {}", funcid);
        }

        let nodes = self.GetSnapshotNodes(funcid);

        if Self::PRINT_SCHEDER_INFO {
            error!(
                "TryCreateSnapshot 2 {}/{:?}/{}",
                funcid,
                &nodes,
                self.HasPendingSnapshot(funcid)
            );
        }
        if nodes.len() > Self::MAX_SNAPSHOT_PER_FUNC {
            return Ok(());
        }

        if !self.HasPendingSnapshot(funcid) {
            let (nodename, terminatePods) = match self.GetBestNodeToSnapshot(&func).await {
                Ok(ret) => ret,
                Err(e) => {
                    if Self::PRINT_SCHEDER_INFO {
                        error!("TryCreateSnapshot 3 {:?}/{:?}", funcid, &e);
                    }
                    if nodes.len() > 0 {
                        return Ok(());
                    }

                    return Err(e);
                }
            };

            if Self::PRINT_SCHEDER_INFO {
                error!("TryCreateSnapshot 4 {}", funcid);
            }
            // error!(
            //     "TryCreateSnapshot 2 name {} nodes.len() {}",
            //     &nodename,
            //     nodes.len()
            // );
            let resources;
            let nodeAgentUrl;

            {
                let nodeStatus = self.nodes.get_mut(&nodename).unwrap();
                let contextCnt = nodeStatus.node.object.resources.GPUResource().maxContextCnt;
                let snapshotResource = func.object.spec.SnapshotResource(contextCnt);
                resources =
                    nodeStatus.AllocResource(&snapshotResource, "CreateSnapshot", funcid, true)?;
                nodeAgentUrl = nodeStatus.node.NodeAgentUrl();
            }

            let id = match self
                .StartWorker(
                    &nodeAgentUrl,
                    &func,
                    &resources,
                    &resources,
                    na::CreatePodType::Snapshot,
                    &terminatePods,
                )
                .await
            {
                Err(e) => {
                    let nodeStatus = self.nodes.get_mut(&nodename).unwrap();
                    nodeStatus.FreeResource(&resources, &func.Id())?;
                    return Err(e);
                }
                Ok(a) => a,
            };

            let podKey = FuncPod::FuncPodKey(
                &func.tenant,
                &func.namespace,
                &func.name,
                func.Version(),
                &format!("{id}"),
            );

            let pendingPod = PendingPod::New(&nodename, &podKey, &funcid, &resources);
            let nodeStatus = self.nodes.get_mut(&nodename).unwrap();
            nodeStatus.AddPendingPod(&pendingPod)?;

            self.funcs
                .get_mut(funcid)
                .unwrap()
                .AddPendingPod(&pendingPod)?;

            self.AddPendingSnapshot(funcid, &nodename);
        }

        return Ok(());
    }

    pub async fn ProcessRemoveFunc(&mut self, spec: &Function) -> Result<()> {
        let pods = self.GetFuncPods(&spec.tenant, &spec.namespace, &spec.name, spec.Version())?;

        if pods.len() == 0 {
            return Ok(());
        }

        for pod in &pods {
            let pod = &pod.pod;

            match self.StopWorker(&pod).await {
                Ok(()) => (),
                Err(e) => {
                    error!(
                        "ProcessRemoveFunc fail to stopper func worker {:?} with error {:#?}",
                        pod.PodKey(),
                        e
                    );
                }
            }
        }

        self.RemoveSnapshotByFunckey(&spec.Key()).await?;

        return Ok(());
    }

    pub fn PushLeaseWorkerReq(
        &mut self,
        fpKey: &str,
        req: na::LeaseWorkerReq,
        tx: Sender<na::LeaseWorkerResp>,
    ) -> Result<()> {
        let fpStatus = match self.funcs.get_mut(fpKey) {
            None => {
                return Err(Error::NotExist(format!(
                    "CreateWorker can't find funcpcakge with id {} {:?}",
                    fpKey,
                    self.funcs.keys(),
                )));
            }
            Some(fpStatus) => fpStatus,
        };
        fpStatus.PushLeaseWorkerReq(req, tx);
        return Ok(());
    }

    pub async fn ResumePod(&mut self, fpKey: &str) -> Result<()> {
        use std::ops::Bound::*;
        let start = fpKey.to_owned();
        let mut vec = Vec::new();
        for (key, _) in self
            .funcs
            .range::<String, _>((Included(start.clone()), Unbounded))
        {
            if key.starts_with(&start) {
                vec.push(key.clone());
            } else {
                break;
            }
        }

        if vec.len() == 0 {
            return Err(Error::NotExist(format!("GetFunc {}", fpKey)));
        }

        if vec.len() > 1 {
            return Err(Error::CommonError(format!(
                "CreateWorker get multiple func for {}  with keys {:#?}",
                fpKey, &vec
            )));
        }

        let key = &vec[0];

        let fp = match self.funcs.get(key) {
            None => {
                return Err(Error::NotExist(format!(
                    "CreateWorker can't find funcpcakge with id {} {:?}",
                    fpKey,
                    self.funcs.keys(),
                )));
            }
            Some(fpStatus) => fpStatus.func.clone(),
        };

        let (pod, terminatePods) = self.GetBestResumeWorker(&fp).await?;
        let resources;
        let naUrl;
        let nodename = pod.pod.object.spec.nodename.clone();
        let id = pod.pod.object.spec.id.clone();

        {
            let readyResource =
                self.ReadyResource(&fp.object.spec.RunningResource(), fpKey, &nodename);
            let nodeStatus = self.nodes.get_mut(&nodename).unwrap();

            let standbyResource = pod.pod.object.spec.allocResources.clone();

            nodeStatus.FreeResource(&standbyResource, &fp.name)?;
            resources = nodeStatus.AllocResource(&readyResource, "ResumePod", &id, false)?;
            naUrl = nodeStatus.node.NodeAgentUrl();
        }

        self.ResumeWorker(
            &naUrl,
            &pod.pod.tenant,
            &pod.pod.namespace,
            &pod.pod.object.spec.funcname,
            pod.pod.object.spec.fprevision,
            &id,
            &resources,
            &terminatePods,
        )
        .await?;

        let podKey = FuncPod::FuncPodKey(
            &fp.tenant,
            &fp.namespace,
            &fp.name,
            fp.Version(),
            &format!("{id}"),
        );
        let fpKey = FuncPod::FuncObjectKey(&fp.tenant, &fp.namespace, &fp.name, fp.Version());
        let pendingPod = PendingPod::New(&nodename, &podKey, &fpKey, &resources);
        let nodeStatus = self.nodes.get_mut(&nodename).unwrap();
        nodeStatus.AddPendingPod(&pendingPod)?;

        return Ok(());
    }

    pub async fn StartWorker(
        &mut self,
        naUrl: &str,
        func: &Function,
        allocResources: &NodeResources,
        resourceQuota: &NodeResources,
        createType: na::CreatePodType,
        terminatePods: &Vec<WorkerPod>,
    ) -> Result<u64> {
        let tenant = &func.tenant;
        let namespace = &func.namespace;
        let funcname = &func.name;
        let fpRevision = func.Version();
        let id = UID.get().unwrap().Get().await? as u64; // inner.NextWorkerId();

        let mut client =
            na::node_agent_service_client::NodeAgentServiceClient::connect(naUrl.to_owned())
                .await?;

        let mut annotations = Vec::new();
        annotations.push(Kv {
            key: FUNCPOD_TYPE.to_owned(),
            val: FUNCPOD_PROMPT.to_owned(),
        });

        annotations.push(Kv {
            key: FUNCPOD_FUNCNAME.to_owned(),
            val: funcname.to_owned(),
        });

        let mut tps = Vec::new();
        for p in terminatePods {
            let pod = p.pod.clone();
            let termniatePod = TerminatePodReq {
                tenant: pod.tenant.clone(),
                namespace: pod.namespace.clone(),
                funcname: pod.object.spec.funcname.clone(),
                fprevision: pod.object.spec.fprevision.clone(),
                id: pod.object.spec.id.clone(),
            };
            tps.push(termniatePod);
        }

        let request = tonic::Request::new(na::CreateFuncPodReq {
            tenant: tenant.to_owned(),
            namespace: namespace.to_owned(),
            funcname: funcname.to_owned(),
            fprevision: fpRevision,
            id: format!("{id}"),
            labels: Vec::new(),
            annotations: annotations,
            create_type: createType.into(),
            funcspec: serde_json::to_string(&func.object.spec)?,
            alloc_resources: serde_json::to_string(allocResources).unwrap(),
            resource_quota: serde_json::to_string(resourceQuota).unwrap(),
            terminate_pods: tps,
        });

        // use another thread to start pod to avoid block main thread
        let _handle = tokio::spawn(async move {
            let response = match client.create_func_pod(request).await {
                Err(e) => {
                    error!("StartWorker create_func_pod fail with error {:?}", e);
                    return;
                }
                Ok(r) => r,
            };
            let resp = response.into_inner();
            if !resp.error.is_empty() {
                error!("StartWorker fail with error {}", &resp.error);
            }
        });

        return Ok(id);
    }

    // pub async fn HibernateWorker(
    //     &self,
    //     naUrl: &str,
    //     tenant: &str,
    //     namespace: &str,
    //     funcname: &str,
    //     fprevision: i64,
    //     id: &str,
    // ) -> Result<()> {
    //     let mut client =
    //         na::node_agent_service_client::NodeAgentServiceClient::connect(naUrl.to_owned())
    //             .await?;

    //     let request = tonic::Request::new(na::HibernatePodReq {
    //         tenant: tenant.to_owned(),
    //         namespace: namespace.to_owned(),
    //         funcname: funcname.to_owned(),
    //         fprevision: fprevision,
    //         id: id.to_owned(),
    //         hibernate_type: 1,
    //     });
    //     let response = client.hibernate_pod(request).await?;
    //     let resp = response.into_inner();
    //     if resp.error.len() != 0 {
    //         error!(
    //             "Scheduler: Fail to Hibernate worker {} {} {} {}",
    //             namespace, funcname, id, resp.error
    //         );
    //     }

    //     return Ok(());
    // }

    // pub async fn WakeupWorker(
    //     &self,
    //     naUrl: &str,
    //     tenant: &str,
    //     namespace: &str,
    //     funcname: &str,
    //     fprevsion: i64,
    //     id: &str,
    //     allocResources: &NodeResources,
    // ) -> Result<()> {
    //     let mut client =
    //         na::node_agent_service_client::NodeAgentServiceClient::connect(naUrl.to_owned())
    //             .await?;

    //     let request = tonic::Request::new(na::WakeupPodReq {
    //         tenant: tenant.to_owned(),
    //         namespace: namespace.to_owned(),
    //         funcname: funcname.to_owned(),
    //         fprevision: fprevsion,
    //         id: id.to_owned(),
    //         hibernate_type: 1,
    //         alloc_resources: serde_json::to_string(allocResources).unwrap(),
    //     });
    //     let response = client.wakeup_pod(request).await?;
    //     let resp = response.into_inner();
    //     if resp.error.len() != 0 {
    //         error!(
    //             "Scheduler: Fail to Wakeup worker {} {} {} {}",
    //             namespace, funcname, id, resp.error
    //         );
    //     }

    //     return Ok(());
    // }

    pub async fn ResumeWorker(
        &self,
        naUrl: &str,
        tenant: &str,
        namespace: &str,
        funcname: &str,
        fprevsion: i64,
        id: &str,
        allocResources: &NodeResources,
        terminatePods: &Vec<WorkerPod>,
    ) -> Result<()> {
        let mut client =
            na::node_agent_service_client::NodeAgentServiceClient::connect(naUrl.to_owned())
                .await?;

        let mut tps = Vec::new();
        for p in terminatePods {
            let pod = p.pod.clone();
            let termniatePod = TerminatePodReq {
                tenant: pod.tenant.clone(),
                namespace: pod.namespace.clone(),
                funcname: pod.object.spec.funcname.clone(),
                fprevision: pod.object.spec.fprevision.clone(),
                id: pod.object.spec.id.clone(),
            };
            tps.push(termniatePod);
        }

        let request = tonic::Request::new(na::ResumePodReq {
            tenant: tenant.to_owned(),
            namespace: namespace.to_owned(),
            funcname: funcname.to_owned(),
            fprevision: fprevsion,
            id: id.to_owned(),
            alloc_resources: serde_json::to_string(allocResources).unwrap(),
            terminate_pods: tps,
        });

        let response = client.resume_pod(request).await?;
        let resp = response.into_inner();
        if resp.error.len() != 0 {
            error!(
                "Scheduler: Fail to ResumeWorker worker {} {} {} {}",
                namespace, funcname, id, resp.error
            );
        }

        return Ok(());
    }

    pub async fn StopWorker(&mut self, pod: &FuncPod) -> Result<()> {
        let naUrl;

        {
            let nodename = pod.object.spec.nodename.clone();
            let nodeStatus = self.nodes.get_mut(&nodename).unwrap();
            if pod.object.status.state != PodState::Failed {
                // failure pod resource has been freed
                self.stoppingPods.insert(pod.PodKey());
                nodeStatus.FreeResource(&pod.object.spec.allocResources, &pod.PodKey())?;
            }

            naUrl = nodeStatus.node.NodeAgentUrl();
        }

        return self
            .StopWorkerInner(
                &naUrl,
                &pod.tenant,
                &pod.namespace,
                &pod.object.spec.funcname,
                pod.object.spec.fprevision,
                &pod.object.spec.id,
            )
            .await;
    }

    pub async fn StopWorkerInner(
        &self,
        naUrl: &str,
        tenant: &str,
        namespace: &str,
        funcname: &str,
        fprevision: i64,
        id: &str,
    ) -> Result<()> {
        let mut client =
            na::node_agent_service_client::NodeAgentServiceClient::connect(naUrl.to_owned())
                .await?;

        let request = tonic::Request::new(na::TerminatePodReq {
            tenant: tenant.to_owned(),
            namespace: namespace.to_owned(),
            funcname: funcname.to_owned(),
            fprevision: fprevision,
            id: id.to_owned(),
        });
        let response = client.terminate_pod(request).await?;
        let resp = response.into_inner();
        if resp.error.len() != 0 {
            error!(
                "Fail to stop worker {} {} {} {} {}",
                tenant, namespace, funcname, id, resp.error
            );
        }

        return Ok(());
    }

    // return whether all list done
    pub async fn ListDone(&mut self, listType: ListType) -> Result<bool> {
        match listType {
            ListType::Func => self.funcListDone = true,
            ListType::FuncPod => self.funcPodListDone = true,
            ListType::Node => self.nodeListDone = true,
            ListType::Snapshot => self.snapshotListDone = true,
        }

        if self.nodeListDone && self.funcListDone && self.funcPodListDone && self.snapshotListDone {
            self.listDone = true;

            self.RefreshScheduling().await?;

            return Ok(true);
        }

        return Ok(false);
    }

    pub fn AddNode(&mut self, node: Node) -> Result<()> {
        info!("add node {:#?}", &node);

        let nodeName = node.name.clone();
        if self.nodes.contains_key(&nodeName) {
            return Err(Error::Exist(format!("NodeMgr::add {}", nodeName)));
        }

        let total = node.object.resources.clone();
        let pods = match self.nodePods.remove(&nodeName) {
            None => BTreeMap::new(),
            Some(pods) => pods,
        };
        let nodeStatus = NodeStatus::New(node, total, pods)?;

        self.nodes.insert(nodeName, nodeStatus);

        return Ok(());
    }

    pub fn UpdateNode(&mut self, node: Node) -> Result<()> {
        let nodeName = node.name.clone();
        if !self.nodes.contains_key(&nodeName) {
            return Err(Error::NotExist(format!("NodeMgr::UpdateNode {}", nodeName)));
        }

        error!("UpdateNode the node is {:#?}", &node);

        let ns = self.nodes.get_mut(&nodeName).unwrap();
        ns.state = node.object.state;
        ns.node = node;

        return Ok(());
    }

    pub fn RemoveNode(&mut self, node: Node) -> Result<()> {
        info!("remove node {}", &node.name);
        let key = node.name.clone();
        if !self.nodes.contains_key(&key) {
            return Err(Error::NotExist(format!("NodeMgr::Remove {}", key)));
        }

        self.nodes.remove(&key);

        return Ok(());
    }

    pub fn AddPod(&mut self, pod: FuncPod) -> Result<()> {
        let podKey = pod.PodKey();
        let nodename = pod.object.spec.nodename.clone();
        let fpKey = pod.FuncKey();

        let boxPod: WorkerPod = pod.into();
        assert!(self.pods.insert(podKey.clone(), boxPod.clone()).is_none());

        if boxPod.State().IsIdle() && boxPod.pod.object.status.state == PodState::Ready {
            let returnId = boxPod.SetIdle();

            self.idlePods.insert(returnId, boxPod.pod.PodKey());
        }

        match self.nodes.get_mut(&nodename) {
            None => match self.nodePods.get_mut(&nodename) {
                None => {
                    let mut pods = BTreeMap::new();
                    pods.insert(podKey.clone(), boxPod.clone());
                    self.nodePods.insert(nodename, pods);
                }
                Some(pods) => {
                    pods.insert(podKey.clone(), boxPod.clone());
                }
            },
            Some(nodeStatus) => {
                nodeStatus.AddPod(&boxPod)?;
            }
        }

        match self.funcs.get_mut(&fpKey) {
            None => match self.packagePods.get_mut(&fpKey) {
                None => {
                    let mut pods = BTreeMap::new();
                    pods.insert(podKey, boxPod);
                    self.packagePods.insert(fpKey, pods);
                }
                Some(pods) => {
                    pods.insert(podKey, boxPod);
                }
            },
            Some(fpStatus) => fpStatus.AddPod(&boxPod)?,
        }

        return Ok(());
    }

    pub async fn UpdatePod(&mut self, pod: FuncPod) -> Result<()> {
        let podKey = pod.PodKey();
        let nodeName = pod.object.spec.nodename.clone();
        let funcKey = pod.FuncKey();

        let boxPod: WorkerPod = pod.into();

        let oldPod = self
            .pods
            .insert(podKey.clone(), boxPod.clone())
            .expect("UpdatePod get none old pod");

        match self.nodes.get_mut(&nodeName) {
            None => match self.nodePods.get_mut(&nodeName) {
                None => {
                    let mut pods = BTreeMap::new();
                    pods.insert(podKey.clone(), boxPod.clone());
                    self.nodePods.insert(nodeName, pods);
                }
                Some(pods) => {
                    pods.insert(podKey.clone(), boxPod.clone());
                }
            },
            Some(nodeStatus) => {
                nodeStatus.UpdatePod(&boxPod, &oldPod)?;
            }
        }

        match self.funcs.get_mut(&funcKey) {
            None => match self.packagePods.get_mut(&funcKey) {
                None => {
                    let mut pods = BTreeMap::new();
                    pods.insert(podKey, boxPod);
                    self.packagePods.insert(funcKey, pods);
                }
                Some(pods) => {
                    pods.insert(podKey, boxPod);
                }
            },
            Some(fpStatus) => {
                fpStatus.UpdatePod(&boxPod)?;
            }
        }

        return Ok(());
    }

    pub async fn RemovePod(&mut self, pod: &FuncPod) -> Result<()> {
        let podKey: String = pod.PodKey();
        let nodeName = pod.object.spec.nodename.clone();
        let funcKey = pod.FuncKey();

        assert!(self.pods.remove(&podKey).is_some());

        let podCreateType = pod.object.spec.create_type;

        let state = pod.object.status.state;
        if state == PodState::Failed {
            match self.funcs.get(&funcKey) {
                None => (),
                Some(status) => {
                    let mut func = status.func.clone();

                    error!(
                        "RemovePod failure pod {} with podCreateType state {:?}",
                        &podKey, podCreateType
                    );
                    match podCreateType {
                        CreatePodType::Snapshot => {
                            func.object.status.snapshotingFailureCnt += 1;
                            if func.object.status.snapshotingFailureCnt >= 3 {
                                func.object.status.state = FuncState::Fail;
                            }

                            let client = GetClient().await.unwrap();

                            // update the func
                            client.Update(&func.DataObject(), 0).await.unwrap();
                        }
                        CreatePodType::Restore => {
                            func.object.status.resumingFailureCnt += 1;
                            // if func.object.status.resumingFailureCnt >= 3 {
                            //     func.object.status.state = FuncState::Fail;
                            // }

                            // restore failure update will lead all pod reset
                            // todo: put the resumingFailureCnt in another database
                        }
                        _ => (),
                    }
                }
            }
        } else {
            match self.funcs.get(&funcKey) {
                None => (),
                Some(status) => match podCreateType {
                    CreatePodType::Restore => {
                        let mut func = status.func.clone();
                        func.object.status.resumingFailureCnt = 0;
                    }
                    _ => (),
                },
            }
        }

        match self.nodes.get_mut(&nodeName) {
            None => (), // node information doesn't reach scheduler, will process when it arrives
            Some(nodeStatus) => {
                let stopping = self.stoppingPods.remove(&pod.PodKey());
                nodeStatus.RemovePod(&pod.PodKey(), &pod.object.spec.allocResources, stopping)?;
            }
        }
        match self.funcs.get_mut(&funcKey) {
            None => (), // fp information doesn't reach scheduler, will process when it arrives
            Some(fpStatus) => {
                fpStatus.RemovePod(&pod.PodKey()).unwrap();
            }
        }

        if podCreateType == CreatePodType::Snapshot {
            self.pendingsnapshots.remove(&funcKey);
        }

        return Ok(());
    }

    pub fn AddFunc(&mut self, spec: Function) -> Result<()> {
        let fpId = spec.Id();
        if self.funcs.contains_key(&fpId) {
            return Err(Error::Exist(format!("FuncMgr::add {}", fpId)));
        }

        let pods = match self.packagePods.remove(&fpId) {
            None => BTreeMap::new(),
            Some(pods) => pods,
        };

        let package = spec;
        let fpStatus = FuncStatus::New(package, pods)?;
        self.funcs.insert(fpId, fpStatus);

        return Ok(());
    }

    pub fn RemoveFunc(&mut self, spec: Function) -> Result<()> {
        let key = spec.Id();
        if !self.funcs.contains_key(&key) {
            return Err(Error::NotExist(format!(
                "FuncMgr::Remove {}/{:?}",
                key,
                self.funcs.keys()
            )));
        }

        self.funcs.remove(&key);

        return Ok(());
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ListType {
    Node,
    FuncPod,
    Func,
    Snapshot,
}

pub async fn GetClient() -> Result<CacherClient> {
    use rand::Rng;

    let addrs = &SCHEDULER_CONFIG.stateSvcAddrs;
    let size = addrs.len();
    let offset: usize = rand::thread_rng().gen_range(0..size);
    for i in 0..size {
        let idx = (offset + i) % size;
        let addr = &addrs[idx];

        match CacherClient::New(addr.clone()).await {
            Ok(client) => return Ok(client),
            Err(e) => {
                println!(
                    "informer::GetClient fail to connect to {} with error {:?}",
                    addr, e
                );
            }
        }
    }

    let errstr = format!(
        "GetClient fail: can't connect any valid statesvc {:?}",
        addrs
    );
    return Err(Error::CommonError(errstr));
}
