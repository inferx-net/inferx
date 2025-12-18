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

use indexmap::IndexSet;
use inferxlib::data_obj::ObjRef;
use inferxlib::obj_mgr::funcpolicy_mgr::FuncPolicy;
use inferxlib::obj_mgr::funcpolicy_mgr::FuncPolicySpec;
use inferxlib::obj_mgr::funcstatus_mgr::FunctionStatus;
use rand::prelude::SliceRandom;
use rand::thread_rng;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::BinaryHeap;
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
use tokio::time::{Duration, Interval};

use crate::audit::SnapshotScheduleAudit;
use crate::audit::SqlAudit;
use crate::audit::POD_AUDIT_AGENT;
use crate::common::*;
use crate::gateway::metrics::Nodelabel;
use crate::gateway::metrics::PodLabels;
use crate::gateway::metrics::SCHEDULER_METRICS;
use crate::metastore::cacher_client::CacherClient;
use crate::metastore::unique_id::UID;
use crate::na::RemoveSnapshotReq;
use crate::na::TerminatePodReq;
use crate::peer_mgr::PeerMgr;
use crate::scheduler::scheduler::SetIdleSource;
use crate::scheduler::scheduler::TimedTask;
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

use super::scheduler::BiIndex;
use super::scheduler::FuncNodePair;
use super::scheduler::SchedTask;
use super::scheduler::SnapshotScheduleInfo;
use super::scheduler::SnapshotScheduleState;
use super::scheduler::TaskQueue;
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

        error!("FreeResource RemovePod ...");
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

    pub fn ReadyResourceQuota(&self, req: &Resources) -> Result<NodeResources> {
        let res = self.available.ReadyResourceQuota(req);
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

pub trait ResourceAlloc {
    fn CanAlloc(&self, req: &Resources, createSnapshot: bool) -> AllocState;
    fn Alloc(&mut self, req: &Resources, createSnapshot: bool) -> Result<NodeResources>;
}

impl ResourceAlloc for NodeResources {
    fn CanAlloc(&self, req: &Resources, createSnapshot: bool) -> AllocState {
        return AllocState {
            cpu: self.cpu >= req.cpu,
            memory: self.memory >= req.memory,
            cacheMem: self.cacheMemory >= req.cacheMemory,
            gpuType: self.gpuType.CanAlloc(&req.gpu.type_),
            gpu: self.gpus.CanAlloc(&req.gpu, createSnapshot).is_some(),
        };
    }

    fn Alloc(&mut self, req: &Resources, createSnapshot: bool) -> Result<NodeResources> {
        let state = self.CanAlloc(req, createSnapshot);
        if !state.Ok() {
            return Err(Error::ScheduleFail(state));
        }

        self.memory -= req.memory;
        self.cacheMemory -= req.cacheMemory;
        let gpus = self.gpus.Alloc(&req.gpu, createSnapshot)?;

        return Ok(NodeResources {
            nodename: self.nodename.clone(),
            cpu: req.cpu,
            memory: req.memory,
            cacheMemory: req.cacheMemory,
            gpuType: self.gpuType.clone(),
            gpus: gpus,
            maxContextCnt: self.maxContextCnt,
        });
    }
}

#[derive(Debug, Clone)]
pub struct LeaseReq {
    pub req: na::LeaseWorkerReq,
    pub time: SystemTime,
}

#[derive(Debug)]
pub struct FuncStatus {
    pub func: Function,

    pub pods: BTreeMap<String, WorkerPod>,
    // podname --> PendingPod
    pub pendingPods: BTreeMap<String, PendingPod>,

    pub leaseWorkerReqs: VecDeque<(LeaseReq, Sender<na::LeaseWorkerResp>)>,
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
        let req = LeaseReq {
            req: req,
            time: SystemTime::now(),
        };
        self.leaseWorkerReqs.push_back((req, tx));
    }

    pub fn PopLeaseWorkerReq(&mut self) -> Option<(LeaseReq, Sender<na::LeaseWorkerResp>)> {
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

    pub async fn UpdatePod(&mut self, pod: &WorkerPod) -> Result<()> {
        let podKey = pod.pod.PodKey();
        if self.pods.insert(podKey.clone(), pod.clone()).is_none() {
            error!("podkey is {}", &podKey);
            panic!("podkey is {}", &podKey);
        }

        if pod.pod.object.status.state == PodState::Ready && pod.State().IsIdle() {
            loop {
                match self.PopLeaseWorkerReq() {
                    Some((req, tx)) => {
                        let elapsed = req.time.elapsed().unwrap().as_millis();

                        let req = &req.req;
                        pod.SetWorking(req.gateway_id); // first time get ready, we don't need to remove it from idlePods in scheduler

                        let labels = PodLabels {
                            tenant: req.tenant.clone(),
                            namespace: req.namespace.clone(),
                            funcname: req.funcname.clone(),
                            revision: req.fprevision,
                            nodename: pod.pod.object.spec.nodename.clone(),
                        };
                        SCHEDULER_METRICS
                            .lock()
                            .await
                            .podLeaseCnt
                            .get_or_create(&labels)
                            .inc();

                        SCHEDULER_METRICS
                            .lock()
                            .await
                            .coldStartPodLatency
                            .get_or_create(&labels)
                            .observe(elapsed as f64 / 1000.0);

                        let nodelabel = Nodelabel {
                            nodename: pod.pod.object.spec.nodename.clone(),
                        };

                        let gpuCnt = pod.pod.object.spec.reqResources.gpu.gpuCount;

                        let cnt = SCHEDULER_METRICS
                            .lock()
                            .await
                            .usedGpuCnt
                            .Inc(nodelabel.clone(), gpuCnt);

                        SCHEDULER_METRICS
                            .lock()
                            .await
                            .usedGPU
                            .get_or_create(&nodelabel)
                            .set(cnt as i64);

                        error!("user GPU inc {:?} {} {}", &req.funcname, gpuCnt, cnt);

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
                        pod.SetIdle(SetIdleSource::UpdatePod);
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
    ConnectScheduler((na::ConnectReq, Sender<na::ConnectResp>)),
    LeaseWorker((na::LeaseWorkerReq, Sender<na::LeaseWorkerResp>)),
    ReturnWorker((na::ReturnWorkerReq, Sender<na::ReturnWorkerResp>)),
    RefreshGateway(na::RefreshGatewayReq),
}

#[derive(Debug, Default)]
pub struct SchedulerHandler {
    // nodename -> nodeEpoch
    pub nodeEpoch: BTreeMap<String, i64>,

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
    pub funcstatus: BTreeMap<String, FunctionStatus>,

    /*********************snapshot ******************* */
    // funcid -> [nodename->SnapshotState]
    pub snapshots: BTreeMap<String, BTreeMap<String, ContainerSnapshot>>,

    // funcid -> BTreeSet<nodename>
    pub pendingsnapshots: BTreeMap<String, BTreeSet<String>>,

    /********************idle pods ************************* */
    // returnId --> PodKey()
    pub idlePods: IndexSet<String>,

    /********************stopping pods ************************* */
    pub stoppingPods: BTreeSet<String>,

    // temp pods storage when the func is not ready
    // funcname name -> <Podid --> WorkerPod>
    pub funcPods: BTreeMap<String, BTreeMap<String, WorkerPod>>,

    // Snapshot schedule state, id1: funcid, id2: nodename
    pub SnapshotSched: BiIndex<SnapshotScheduleInfo>,

    pub funcpolicy: BTreeMap<String, FuncPolicySpec>,

    pub nodeListDone: bool,
    pub funcListDone: bool,
    pub funcstatusListDone: bool,
    pub funcPodListDone: bool,
    pub snapshotListDone: bool,
    pub funcpolicyDone: bool,
    pub listDone: bool,

    pub nextWorkId: AtomicU64,

    pub taskQueue: TaskQueue,

    delayed_tasks: BinaryHeap<TimedTask>,

    delay_interval: Option<Interval>,
}

impl SchedulerHandler {
    pub fn New() -> Self {
        return Self {
            nextWorkId: AtomicU64::new(1),
            delayed_tasks: BinaryHeap::new(),
            ..Default::default()
        };
    }

    pub fn schedule_delayed_task(&mut self, delay: Duration, task: SchedTask) {
        let when = Instant::now() + delay;
        self.delayed_tasks.push(TimedTask { when, task });
    }

    pub fn drain_due_delayed_tasks(&mut self) {
        let now = Instant::now();
        while let Some(entry) = self.delayed_tasks.peek() {
            if entry.when <= now {
                let TimedTask { task, .. } = self.delayed_tasks.pop().unwrap();
                self.taskQueue.AddTask(task);
            } else {
                break;
            }
        }
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
                        let gpuCnt = worker.pod.object.spec.reqResources.gpu.gpuCount;
                        let nodelabel = Nodelabel {
                            nodename: worker.pod.object.spec.nodename.clone(),
                        };

                        let cnt = SCHEDULER_METRICS
                            .lock()
                            .await
                            .usedGpuCnt
                            .Dec(nodelabel.clone(), gpuCnt);

                        SCHEDULER_METRICS
                            .lock()
                            .await
                            .usedGPU
                            .get_or_create(&nodelabel)
                            .set(cnt as i64);

                        error!(
                            "user GPU desc timeout {:?} {} {}",
                            &worker.pod.object.spec.funcname, gpuCnt, cnt
                        );

                        worker.SetIdle(SetIdleSource::ProcessGatewayTimeout);
                        // how to handle the recovered failure gateway?
                        let podKey = worker.pod.PodKey();
                        self.idlePods.insert(podKey.clone());
                    }
                }
                _ => (),
            }
        }

        return Ok(());
    }

    pub async fn ProcessConnectReq(
        &self,
        req: na::ConnectReq,
        tx: Sender<na::ConnectResp>,
    ) -> Result<()> {
        let gatewayId = req.gateway_id;
        let mut pods = Vec::new();
        for w in &req.workers {
            let pod =
                match self.GetFuncPod(&w.tenant, &w.namespace, &w.funcname, w.fprevision, &w.id) {
                    Err(_e) => {
                        error!("ProcessConnectReq get non-exist pod {:?}", w);
                        continue;
                    }
                    Ok(p) => p,
                };

            if pod.State() != WorkerPodState::Idle
                && pod.State() != WorkerPodState::Working(gatewayId)
            // the scheduler lose connect and reconnect, will it happend
            {
                let resp = na::ConnectResp {
                    error: format!(
                        "ProcessConnectReq the leasing worker {:?} has been reassigned, state {:?} expect idle or {:?} \n likely the scheduler restarted and the gateway doesn't connect ontime",
                        w, pod.State(), gatewayId
                    ),
                    ..Default::default()
                };
                tx.send(resp).unwrap();
                return Ok(());
            }

            pods.push(pod);
        }

        for pod in &pods {
            pod.SetWorking(gatewayId);
        }

        let resp = na::ConnectResp {
            error: String::new(),
        };
        tx.send(resp).ok();

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
            error!(
                "ProcessLeaseWorkerReq pod {:?} state {:?}",
                pod.PodKey(),
                worker.State()
            );
            if pod.object.status.state == PodState::Ready && worker.State().IsIdle() {
                worker.SetWorking(req.gateway_id);
                let podKey = worker.pod.PodKey();
                let remove = self.idlePods.shift_remove(&podKey);
                error!("ProcessLeaseWorkerReq using idlepod work {:?}", &remove);

                let peer = match PEER_MGR.LookforPeer(pod.object.spec.ipAddr) {
                    Ok(p) => p,
                    Err(e) => {
                        return Err(e);
                    }
                };

                let labels = PodLabels {
                    tenant: req.tenant.clone(),
                    namespace: req.namespace.clone(),
                    funcname: req.funcname.clone(),
                    revision: req.fprevision,
                    nodename: pod.object.spec.nodename.clone(),
                };

                SCHEDULER_METRICS
                    .lock()
                    .await
                    .podLeaseCnt
                    .get_or_create(&labels)
                    .inc();

                let nodelabel = Nodelabel {
                    nodename: pod.object.spec.nodename.clone(),
                };

                let gpuCnt = pod.object.spec.reqResources.gpu.gpuCount;

                let cnt = SCHEDULER_METRICS
                    .lock()
                    .await
                    .usedGpuCnt
                    .Inc(nodelabel.clone(), gpuCnt);

                SCHEDULER_METRICS
                    .lock()
                    .await
                    .usedGPU
                    .get_or_create(&nodelabel)
                    .set(cnt as i64);

                error!("user GPU inc {:?} {} {}", &req.funcname, gpuCnt, cnt);
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

        let funcname = format!(
            "{}/{}/{}/{}",
            &req.tenant, &req.namespace, &req.funcname, req.fprevision
        );

        let fp = match self.funcs.get(&funcname) {
            None => {
                let resp = na::LeaseWorkerResp {
                    error: format!(
                        "ProcessLeaseWorkerReq can't find func with id {} {:?}",
                        funcname,
                        self.funcs.keys(),
                    ),
                    ..Default::default()
                };
                tx.send(resp).unwrap();
                return Err(Error::NotExist(format!(
                    "ProcessLeaseWorkerReq can't find func with id {} {:?}",
                    funcname,
                    self.funcs.keys(),
                )));
            }
            Some(fpStatus) => fpStatus.func.clone(),
        };

        let policy = self.FuncPolicy(&req.tenant, &fp.object.spec.policy);

        let readyCnt = self.ReadyPodCount(&funcname);

        if policy.maxReplica <= readyCnt as u64 {
            let resp = na::LeaseWorkerResp {
                error: format!(
                    "ProcessLeaseWorkerReq can't create pod for func {} as reach max_replica limitation {} ready pod count {}",
                    funcname,
                    policy.maxReplica,
                    readyCnt
                ),
                ..Default::default()
            };
            tx.send(resp).unwrap();
            return Err(Error::NotExist(format!(
                "ProcessLeaseWorkerReq can't create pod for func {} as reach max_replica limitation {} ready pod count {}",
                funcname,
                policy.maxReplica,
                readyCnt
            )));
        }

        match self.ResumePod(&funcname).await {
            Err(e) => {
                let resp = na::LeaseWorkerResp {
                    error: format!("{:?}", e),
                    ..Default::default()
                };
                tx.send(resp).unwrap();
            }
            Ok(_) => {
                self.PushLeaseWorkerReq(&funcname, req, tx)?;
            }
        }

        return Ok(());
    }

    pub async fn ProcessReturnWorkerReq(
        &mut self,
        req: na::ReturnWorkerReq,
        tx: Sender<na::ReturnWorkerResp>,
    ) -> Result<()> {
        let worker = match self.GetFuncPod(
            &req.tenant,
            &req.namespace,
            &req.funcname,
            req.fprevision,
            &req.id,
        ) {
            Err(e) => {
                error!(
                    "ProcessReturnWorkerReq can't find pod {:#?} with error e {:?}",
                    req, e
                );
                return Ok(());
            }
            Ok(w) => w,
        };

        info!("ProcessReturnWorkerReq return pod {}", worker.pod.PodKey());

        if worker.State().IsIdle() {
            // when the scheduler restart, this issue will happen, fix this.
            error!(
                "ProcessReturnWorkerReq fail the {} state {:?}, likely there is scheduler restart",
                worker.pod.PodKey(),
                worker.State()
            );
        }

        let nodelabel = Nodelabel {
            nodename: worker.pod.object.spec.nodename.clone(),
        };

        let gpuCnt = worker.pod.object.spec.reqResources.gpu.gpuCount;

        let cnt = SCHEDULER_METRICS
            .lock()
            .await
            .usedGpuCnt
            .Dec(nodelabel.clone(), gpuCnt);

        SCHEDULER_METRICS
            .lock()
            .await
            .usedGPU
            .get_or_create(&nodelabel)
            .set(cnt as i64);

        error!("user GPU desc {:?} {} {}", &req.funcname, gpuCnt, cnt);
        // in case the gateway dead and recover and try to return an out of date pod
        // assert!(
        //     !worker.State().IsIdle(),
        //     "the state is {:?}",
        //     worker.State()
        // );

        if req.failworker {
            error!(
                "ProcessReturnWorkerReq return and kill failure pod {}",
                worker.pod.PodKey()
            );
            let state = worker.State();
            // the gateway might lease the failure pod again and return multiple times.
            // we can't stopworker mutiple times. do some check.
            if state != WorkerPodState::Terminating {
                worker.SetState(WorkerPodState::Terminating);
                match self.StopWorker(&worker.pod).await {
                    Ok(()) => (),
                    Err(e) => {
                        error!(
                            "ProcessReturnWorkerReq kill failure pod: fail to stop func worker {:?} with error {:#?}",
                            worker.pod.PodKey(),
                            e
                        );
                    }
                }
            }
        } else {
            worker.SetIdle(SetIdleSource::ProcessReturnWorkerReq);
            self.idlePods.insert(worker.pod.PodKey());
        }

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
                // error!("get function key is {} keys {:#?}", &fpkey, self.funcs.keys());
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
        if !self.snapshots.contains_key(&funckey) {
            error!(
                "UpdateSnapshot get snapshot will non exist funckey {}",
                funckey
            );
            return Ok(());
        }

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
        info!("RemoveSnapshotByFunckey remove {} start", funckey);
        let snapshot = self.snapshots.get(funckey);
        let mut deletes = Vec::new();
        if let Some(snapshot) = snapshot {
            let nodes: Vec<String> = snapshot.keys().cloned().collect();
            for nodename in nodes {
                let ret = self.RemoveSnapshotFromNode(&nodename, &funckey).await;
                match ret {
                    Ok(()) => {
                        deletes.push(nodename);
                    }
                    Err(e) => {
                        error!(
                            "fail to remove snapshot {:?} from node {} with error {:?}",
                            funckey, nodename, e
                        );
                    }
                }
            }
        }

        let mut remove = false;
        let snapshot = self.snapshots.get_mut(funckey);
        if let Some(snapshot) = snapshot {
            for nodename in deletes {
                snapshot.remove(&nodename);
            }

            if snapshot.len() == 0 {
                remove = true;
            }
        }

        if remove {
            info!("RemoveSnapshotByFunckey remove {} done", funckey);
            self.snapshots.remove(funckey);
        } else {
            info!(
                "RemoveSnapshotByFunckey remove {} fail, will retry later",
                funckey
            );
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
            if self.HasPod(&funckey) {
                // we delete snapshot only after related pod are removed
                continue;
            }

            match self.RemoveSnapshotByFunckey(&funckey).await {
                Ok(()) => (),
                Err(e) => {
                    error!("CleanSnapshots get fail {:?}", e);
                }
            }
        }

        return Ok(());
    }

    pub async fn CleanPods(&mut self) -> Result<()> {
        let mut cleanfuncs = BTreeSet::new();
        for (_podkey, pod) in &self.pods {
            if !self.funcs.contains_key(&pod.pod.FuncKey()) {
                cleanfuncs.insert(pod.pod.FuncKey());
            }
        }

        // if cleanfuncs.len() > 0 {
        //     error!("CleanPods available funcs {:?}", &self.funcs.keys());
        //     error!("CleanPods clear funcs {:?}", &cleanfuncs);
        // }

        for funckey in &cleanfuncs {
            match self.RemovePodsByFunckey(&funckey).await {
                Ok(_) => (),
                Err(e) => {
                    error!("CleanPods get fail {:?}", e);
                }
            }
        }

        return Ok(());
    }

    pub fn HasPod(&self, funckey: &str) -> bool {
        let key = funckey.to_owned() + "/";

        for (_, p) in self.pods.range(key..) {
            if p.pod.FuncKey() == funckey {
                return true;
            } else {
                return false;
            }
        }

        return false;
    }

    // return has pods
    pub async fn RemovePodsByFunckey(&mut self, funckey: &str) -> Result<bool> {
        info!("RemovePodsByFunckey remove {} start", funckey);
        let mut remove = true;
        let mut pods = Vec::new();

        // add "/" after funckey, the key will be <tenant>/<namespace>/<funcname>/<revision>/
        let key = funckey.to_owned() + "/";

        for (_, p) in self.pods.range(key..) {
            if p.pod.FuncKey() == funckey {
                pods.push(p.clone());
                remove = false;
            } else {
                break;
            }
        }

        for p in pods {
            match self.StopWorker(&p.pod).await {
                Err(e) => {
                    info!(
                        "RemovePodsByFunckey stopworker {} fail with error {:?}",
                        p.pod.PodKey(),
                        e
                    );
                }
                Ok(()) => (),
            }
        }

        if remove {
            info!("RemovePodsByFunckey remove {} done", funckey);
            self.funcs.remove(funckey);
        } else {
            info!(
                "RemovePodsByFunckey remove {} fail, will retry later",
                funckey
            );
        }

        return Ok(remove);
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
        if self.delay_interval.is_none() {
            self.delay_interval = Some(tokio::time::interval(Duration::from_millis(100)));
        }
        let delay_interval = self.delay_interval.as_mut().unwrap();
        tokio::select! {
            biased;
            m = msgRx.recv() => {
                if let Some(msg) = m {
                    match msg {
                        WorkerHandlerMsg::ConnectScheduler((m, tx)) => {
                            self.ProcessConnectReq(m, tx).await?;
                        }
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
            _ = delay_interval.tick() => {
                self.drain_due_delayed_tasks();
            }
            _ = interval.tick() => {
                if self.listDone {
                    // retry scheduling to see wheter there is more resource avaiable
                    self.RefreshScheduling().await?;
                    // we need to delete pod at first before clean snapshot
                    self.CleanPods().await?;
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
                                        self.ProcessAddFunc(&funcid).await;
                                        self.taskQueue.AddFunc(&funcid);
                                    }
                                }
                                FunctionStatus::KEY => {
                                    let funcstatus = FunctionStatus::FromDataObject(obj)?;
                                    let funcid = funcstatus.Id();
                                    self.funcstatus.insert(funcid, funcstatus);
                                }
                                Node::KEY => {
                                    let node = Node::FromDataObject(obj)?;
                                    self.CheckNodeEpoch(&node.name, node.object.nodeEpoch).await;
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
                                    self.AddNode(node).await?;

                                }
                                FuncPod::KEY => {
                                    let pod = FuncPod::FromDataObject(obj)?;
                                    self.CheckNodeEpoch(&pod.object.spec.nodename, pod.srcEpoch).await;

                                    self.AddPod(pod.clone())?;
                                }
                                ContainerSnapshot::KEY => {
                                    let snapshot = FuncSnapshot::FromDataObject(obj)?;
                                    self.CheckNodeEpoch(&snapshot.object.nodename, snapshot.srcEpoch).await;
                                    error!("get new snapshot {}", snapshot.StoreKey());
                                    self.AddSnapshot(&snapshot)?;
                                }
                                FuncPolicy::KEY => {
                                    let policy = FuncPolicy::FromDataObject(obj)?;
                                    let key = policy.Key();
                                    self.funcpolicy.insert(key, policy.object);
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
                                        self.ProcessAddFunc(&fpId).await;
                                        self.taskQueue.AddFunc(&fpId);
                                    }
                                }
                                FunctionStatus::KEY => {
                                    let funcstatus = FunctionStatus::FromDataObject(obj)?;
                                    let funcid = funcstatus.Id();
                                    self.funcstatus.insert(funcid, funcstatus);
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
                                    error!("UpdateSnapshot snapshot {}", snapshot.StoreKey());
                                }
                                FuncPolicy::KEY => {
                                    let policy = FuncPolicy::FromDataObject(obj)?;
                                    let key = policy.Key();
                                    self.funcpolicy.insert(key, policy.object);
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
                                FunctionStatus::KEY => {
                                    let funcstatus = FunctionStatus::FromDataObject(obj)?;
                                    let funcid = funcstatus.Id();
                                    self.funcstatus.remove(&funcid);
                                }
                                Node::KEY => {
                                    let node = Node::FromDataObject(obj)?;
                                    let cidr = ipnetwork::Ipv4Network::from_str(&node.object.cidr).unwrap();
                                    PEER_MGR.RemovePeer(cidr.ip().into()).unwrap();
                                    self.RemoveNode(node).await?;
                                }
                                FuncPod::KEY => {
                                    let pod = FuncPod::FromDataObject(obj)?;
                                    self.RemovePod(&pod).await?;
                                }
                                ContainerSnapshot::KEY => {
                                    let snapshot = FuncSnapshot::FromDataObject(obj)?;
                                    self.RemoveSnapshot(&snapshot).await?;
                                    error!("RemoveSnapshot snapshot {}", snapshot.StoreKey());
                                }
                                FuncPolicy::KEY => {
                                    let policy = FuncPolicy::FromDataObject(obj)?;
                                    let key = policy.Key();
                                    self.funcpolicy.remove(&key);
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
                                FunctionStatus::KEY => {
                                    self.ListDone(ListType::FuncStatus).await?;
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
                                FuncPolicy::KEY => {
                                    self.ListDone(ListType::Funcpolicy).await?;
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
            t = self.taskQueue.Next() => {
                let task = match t {
                    None => {
                        return Ok(());
                    }
                    Some(t) => t
                };

                self.ProcessTask(&task).await?;
            }
            _ = closeNotfiy.notified() => {
                return Err(Error::ProcessDone);
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

        if readyResource.readyMemory > 0 {
            readyResource.memory = readyResource.readyMemory;
        } else {
            readyResource.memory -= cacheMemory;
        }

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

    pub fn ReadyPodCount(&self, funcname: &str) -> usize {
        let mut count = 0;
        match self.funcs.get(funcname) {
            None => (),
            Some(status) => {
                for (_, pod) in &status.pods {
                    // error!(
                    //     "ReadyPodCount podname {:?}/{:?}",
                    //     pod.pod.PodKey(),
                    //     pod.State()
                    // );
                    if pod.State().IsResumed() {
                        count += 1;
                    }
                }
            }
        }

        return count;
    }

    // find a node to run the pod (snapshot or standy resume), if there is no enough free resource, add list of evaction pods
    // ret ==> (node, evacation pods)
    pub async fn FindNode4Pod(
        &mut self,
        func: &Function,
        forStandby: bool,
        candidateNodes: &BTreeSet<String>,
        createSnapshot: bool,
    ) -> Result<(String, Vec<WorkerPod>, NodeResources)> {
        let mut nodeSnapshots = BTreeMap::new();
        let mut allocStates = BTreeMap::new();

        // go through candidate list to look for node has enough free resource, if so return
        // TODO may need wait for nodes info to be available
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

            let state = nr.CanAlloc(&req, createSnapshot);
            if state.Ok() {
                info!(
                    "FindNode4Pod 1 for resuming func {:?} with nr {:#?}",
                    func.Id(),
                    &nr
                );

                return Ok((nodename.clone(), Vec::new(), nr));
            }

            allocStates.insert(nodename.clone(), state);
            nodeSnapshots.insert(nodename.clone(), (nr, Vec::<WorkerPod>::new()));
        }

        let mut missWorkers = Vec::new();

        let mut findnodeName = None;
        let mut nodeResource: NodeResources = NodeResources::default();

        // try to simulate killing idle pods and see whether can find good node
        info!(
            "FindNode4Pod 2 for resuming func {:?} with idle pods {:#?} nodeSnapshots is {:#?}",
            func.Id(),
            &self.idlePods,
            &nodeSnapshots
        );

        for podKey in &self.idlePods {
            match self.pods.get(podKey) {
                None => {
                    missWorkers.push(podKey.to_owned());
                    continue;
                }
                Some(pod) => {
                    // we won't kill another same func instance to start a new one
                    if pod.pod.FuncKey() == func.Id() {
                        continue;
                    }
                    match nodeSnapshots.get_mut(&pod.pod.object.spec.nodename) {
                        None => (),
                        Some((nr, workids)) => {
                            if !self.VerifyMinReplicaPolicy(pod, &workids) {
                                continue;
                            }

                            workids.push(pod.clone());
                            info!(
                                "FindNode4Pod 2 for resuming 2 func {:?} with idle pod {:#?}",
                                func.Id(),
                                podKey,
                            );
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
                            let state = nr.CanAlloc(&req, createSnapshot);
                            if state.Ok() {
                                findnodeName = Some(pod.pod.object.spec.nodename.clone());
                                nodeResource = nr.clone();
                                break;
                            }
                            allocStates.insert(pod.pod.object.spec.nodename.clone(), state);
                        }
                    }
                }
            }
        }

        for podKey in &missWorkers {
            self.idlePods.shift_remove(podKey);
            error!("FindNode4Pod remove idlepod missing {:?}", &podKey);
        }

        if findnodeName.is_none() {
            error!(
                "can't find enough resource for {}, nodes state {:#?}",
                func.Key(),
                allocStates.values()
            );
            return Err(Error::SchedulerNoEnoughResource(format!(
                "can't find enough resource for {}, nodes state {:#?}",
                func.Key(),
                allocStates.values()
            )));
        }

        let nodename = findnodeName.unwrap();

        let (_, workids) = nodeSnapshots.get(&nodename).unwrap().clone();

        return Ok((nodename, workids, nodeResource));
    }

    pub async fn GetBestResumeWorker(
        &mut self,
        fp: &Function,
    ) -> Result<(WorkerPod, Vec<WorkerPod>, NodeResources)> {
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

        let (nodename, termimalworkers, nodeResource) =
            self.FindNode4Pod(fp, true, &nodes, false).await?;

        for worker in &pods {
            if worker.pod.object.status.state != PodState::Standby {
                continue;
            }

            if worker.State() != WorkerPodState::Standby {
                continue;
            }

            if &worker.pod.object.spec.nodename == &nodename {
                return Ok((worker.clone(), termimalworkers, nodeResource));
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
                        if ns.total.CanAlloc(&func.object.spec.resources, true).Ok() {
                            nodes.insert(ns.node.name.clone());
                        }
                    }
                }
            }
        }

        return nodes;
    }

    // pub async fn GetBestNodeToSnapshot(
    //     &mut self,
    //     func: &Function,
    // ) -> Result<(String, Vec<(u64, WorkerPod)>, NodeResources)> {
    //     let snapshotCandidateNodes = self.GetSnapshotCandidateNodes(&func);
    //     if snapshotCandidateNodes.len() == 0 {
    //         return Err(Error::SchedulerErr(format!(
    //             "GetBestNodeToSnapshot can't schedule {}, no enough resource",
    //             func.Id(),
    //         )));
    //     }

    //     let (nodename, workers, nodeResource) = self
    //         .FindNode4Pod(func, false, &snapshotCandidateNodes, true)
    //         .await?;

    //     return Ok((nodename, workers, nodeResource));
    // }

    pub async fn GetBestNodeToRestore(&mut self, fp: &Function) -> Result<String> {
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
        let mut funcids: Vec<String> = self.funcs.keys().cloned().collect();
        funcids.shuffle(&mut thread_rng());
        for fpId in &funcids {
            if self.ProcessAddFunc(fpId).await {
                break;
            }
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

    pub fn InitSnapshotTask(&mut self) -> Result<()> {
        let funcIds: Vec<String> = self.funcs.keys().cloned().collect();
        let nodes: Vec<String> = self.nodes.keys().cloned().collect();
        for funcId in &funcIds {
            for nodename in &nodes {
                self.AddSnapshotTask(nodename, funcId);
            }
        }

        return Ok(());
    }

    pub fn AddSnapshotTask(&mut self, nodename: &str, funcId: &str) {
        self.schedule_delayed_task(
            Duration::from_secs(1),
            SchedTask::SnapshotTask(FuncNodePair {
                nodename: nodename.to_owned(),
                funcId: funcId.to_owned(),
            }),
        );
    }

    pub fn AddStandbyTask(&mut self, nodename: &str) {
        self.schedule_delayed_task(
            Duration::from_secs(1),
            SchedTask::StandbyTask(nodename.to_owned()),
        );
    }

    pub async fn ProcessTask(&mut self, task: &SchedTask) -> Result<()> {
        match task {
            SchedTask::AddNode(nodename) => {
                self.schedule_delayed_task(
                    Duration::from_secs(3),
                    SchedTask::DelayedInitNode(nodename.clone()),
                );
            }
            SchedTask::DelayedInitNode(nodename) => {
                let funcIds: Vec<String> = self.funcs.keys().cloned().collect();
                for funcId in &funcIds {
                    self.AddSnapshotTask(nodename, &funcId);
                }
                self.AddStandbyTask(nodename);
            }
            SchedTask::AddFunc(funcId) => {
                let nodes: Vec<String> = self.nodes.keys().cloned().collect();
                for nodename in &nodes {
                    self.AddSnapshotTask(nodename, funcId);
                }
            }
            SchedTask::SnapshotTask(p) => {
                self.TryCreateSnapshotOnNode(&p.funcId, &p.nodename)
                    .await
                    .ok();
            }
            SchedTask::StandbyTask(nodename) => {
                self.TryCreateStandbyOnNode(&nodename).await.ok();
            }

            _ => (),
        }

        return Ok(());
    }

    pub fn VerifyMinReplicaPolicy(&self, pod: &WorkerPod, workids: &Vec<WorkerPod>) -> bool {
        let evictfuncname = pod.pod.FuncKey();
        let totalevictcnt = {
            let mut count = 0;
            for pod in workids.clone() {
                if &pod.pod.FuncKey() == &evictfuncname {
                    count += 1;
                }
            }
            count
        };

        let policy = self.FuncPolicy(&pod.pod.tenant, &pod.pod.object.spec.funcspec.policy);

        if self.ReadyPodCount(&evictfuncname) - totalevictcnt <= policy.minReplica as usize {
            return false;
        }

        return true;
    }

    pub fn TryFreeResources(
        &mut self,
        nodename: &str,
        funcId: &str,
        available: &mut NodeResources,
        reqResource: &Resources,
        createSnapshot: bool,
    ) -> Result<Vec<WorkerPod>> {
        let mut workids: Vec<WorkerPod> = Vec::new();
        let mut missWorkers = Vec::new();

        for podKey in &self.idlePods {
            match self.pods.get(podKey) {
                None => {
                    missWorkers.push(podKey.to_owned());
                    continue;
                }
                Some(pod) => {
                    if &pod.pod.object.spec.nodename != nodename {
                        continue;
                    }

                    if !self.VerifyMinReplicaPolicy(pod, &workids) {
                        continue;
                    }

                    workids.push(pod.clone());
                    available.Add(&pod.pod.object.spec.allocResources).unwrap();
                    if available.CanAlloc(&reqResource, createSnapshot).Ok() {
                        break;
                    }
                }
            }
        }

        for workerid in &missWorkers {
            self.idlePods.shift_remove(workerid);
            error!("FindNode4Pod remove idlepod missing {:?}", &workerid);
        }

        let state = available.CanAlloc(&reqResource, createSnapshot);
        if !state.Ok() {
            return Err(Error::SchedulerNoEnoughResource(format!(
                "Node {} has no enough free resource to run {} {:?}",
                nodename, funcId, state
            )));
        }

        return Ok(workids);
    }

    pub fn SetSnapshotStatus(
        &mut self,
        funcId: &str,
        nodename: &str,
        state: SnapshotScheduleState,
    ) {
        let update = self.SnapshotSched.Set(
            funcId,
            nodename,
            SnapshotScheduleInfo::New(funcId, nodename, state.clone()),
        );

        if update {
            match SnapshotScheduleAudit::New(funcId, nodename, &state) {
                Err(e) => {
                    error!("SetSnapshotStatus fail with {:?}", e);
                }
                Ok(a) => {
                    POD_AUDIT_AGENT.AuditSnapshotSchedule(a);
                }
            }
        }
    }

    pub async fn GetFuncPodCount(
        &self,
        tenant: &str,
        namespace: &str,
        fpname: &str,
        fprevision: i64,
        podtype: &str,
        nodename: &str,
    ) -> Result<u64> {
        let addr = SCHEDULER_CONFIG.auditdbAddr.clone();
        if addr.len() == 0 {
            return Err(Error::CommonError(format!(
                "GetFuncPodCount can't get auditdbAddr"
            )));
        }
        let sqlaudit = SqlAudit::New(&addr).await?;

        return sqlaudit
            .FuncCount(tenant, namespace, fpname, fprevision, podtype, nodename)
            .await;
    }

    pub async fn TryCreateSnapshotOnNode(&mut self, funcId: &str, nodename: &str) -> Result<()> {
        match self.snapshots.get(funcId) {
            None => (),
            Some(ss) => {
                match ss.get(nodename) {
                    Some(_) => {
                        // there is snapshot on the node
                        return Ok(());
                    }
                    None => (),
                }
            }
        }

        match self.pendingsnapshots.get(funcId) {
            None => (),
            Some(m) => {
                if m.contains(nodename) {
                    // doing snapshot in the node
                    return Ok(());
                }
            }
        }

        let func = match self.funcs.get(funcId) {
            None => return Ok(()),
            Some(fpStatus) => fpStatus.func.clone(),
        };

        match self.funcstatus.get(funcId) {
            None => {
                error!(
                    "TryCreateSnapshotOnNode can't get funcstatus for {}",
                    funcId
                );
            }
            Some(status) => {
                if status.object.state == FuncState::Fail {
                    return Ok(());
                }
            }
        }

        let nodeStatus = match self.nodes.get(nodename) {
            None => return Ok(()),
            Some(n) => n,
        };

        let spec = &func.object.spec;
        if !nodeStatus.node.object.blobStoreEnable {
            if spec.standby.gpuMem == StandbyType::Blob
                || spec.standby.PageableMem() == StandbyType::Blob
                || spec.standby.pinndMem == StandbyType::Blob
            {
                self.SetSnapshotStatus(
                    funcId,
                    nodename,
                    SnapshotScheduleState::Cannot(format!("NodAgent doesn't support blob")),
                );
                return Err(Error::SchedulerNoEnoughResource(
                    "NodAgent doesn't support blob".to_owned(),
                ));
            }
        }

        if nodeStatus.state != NAState::NodeAgentAvaiable {
            self.AddSnapshotTask(nodename, funcId);

            return Err(Error::SchedulerNoEnoughResource(
                "NodAgent node ready".to_owned(),
            ));
        }

        let contextCount = nodeStatus.node.object.resources.GPUResource().maxContextCnt;
        let reqResource = func.object.spec.SnapshotResource(contextCount).clone();

        let state = nodeStatus.total.CanAlloc(&reqResource, true);
        if !state.Ok() {
            self.SetSnapshotStatus(
                funcId,
                nodename,
                SnapshotScheduleState::Cannot(format!("has no enough resource to run {:?}", state)),
            );
            return Err(Error::SchedulerNoEnoughResource(format!(
                "Node {} has no enough resource to run {}",
                nodename, funcId
            )));
        }

        let mut nodeResources: NodeResources = nodeStatus.available.clone();

        let terminateWorkers =
            match self.TryFreeResources(nodename, funcId, &mut nodeResources, &reqResource, true) {
                Err(Error::SchedulerNoEnoughResource(s)) => {
                    self.SetSnapshotStatus(
                        funcId,
                        nodename,
                        SnapshotScheduleState::Waiting(format!("Resource is busy")),
                    );
                    self.AddSnapshotTask(nodename, funcId);
                    return Err(Error::SchedulerNoEnoughResource(s));
                }
                Err(e) => return Err(e),
                Ok(t) => t,
            };

        let resources;
        let nodeAgentUrl;

        {
            let nodeStatus = self.nodes.get_mut(nodename).unwrap();
            let contextCnt = nodeStatus.node.object.resources.GPUResource().maxContextCnt;
            let snapshotResource = func.object.spec.SnapshotResource(contextCnt);
            // resources =
            //     nodeStatus.AllocResource(&snapshotResource, "CreateSnapshot", funcid, true)?;
            resources = nodeResources.Alloc(&snapshotResource, true)?;
            nodeAgentUrl = nodeStatus.node.NodeAgentUrl();
        }

        let id: u64 = match self
            .StartWorker(
                &nodeAgentUrl,
                &func,
                &resources,
                &resources,
                na::CreatePodType::Snapshot,
                &terminateWorkers,
            )
            .await
        {
            Err(e) => {
                self.SetSnapshotStatus(
                    funcId,
                    nodename,
                    SnapshotScheduleState::ScheduleFail(format!("snapshoting sched fail {:?}", &e)),
                );
                self.AddSnapshotTask(nodename, funcId);
                return Err(e);
            }
            Ok(id) => id,
        };

        self.SetSnapshotStatus(funcId, nodename, SnapshotScheduleState::Scheduled);

        for worker in &terminateWorkers {
            let podKey = worker.pod.PodKey();
            let removed = self.idlePods.shift_remove(&podKey);
            assert!(removed);
            match self.pods.get(&podKey) {
                None => unreachable!(),
                Some(pod) => {
                    let nodename = pod.pod.object.spec.nodename.clone();
                    let nodeStatus = self.nodes.get_mut(&nodename).unwrap();
                    self.stoppingPods.insert(podKey.clone());

                    error!("FreeResource create snapshot1 ... ");
                    nodeStatus
                        .FreeResource(&pod.pod.object.spec.allocResources, &pod.pod.PodKey())?;
                }
            }
        }

        let podKey = FuncPod::FuncPodKey(
            &func.tenant,
            &func.namespace,
            &func.name,
            func.Version(),
            &format!("{id}"),
        );

        let pendingPod = PendingPod::New(&nodename, &podKey, funcId, &resources);
        let nodeStatus = self.nodes.get_mut(nodename).unwrap();
        nodeStatus.AddPendingPod(&pendingPod)?;

        let contextCnt = nodeStatus.node.object.resources.GPUResource().maxContextCnt;
        let snapshotResource = func.object.spec.SnapshotResource(contextCnt);
        nodeStatus.AllocResource(&snapshotResource, "CreateSnapshot", "", true)?;

        self.funcs
            .get_mut(funcId)
            .unwrap()
            .AddPendingPod(&pendingPod)?;

        self.AddPendingSnapshot(funcId, &nodename);
        return Ok(());
    }

    pub fn FuncPolicy(&self, tenant: &str, p: &ObjRef<FuncPolicySpec>) -> FuncPolicySpec {
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

                match self.funcpolicy.get(&l.Key(tenant)) {
                    None => {
                        return FuncPolicySpec::default();
                    }
                    Some(p) => return p.clone(),
                }
            }
        }
    }

    pub async fn TryCreateStandbyOnNode(&mut self, nodename: &str) -> Result<()> {
        if !self.nodes.contains_key(nodename) {
            return Ok(());
        }

        // add another StandbyTask for the node
        if self.nodes.contains_key(nodename) {
            self.AddStandbyTask(nodename);
        }

        match self.nodes.get(nodename) {
            None => {
                return Ok(());
            }
            Some(ns) => {
                if ns.pendingPods.len() > 0 {
                    return Ok(());
                }
            }
        }

        let mut funcPodCnt = BTreeMap::new();
        for (funcId, m) in &self.snapshots {
            match m.get(nodename) {
                None => (),
                Some(snapshot) => {
                    if snapshot.state == SnapshotState::Ready {
                        funcPodCnt.insert(funcId.to_owned(), 0);
                    }
                }
            }
        }

        for (_, pod) in &self.pods {
            if &pod.pod.object.spec.nodename != nodename {
                continue;
            }

            let funcId = pod.pod.FuncKey();
            let state = pod.pod.object.status.state;

            if state.BlockStandby() {
                // avoid conflict
                return Ok(());
            }

            if state != PodState::Standby && state != PodState::PullingImage {
                continue;
            }

            match funcPodCnt.get(&funcId) {
                None => {
                    continue;
                }
                Some(c) => {
                    funcPodCnt.insert(funcId, *c + 1);
                }
            }
        }

        let mut needStandby = Vec::new();
        for (funcId, &cnt) in &funcPodCnt {
            let func = match self.funcs.get(funcId) {
                None => continue,
                Some(f) => f,
            };

            let tenant = func.func.tenant.clone();

            let policy = self.FuncPolicy(&tenant, &func.func.object.spec.policy);

            if policy.standbyPerNode > cnt {
                needStandby.push(funcId.to_owned());
            }
        }

        if needStandby.len() == 0 {
            return Ok(());
        }

        {
            let mut rng = thread_rng();
            needStandby.shuffle(&mut rng);
        }

        let funcId = &needStandby[0];
        let nodename = nodename.to_owned();
        let allocResources;
        let nodeAgentUrl;
        let resourceQuota;

        let function = match self.funcs.get(funcId) {
            None => return Ok(()),
            Some(f) => f.func.clone(),
        };

        let standbyResource = self.StandyResource(funcId, &nodename);
        {
            let nodeStatus = match self.nodes.get_mut(&nodename) {
                None => return Ok(()), // the node information is not synced
                Some(ns) => ns,
            };

            allocResources =
                nodeStatus.AllocResource(&standbyResource, "CreateStandby", funcId, false)?;
            resourceQuota = nodeStatus.ReadyResourceQuota(&function.object.spec.resources)?;
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
                error!(
                    "FreeResource create standby ... {}, error {:?}",
                    function.Objectkey(),
                    e
                );
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

        let pendingPod = PendingPod::New(&nodename, &podKey, &funcId, &allocResources);
        let nodeStatus = self.nodes.get_mut(&nodename).unwrap();
        nodeStatus.AddPendingPod(&pendingPod)?;

        self.funcs
            .get_mut(funcId)
            .unwrap()
            .AddPendingPod(&pendingPod)?;

        return Ok(());
    }

    // return whether we prewarm a
    pub async fn ProcessAddFunc(&mut self, funcid: &str) -> bool {
        // the func is doing something, skip this round
        if self.funcPods.contains_key(funcid) {
            return false;
        }

        let func = match self.funcs.get(funcid) {
            None => {
                return false;
            }
            Some(fpStatus) => fpStatus.func.clone(),
        };

        let funcState = func.object.status.state;

        if funcState == FuncState::Fail {
            return false;
        }

        let policy = self.FuncPolicy(&func.tenant, &func.object.spec.policy);
        if policy.minReplica > self.ReadyPodCount(funcid) as u64 {
            match self.ResumePod(&funcid).await {
                Err(e) => {
                    error!(
                        "ProcessAddFunc Prewarm one pod fail for func {} error {:?}",
                        funcid, e
                    );
                    return false;
                }
                Ok(_) => {
                    error!("Prewarm one pod for func {}", funcid);
                    return true;
                }
            }
        }

        return false;
    }

    pub const PRINT_SCHEDER_INFO: bool = false;

    pub async fn ProcessRemoveFunc(&mut self, spec: &Function) -> Result<()> {
        let hasPod = self.RemovePodsByFunckey(&spec.Key()).await?;

        if !hasPod {
            self.RemoveSnapshotByFunckey(&spec.Key()).await?;
        }

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

        let (pod, terminateWorkers, mut nodeResource) = self.GetBestResumeWorker(&fp).await?;
        let naUrl;
        let nodename = pod.pod.object.spec.nodename.clone();
        let id = pod.pod.object.spec.id.clone();

        let readyResource = self.ReadyResource(&fp.object.spec.RunningResource(), fpKey, &nodename);
        let standbyResource = pod.pod.object.spec.allocResources.clone();
        nodeResource.Add(&standbyResource)?;
        let resources = nodeResource.Alloc(&readyResource, false)?;

        {
            // let readyResource =
            //     self.ReadyResource(&fp.object.spec.RunningResource(), fpKey, &nodename);
            let nodeStatus = self.nodes.get_mut(&nodename).unwrap();

            // let standbyResource = pod.pod.object.spec.allocResources.clone();

            // nodeStatus.FreeResource(&standbyResource, &fp.name)?;
            // resources = nodeStatus.AllocResource(&readyResource, "ResumePod", &id, false)?;
            naUrl = nodeStatus.node.NodeAgentUrl();
        }

        let mut terminatePods = Vec::new();
        for w in &terminateWorkers {
            terminatePods.push(w.pod.PodKey());
        }

        info!(
            "ResumePod pod {} with terminating {:#?}",
            pod.pod.PodKey(),
            &terminatePods
        );

        pod.SetState(WorkerPodState::Resuming);
        match self
            .ResumeWorker(
                &naUrl,
                &pod.pod.tenant,
                &pod.pod.namespace,
                &pod.pod.object.spec.funcname,
                pod.pod.object.spec.fprevision,
                &id,
                &resources,
                &terminateWorkers,
            )
            .await
        {
            Err(e) => {
                pod.SetState(WorkerPodState::Standby);
                return Err(e);
            }
            Ok(()) => (),
        }

        for pod in &terminateWorkers {
            pod.SetState(WorkerPodState::Terminating);
        }

        for worker in &terminateWorkers {
            let podKey = worker.pod.PodKey();
            let removed = self.idlePods.shift_remove(&podKey);
            assert!(removed);
            match self.pods.get(&podKey) {
                None => unreachable!(),
                Some(pod) => {
                    let nodename = pod.pod.object.spec.nodename.clone();
                    let nodeStatus = self.nodes.get_mut(&nodename).unwrap();
                    self.stoppingPods.insert(podKey.clone());

                    error!("FreeResource Resumepod xx ...");
                    nodeStatus
                        .FreeResource(&pod.pod.object.spec.allocResources, &pod.pod.PodKey())?;
                }
            }
        }

        {
            let nodeStatus = self.nodes.get_mut(&nodename).unwrap();
            let standbyResource = pod.pod.object.spec.allocResources.clone();
            error!("FreeResource Resumepod 2... ");

            nodeStatus.FreeResource(&standbyResource, &fp.name)?;
            nodeStatus.available.Sub(&resources)?;
            info!(
                "After ResumePod pod {} the node resource is {:#?}",
                pod.pod.PodKey(),
                &nodeStatus.available
            );
        }

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

        let mut spec = func.object.spec.clone();
        let policy = self.FuncPolicy(&tenant, &spec.policy);
        spec.policy = ObjRef::Obj(policy);

        let request = tonic::Request::new(na::CreateFuncPodReq {
            tenant: tenant.to_owned(),
            namespace: namespace.to_owned(),
            funcname: funcname.to_owned(),
            fprevision: fpRevision,
            id: format!("{id}"),
            labels: Vec::new(),
            annotations: annotations,
            create_type: createType.into(),
            funcspec: serde_json::to_string(&spec)?,
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
            naUrl = nodeStatus.node.NodeAgentUrl();
        }

        match self
            .StopWorkerInner(
                &naUrl,
                &pod.tenant,
                &pod.namespace,
                &pod.object.spec.funcname,
                pod.object.spec.fprevision,
                &pod.object.spec.id,
            )
            .await
        {
            Err(e) => return Err(e),
            Ok(()) => {
                let nodename = pod.object.spec.nodename.clone();
                let nodeStatus = self.nodes.get_mut(&nodename).unwrap();
                if pod.object.status.state != PodState::Failed {
                    // failure pod resource has been freed
                    self.stoppingPods.insert(pod.PodKey());
                    error!("FreeResource StopWorker");
                    nodeStatus.FreeResource(&pod.object.spec.allocResources, &pod.PodKey())?;
                }

                return Ok(());
            }
        }
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
            ListType::FuncStatus => self.funcstatusListDone = true,
            ListType::FuncPod => self.funcPodListDone = true,
            ListType::Node => self.nodeListDone = true,
            ListType::Snapshot => self.snapshotListDone = true,
            ListType::Funcpolicy => self.funcpolicyDone = true,
        }

        if self.nodeListDone
            && self.funcListDone
            && self.funcstatusListDone
            && self.funcPodListDone
            && self.snapshotListDone
            && self.funcpolicyDone
        {
            self.listDone = true;

            self.RefreshScheduling().await?;
            self.InitSnapshotTask()?;

            return Ok(true);
        }

        return Ok(false);
    }

    pub async fn AddNode(&mut self, node: Node) -> Result<()> {
        info!("add node {:#?}", &node);

        let nodeName = node.name.clone();
        if self.nodes.contains_key(&nodeName) {
            return Err(Error::Exist(format!("NodeMgr::add {}", nodeName)));
        }

        let nodelabel = Nodelabel {
            nodename: nodeName.clone(),
        };

        let gpuCnt = node.object.resources.gpus.Gpus().len();

        SCHEDULER_METRICS
            .lock()
            .await
            .totalGPU
            .get_or_create(&nodelabel)
            .set(gpuCnt as i64);

        let total = node.object.resources.clone();
        let pods = match self.nodePods.remove(&nodeName) {
            None => BTreeMap::new(),
            Some(pods) => pods,
        };
        let nodeStatus = NodeStatus::New(node, total, pods)?;

        self.nodes.insert(nodeName.clone(), nodeStatus);
        self.taskQueue.AddNode(&nodeName);

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

    pub async fn RemoveNode(&mut self, node: Node) -> Result<()> {
        info!("remove node {}", &node.name);
        let key = node.name.clone();

        let nodelabel = Nodelabel {
            nodename: key.clone(),
        };

        SCHEDULER_METRICS.lock().await.totalGPU.remove(&nodelabel);

        if !self.nodes.contains_key(&key) {
            return Err(Error::NotExist(format!("NodeMgr::Remove {}", key)));
        }

        self.nodes.remove(&key);

        return Ok(());
    }

    // when statesvc and ixproxy restart together, there might be chance the new statesvc will give the node with new epoch
    // the CheckNodeEpoch will delete all the pod, snapshot
    pub async fn CheckNodeEpoch(&mut self, nodename: &str, nodeEpoch: i64) {
        match self.nodeEpoch.get(nodename) {
            None => {
                // one new node
                self.nodeEpoch.insert(nodename.to_owned(), nodeEpoch);
                return;
            }
            Some(&oldEpoch) => {
                if oldEpoch == nodeEpoch {
                    // match, that's good
                    return;
                }

                // one new node create
                self.CleanNode(nodename).await;
                self.nodeEpoch.insert(nodename.to_owned(), nodeEpoch);
            }
        }
    }

    pub async fn CleanNode(&mut self, nodename: &str) {
        let tempPods = self.nodePods.remove(nodename);
        if let Some(pods) = tempPods {
            for (_, wp) in pods {
                self.RemovePod(&wp.pod).await.ok();
            }
        }

        let pods = match self.nodes.get(nodename) {
            Some(ns) => ns.pods.clone(),
            None => BTreeMap::new(),
        };

        for (_, wp) in pods {
            self.RemovePod(&wp.pod).await.ok();
        }

        // remove snapshot
        self.snapshots.remove(nodename);

        let node = match self.nodes.get(nodename) {
            None => {
                return;
            }
            Some(ns) => ns.node.clone(),
        };

        self.RemoveNode(node).await.ok();
        return;
    }

    pub fn AddPod(&mut self, pod: FuncPod) -> Result<()> {
        let podKey = pod.PodKey();
        let nodename = pod.object.spec.nodename.clone();
        let fpKey = pod.FuncKey();

        let boxPod: WorkerPod = WorkerPod::New(pod);
        assert!(self.pods.insert(podKey.clone(), boxPod.clone()).is_none());

        if boxPod.State().IsIdle() && boxPod.pod.object.status.state == PodState::Ready {
            boxPod.SetIdle(SetIdleSource::AddPod);
            self.idlePods.insert(boxPod.pod.PodKey());
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
            None => match self.funcPods.get_mut(&fpKey) {
                None => {
                    let mut pods = BTreeMap::new();
                    pods.insert(podKey, boxPod);
                    self.funcPods.insert(fpKey, pods);
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

        let boxPod: WorkerPod = WorkerPod::New(pod);

        let oldPod = self
            .pods
            .insert(podKey.clone(), boxPod.clone())
            .expect("UpdatePod get none old pod");

        error!(
            "Updatepad pod {}, state {:?}, old work state {:?}",
            &podKey,
            &boxPod.pod.object.status.state,
            oldPod.State()
        );

        match oldPod.State() {
            WorkerPodState::Working(_) => {
                boxPod.SetState(oldPod.State());
            }
            _ => (),
        }

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
            None => match self.funcPods.get_mut(&funcKey) {
                None => {
                    let mut pods = BTreeMap::new();
                    pods.insert(podKey, boxPod);
                    self.funcPods.insert(funcKey, pods);
                }
                Some(pods) => {
                    pods.insert(podKey, boxPod);
                }
            },
            Some(fpStatus) => {
                fpStatus.UpdatePod(&boxPod).await?;
            }
        }

        return Ok(());
    }

    pub async fn RemovePod(&mut self, pod: &FuncPod) -> Result<()> {
        let podKey: String = pod.PodKey();
        let nodeName = pod.object.spec.nodename.clone();
        let funcKey = pod.FuncKey();

        match self.pods.remove(&podKey) {
            None => unreachable!(),
            Some(worker) => {
                let state = worker.State();
                match state {
                    WorkerPodState::Working(_gatewayId) => {
                        let gpuCnt = worker.pod.object.spec.reqResources.gpu.gpuCount;
                        let nodelabel = Nodelabel {
                            nodename: worker.pod.object.spec.nodename.clone(),
                        };

                        let cnt = SCHEDULER_METRICS
                            .lock()
                            .await
                            .usedGpuCnt
                            .Dec(nodelabel.clone(), gpuCnt);

                        SCHEDULER_METRICS
                            .lock()
                            .await
                            .usedGPU
                            .get_or_create(&nodelabel)
                            .set(cnt as i64);

                        error!(
                            "user GPU desc fail pod {:?} {} {}",
                            &worker.pod.object.spec.funcname, gpuCnt, cnt
                        );
                    }
                    _ => (),
                }
            }
        }

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
                            match self.funcstatus.get(&funcKey).cloned() {
                                None => {
                                    error!("RemovePod can't get funcstatus for {}", &funcKey);
                                }
                                Some(mut status) => {
                                    error!("RemovePod the funcstate is {:?}", status);

                                    status.object.snapshotingFailureCnt += 1;

                                    self.AddSnapshotTask(&nodeName, &pod.FuncKey());
                                    if status.object.snapshotingFailureCnt >= 3 {
                                        status.object.state = FuncState::Fail;
                                    }

                                    let client = GetClient().await.unwrap();

                                    // update the func
                                    client.Update(&status.DataObject(), 0).await.unwrap();
                                }
                            }
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

        let pods = match self.funcPods.remove(&fpId) {
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
    FuncStatus,
    Snapshot,
    Funcpolicy,
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
                    "schedudlerHandler informer::GetClient fail to connect to {} with error {:?}",
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

pub async fn GetClientWithRetry() -> Result<CacherClient> {
    let mut delay = Duration::from_millis(100);
    let max_delay = Duration::from_secs(3);
    let max_retries = 5;

    let addr = &SCHEDULER_CONFIG.stateSvcAddrs;

    for attempt in 1..=max_retries {
        match CacherClient::New(addr[0].clone()).await {
            Ok(client) => {
                error!(
                    "Connected to state service at {} after {} attempt(s)",
                    addr[0], attempt
                );
                return Ok(client);
            }
            Err(e) => {
                error!(
                    "fail to connect to {} (attempt {}/{}), err={:?}",
                    addr[0], attempt, max_retries, e
                );
            }
        }

        if attempt < max_retries {
            tokio::time::sleep(delay).await;
            delay = std::cmp::min(delay * 2, max_delay);
        }
    }

    let errstr = format!("GetClient fail: after {} attempt(s)", max_retries);
    return Err(Error::CommonError(errstr));
}
