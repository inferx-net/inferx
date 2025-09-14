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

use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::sync::Mutex;

use inferxlib::node::WorkerPodState;
use inferxlib::obj_mgr::pod_mgr::FuncPod;
use inferxlib::obj_mgr::pod_mgr::PodState;
use std::ops::Deref;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::sync::Notify;

use crate::common::*;
use crate::metastore::informer::EventHandler;
use crate::metastore::store::ThreadSafeStore;
use crate::na;
use crate::node_config::SchedulerConfig;
use crate::node_config::NODE_CONFIG;
use crate::scheduler::sched_obj_repo::SchedObjRepo;
use crate::scheduler::scheduler_register::SchedulerRegister;
use crate::scheduler::scheduler_svc::RunSchedulerSvc;
use inferxlib::data_obj::DeltaEvent;

use super::scheduler_handler::SchedulerHandler;
use super::scheduler_handler::WorkerHandlerMsg;

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

pub async fn ExecSchedulerSvc() -> Result<()> {
    let objRepo = SchedObjRepo::New(SCHEDULER_CONFIG.stateSvcAddrs.clone()).await?;

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
pub struct WorkerPod(Arc<WorkerPodInner>);

impl WorkerPod {
    pub fn State(&self) -> WorkerPodState {
        return *self.workerState.lock().unwrap();
    }

    pub fn SetState(&self, state: WorkerPodState) {
        *self.workerState.lock().unwrap() = state;
    }

    pub fn SetIdle(&self) -> u64 {
        let returnId = IDLE_POD_SEQNUM.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *self.workerState.lock().unwrap() = WorkerPodState::Idle(returnId);
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
            WorkerPodState::Idle(id) => {
                returnId = id;
            }
        }
        *self.workerState.lock().unwrap() = WorkerPodState::Working(gatewayId);
        return returnId;
    }
}

impl Deref for WorkerPod {
    type Target = Arc<WorkerPodInner>;

    fn deref(&self) -> &Arc<WorkerPodInner> {
        &self.0
    }
}

impl From<FuncPod> for WorkerPod {
    fn from(item: FuncPod) -> Self {
        let inner = WorkerPodInner {
            pod: item,
            workerState: Mutex::new(WorkerPodState::Init),
        };
        let ret: WorkerPod = Self(Arc::new(inner));

        if ret.pod.object.status.state == PodState::Ready
            || ret.pod.object.status.state == PodState::Standby
        {
            ret.SetIdle();
        }

        return ret;
    }
}
