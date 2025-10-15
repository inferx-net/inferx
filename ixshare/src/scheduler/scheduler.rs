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
use std::pin::Pin;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::sync::Mutex;

use once_cell::sync::OnceCell;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::Stream;
use tokio_stream::StreamExt;

use inferxlib::node::WorkerPodState;
use inferxlib::obj_mgr::pod_mgr::FuncPod;
use inferxlib::obj_mgr::pod_mgr::PodState;
use std::ops::Deref;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::sync::Mutex as TMutex;
use tokio::sync::Notify;

use crate::common::*;
use crate::gateway::metrics::SCHEDULER_METRICS;
use crate::metastore::informer::EventHandler;
use crate::metastore::store::ThreadSafeStore;
use crate::na;
use crate::node_config::SchedulerConfig;
use crate::node_config::NODE_CONFIG;
use crate::scheduler::sched_obj_repo::SchedObjRepo;
use crate::scheduler::scheduler_http::SchedulerHttpSrv;
use crate::scheduler::scheduler_register::SchedulerRegister;
use crate::scheduler::scheduler_svc::RunSchedulerSvc;
use inferxlib::data_obj::DeltaEvent;

use super::scheduler_handler::SchedulerHandler;
use super::scheduler_handler::WorkerHandlerMsg;

pub static SCHED_OBJREPO: OnceCell<SchedObjRepo> = OnceCell::new();

lazy_static::lazy_static! {
    pub static ref SCHEDULER_CONFIG: SchedulerConfig = SchedulerConfig::New(&NODE_CONFIG);
    pub static ref SCHEDULER: Scheduler = Scheduler::New();
    static ref IDLE_POD_SEQNUM: AtomicU64 = AtomicU64::new(0);
}

#[derive(Debug)]
pub struct Scheduler {
    pub closeNotify: Arc<Notify>,

    pub eventTx: mpsc::Sender<DeltaEvent>,
    pub msgTx: mpsc::Sender<WorkerHandlerMsg>,

    pub eventRx: Mutex<Option<mpsc::Receiver<DeltaEvent>>>,
    pub msgRx: Mutex<Option<mpsc::Receiver<WorkerHandlerMsg>>>,
}

impl EventHandler for Scheduler {
    fn handle(&self, _store: &ThreadSafeStore, event: &DeltaEvent) {
        self.ProcessDeltaEvent(event).unwrap();
    }
}

impl Scheduler {
    pub fn New() -> Self {
        let (eventTx, eventRx) = mpsc::channel::<DeltaEvent>(1000);
        let (msgTx, msgRx) = mpsc::channel::<WorkerHandlerMsg>(1000);

        return Self {
            closeNotify: Arc::new(Notify::new()),
            eventTx: eventTx,
            msgTx: msgTx,

            eventRx: Mutex::new(Some(eventRx)),
            msgRx: Mutex::new(Some(msgRx)),
        };
    }

    pub fn ProcessDeltaEvent(&self, event: &DeltaEvent) -> Result<()> {
        self.eventTx.try_send(event.clone()).unwrap();
        return Ok(());
    }

    pub async fn RefreshGateway(
        &self,
        req: na::RefreshGatewayReq,
    ) -> Result<na::RefreshGatewayResp> {
        let resp = na::RefreshGatewayResp {
            error: "".to_owned(),
        };

        self.msgTx
            .try_send(WorkerHandlerMsg::RefreshGateway(req))
            .unwrap();

        return Ok(resp);
    }

    pub async fn LeaseWorker(&self, req: na::LeaseWorkerReq) -> Result<na::LeaseWorkerResp> {
        let (tx, rx) = oneshot::channel();

        self.msgTx
            .try_send(WorkerHandlerMsg::LeaseWorker((req, tx)))
            .unwrap();

        match rx.await {
            Err(e) => {
                return Ok(na::LeaseWorkerResp {
                    error: format!("{:?}", e),
                    ..Default::default()
                });
            }
            Ok(resp) => {
                //error!("LeaseWorker response {:?}", &resp);
                return Ok(resp);
            }
        }
    }

    pub async fn ReturnWorker(&self, req: na::ReturnWorkerReq) -> Result<na::ReturnWorkerResp> {
        let (tx, rx) = oneshot::channel();

        self.msgTx
            .try_send(WorkerHandlerMsg::ReturnWorker((req, tx)))
            .unwrap();

        match rx.await {
            Err(e) => {
                return Ok(na::ReturnWorkerResp {
                    error: format!("{:?}", e),
                    ..Default::default()
                })
            }
            Ok(resp) => return Ok(resp),
        }
    }

    pub async fn StartProcess(&self) -> Result<()> {
        let mut eventRx = self.eventRx.lock().unwrap().take().unwrap();
        let mut msgRx = self.msgRx.lock().unwrap().take().unwrap();
        let closeNotify = self.closeNotify.clone();

        let mut handler = SchedulerHandler::New();
        loop {
            match handler
                .Process(closeNotify.clone(), &mut eventRx, &mut msgRx)
                .await
            {
                Err(e) => {
                    error!("scheduler handler fail with {:?}", e);
                }
                Ok(()) => {
                    break;
                }
            }
        }

        return Ok(());
    }
}

pub async fn SchedulerSvc() -> Result<()> {
    SCHEDULER_METRICS.lock().await.Register().await;

    let objRepo = SchedObjRepo::New(SCHEDULER_CONFIG.stateSvcAddrs.clone()).await?;

    SCHED_OBJREPO.set(objRepo.clone()).unwrap();

    let schedulerSvcFuture = RunSchedulerSvc();

    tokio::select! {
        res = objRepo.Process() => {
            info!("SchedObjRepo finish {:?}", res);
        }
        res  = schedulerSvcFuture => {
            info!("schedulersvc finish {:?}", res);
        }
        res  = SchedulerProcess() => {
            info!("schedulerRegister finish {:?}", res);
        }
        res = SchedulerHttpSrv() => {
            info!("SchedulerHttpSrv finish {:?}", res);
        }
    }

    return Ok(());
}

pub async fn SchedulerProcess() -> Result<()> {
    let schedulerRegister = SchedulerRegister::New(
        &SCHEDULER_CONFIG.etcdAddrs,
        &SCHEDULER_CONFIG.nodeIp,
        SCHEDULER_CONFIG.schedulerPort,
    )
    .await?;

    let leaseId = schedulerRegister.CreateObject().await?;

    let handle = tokio::spawn(async move {
        tokio::select! {
            res = SCHEDULER.StartProcess() => {
                info!("SCHEDULER process finish {:?}", res);
            }
            res  = schedulerRegister.Process(leaseId) => {
                info!("schedulerRegister finish {:?}", res);
            }
        }
    });

    let _ = handle.await;

    return Ok(());
}

#[derive(Debug)]
pub struct WorkerPodInner {
    pub pod: FuncPod,
    pub workerState: Mutex<WorkerPodState>,
}

#[derive(Debug, Clone)]
pub struct WorkerPod(pub Arc<WorkerPodInner>);

impl WorkerPod {
    pub fn State(&self) -> WorkerPodState {
        return *self.workerState.lock().unwrap();
    }

    pub fn SetState(&self, state: WorkerPodState) {
        *self.workerState.lock().unwrap() = state;
    }

    pub fn SetIdle(&self) -> u64 {
        let returnId = IDLE_POD_SEQNUM.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.SetState(WorkerPodState::Idle(returnId));
        return returnId;
    }

    pub fn SetWorking(&self, gatewayId: i64) -> u64 {
        let returnId;
        match *self.workerState.lock().unwrap() {
            WorkerPodState::Working(gatewayId) => {
                unreachable!("WorkerPod::SetWorking gateway {}", gatewayId);
            }
            WorkerPodState::Init => {
                returnId = 0;
            }
            WorkerPodState::Resuming | WorkerPodState::Standby => {
                unreachable!("WorkerPod::SetWorking Resuming");
            }
            WorkerPodState::Idle(id) => {
                returnId = id;
            }
        }
        *self.workerState.lock().unwrap() = WorkerPodState::Working(gatewayId);
        return returnId;
    }

    pub fn New(pod: FuncPod) -> Self {
        let inner = WorkerPodInner {
            pod: pod,
            workerState: Mutex::new(WorkerPodState::Init),
        };
        let ret: WorkerPod = Self(Arc::new(inner));

        error!(
            "New workerpod pod {} {:?}",
            ret.pod.PodKey(),
            ret.pod.object.status.state
        );

        if ret.pod.object.status.state == PodState::Ready {
            ret.SetIdle();
        }

        if ret.pod.object.status.state == PodState::Standby {
            ret.SetState(WorkerPodState::Standby);
        }

        return ret;
    }
}

impl Deref for WorkerPod {
    type Target = Arc<WorkerPodInner>;

    fn deref(&self) -> &Arc<WorkerPodInner> {
        &self.0
    }
}

pub struct TaskQueue {
    pub tx: tokio::sync::mpsc::Sender<SchedTask>,
    pub throttle: TMutex<Pin<Box<dyn Stream<Item = SchedTask> + Send>>>,
}

impl std::fmt::Debug for TaskQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskQueue")
            .field("throttle", &"<stream>") // hide non-Debug field
            .finish()
    }
}

impl Default for TaskQueue {
    fn default() -> Self {
        return Self::New(2000, 50);
    }
}

impl TaskQueue {
    pub fn New(bufsize: usize, throttle: u64) -> Self {
        let (tx, rx) = tokio::sync::mpsc::channel::<SchedTask>(bufsize);
        let stream: ReceiverStream<SchedTask> = tokio_stream::wrappers::ReceiverStream::new(rx);
        let throttled = stream.throttle(std::time::Duration::from_millis(throttle));
        return Self {
            tx: tx,
            throttle: TMutex::new(Box::pin(throttled)),
        };
    }

    pub async fn Next(&self) -> Option<SchedTask> {
        let mut t = self.throttle.lock().await;
        return t.next().await;
    }

    pub fn AddTask(&self, task: SchedTask) {
        self.tx.try_send(task).unwrap();
    }

    pub fn AddSnapshotTask(&self, nodename: &str, funcId: &str) {
        self.AddTask(SchedTask::SnapshotTask(FuncNodePair {
            nodename: nodename.to_owned(),
            funcId: funcId.to_owned(),
        }));
    }

    pub fn AddNode(&self, nodename: &str) {
        self.AddTask(SchedTask::AddNode(nodename.to_owned()));
    }

    pub fn AddFunc(&self, funcId: &str) {
        self.AddTask(SchedTask::AddFunc(funcId.to_owned()));
    }
}

#[derive(Debug, Clone)]
pub enum SchedTask {
    RefreshSnapshot,
    RemoveSnapshotFromNode(RemoveSnapshotFromNode),
    SnapshotTask(FuncNodePair),
    StandbyTask(String),
    AddNode(String),
    AddFunc(String),
}

#[derive(Debug, Clone)]
pub struct FuncNodePair {
    pub funcId: String,
    pub nodename: String,
}

#[derive(Debug, Clone)]
pub struct RemoveSnapshotFromNode {
    pub nodeName: String,
    pub nodeAgentUrl: String,
    pub funckey: String,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum SnapshotScheduleState {
    Init,
    Done,
    Scheduled,
    ScheduleFail(String),
    Cannot(String),
    Waiting(String),
}

impl Default for SnapshotScheduleState {
    fn default() -> Self {
        return Self::Init;
    }
}

impl SnapshotScheduleState {
    pub fn StateName(&self) -> String {
        let s = match self {
            Self::Init => "Init",
            Self::Done => "Done",
            Self::Scheduled => "Scheduled",
            Self::ScheduleFail(_) => "ScheduleFail",
            Self::Cannot(_) => "Cannot",
            Self::Waiting(_) => "Waiting",
        };

        return s.to_owned();
    }

    pub fn Detail(&self) -> String {
        let s = match self {
            Self::Init => "Init",
            Self::Done => "Done",
            Self::Scheduled => "Scheduled",
            Self::ScheduleFail(s) => return s.clone(),
            Self::Cannot(s) => return s.clone(),
            Self::Waiting(s) => return s.clone(),
        };

        return s.to_owned();
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SnapshotScheduleInfoInner {
    pub funcId: String,
    pub nodename: String,
    pub state: SnapshotScheduleState,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnapshotScheduleInfo(Arc<SnapshotScheduleInfoInner>);

impl SnapshotScheduleInfo {
    pub fn New(funcId: &str, nodename: &str, state: SnapshotScheduleState) -> Self {
        let inner = SnapshotScheduleInfoInner {
            funcId: funcId.to_owned(),
            nodename: nodename.to_owned(),
            state: state,
        };

        return Self(Arc::new(inner));
    }
}

#[derive(Debug, Clone, Default)]
pub struct BiIndex<Info: Clone + PartialEq> {
    byId1: BTreeMap<String, BTreeMap<String, Info>>,
    byId2: BTreeMap<String, BTreeMap<String, Info>>,
}

impl<Info: Clone + PartialEq> BiIndex<Info> {
    pub fn New() -> Self {
        Self {
            byId1: BTreeMap::new(),
            byId2: BTreeMap::new(),
        }
    }

    /// Insert (id1, id2, info)
    /// return true: Update, false: No change
    pub fn Set(&mut self, id1: &str, id2: &str, info: Info) -> bool {
        match self.Get12(id1, id2) {
            None => (),
            Some(old) => {
                if old == info {
                    return false;
                }
            }
        }

        self.byId1
            .entry(id1.to_string())
            .or_default()
            .insert(id2.to_string(), info.clone());

        self.byId2
            .entry(id2.to_string())
            .or_default()
            .insert(id1.to_string(), info);

        return true;
    }

    /// Get a clone of info by id1/id2
    pub fn Get12(&self, id1: &str, id2: &str) -> Option<Info> {
        self.byId1.get(id1)?.get(id2).cloned()
    }

    /// Get a clone of info by id2/id1
    pub fn Get21(&self, id2: &str, id1: &str) -> Option<Info> {
        self.byId2.get(id2)?.get(id1).cloned()
    }

    /// Remove a single (id1, id2) pair
    pub fn RemovePair(&mut self, id1: &str, id2: &str) {
        if let Some(map2) = self.byId1.get_mut(id1) {
            map2.remove(id2);
            if map2.is_empty() {
                self.byId1.remove(id1);
            }
        }

        if let Some(map1) = self.byId2.get_mut(id2) {
            map1.remove(id1);
            if map1.is_empty() {
                self.byId2.remove(id2);
            }
        }
    }

    /// Remove all entries by id1
    pub fn RemoveById1(&mut self, id1: &str) {
        if let Some(map2) = self.byId1.remove(id1) {
            for id2 in map2.keys() {
                if let Some(map1) = self.byId2.get_mut(id2) {
                    map1.remove(id1);
                    if map1.is_empty() {
                        self.byId2.remove(id2);
                    }
                }
            }
        }
    }

    /// Remove all entries by id2
    pub fn RemoveById2(&mut self, id2: &str) {
        if let Some(map1) = self.byId2.remove(id2) {
            for id1 in map1.keys() {
                if let Some(map2) = self.byId1.get_mut(id1) {
                    map2.remove(id2);
                    if map2.is_empty() {
                        self.byId1.remove(id1);
                    }
                }
            }
        }
    }

    /// Get all entries for a given id1
    pub fn GetById1(&self, id1: &str) -> Option<BTreeMap<String, Info>> {
        self.byId1.get(id1).map(|m| m.clone())
    }

    /// Get all entries for a given id2
    pub fn GetById2(&self, id2: &str) -> Option<BTreeMap<String, Info>> {
        self.byId2.get(id2).map(|m| m.clone())
    }

    pub fn Len(&self) -> usize {
        self.byId1.values().map(|m| m.len()).sum()
    }

    pub fn IsEmpty(&self) -> bool {
        self.byId1.is_empty()
    }
}
