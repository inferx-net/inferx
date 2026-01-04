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

use inferxlib::data_obj::ObjRef;
use inferxlib::obj_mgr::funcpolicy_mgr::FuncPolicy;
use inferxlib::obj_mgr::funcpolicy_mgr::FuncPolicySpec;
use inferxlib::obj_mgr::funcstatus_mgr::FunctionStatus;
use lru::LruCache;
use once_cell::sync::Lazy;
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
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::sync::oneshot::Sender;
use tokio::sync::Notify;
use tokio::sync::Semaphore;
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

// Global semaphore for limiting concurrent RPCs to NodeAgents
// Prevents overwhelming NodeAgent with too many concurrent requests
static GLOBAL_RPC_SEMAPHORE: Lazy<Arc<Semaphore>> = Lazy::new(|| {
    Arc::new(Semaphore::new(16)) // Max 16 concurrent RPCs
});

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

    // Track pods that are being stopped/terminated
    // Prevents premature resource freeing when deletion events arrive before termination completes
    pub stoppingPods: BTreeSet<String>,

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
            stoppingPods: BTreeSet::new(),
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
    ) -> Result<()> {
        let pendingExist = self.pendingPods.remove(podKey).is_some();
        let exist = match self.pods.remove(podKey) {
            Some(_pod) => true,
            None => false,
        };

        // Check if pod was in stopping state and remove it
        let stopping = self.stoppingPods.remove(podKey);

        // Free resources on pod deletion - this is the ONLY place resources are freed
        // The 'stopping' flag is kept for logging/tracking purposes but doesn't affect resource freeing
        // All pods get their resources freed here on deletion, ensuring no double-free and no leaks
        if pendingExist || exist {
            self.FreeResource(resources, podKey)?;
            if stopping {
                info!("Freed resources for stopping pod {} on deletion", podKey);
            }
        }

        return Ok(());
    }

    pub fn AllocResource(
        &mut self,
        req: &Resources,
        action: &str,
        owner: &str,
        createSnapshot: bool,
    ) -> Result<NodeResources> {
        // TODO: Add bounds checking to prevent resource accounting bugs
        // Resources can temporarily go negative during async operations (by design),
        // but should never exceed -total or -2×total. Add check like:
        //   if self.available.cpu < -(self.total.cpu * 2) {
        //       return Err(Error::ResourceAccountingBug(...));
        //   }
        // This would catch bugs while still allowing legitimate temporary negatives.

        let res = self.available.Alloc(req, createSnapshot)?;
        info!(
            "AllocResource node={} action={} owner={} allocated={} available_after={}",
            self.available.nodename,
            action,
            owner,
            serde_json::to_string(&res).unwrap_or_default(),
            serde_json::to_string(&self.available).unwrap_or_default()
        );
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

    pub fn FreeResource(&mut self, free: &NodeResources, podkey: &str) -> Result<()> {
        // TODO: Add bounds checking after freeing resources
        // After Add(), available should generally be >= 0 (or at worst slightly negative
        // if in-flight operations). If available is very positive (> total), it indicates
        // a double-free bug. Add check like:
        //   if self.available.cpu > self.total.cpu {
        //       return Err(Error::ResourceAccountingBug("Double-free detected"));
        //   }

        self.available.Add(free)?;
        info!(
            "FreeResource node={} pod={} freed={} available_after={}",
            self.available.nodename,
            podkey,
            serde_json::to_string(free).unwrap_or_default(),
            serde_json::to_string(&self.available).unwrap_or_default()
        );
        return Ok(());
    }

    // Add a pod to the stopping set
    pub fn AddStoppingPod(&mut self, podKey: &str) {
        self.stoppingPods.insert(podKey.to_string());
    }

    // Remove a pod from the stopping set
    // Returns true if the pod was in the stopping set
    pub fn RemoveStoppingPod(&mut self, podKey: &str) -> bool {
        self.stoppingPods.remove(podKey)
    }
}

pub trait ResourceAlloc {
    fn CanAlloc(&self, req: &Resources, createSnapshot: bool) -> AllocState;
    fn Alloc(&mut self, req: &Resources, createSnapshot: bool) -> Result<NodeResources>;
}

impl ResourceAlloc for NodeResources {
    fn CanAlloc(&self, req: &Resources, createSnapshot: bool) -> AllocState {
        return AllocState {
            cpu: self.cpu >= req.cpu as i64,
            memory: self.memory >= req.memory as i64,
            cacheMem: self.cacheMemory >= req.cacheMemory as i64,
            gpuType: self.gpuType.CanAlloc(&req.gpu.type_),
            gpu: self.gpus.CanAlloc(&req.gpu, createSnapshot).is_some(),
        };
    }

    fn Alloc(&mut self, req: &Resources, createSnapshot: bool) -> Result<NodeResources> {
        let state = self.CanAlloc(req, createSnapshot);
        if !state.Ok() {
            return Err(Error::ScheduleFail(state));
        }

        self.memory -= req.memory as i64;
        self.cacheMemory -= req.cacheMemory as i64;
        let gpus = self.gpus.Alloc(&req.gpu, createSnapshot)?;

        return Ok(NodeResources {
            nodename: self.nodename.clone(),
            cpu: req.cpu as i64,
            memory: req.memory as i64,
            cacheMemory: req.cacheMemory as i64,
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

    // Track pods that are being stopped/terminated
    // Prevents premature resource freeing when deletion events arrive before termination completes
    pub stoppingPods: BTreeSet<String>,

    pub leaseWorkerReqs: VecDeque<(LeaseReq, Sender<na::LeaseWorkerResp>)>,
}

impl FuncStatus {
    pub fn New(fp: Function, pods: BTreeMap<String, WorkerPod>) -> Result<Self> {
        return Ok(Self {
            func: fp,
            pods: pods,
            pendingPods: BTreeMap::new(),
            stoppingPods: BTreeSet::new(),
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
        self.stoppingPods.remove(podKey);
        return Ok(());
    }

    // Add a pod to the stopping set
    pub fn AddStoppingPod(&mut self, podKey: &str) {
        self.stoppingPods.insert(podKey.to_string());
    }

    // Remove a pod from the stopping set
    // Returns true if the pod was in the stopping set
    pub fn RemoveStoppingPod(&mut self, podKey: &str) -> bool {
        self.stoppingPods.remove(podKey)
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
    DumpState(Sender<Value>),
    CleanupDuplicateSnapshot {
        funcid: String,
        nodename: String,
        podkey: String,
        resources: NodeResources,
        terminated_pod_keys: Vec<String>,
    },
    ResumeWorkerComplete {
        pod_key: String,
        nodename: String,
        result: Result<()>,
        resources: NodeResources,
        terminated_pod_keys: Vec<String>,
    },
    StartWorkerComplete {
        funcid: String,
        nodename: String,
        podkey: String,
        create_type: na::CreatePodType,
        result: Result<()>,
        resources: NodeResources,
        terminated_pod_keys: Vec<String>,
    },
    RemoveSnapshotComplete {
        funckey: String,
        nodename: String,
        result: Result<()>,
    },
    StopWorkerComplete {
        pod_key: String,
        result: Result<()>,
    },
}

#[derive(Debug)]
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
    pub idlePods: LruCache<String, ()>,

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

    pub msgTx: mpsc::Sender<WorkerHandlerMsg>,

    // Per-node semaphores to limit concurrent operations on the same node
    // Prevents overlapping GPU resets and expensive docker cleanup operations
    node_semaphores: BTreeMap<String, Arc<Semaphore>>,

    // Track pending snapshot removal RPCs to prevent duplicate requests
    // Key format: "funckey:nodename"
    // Prevents spawning duplicate RemoveSnapshot RPCs before the first completes
    pending_snapshot_removals: BTreeSet<String>,
}

impl Default for SchedulerHandler {
    fn default() -> Self {
        let (tx, _rx) = mpsc::channel(1);
        Self {
            nodeEpoch: BTreeMap::new(),
            pods: BTreeMap::new(),
            gateways: BTreeMap::new(),
            nodes: BTreeMap::new(),
            nodePods: BTreeMap::new(),
            funcs: BTreeMap::new(),
            funcstatus: BTreeMap::new(),
            snapshots: BTreeMap::new(),
            pendingsnapshots: BTreeMap::new(),
            idlePods: LruCache::unbounded(),
            funcPods: BTreeMap::new(),
            SnapshotSched: BiIndex::New(),
            funcpolicy: BTreeMap::new(),
            nodeListDone: false,
            funcListDone: false,
            funcstatusListDone: false,
            funcPodListDone: false,
            snapshotListDone: false,
            funcpolicyDone: false,
            listDone: false,
            nextWorkId: AtomicU64::new(1),
            taskQueue: TaskQueue::default(),
            delayed_tasks: BinaryHeap::new(),
            delay_interval: None,
            msgTx: tx,
            node_semaphores: BTreeMap::new(),
            pending_snapshot_removals: BTreeSet::new(),
        }
    }
}

impl SchedulerHandler {
    pub fn New(msgTx: mpsc::Sender<WorkerHandlerMsg>) -> Self {
        let mut handler = Self::default();
        handler.msgTx = msgTx;
        handler
    }

    /// Get or create a per-node semaphore with max 2 concurrent operations
    /// This prevents overlapping GPU resets and expensive docker cleanup on the same node
    fn get_node_semaphore(&mut self, nodename: &str) -> Arc<Semaphore> {
        self.node_semaphores
            .entry(nodename.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(2)))
            .clone()
    }

    /// Spawn an RPC task with global and per-node concurrency limits, timeout protection,
    /// and automatic result reporting back to the scheduler via message channel.
    ///
    /// This helper ensures:
    /// - Non-blocking: returns immediately, RPC runs in background
    /// - Global limit: max 16 concurrent RPCs across all nodes
    /// - Per-node limit: max 2 concurrent RPCs per node (prevents GPU reset conflicts)
    /// - Timeout: configurable timeout with automatic cleanup
    /// - Error handling: all results (success/error/timeout) sent back to scheduler
    fn spawn_rpc<F, Fut>(
        &mut self,
        nodename: &str,
        timeout: Duration,
        rpc_operation: F,
        completion_msg: impl FnOnce(Result<()>) -> WorkerHandlerMsg + Send + 'static,
    ) where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<()>> + Send + 'static,
    {
        let global_sem = GLOBAL_RPC_SEMAPHORE.clone();
        let node_sem = self.get_node_semaphore(nodename);
        let msgTx = self.msgTx.clone();

        tokio::spawn(async move {
            // Acquire semaphores (blocks this task, not the scheduler)
            let _global_permit = global_sem.acquire().await.unwrap();
            let _node_permit = node_sem.acquire().await.unwrap();

            // Execute RPC with timeout
            let result = match tokio::time::timeout(timeout, rpc_operation()).await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(Error::CommonError("RPC timeout".to_string())),
            };

            // Send result back to scheduler
            msgTx.send(completion_msg(result)).await.ok();
        });
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

    pub fn DumpState(&self) -> Value {
        let mut idlePods: Vec<String> = Vec::new();
        for (podKey, _) in self.idlePods.iter() {
            idlePods.push(podKey.clone());
        }

        let mut pods = serde_json::Map::new();
        for (podKey, worker) in &self.pods {
            pods.insert(
                podKey.clone(),
                json!({
                    "state": format!("{:?}", worker.State()),
                    "pod": worker.pod.clone(),
                }),
            );
        }

        let mut nodes = serde_json::Map::new();
        for (nodename, ns) in &self.nodes {
            let mut nodePods = serde_json::Map::new();
            for (podKey, pod) in &ns.pods {
                nodePods.insert(
                    podKey.clone(),
                    json!({
                        "state": format!("{:?}", pod.State()),
                        "func": pod.pod.FuncKey(),
                    }),
                );
            }

            let pending: Vec<String> = ns.pendingPods.keys().cloned().collect();
            let stopping: Vec<String> = ns.stoppingPods.iter().cloned().collect();

            nodes.insert(
                nodename.clone(),
                json!({
                    "state": format!("{:?}", ns.state),
                    "node": ns.node.clone(),
                    "total": ns.total.clone(),
                    "available": ns.available.clone(),
                    "pendingPods": pending,
                    "stoppingPods": stopping,
                    "pods": nodePods,
                }),
            );
        }

        let mut funcs = serde_json::Map::new();
        for (funcId, status) in &self.funcs {
            let pending: Vec<String> = status.pendingPods.keys().cloned().collect();
            let pods: Vec<String> = status.pods.keys().cloned().collect();
            let stoppingPods: Vec<String> = status.stoppingPods.iter().cloned().collect();

            funcs.insert(
                funcId.clone(),
                json!({
                    "func": status.func.clone(),
                    "pendingPods": pending,
                    "pods": pods,
                    "stoppingPods": stoppingPods,
                    "leaseQueueDepth": status.leaseWorkerReqs.len(),
                }),
            );
        }

        let funcstatus = json!(self.funcstatus);

        let mut snapshots = serde_json::Map::new();
        for (funcId, nodesMap) in &self.snapshots {
            let mut sn = serde_json::Map::new();
            for (nodename, snapshot) in nodesMap {
                sn.insert(nodename.clone(), json!(snapshot.clone()));
            }
            snapshots.insert(funcId.clone(), json!(sn));
        }

        let mut pendingsnapshots = serde_json::Map::new();
        for (funcId, nodes) in &self.pendingsnapshots {
            pendingsnapshots.insert(
                funcId.clone(),
                json!(nodes.iter().cloned().collect::<Vec<_>>()),
            );
        }

        let mut funcPods = serde_json::Map::new();
        for (funcId, pods) in &self.funcPods {
            funcPods.insert(
                funcId.clone(),
                json!(pods.keys().cloned().collect::<Vec<_>>()),
            );
        }

        let funcpolicy = json!(self.funcpolicy);

        let now = std::time::Instant::now();
        let mut gateways = serde_json::Map::new();
        for (gatewayId, refresh) in &self.gateways {
            gateways.insert(
                gatewayId.to_string(),
                json!({
                    "lastRefreshMs": now.duration_since(*refresh).as_millis()
                }),
            );
        }

        json!({
            "gateways": gateways,
            "idlePods": idlePods,
            "pods": pods,
            "nodes": nodes,
            "funcs": funcs,
            "funcstatus": funcstatus,
            "snapshots": snapshots,
            "pendingSnapshots": pendingsnapshots,
            "funcPods": funcPods,
            "funcpolicy": funcpolicy,
            "listFlags": {
                "nodeListDone": self.nodeListDone,
                "funcListDone": self.funcListDone,
                "funcstatusListDone": self.funcstatusListDone,
                "funcPodListDone": self.funcPodListDone,
                "snapshotListDone": self.snapshotListDone,
                "funcpolicyDone": self.funcpolicyDone,
                "listDone": self.listDone,
            }
        })
    }

    pub async fn ProcessRefreshGateway(&mut self, req: na::RefreshGatewayReq) -> Result<()> {
        let gatewayId = req.gateway_id;
        let now = std::time::Instant::now();
        self.gateways.insert(gatewayId, now);
        return Ok(());
    }

    /// Determine if it's safe to restore resources after RPC failure
    ///
    /// Returns true only when we're certain the RPC never reached the NodeAgent.
    /// This is conservative - when in doubt, we don't restore and let reconciliation handle it.
    ///
    /// Safe conditions (RPC definitely didn't execute):
    /// - Connection refused: NodeAgent not started yet
    /// - DNS errors: Can't resolve hostname
    /// - Network unreachable: No route to host
    /// - Service unavailable: gRPC service not running
    ///
    /// Unsafe conditions (RPC might have been processed):
    /// - Timeouts: RPC might have completed but response lost
    /// - Connection timeouts: Could occur after TCP connect (request might be sent)
    /// - Internal errors: NodeAgent might have crashed during processing
    /// - Unknown errors: Uncertain state
    fn is_safe_to_restore(e: &Error) -> bool {
        match e {
            Error::TonicTransportErr(transport_err) => {
                let err_str = format!("{:?}", transport_err);

                // Only pre-connection transport errors are safe
                // NOTE: Do NOT include "connection timeout" - it can fire after TCP connect
                err_str.contains("Connection refused")
                    || err_str.contains("ConnectionRefused")
                    || err_str.contains("dns error")
                    || err_str.contains("failed to lookup")
                    || err_str.contains("No route to host")
                    || err_str.contains("Network unreachable")
            }
            Error::TonicStatus(status) => {
                // Only status codes that mean request wasn't processed
                matches!(
                    status.code(),
                    tonic::Code::Unavailable | tonic::Code::Unimplemented
                )
            }
            Error::CommonError(msg) => {
                // Keep existing connection refused check for wrapped errors
                msg.contains("Connection refused") || msg.contains("ConnectionRefused")
            }
            _ => false,
        }
    }

    /// Handle completion of async ResumeWorker RPC
    ///
    /// On success: Pod state will be updated to Ready by the informer when NodeAgent reports it
    /// On failure: Revert pod to Standby state, restore terminated workers, and restore resources
    pub async fn ProcessResumeWorkerComplete(
        &mut self,
        pod_key: &str,
        nodename: &str,
        result: Result<()>,
        resources: &NodeResources,
        terminated_pod_keys: Vec<String>,
    ) -> Result<()> {
        match result {
            Ok(()) => {
                // RPC succeeded - but pod is not Ready yet
                // Pod will become Ready via Kubernetes informer event later
                // Standby resources will be freed when Ready event arrives
                // Don't free standby here to avoid double-free
                info!(
                    "ResumeWorker RPC succeeded for pod {} - waiting for Ready event to free standby allocation",
                    pod_key
                );
                Ok(())
            }
            Err(e) => {
                // Check if this is a safe-to-restore error (RPC never reached NodeAgent)
                if Self::is_safe_to_restore(&e) {
                    // NodeAgent never received the request - safe to restore all state
                    error!(
                        "ResumeWorker RPC failed (safe to restore) for pod {} on node {}: {:?} - restoring state",
                        pod_key, nodename, e
                    );

                    // 1. Restore ready resource allocation
                    if let Some(nodeStatus) = self.nodes.get_mut(nodename) {
                        // Add back the ready resources that were reserved
                        nodeStatus.available.Add(resources)?;

                        // Remove from pending pods
                        nodeStatus.pendingPods.remove(pod_key);

                        info!(
                            "Restored {} ready allocation for pod {} on node {}, available is now {:#?}",
                            serde_json::to_string(&resources).unwrap_or_default(),
                            pod_key,
                            nodename,
                            &nodeStatus.available
                        );
                    }

                    // 2. Restore pod state to Standby, remove from func's pending pods,
                    //    and fail any queued lease requests
                    if let Some(worker) = self.pods.get(pod_key) {
                        worker.SetState(WorkerPodState::Standby);

                        // Remove from func's pending pods to unblock future scheduling
                        let funcid = worker.pod.FuncKey();
                        if let Some(funcStatus) = self.funcs.get_mut(&funcid) {
                            funcStatus.pendingPods.remove(pod_key);

                            // Fail the ONE lease request that triggered this Resume attempt
                            // (Resume RPC failed, pod won't become Ready)
                            // Don't pop all requests - other requests might be handled by other Resume attempts
                            if let Some((_lease_req, lease_tx)) = funcStatus.PopLeaseWorkerReq() {
                                let resp = na::LeaseWorkerResp {
                                    error: format!(
                                        "Resume RPC failed (safe to restore): {:?}. Gateway should retry.",
                                        e
                                    ),
                                    ..Default::default()
                                };
                                lease_tx.send(resp).ok(); // Ignore send errors (channel might be closed)
                                info!(
                                    "Failed one queued lease request for func {} - gateway can retry",
                                    funcid
                                );
                            }

                            info!(
                                "Removed pod {} from func {} pending pods",
                                pod_key, funcid
                            );
                        }

                        info!("Restored pod {} to Standby state", pod_key);
                    } else {
                        error!("Cannot restore pod {} - not found in pods map", pod_key);
                    }

                    // 3. Restore terminated pods to Idle
                    for terminated_podkey in &terminated_pod_keys {
                        if let Some(terminated_worker) = self.pods.get(terminated_podkey) {
                            let pod_nodename = terminated_worker.pod.object.spec.nodename.clone();
                            let pod_funcid = terminated_worker.pod.FuncKey();

                            // Remove from both node and func stoppingPods
                            if let Some(nodeStatus) = self.nodes.get_mut(&pod_nodename) {
                                if nodeStatus.RemoveStoppingPod(terminated_podkey) {
                                    terminated_worker.SetState(WorkerPodState::Idle);
                                    self.idlePods.put(terminated_podkey.clone(), ());

                                    if let Some(funcStatus) = self.funcs.get_mut(&pod_funcid) {
                                        funcStatus.RemoveStoppingPod(terminated_podkey);
                                    }

                                    info!(
                                        "Restored terminated pod {} to Idle (resume failed)",
                                        terminated_podkey
                                    );
                                }
                            }
                        }
                    }

                    // Note: LeaseWorker requests are queued per-func (in funcStatus.leaseWorkerReqs).
                    // We fail all queued requests above so gateway can retry immediately.
                    // The restored Standby pod is now available for those retries.

                    return Ok(());
                }

                // RPC failed but we can't tell if NodeAgent received the request or not
                // NodeAgent guarantees: if pod becomes Ready, terminated pods are deleted (atomic)
                // So we wait for pod events to tell us what actually happened:
                // - If pod becomes Ready: NodeAgent succeeded, handle via Ready event
                // - If pod stays Resuming: NodeAgent failed, reconciliation will handle it
                error!(
                    "ResumeWorker RPC failed for pod {}: {:?} - waiting for pod events to determine outcome",
                    pod_key, e
                );

                info!(
                    "Pod {} RPC failed, not rolling back - pod state will be determined by Kubernetes events. \
                    Pod state: Resuming, {} terminated pods marked as Terminating",
                    pod_key,
                    terminated_pod_keys.len()
                );

                // Don't rollback:
                // - Don't change pod state (stays in Resuming)
                // - Don't free ready resources (stay reserved)
                // - Don't touch terminated pods (stay in Terminating)
                // - Don't remove from pendingPods
                //
                // If NodeAgent succeeded:
                // - Pod will become Ready via informer event
                // - Ready event handler will free standby resources
                // - Terminated pods will be deleted, deletion events free their resources
                //
                // If NodeAgent failed (RPC truly failed):
                // - Pod stays in Resuming state
                // - TODO: Reconciliation loop will detect stuck pod and retry or clean up
                // - Resources stay reserved until reconciliation fixes it

                Ok(())
            }
        }
    }

    pub fn ProcessRemoveSnapshotComplete(
        &mut self,
        funckey: &str,
        nodename: &str,
        result: Result<()>,
    ) {
        // Clear pending marker - RPC completed (success or failure)
        let pending_key = format!("{}:{}", funckey, nodename);
        self.pending_snapshot_removals.remove(&pending_key);

        match result {
            Ok(()) => {
                info!(
                    "RemoveSnapshot RPC succeeded for funckey {} on node {}",
                    funckey, nodename
                );

                // Remove the snapshot from our tracking
                if let Some(snapshot) = self.snapshots.get_mut(funckey) {
                    snapshot.remove(nodename);

                    // If this was the last node with this snapshot, remove the funckey entry
                    if snapshot.is_empty() {
                        self.snapshots.remove(funckey);
                        info!(
                            "All snapshots removed for funckey {}, removed from tracking",
                            funckey
                        );
                    } else {
                        info!(
                            "Removed snapshot for funckey {} from node {}, {} nodes remaining",
                            funckey,
                            nodename,
                            snapshot.len()
                        );
                    }
                }
            }
            Err(e) => {
                error!(
                    "RemoveSnapshot RPC failed for funckey {} on node {}: {:?} - will retry on next cleanup cycle",
                    funckey, nodename, e
                );
                // Don't remove from tracking - will be retried on next CleanSnapshots call
            }
        }
    }

    pub fn ProcessStopWorkerComplete(
        &mut self,
        pod_key: &str,
        result: Result<()>,
    ) {
        match result {
            Ok(()) => {
                info!(
                    "StopWorker RPC succeeded for pod {} - pod will be deleted via informer event",
                    pod_key
                );
                // Don't pre-free resources or update state here
                // Wait for pod deletion event from Kubernetes informer to:
                // 1. Free the pod's resources
                // 2. Remove pod from tracking
                // 3. Remove from stoppingPods if present
                // This ensures resources are freed exactly once
            }
            Err(e) => {
                error!(
                    "StopWorker RPC failed for pod {}: {:?}",
                    pod_key, e
                );
                // Don't take any action - pod might already be deleted or in bad state
                // Informer events will handle cleanup if pod gets deleted
                // If pod still exists, it can be retried later
            }
        }
    }

    pub fn ProcessStartWorkerComplete(
        &mut self,
        funcid: &str,
        nodename: &str,
        podkey: &str,
        create_type: na::CreatePodType,
        result: Result<()>,
        resources: &NodeResources,
        terminated_pod_keys: &Vec<String>,
    ) -> Result<()> {
        match result {
            Ok(()) => {
                // RPC succeeded - pod will become Ready via informer event
                info!(
                    "StartWorker RPC succeeded for {:?} pod {} - waiting for Ready event",
                    create_type, podkey
                );
                Ok(())
            }
            Err(e) => {
                // Check if it's safe to restore (RPC never reached NodeAgent)
                if Self::is_safe_to_restore(&e) {
                    // NodeAgent never received the request - safe to restore and retry
                    error!(
                        "StartWorker RPC failed (safe to restore) for {:?} pod {} on node {}: {:?} - restoring state and will retry",
                        create_type, podkey, nodename, e
                    );

                    // Clean up pending pod tracking
                    if let Some(nodeStatus) = self.nodes.get_mut(nodename) {
                        // Remove from pending pods
                        nodeStatus.pendingPods.remove(podkey);

                        // Free allocated resources
                        nodeStatus.FreeResource(resources, podkey)?;

                        info!(
                            "Freed resources for failed pod {} on node {}, will retry via task queue",
                            podkey, nodename
                        );
                    }

                    // Remove from func's pending pods
                    if let Some(funcStatus) = self.funcs.get_mut(funcid) {
                        funcStatus.pendingPods.remove(podkey);
                    }

                    // Remove from pending snapshots (for Snapshot type)
                    // CRITICAL: If we don't remove this, the deduplication check at line 2810-2816
                    // will block retry attempts thinking a snapshot is still in progress
                    if create_type == na::CreatePodType::Snapshot {
                        self.RemovePendingSnapshot(funcid, nodename);
                        info!(
                            "Removed pending snapshot tracking for func {} on node {} - allows retry",
                            funcid, nodename
                        );
                    }

                    // Restore terminated pods that were marked for deletion
                    // They should go back to idle since the new pod creation failed
                    for terminated_podkey in terminated_pod_keys {
                        // Put it back in idle pool if it still exists
                        if let Some(worker) = self.pods.get(terminated_podkey) {
                            let pod_nodename = worker.pod.object.spec.nodename.clone();
                            let pod_funcid = worker.pod.FuncKey();

                            // Remove from both node and func stoppingPods
                            if let Some(nodeStatus) = self.nodes.get_mut(&pod_nodename) {
                                if nodeStatus.RemoveStoppingPod(terminated_podkey) {
                                    worker.SetState(WorkerPodState::Idle);
                                    self.idlePods.put(terminated_podkey.clone(), ());

                                    if let Some(funcStatus) = self.funcs.get_mut(&pod_funcid) {
                                        funcStatus.RemoveStoppingPod(terminated_podkey);
                                    }

                                    info!(
                                        "Restored terminated pod {} to idle (new pod creation failed)",
                                        terminated_podkey
                                    );
                                }
                            }
                        }
                    }

                    // Re-queue task for retry based on type
                    match create_type {
                        na::CreatePodType::Snapshot => {
                            // CRITICAL: Must re-queue snapshot task
                            // TryCreateSnapshotOnNode only adds task on early failures (no resources, node not ready)
                            // When StartWorker succeeds but RPC fails in background, no task is queued
                            // So we must re-queue here to retry snapshot creation
                            self.AddSnapshotTask(nodename, funcid);
                            info!(
                                "Re-queued SnapshotTask for func {} on node {} after safe-to-restore error",
                                funcid, nodename
                            );
                        }
                        na::CreatePodType::Restore => {
                            // No need to re-queue for Restore (standby)
                            // TryAdjustStandbyPodsOnNode always queues next task at start (line 3218)
                            info!(
                                "Cleaned up failed Restore pod {} on node {} - already queued for retry",
                                podkey, nodename
                            );
                        }
                        _ => {
                            warn!(
                                "Unexpected create_type {:?} in ProcessStartWorkerComplete",
                                create_type
                            );
                        }
                    }

                    return Ok(());
                }

                // Other errors (not safe to restore - RPC might have been processed)
                error!(
                    "StartWorker RPC failed for {:?} pod {}: {:?} - not safe to restore, cleanup may be needed",
                    create_type, podkey, e
                );

                // For other errors:
                // - DUPLICATE_SNAPSHOT is already handled by CleanupDuplicateSnapshot message
                // - Other failures will be handled by reconciliation or pod events
                // Don't clean up here as the pod might actually succeed later

                Ok(())
            }
        }
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
                        self.idlePods.put(podKey.clone(), ());
                    }
                }
                _ => (),
            }
        }

        return Ok(());
    }

    /// Reconcile pods stuck in transitional states
    ///
    /// TODO: Implement reconciliation for:
    ///
    /// 1. Pods stuck in Resuming state (> 60 seconds):
    ///    - Check actual pod state from Kubernetes
    ///    - If still not Ready: revert to Standby, free ready resources
    ///    - If actually Ready: wait for informer event (shouldn't happen)
    ///
    /// 2. Pods stuck in Terminating state (> 60 seconds):
    ///    - Check if pod still exists in Kubernetes
    ///    - If exists: retry termination request to NodeAgent
    ///    - If deleted: clean up stoppingPods (shouldn't happen)
    ///
    /// Call this periodically (e.g., every 1-2 minutes) from the main event loop
    pub async fn ReconcileStuckPods(&mut self) -> Result<()> {
        // TODO: Implement reconciliation logic
        Ok(())
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
                let remove = self.idlePods.pop(&podKey).is_some();
                assert!(remove);
                error!("ProcessLeaseWorkerReq using idlepod work {:?}", &podKey);

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

        // Try to resume a pod - this may fail before spawning RPC (no standby, alloc failure, etc.)
        // If it succeeds, RPC is spawned asynchronously and we queue the lease request
        match self.ResumePod(&funcname).await {
            Err(e) => {
                // ResumePod failed before spawning RPC (no standby, alloc failure, etc.)
                // Send error response to caller
                let resp = na::LeaseWorkerResp {
                    error: format!("Failed to resume pod: {:?}", e),
                    ..Default::default()
                };
                tx.send(resp).unwrap();
                return Ok(()); // Don't crash the scheduler loop
            }
            Ok(_) => {
                // ResumePod succeeded - RPC spawned asynchronously
                // Queue the lease request to be processed when pod becomes Ready
                self.PushLeaseWorkerReq(&funcname, req, tx)?;
                return Ok(());
            }
        }
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
                match self.StopWorker(&worker.pod) {
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
            self.idlePods.put(worker.pod.PodKey(), ());
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

    pub fn ProcessCleanupDuplicateSnapshot(
        &mut self,
        funcid: &str,
        nodename: &str,
        podkey: &str,
        resources: &NodeResources,
        terminated_pod_keys: &Vec<String>,
    ) -> Result<()> {
        error!(
            "Processing cleanup for duplicate snapshot: funcid={}, node={}, pod={}, restoring {} terminated pods",
            funcid, nodename, podkey, terminated_pod_keys.len()
        );

        // 1. Remove from pendingsnapshots
        self.RemovePendingSnapshot(funcid, nodename);

        // 2. Remove pending pod from node and free snapshot resource
        if let Some(nodeStatus) = self.nodes.get_mut(nodename) {
            nodeStatus.pendingPods.remove(podkey);
            // Free the allocated snapshot resource
            // We need to free using resources parameter passed in, as it's the NodeResources that was allocated
            nodeStatus.FreeResource(resources, "CleanupDuplicateSnapshot")?;
        }

        // 3. Remove pending pod from func
        if let Some(funcStatus) = self.funcs.get_mut(funcid) {
            funcStatus.pendingPods.remove(podkey);
        }

        // 4. Restore terminated idle pods
        for terminated_podkey in terminated_pod_keys {
            // Re-allocate their resources - allocResources is already NodeResources, just need the reqResource which is Resources
            if let Some(pod) = self.pods.get(terminated_podkey) {
                let pod_nodename = pod.pod.object.spec.nodename.clone();
                let pod_funcid = pod.pod.FuncKey();

                // Remove from stoppingPods in both node and func
                if let Some(nodeStatus) = self.nodes.get_mut(&pod_nodename) {
                    nodeStatus.RemoveStoppingPod(terminated_podkey);
                }
                if let Some(funcStatus) = self.funcs.get_mut(&pod_funcid) {
                    funcStatus.RemoveStoppingPod(terminated_podkey);
                }

                // Add back to idlePods
                self.idlePods.put(terminated_podkey.clone(), ());
                if let Some(nodeStatus) = self.nodes.get_mut(&pod_nodename) {
                    // Get the reqResources (Resources type) from the pod spec
                    let req_resources = &pod.pod.object.spec.reqResources;
                    match nodeStatus.AllocResource(
                        req_resources,
                        "RestoreIdlePod",
                        terminated_podkey,
                        false,
                    ) {
                        Ok(_) => {
                            error!(
                                "Restored idle pod {} to idlePods and re-allocated resources",
                                terminated_podkey
                            );
                        }
                        Err(e) => {
                            error!(
                                "Failed to re-allocate resources for idle pod {}: {:?}",
                                terminated_podkey, e
                            );
                        }
                    }
                }
            }
        }

        error!("Cleanup completed for duplicate snapshot on {}", nodename);
        Ok(())
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


    /// Remove all snapshots for a funckey (non-blocking, async) with spawn_rpc
    ///
    /// This spawns individual RemoveSnapshotFromNode calls for each node.
    /// Completion is handled via WorkerHandlerMsg::RemoveSnapshotComplete messages.
    /// The snapshot is removed from self.snapshots only when all nodes have been cleaned.
    pub fn RemoveSnapshotByFunckey(&mut self, funckey: &str) -> Result<()> {
        info!("RemoveSnapshotByFunckey remove {} start", funckey);

        let snapshot = self.snapshots.get(funckey);
        if let Some(snapshot) = snapshot {
            let nodes: Vec<String> = snapshot.keys().cloned().collect();
            for nodename in nodes {
                if let Err(e) = self.RemoveSnapshotFromNode(&nodename, funckey) {
                    error!(
                        "Failed to spawn RemoveSnapshotFromNode for {} on node {}: {:?}",
                        funckey, nodename, e
                    );
                }
            }
        }

        Ok(())
    }

    pub fn CleanSnapshots(&mut self) -> Result<()> {
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

            match self.RemoveSnapshotByFunckey(&funckey) {
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
            match self.RemovePodsByFunckey(&funckey) {
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
    pub fn RemovePodsByFunckey(&mut self, funckey: &str) -> Result<bool> {
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
            match self.StopWorker(&p.pod) {
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


    /// Remove snapshot from node (non-blocking, async) with spawn_rpc
    ///
    /// This spawns the remove snapshot RPC in the background with:
    /// - Duplicate prevention: Skips if removal already in progress
    /// - Global concurrency limit (max 16 concurrent RPCs)
    /// - Per-node concurrency limit (max 2 per node to prevent conflicts)
    /// - 30 second timeout
    /// - Result sent back via WorkerHandlerMsg::RemoveSnapshotComplete
    pub fn RemoveSnapshotFromNode(&mut self, nodename: &str, funckey: &str) -> Result<()> {
        // Check if removal already in progress for this funckey:nodename pair
        let pending_key = format!("{}:{}", funckey, nodename);
        if self.pending_snapshot_removals.contains(&pending_key) {
            // Already have a removal in flight, skip to avoid duplicate RPCs
            return Ok(());
        }

        let nodeStatus = self.nodes.get(nodename).ok_or_else(|| {
            Error::CommonError(format!("Node {} not found", nodename))
        })?;
        let nodeAgentUrl = nodeStatus.node.NodeAgentUrl();

        // Mark this removal as pending
        self.pending_snapshot_removals.insert(pending_key);

        let funckey_for_rpc = funckey.to_owned();
        let nodename_for_rpc = nodename.to_owned();
        let funckey_for_completion = funckey.to_owned();
        let nodename_for_completion = nodename.to_owned();

        // Spawn RPC with semaphore limits and timeout
        self.spawn_rpc(
            nodename,
            Duration::from_secs(30),
            move || {
                let funckey_clone = funckey_for_rpc.clone();
                let nodename_clone = nodename_for_rpc.clone();
                async move {
                    let mut client =
                        na::node_agent_service_client::NodeAgentServiceClient::connect(nodeAgentUrl)
                            .await?;

                    let request = tonic::Request::new(RemoveSnapshotReq {
                        funckey: funckey_clone.clone(),
                    });
                    let response = client.remove_snapshot(request).await?;
                    let resp = response.into_inner();

                    if !resp.error.is_empty() {
                        error!(
                            "RemoveSnapshotFromNode fail for {} on node {} with error {}",
                            funckey_clone, nodename_clone, resp.error
                        );
                        return Err(Error::CommonError(resp.error));
                    }

                    Ok(())
                }
            },
            move |result| WorkerHandlerMsg::RemoveSnapshotComplete {
                funckey: funckey_for_completion,
                nodename: nodename_for_completion,
                result,
            },
        );

        Ok(())
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
                        WorkerHandlerMsg::DumpState(tx) => {
                            let _ = tx.send(self.DumpState());
                        }
                        WorkerHandlerMsg::CleanupDuplicateSnapshot { funcid, nodename, podkey, resources, terminated_pod_keys } => {
                            self.ProcessCleanupDuplicateSnapshot(&funcid, &nodename, &podkey, &resources, &terminated_pod_keys).ok();
                        }
                        WorkerHandlerMsg::ResumeWorkerComplete { pod_key, nodename, result, resources, terminated_pod_keys } => {
                            if let Err(e) = self.ProcessResumeWorkerComplete(&pod_key, &nodename, result, &resources, terminated_pod_keys).await {
                                panic!("CRITICAL: ProcessResumeWorkerComplete failed for pod {}: {:?} - resource accounting corrupted, scheduler must restart", pod_key, e);
                            }
                        }
                        WorkerHandlerMsg::StartWorkerComplete { funcid, nodename, podkey, create_type, result, resources, terminated_pod_keys } => {
                            self.ProcessStartWorkerComplete(&funcid, &nodename, &podkey, create_type, result, &resources, &terminated_pod_keys).ok();
                        }
                        WorkerHandlerMsg::RemoveSnapshotComplete { funckey, nodename, result } => {
                            self.ProcessRemoveSnapshotComplete(&funckey, &nodename, result);
                        }
                        WorkerHandlerMsg::StopWorkerComplete { pod_key, result } => {
                            self.ProcessStopWorkerComplete(&pod_key, result);
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
                    self.RefreshScheduling(None).await?;
                    // we need to delete pod at first before clean snapshot
                    self.CleanPods().await?;
                    self.CleanSnapshots()?;
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
                                    error!("UpdateSnapshot snapshot {} state: {:?}", snapshot.StoreKey(), &snapshot);
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
                                    self.ListDone(ListType::Func, None).await?;
                                }
                                FunctionStatus::KEY => {
                                    self.ListDone(ListType::FuncStatus, None).await?;
                                }
                                Node::KEY => {
                                    self.ListDone(ListType::Node, None).await?;
                                }
                                FuncPod::KEY => {
                                    self.ListDone(ListType::FuncPod, None).await?;
                                }
                                ContainerSnapshot::KEY => {
                                    self.ListDone(ListType::Snapshot, None).await?;
                                }
                                FuncPolicy::KEY => {
                                    self.ListDone(ListType::Funcpolicy, None).await?;
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
                func.object.spec.SnapshotResource(contextCount as u64)
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

        for (podKey, _) in self.idlePods.iter().rev() {
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
                                func.object.spec.SnapshotResource(nr.maxContextCnt as u64)
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
            self.idlePods.pop(podKey);
            error!("FindNode4Pod remove idlepod missing {:?}", &podKey);
        }

        if findnodeName.is_none() {
            let req = if !forStandby {
                func.object.spec.SnapshotResource(1)
            } else {
                func.object.spec.RunningResource()
            };

            let mut node_details = String::new();
            for (nodename, (nr, _)) in &nodeSnapshots {
                node_details.push_str(&format!("\n  Node '{}': available={:#?}", nodename, nr));
            }

            error!(
                "can't find enough resource for {}, required resources: {:#?}, nodes allocation state: {:#?}{}",
                func.Key(),
                req,
                allocStates.values(),
                node_details
            );
            return Err(Error::SchedulerNoEnoughResource(format!(
                "can't find enough resource for {}, required resources: {:#?}, nodes allocation state: {:#?}{}",
                func.Key(),
                req,
                allocStates.values(),
                node_details
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

    pub async fn RefreshScheduling(
        &mut self,
        _msgTx: Option<&mpsc::Sender<WorkerHandlerMsg>>,
    ) -> Result<()> {
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
                // Skip if snapshot already exists on this node
                if let Some(snapshots_for_func) = self.snapshots.get(funcId) {
                    if snapshots_for_func.contains_key(nodename) {
                        continue;
                    }
                }

                // Skip if snapshot is already pending on this node
                if let Some(pending_nodes) = self.pendingsnapshots.get(funcId) {
                    if pending_nodes.contains(nodename) {
                        continue;
                    }
                }

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
                self.TryAdjustStandbyPodsOnNode(&nodename).await.ok();
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

        for (podKey, _) in self.idlePods.iter().rev() {
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
            self.idlePods.pop(workerid);
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
        let reqResource = func
            .object
            .spec
            .SnapshotResource(contextCount as u64)
            .clone();

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

        // Track pods marked for termination - don't free their resources yet
        // Resources will be freed when the snapshot pod becomes Ready or fails
        for worker in &terminateWorkers {
            let terminated_podkey = worker.pod.PodKey();
            // Remove from idle pool but don't free resources yet
            let removed = self.idlePods.pop(&terminated_podkey).is_some();
            assert!(removed);
            // Set state to Terminating
            worker.SetState(WorkerPodState::Terminating);
        }

        // CRITICAL: Reserve snapshot resource BEFORE RPC to prevent race condition
        // This follows the same pattern as ResumePod:
        // 1. Allocate from the simulated nodeResources (which includes freed idle pod resources)
        // 2. Subtract directly from actual available (may go temporarily negative with i64)
        let resources;
        let nodeAgentUrl;
        {
            let nodeStatus = self.nodes.get_mut(nodename).unwrap();

            // Add terminated pods to node's stoppingPods so deletion events don't free resources prematurely
            for worker in &terminateWorkers {
                let terminated_podkey = worker.pod.PodKey();
                nodeStatus.AddStoppingPod(&terminated_podkey);
            }

            // Also add to each pod's func stoppingPods (pods may belong to different funcs)
            for worker in &terminateWorkers {
                let terminated_podkey = worker.pod.PodKey();
                let terminated_funckey = worker.pod.FuncKey();
                if let Some(funcStatus) = self.funcs.get_mut(&terminated_funckey) {
                    funcStatus.AddStoppingPod(&terminated_podkey);
                } else {
                    error!(
                        "TryCreateSnapshotOnNode: missing funcStatus for {} when marking stopping pod {}",
                        terminated_funckey, terminated_podkey
                    );
                }
            }
            let contextCnt = nodeStatus.node.object.resources.GPUResource().maxContextCnt;
            let snapshotResource = func.object.spec.SnapshotResource(contextCnt as u64);

            // Allocate from simulated nodeResources (which has idle pods' resources added by TryFreeResources)
            // This validates the allocation will work once idle pods are terminated
            resources = nodeResources.Alloc(&snapshotResource, true)?;

            // Subtract directly from actual available (no validation, may go negative)
            // The idle pods still hold their resources until deleted, so available may go negative temporarily
            nodeStatus.available.Sub(&resources)?;

            nodeAgentUrl = nodeStatus.node.NodeAgentUrl();

            info!(
                "Before CreateSnapshot RPC for func {} reserved {} snapshot resource (idle pods still allocated), node available is {:#?}",
                funcId,
                serde_json::to_string(&resources).unwrap_or_default(),
                &nodeStatus.available
            );
        }

        // 3. Spawn async snapshot RPC (non-blocking) - returns pod ID immediately
        // The RPC executes in background; tracking happens after to use the generated ID
        let id: u64 = match self
            .StartWorker(
                &nodeAgentUrl,
                &func,
                &resources,
                &resources,
                na::CreatePodType::Snapshot,
                &terminateWorkers,
                nodename,
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

        // 4. Track pending pod using the generated ID
        // This is safe because scheduler is single-threaded - no operations can interleave
        self.SetSnapshotStatus(funcId, nodename, SnapshotScheduleState::Scheduled);

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


    /// Adjust standby pods on a node following the async pattern from ResumePod
    ///
    /// - Uses StartWorker (async, non-blocking)
    /// - Allocates resources BEFORE RPC to prevent race conditions
    /// - No rollback on RPC error - completion handler deals with it
    /// - RPC result handled via ProcessStartWorkerComplete message
    pub async fn TryAdjustStandbyPodsOnNode(&mut self, nodename: &str) -> Result<()> {
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
        let mut excessStandby = Vec::new();

        for (funcId, &cnt) in &funcPodCnt {
            let func = match self.funcs.get(funcId) {
                None => continue,
                Some(f) => f,
            };

            let tenant = func.func.tenant.clone();

            let policy = self.FuncPolicy(&tenant, &func.func.object.spec.policy);

            if policy.standbyPerNode > cnt {
                // Need more standby pods
                needStandby.push(funcId.to_owned());
            } else if policy.standbyPerNode < cnt {
                // Have excess standby pods - collect them for cleanup
                excessStandby.push((funcId.to_owned(), cnt - policy.standbyPerNode));
            }
        }

        // First, cleanup excess standby pods
        for (funcId, excess_count) in excessStandby {
            let mut standby_pods = Vec::new();

            // Collect standby pods for this function on this node
            for (podKey, pod) in &self.pods {
                if pod.pod.FuncKey() != funcId {
                    continue;
                }
                if &pod.pod.object.spec.nodename != nodename {
                    continue;
                }
                if pod.pod.object.status.state != PodState::Standby {
                    continue;
                }
                standby_pods.push((podKey.clone(), pod.pod.clone()));
            }

            // Sort for deterministic selection
            standby_pods.sort_by(|a, b| a.0.cmp(&b.0));

            // Terminate excess pods
            let to_terminate = std::cmp::min(excess_count as usize, standby_pods.len());
            for i in 0..to_terminate {
                let (pod_key, pod) = &standby_pods[i];
                error!(
                    "AdjustStandbyPodsOnNode: terminating excess standby pod {} on node {} for func {}",
                    pod_key, nodename, funcId
                );

                if let Err(e) = self.StopWorker(pod) {
                    error!(
                        "AdjustStandbyPodsOnNode: failed to stop worker {}: {:?}",
                        pod_key, e
                    );
                }
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

        // CRITICAL: Allocate standby resource BEFORE RPC to prevent race condition
        // This follows the same pattern as ResumePod and TryCreateSnapshotOnNode
        {
            let nodeStatus = match self.nodes.get_mut(&nodename) {
                None => return Ok(()), // the node information is not synced
                Some(ns) => ns,
            };

            allocResources =
                nodeStatus.AllocResource(&standbyResource, "CreateStandby", funcId, false)?;
            resourceQuota = nodeStatus.ReadyResourceQuota(&function.object.spec.resources)?;
            nodeAgentUrl = nodeStatus.node.NodeAgentUrl();

            info!(
                "Before CreateStandby RPC for func {} on node {} allocated standby resource {}, node available is {:#?}",
                funcId,
                nodename,
                serde_json::to_string(&allocResources).unwrap_or_default(),
                &nodeStatus.available
            );
        }

        // Spawn async standby RPC (non-blocking) - returns pod ID immediately
        // The RPC executes in background; result handled in ProcessStartWorkerComplete
        // Note: StartWorker only returns Err if UID generation fails (before spawning RPC)
        //       In that case, we must rollback the resource allocation
        let id = match self
            .StartWorker(
                &nodeAgentUrl,
                &function,
                &allocResources,
                &resourceQuota,
                na::CreatePodType::Restore,
                &Vec::new(),
                &nodename, // Pass nodename for async tracking
            )
            .await
        {
            Err(e) => {
                // UID generation failed - RPC was never spawned, rollback resource allocation
                error!(
                    "StartWorker failed (UID generation) for standby pod on {}: {:?}, rolling back resources",
                    nodename, e
                );
                let nodeStatus = match self.nodes.get_mut(&nodename) {
                    None => return Ok(()), // node disappeared
                    Some(ns) => ns,
                };
                nodeStatus.FreeResource(&allocResources, "")?;
                return Err(e);
            }
            Ok(id) => id, // RPC spawned successfully, continue tracking
        };

        // Track the pending pod - RPC is now running in background
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

        info!(
            "CreateStandby RPC spawned for func {} on node {}, pod {}, waiting for completion",
            funcId, nodename, podKey
        );

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
        let hasPod = self.RemovePodsByFunckey(&spec.Key())?;

        if !hasPod {
            self.RemoveSnapshotByFunckey(&spec.Key())?;
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

        // Track pods marked for termination - don't free their resources yet
        // Resources will be freed when the resume pod becomes Ready or fails
        for worker in &terminateWorkers {
            let terminated_podkey = worker.pod.PodKey();
            // Remove from idle pool but don't free resources yet
            let removed = self.idlePods.pop(&terminated_podkey).is_some();
            assert!(removed);
        }

        // CRITICAL: Do all bookkeeping BEFORE spawning RPC to prevent race conditions
        // If we do accounting after spawn, another ResumePod could see resources as available
        // and double-book the same resources before accounting completes.

        // 1. Update pod states
        pod.SetState(WorkerPodState::Resuming);
        for worker in &terminateWorkers {
            worker.SetState(WorkerPodState::Terminating);
        }

        // 2. Update resource accounting
        // CRITICAL: Subtract FULL ready allocation before RPC to prevent race condition.
        // The standby allocation remains in place until RPC succeeds.
        //
        // With i64 resources, available can go temporarily negative, which is safe.
        // This prevents the race where freeing standby early allows another pod to grab it
        // before this RPC completes.
        //
        // Flow:
        //   - Pod currently holds: standby (already subtracted from available)
        //   - Before RPC: Sub(ready) → reserve ready allocation (may go negative)
        //   - On success: FreeResource(standby) → release standby
        //   - On failure: Add(ready) → release ready, standby stays allocated
        {
            let nodeStatus = self.nodes.get_mut(&nodename).unwrap();

            // Add terminated pods to node's stoppingPods so deletion events don't free resources
            for worker in &terminateWorkers {
                let terminated_podkey = worker.pod.PodKey();
                nodeStatus.AddStoppingPod(&terminated_podkey);
            }

            // Also add to each pod's func stoppingPods (pods may belong to different funcs)
            for worker in &terminateWorkers {
                let terminated_podkey = worker.pod.PodKey();
                let terminated_funckey = worker.pod.FuncKey();
                if let Some(funcStatus) = self.funcs.get_mut(&terminated_funckey) {
                    funcStatus.AddStoppingPod(&terminated_podkey);
                } else {
                    error!(
                        "ResumePod: missing funcStatus for {} when marking stopping pod {}",
                        terminated_funckey, terminated_podkey
                    );
                }
            }

            // Reserve ready allocation (standby stays allocated until RPC succeeds)
            nodeStatus.available.Sub(&resources)?;

            info!(
                "Before ResumePod RPC for pod {} reserved {} ready (standby {} remains allocated), node available is {:#?}",
                pod.pod.PodKey(),
                serde_json::to_string(&resources).unwrap_or_default(),
                serde_json::to_string(&standbyResource).unwrap_or_default(),
                &nodeStatus.available
            );
        }

        // 3. Track pending pod
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

        // 4. NOW spawn async resume RPC (non-blocking) - all state is set up
        self.ResumeWorker(
            &naUrl,
            &pod.pod.tenant,
            &pod.pod.namespace,
            &pod.pod.object.spec.funcname,
            pod.pod.object.spec.fprevision,
            &id,
            &resources,
            &terminateWorkers,
            &nodename,
        )?;

        // ResumeWorker returns immediately - actual RPC happens in background
        // Result will be handled in ProcessResumeWorkerComplete

        return Ok(());
    }


    /// Create worker using spawn_rpc for proper concurrency control
    /// Returns the generated pod ID immediately, RPC executes in background
    pub async fn StartWorker(
        &mut self,
        naUrl: &str,
        func: &Function,
        allocResources: &NodeResources,
        resourceQuota: &NodeResources,
        createType: na::CreatePodType,
        terminatePods: &Vec<WorkerPod>,
        nodename: &str,
    ) -> Result<u64> {
        let tenant = &func.tenant;
        let namespace = &func.namespace;
        let funcname = &func.name;
        let fpRevision = func.Version();
        let id = UID.get().unwrap().Get().await? as u64;

        // Build pod key and func id for tracking
        let podkey = FuncPod::FuncPodKey(tenant, namespace, funcname, fpRevision, &format!("{id}"));
        let funcid = FuncPod::FuncObjectKey(tenant, namespace, funcname, fpRevision);

        // Clone data needed for the async task
        let naUrl = naUrl.to_owned();
        let tenant = tenant.to_owned();
        let namespace = namespace.to_owned();
        let funcname = funcname.to_owned();
        let allocResources = allocResources.clone();
        let resourceQuota = resourceQuota.clone();
        let cleanup_resources = allocResources.clone();

        // Build terminate pods list and capture keys
        let mut tps = Vec::new();
        let mut terminated_pod_keys = Vec::new();
        for p in terminatePods {
            let pod = p.pod.clone();
            terminated_pod_keys.push(pod.PodKey());
            let termniatePod = TerminatePodReq {
                tenant: pod.tenant.clone(),
                namespace: pod.namespace.clone(),
                funcname: pod.object.spec.funcname.clone(),
                fprevision: pod.object.spec.fprevision.clone(),
                id: pod.object.spec.id.clone(),
            };
            tps.push(termniatePod);
        }

        // Build annotations
        let mut annotations = Vec::new();
        annotations.push(Kv {
            key: FUNCPOD_TYPE.to_owned(),
            val: FUNCPOD_PROMPT.to_owned(),
        });
        annotations.push(Kv {
            key: FUNCPOD_FUNCNAME.to_owned(),
            val: funcname.clone(),
        });

        // Get function spec
        let mut spec = func.object.spec.clone();
        let policy = self.FuncPolicy(&tenant, &spec.policy);
        spec.policy = ObjRef::Obj(policy);

        let msgTx = self.msgTx.clone();
        let nodename_owned = nodename.to_owned();
        let nodename_owned_clone = nodename.to_owned();
        let podkey_clone = podkey.clone();
        let funcid_clone = funcid.clone();
        let create_type_copy = createType.clone();
        let cleanup_resources_clone = cleanup_resources.clone();
        let terminated_pod_keys_clone = terminated_pod_keys.clone();

        // Spawn RPC with semaphore limits and timeout
        self.spawn_rpc(
            nodename,
            Duration::from_secs(30),
            move || async move {
                let mut client =
                    na::node_agent_service_client::NodeAgentServiceClient::connect(naUrl).await?;

                let request = tonic::Request::new(na::CreateFuncPodReq {
                    tenant: tenant.clone(),
                    namespace: namespace.clone(),
                    funcname: funcname.clone(),
                    fprevision: fpRevision,
                    id: format!("{id}"),
                    labels: Vec::new(),
                    annotations: annotations,
                    create_type: createType.into(),
                    funcspec: serde_json::to_string(&spec)?,
                    alloc_resources: serde_json::to_string(&allocResources).unwrap(),
                    resource_quota: serde_json::to_string(&resourceQuota).unwrap(),
                    terminate_pods: tps,
                });

                let response = client.create_func_pod(request).await?;
                let resp = response.into_inner();

                if !resp.error.is_empty() {
                    // Check if this is a duplicate snapshot error
                    if resp.error.starts_with("DUPLICATE_SNAPSHOT:") {
                        error!(
                            "Duplicate snapshot detected for {} on {}, sending cleanup message",
                            funcid_clone, nodename_owned
                        );
                        let cleanup_msg = WorkerHandlerMsg::CleanupDuplicateSnapshot {
                            funcid: funcid_clone.clone(),
                            nodename: nodename_owned.clone(),
                            podkey: podkey_clone.clone(),
                            resources: cleanup_resources.clone(),
                            terminated_pod_keys: terminated_pod_keys.clone(),
                        };
                        msgTx.try_send(cleanup_msg).unwrap();
                        // Return error to trigger completion handler with error
                        return Err(Error::CommonError(resp.error));
                    }
                    error!("StartWorker fail with error {}", &resp.error);
                    return Err(Error::CommonError(resp.error));
                }

                Ok(())
            },
            move |result| WorkerHandlerMsg::StartWorkerComplete {
                funcid,
                nodename: nodename_owned_clone,
                podkey,
                create_type: create_type_copy,
                result,
                resources: cleanup_resources_clone,
                terminated_pod_keys: terminated_pod_keys_clone,
            },
        );

        return Ok(id);
    }

    // TODO: dead code, remove
    pub async fn CreateSnapshotWorker(
        &self,
        naUrl: &str,
        func: &Function,
        id: u64,
        allocResources: &NodeResources,
        resourceQuota: &NodeResources,
        terminatePods: &Vec<WorkerPod>,
    ) -> Result<()> {
        let tenant = &func.tenant;
        let namespace = &func.namespace;
        let funcname = &func.name;
        let fpRevision = func.Version();

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
            create_type: na::CreatePodType::Snapshot.into(),
            funcspec: serde_json::to_string(&spec)?,
            alloc_resources: serde_json::to_string(allocResources).unwrap(),
            resource_quota: serde_json::to_string(resourceQuota).unwrap(),
            terminate_pods: tps,
        });

        // Wait for response (blocks scheduler but prevents race conditions)
        let response = client.create_func_pod(request).await?;
        let resp = response.into_inner();
        if !resp.error.is_empty() {
            error!(
                "Scheduler: Fail to CreateSnapshotWorker {} {} {} {}",
                namespace, funcname, id, resp.error
            );
            return Err(Error::CommonError(resp.error));
        }

        Ok(())
    }

    /// Resume a standby worker pod to Ready state (non-blocking, async) with spawn_rpc
    ///
    /// This spawns the resume RPC in the background with:
    /// - Global concurrency limit (max 16 concurrent RPCs)
    /// - Per-node concurrency limit (max 2 per node to prevent GPU reset conflicts)
    /// - 30 second timeout
    /// - Result sent back via WorkerHandlerMsg::ResumeWorkerComplete
    pub fn ResumeWorker(
        &mut self,
        naUrl: &str,
        tenant: &str,
        namespace: &str,
        funcname: &str,
        fprevsion: i64,
        id: &str,
        allocResources: &NodeResources,
        terminatePods: &Vec<WorkerPod>,
        nodename: &str,
    ) -> Result<()> {
        // Build pod key for tracking completion
        let pod_key = format!("{}/{}/{}/{}", tenant, namespace, funcname, id);

        // Clone data needed for the async task and completion handler
        let naUrl = naUrl.to_owned();
        let tenant = tenant.to_owned();
        let namespace = namespace.to_owned();
        let funcname = funcname.to_owned();
        let id = id.to_owned();
        let allocResources = allocResources.clone();
        let nodename_clone = nodename.to_owned();
        let resources_for_completion = allocResources.clone();

        // Capture terminated pod keys for completion handler
        let mut terminated_pod_keys = Vec::new();
        let mut tps = Vec::new();
        for p in terminatePods {
            let pod = p.pod.clone();
            terminated_pod_keys.push(pod.PodKey());
            let termniatePod = TerminatePodReq {
                tenant: pod.tenant.clone(),
                namespace: pod.namespace.clone(),
                funcname: pod.object.spec.funcname.clone(),
                fprevision: pod.object.spec.fprevision.clone(),
                id: pod.object.spec.id.clone(),
            };
            tps.push(termniatePod);
        }

        // Spawn RPC with semaphore limits and timeout
        self.spawn_rpc(
            nodename,
            Duration::from_secs(30),
            move || async move {
                let mut client =
                    na::node_agent_service_client::NodeAgentServiceClient::connect(naUrl).await?;

                let request = tonic::Request::new(na::ResumePodReq {
                    tenant: tenant.clone(),
                    namespace: namespace.clone(),
                    funcname: funcname.clone(),
                    fprevision: fprevsion,
                    id: id.clone(),
                    alloc_resources: serde_json::to_string(&allocResources).unwrap(),
                    terminate_pods: tps,
                });

                let response = client.resume_pod(request).await?;
                let resp = response.into_inner();

                if !resp.error.is_empty() {
                    error!(
                        "Scheduler: Fail to ResumeWorker worker {} {} {} {}",
                        namespace, funcname, id, resp.error
                    );
                    return Err(Error::CommonError(resp.error));
                }

                Ok(())
            },
            move |result| WorkerHandlerMsg::ResumeWorkerComplete {
                pod_key,
                nodename: nodename_clone,
                result,
                resources: resources_for_completion,
                terminated_pod_keys,
            },
        );

        Ok(())
    }


    /// Stop/terminate a worker pod (non-blocking, async) with spawn_rpc
    ///
    /// Uses spawn_rpc pattern.
    /// Result will be delivered via WorkerHandlerMsg::StopWorkerComplete.
    /// Pod deletion will be detected via Kubernetes informer events.
    pub fn StopWorker(&mut self, pod: &FuncPod) -> Result<()> {
        let nodename = pod.object.spec.nodename.clone();
        let nodeStatus = self.nodes.get(&nodename).ok_or_else(|| {
            Error::CommonError(format!("Node {} not found", nodename))
        })?;
        let naUrl = nodeStatus.node.NodeAgentUrl();

        self.StopWorkerInner(
            &naUrl,
            &nodename,
            &pod.tenant,
            &pod.namespace,
            &pod.object.spec.funcname,
            pod.object.spec.fprevision,
            &pod.object.spec.id,
        )
    }


    /// Stop/terminate a worker pod (non-blocking, async) with spawn_rpc
    ///
    /// This spawns the terminate pod RPC in the background with:
    /// - Global concurrency limit (max 16 concurrent RPCs)
    /// - Per-node concurrency limit (max 2 per node to prevent conflicts)
    /// - 30 second timeout
    /// - Result sent back via WorkerHandlerMsg::StopWorkerComplete
    ///
    /// This doesn't wait for NodeAgent response.
    /// Pod deletion will be detected via Kubernetes informer events.
    pub fn StopWorkerInner(
        &mut self,
        naUrl: &str,
        nodename: &str,
        tenant: &str,
        namespace: &str,
        funcname: &str,
        fprevision: i64,
        id: &str,
    ) -> Result<()> {
        let pod_key = FuncPod::FuncPodKey(tenant, namespace, funcname, fprevision, id);

        let naUrl = naUrl.to_owned();
        let tenant = tenant.to_owned();
        let namespace = namespace.to_owned();
        let funcname = funcname.to_owned();
        let id = id.to_owned();
        let pod_key_for_completion = pod_key.clone();

        // Spawn RPC with semaphore limits and timeout
        self.spawn_rpc(
            nodename,
            Duration::from_secs(30),
            move || {
                let tenant_clone = tenant.clone();
                let namespace_clone = namespace.clone();
                let funcname_clone = funcname.clone();
                let id_clone = id.clone();
                async move {
                    let mut client =
                        na::node_agent_service_client::NodeAgentServiceClient::connect(naUrl)
                            .await?;

                    let request = tonic::Request::new(na::TerminatePodReq {
                        tenant: tenant_clone.clone(),
                        namespace: namespace_clone.clone(),
                        funcname: funcname_clone.clone(),
                        fprevision: fprevision,
                        id: id_clone.clone(),
                    });
                    let response = client.terminate_pod(request).await?;
                    let resp = response.into_inner();

                    if !resp.error.is_empty() {
                        error!(
                            "StopWorkerInner fail for {} {} {} {} {}",
                            tenant_clone, namespace_clone, funcname_clone, id_clone, resp.error
                        );
                        return Err(Error::CommonError(resp.error));
                    }

                    Ok(())
                }
            },
            move |result| WorkerHandlerMsg::StopWorkerComplete {
                pod_key: pod_key_for_completion,
                result,
            },
        );

        Ok(())
    }

    // return whether all list done
    pub async fn ListDone(
        &mut self,
        listType: ListType,
        msgTx: Option<&mpsc::Sender<WorkerHandlerMsg>>,
    ) -> Result<bool> {
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

            self.RefreshScheduling(msgTx).await?;
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

        // remove all snapshots on this node
        let funcIds: Vec<String> = self.snapshots.keys().cloned().collect();
        for funcId in funcIds {
            if let Some(snapshots_for_func) = self.snapshots.get_mut(&funcId) {
                snapshots_for_func.remove(nodename);
                if snapshots_for_func.is_empty() {
                    self.snapshots.remove(&funcId);
                }
            }
        }

        // remove pending snapshots for this node
        let funcIds: Vec<String> = self.pendingsnapshots.keys().cloned().collect();
        for funcId in funcIds {
            self.RemovePendingSnapshot(&funcId, nodename);
        }

        // Clean per-node semaphore (old epoch may have in-flight permits)
        self.node_semaphores.remove(nodename);

        // Clean pending snapshot removal RPCs for this node
        // Format: "funckey:nodename" - remove all entries ending with ":nodename"
        let suffix = format!(":{}", nodename);
        self.pending_snapshot_removals.retain(|key| !key.ends_with(&suffix));

        // Clean snapshot schedule entries for this node
        // BiIndex tracks (funcid, nodename) pairs for scheduled snapshots
        self.SnapshotSched.RemoveById2(nodename);

        // Clean queued lease requests for pods that were on this node
        // These requests are waiting for pods that no longer exist after node restart
        for (_, func_status) in self.funcs.iter_mut() {
            func_status.leaseWorkerReqs.retain(|(_lease_req, _tx)| {
                // Check if any pod being requested was on the restarted node
                // If so, fail the request since those pods are gone
                // Note: We can't easily check pod locations here without pod state,
                // so we'll let the lease requests time out naturally or fail when
                // they try to lease non-existent pods
                true // Keep all requests for now - they'll fail naturally
            });
        }

        // Note: taskQueue and delayed_tasks are not cleaned here because:
        // 1. taskQueue is a stream-based queue that can't be efficiently filtered
        // 2. delayed_tasks is a BinaryHeap that can't be efficiently filtered
        // Tasks referencing old node epoch will fail naturally when executed:
        // - SnapshotTask(funcid, nodename) - will fail if node doesn't exist
        // - RemoveSnapshotFromNode - will fail if snapshot doesn't exist
        // - AddNode/DelayedInitNode - will be no-op or succeed with new epoch
        // - StandbyTask - will schedule on available nodes

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

        // Reconstruct pendingsnapshots for snapshot pods during restart
        // This ensures that in-progress snapshots are tracked properly
        if boxPod.pod.object.spec.create_type == CreatePodType::Snapshot
            && boxPod.pod.object.status.state != PodState::Failed
        {
            self.AddPendingSnapshot(&fpKey, &nodename);
        }

        if boxPod.State().IsIdle() && boxPod.pod.object.status.state == PodState::Ready {
            boxPod.SetIdle(SetIdleSource::AddPod);
            self.idlePods.put(boxPod.pod.PodKey(), ());
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

                // Free standby resources when pod transitions from Standby to Resuming
                // This balances the resource accounting:
                // - ResumePod: Sub(ready resources) before RPC
                // - Standby→Resuming: Add(standby resources) when resume accepted
                // - RemovePod: Add(ready resources) when pod deleted
                if oldPod.pod.object.status.state == PodState::Standby
                    && boxPod.pod.object.status.state == PodState::Resuming
                {
                    let standbyResource = oldPod.pod.object.spec.allocResources.clone();

                    nodeStatus.available.Add(&standbyResource)?;
                    info!(
                        "Pod {} transitioned Standby→Resuming, freed standby allocation {}",
                        podKey,
                        serde_json::to_string(&standbyResource).unwrap_or_default()
                    );
                }

                // When pod fails, let deletion events handle cleanup
                // Don't try to restore terminated pods - termination request was already sent to NodeAgent
                // Deletion events will clean up stoppingPods and free resources naturally
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

        // Remove from idlePods if present, put it here because node deletion event may arrive before pod deletion event.
        self.idlePods.pop(&pod.PodKey());

        match self.nodes.get_mut(&nodeName) {
            None => (), // node information doesn't reach scheduler, will process when it arrives
            Some(nodeStatus) => {
                nodeStatus.RemovePod(&pod.PodKey(), &pod.object.spec.allocResources)?;
            }
        }
        match self.funcs.get_mut(&funcKey) {
            None => (), // fp information doesn't reach scheduler, will process when it arrives
            Some(fpStatus) => {
                fpStatus.RemovePod(&pod.PodKey()).unwrap();
            }
        }

        if podCreateType == CreatePodType::Snapshot {
            self.RemovePendingSnapshot(&funcKey, &nodeName);
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

        // Clean up pending snapshots for this function
        self.pendingsnapshots.remove(&key);

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
