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
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;
use tokio::sync::{mpsc, Notify};

use crate::audit::SqlAudit;
use crate::common::*;
use crate::gateway::scheduler_client::SchedulerClient;
use crate::node_config::NODE_CONFIG;
use crate::peer_mgr::NA_CONFIG;
use crate::scheduler::scheduler_handler::GetClient;
use inferxlib::obj_mgr::func_mgr::*;

use super::func_worker::*;
use super::gw_obj_repo::GwObjRepo;

pub async fn GatewaySvc(notify: Option<Arc<Notify>>) -> Result<()> {
    use crate::gateway::func_agent_mgr::FuncAgentMgr;
    use crate::gateway::gw_obj_repo::{GwObjRepo, NamespaceStore};
    use crate::gateway::http_gateway::*;

    match notify {
        Some(n) => {
            n.notified().await;
        }
        None => (),
    }

    let namespaceStore = NamespaceStore::New(&NA_CONFIG.etcdAddrs.to_vec()).await?;

    let addr = NODE_CONFIG.auditdbAddr.clone();
    if addr.len() == 0 {
        // auditdb is not enabled
        return Ok(());
    }

    let sqlaudit = SqlAudit::New(&addr).await?;
    let client = GetClient().await?;

    let objRepo = GwObjRepo::New(NA_CONFIG.stateSvcAddrs.to_vec())
        .await
        .unwrap();

    let funcAgentMgr = FuncAgentMgr::New(&objRepo);
    objRepo.SetFuncAgentMgr(&funcAgentMgr);

    let gateway = HttpGateway {
        objRepo: objRepo.clone(),
        funcAgentMgr: funcAgentMgr,
        namespaceStore: namespaceStore,
        sqlAudit: sqlaudit,
        client: client,
    };

    let handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(2000));
        let schedulerClient = SchedulerClient {};
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    schedulerClient.RefreshGateway().await.ok();
                }
            }
        }
    });

    tokio::select! {
        res = gateway.HttpServe() => {
           error!("HttpServe finish with res {:?}", &res);
        }
        res = objRepo.Process() => {
            error!("objRepo finish with res {:?}", &res);
        }
        _ = handle => {
            error!("refresh gateway fail ...");
        }
    }

    return Ok(());
}

#[derive(Debug)]
pub struct FuncAgentMgrInner {
    pub agents: Mutex<BTreeMap<String, FuncAgent>>,
    pub objRepo: GwObjRepo,
}

#[derive(Debug, Clone)]
pub struct FuncAgentMgr(Arc<FuncAgentMgrInner>);

impl Deref for FuncAgentMgr {
    type Target = Arc<FuncAgentMgrInner>;

    fn deref(&self) -> &Arc<FuncAgentMgrInner> {
        &self.0
    }
}

impl FuncAgentMgr {
    pub fn New(objRepo: &GwObjRepo) -> Self {
        let inner = FuncAgentMgrInner {
            agents: Mutex::new(BTreeMap::new()),
            objRepo: objRepo.clone(),
        };

        return Self(Arc::new(inner));
    }

    pub async fn GetClient(
        &self,
        tenant: &str,
        namespace: &str,
        funcname: &str,
    ) -> Result<(QHttpCallClient, bool, Function)> {
        let func = self.objRepo.GetFunc(tenant, namespace, funcname)?;
        let agent = {
            let funcId = func.Id();
            let mut inner = self.agents.lock().unwrap();
            match inner.get(&funcId) {
                Some(agent) => agent.clone(),
                None => {
                    let agent = FuncAgent::New(&func);
                    inner.insert(funcId, agent.clone());
                    agent
                }
            }
        };
        let (tx, rx) = oneshot::channel();
        agent.EnqReq(tenant, namespace, funcname, tx);
        match rx.await {
            Err(_) => {
                return Err(Error::CommonError(format!("funcworker fail ...")));
            }
            Ok(res) => match res {
                Err(e) => {
                    return Err(e);
                }
                Ok((client, keepalive)) => {
                    return Ok((client, keepalive, func));
                }
            },
        };
    }
}

#[derive(Debug)]
pub enum WorkerState {
    Creating,
    Working,
    Idle,
    Evicating,
    Fail,
    Killing,
}

#[derive(Debug)]
pub enum WorkerUpdate {
    Ready(FuncWorker), // parallel level
    WorkerFail((FuncWorker, Error)),
    RequestDone(FuncWorker),
    IdleTimeout(FuncWorker),
}

#[derive(Debug)]
pub struct FuncAgentInner {
    pub closeNotify: Arc<Notify>,
    pub stop: AtomicBool,

    pub tenant: String,
    pub namespace: String,
    pub funcName: String,
    pub func: Function,
    pub funcVersion: i64,

    pub waitingReqs: VecDeque<FuncClientReq>,
    pub reqQueueTx: mpsc::Sender<FuncClientReq>,
    pub workerStateUpdateTx: mpsc::Sender<WorkerUpdate>,
    pub availableSlot: usize,
    pub startingSlot: usize,
    pub workers: BTreeMap<String, FuncWorker>,
    pub nextWorkerId: u64,
    pub nextReqId: u64,
}

impl FuncAgentInner {
    pub fn AvailableWorker(&self) -> Option<FuncWorker> {
        for (_, worker) in &self.workers {
            if worker.AvailableSlot() > 0 {
                return Some(worker.clone());
            }
        }

        return None;
    }

    pub fn Key(&self) -> String {
        return format!("{}/{}/{}", &self.tenant, &self.namespace, &self.funcName);
    }

    pub fn RemoveWorker(&mut self, worker: &FuncWorker) -> Result<()> {
        self.workers.remove(&worker.workerId);
        return Ok(());
    }

    pub fn NextWorkerId(&mut self) -> u64 {
        self.nextWorkerId += 1;
        return self.nextWorkerId;
    }

    pub fn AssignReq(&mut self, req: FuncClientReq) {
        for (_, worker) in &self.workers {
            if worker.AvailableSlot() > 0 {
                worker.AssignReq(req);
                self.availableSlot -= 1;
                return;
            }
        }

        panic!("funcagentmgr fail to assign req {:?}", &req);
    }

    pub fn NextReqId(&mut self) -> u64 {
        self.nextReqId += 1;
        return self.nextReqId;
    }
}

#[derive(Debug, Clone)]
pub struct FuncAgent(Arc<Mutex<FuncAgentInner>>);

impl Deref for FuncAgent {
    type Target = Arc<Mutex<FuncAgentInner>>;

    fn deref(&self) -> &Arc<Mutex<FuncAgentInner>> {
        &self.0
    }
}

impl FuncAgent {
    pub fn New(func: &Function) -> Self {
        let (rtx, rrx) = mpsc::channel(30);
        let (wtx, wrx) = mpsc::channel(30);
        let inner = FuncAgentInner {
            closeNotify: Arc::new(Notify::new()),
            stop: AtomicBool::new(false),
            tenant: func.tenant.clone(),
            namespace: func.namespace.clone(),
            funcName: func.name.to_owned(),
            funcVersion: func.Version(),
            func: func.clone(),
            waitingReqs: VecDeque::new(),
            reqQueueTx: rtx,
            workerStateUpdateTx: wtx,
            availableSlot: 0,
            startingSlot: 0,
            workers: BTreeMap::new(),
            nextWorkerId: 0,
            nextReqId: 0,
        };

        let ret = Self(Arc::new(Mutex::new(inner)));

        let clone = ret.clone();
        tokio::spawn(async move {
            clone.Process(rrx, wrx).await.unwrap();
        });

        return ret;
    }

    pub fn EnqReq(
        &self,
        tenant: &str,
        namespace: &str,
        funcname: &str,
        tx: oneshot::Sender<Result<(QHttpCallClient, bool)>>,
    ) {
        let funcReq = FuncClientReq {
            reqId: self.lock().unwrap().NextReqId(),
            tenant: tenant.to_owned(),
            namespace: namespace.to_owned(),
            funcName: funcname.to_owned(),
            keepalive: true,
            tx: tx,
        };
        self.lock().unwrap().reqQueueTx.try_send(funcReq).unwrap();
    }

    pub async fn Close(&self) {
        let closeNotify = self.lock().unwrap().closeNotify.clone();
        closeNotify.notify_one();
    }

    pub fn FuncKey(&self) -> String {
        let inner = self.lock().unwrap();
        return inner.Key();
    }

    pub async fn Process(
        &self,
        reqQueueRx: mpsc::Receiver<FuncClientReq>,
        workerStateUpdateRx: mpsc::Receiver<WorkerUpdate>,
    ) -> Result<()> {
        let mut reqQueueRx = reqQueueRx;
        let mut workerStateUpdateRx = workerStateUpdateRx;

        let closeNotify = self.lock().unwrap().closeNotify.clone();

        loop {
            // let key = self.lock().unwrap().Key();
            tokio::select! {
                _ = closeNotify.notified() => {
                    self.lock().unwrap().stop.store(false, Ordering::SeqCst);
                    break;
                }
                workReq = reqQueueRx.recv() => {
                    if let Some(req) = workReq {
                        self.ProcessReq(req).await;
                    } else {
                        unreachable!("FuncAgent::Process reqQueueRx closed");
                    }
                }
                stateUpdate = workerStateUpdateRx.recv() => {
                    if let Some(update) = stateUpdate {
                        match update {
                            WorkerUpdate::Ready(worker) => {
                                let slot = worker.AvailableSlot();
                                let oldslot = worker.contributeSlot.swap(slot, Ordering::SeqCst);
                                assert!(oldslot == 0);
                                self.IncrSlot(slot);
                                self.TryProcessOneReq();
                            }
                            WorkerUpdate::RequestDone(_worker) => {
                                self.IncrSlot(1);
                                self.TryProcessOneReq();
                            }
                            WorkerUpdate::WorkerFail((worker, e)) => {
                                let slot = worker.AvailableSlot();
                                self.DecrSlot(slot);
                                let workerId = worker.workerId.clone();
                                worker.ReturnWorker().await.ok();


                                let funckey = self.FuncKey();

                                loop {
                                    match reqQueueRx.try_recv() {
                                        Ok(req) => {
                                            match &e {
                                                Error::SchedulerErr(s) => {
                                                    req.Send(Err(Error::SchedulerErr(s.clone())));
                                                }
                                                e => {
                                                    let err = Err(Error::CommonError(format!(
                                                            "fail to run func {:?} with error {:?}", &funckey, e
                                                        )));
                                                    req.Send(err);
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            error!("WorkerUpdate::WorkerFail fail3 {:?}", e);

                                            break;
                                        }
                                    }
                                }

                                loop {
                                    match self.lock().unwrap().waitingReqs.pop_back() {
                                        Some(req) => {
                                            match &e {
                                                Error::SchedulerErr(s) => {
                                                    req.Send(Err(Error::SchedulerErr(s.clone())));
                                                }
                                                e => {
                                                    let err = Err(Error::CommonError(format!(
                                                            "fail to run func {:?} with error {:?}", &funckey, e
                                                        )));
                                                    req.Send(err);
                                                }
                                            }
                                        }
                                        None => {
                                            break;
                                        }
                                    }
                                }

                                self.lock().unwrap().workers.remove(&workerId);
                            }
                            WorkerUpdate::IdleTimeout(worker) => {
                                // there is race condition there might be new request coming after work idle timeout and before funcagent return the worker
                                // need to check whether the work get new requests.
                                if worker.OngoingReq() == 0 {
                                    let slot = worker.contributeSlot.swap(0, Ordering::SeqCst);
                                    let _ = self.DecrSlot(slot);
                                    worker.Close().await;
                                    worker.ReturnWorker().await.ok();
                                    let workerId = worker.workerId.clone();
                                    self.lock().unwrap().workers.remove(&workerId);
                                }
                            }
                        }
                    } else {
                        unreachable!("FuncAgent::Process reqQueueRx closed");
                    }
                }
            }
        }

        return Ok(());
    }

    pub async fn NewWorker(&self) -> Result<()> {
        let keepaliveTime = 10; // 10 millisecond self.lock().unwrap().func.spec.keepalivePolicy.keepaliveTime;

        let workerId = self.lock().unwrap().NextWorkerId();
        let workderId = format!("{}", workerId);

        let tenant;
        let namespace;
        let funcname;
        let fprevision;
        let endpoint;
        {
            let inner = self.lock().unwrap();

            tenant = inner.tenant.clone();
            namespace = inner.namespace.clone();
            funcname = inner.funcName.clone();
            fprevision = inner.funcVersion;
            endpoint = inner.func.object.spec.endpoint.clone();
        }

        self.lock().unwrap().startingSlot += DEFAULT_PARALLEL_LEVEL;
        match FuncWorker::New(
            &workderId,
            &tenant,
            &namespace,
            &funcname,
            fprevision,
            DEFAULT_PARALLEL_LEVEL,
            keepaliveTime,
            endpoint,
            self,
        )
        .await
        {
            Err(e) => {
                error!(
                    "FuncAgent::ProcessReq new funcworker fail with error {:?}",
                    e
                );
                return Err(e);
            }
            Ok(worker) => {
                self.lock()
                    .unwrap()
                    .workers
                    .insert(workderId, worker.clone());

                return Ok(());
            }
        };
    }

    pub fn SendWorkerStatusUpdate(&self, update: WorkerUpdate) {
        let statusUpdateTx = self.lock().unwrap().workerStateUpdateTx.clone();
        statusUpdateTx.try_send(update).unwrap();
    }

    pub fn IncrSlot(&self, cnt: usize) -> usize {
        let mut l = self.lock().unwrap();
        l.availableSlot += cnt;
        return l.availableSlot;
    }

    pub fn DecrSlot(&self, cnt: usize) -> usize {
        let mut l = self.lock().unwrap();
        l.availableSlot -= cnt;
        return l.availableSlot;
    }

    pub fn TryProcessOneReq(&self) {
        let mut inner = self.lock().unwrap();
        if inner.availableSlot == 0 {
            return;
        }

        match inner.waitingReqs.pop_front() {
            None => {
                return;
            }
            Some(req) => {
                for (_, worker) in &inner.workers {
                    if worker.AvailableSlot() > 0 {
                        worker.AssignReq(req);
                        inner.availableSlot -= 1;
                        break;
                    }
                }
            }
        }
    }

    pub async fn ProcessReq(&self, req: FuncClientReq) {
        if self.lock().unwrap().availableSlot == 0 {
            let mut needNewWorker = false;
            {
                let mut inner = self.lock().unwrap();
                inner.waitingReqs.push_back(req);

                if inner.waitingReqs.len() > inner.startingSlot {
                    needNewWorker = true;
                }
            }

            if needNewWorker {
                match self.NewWorker().await {
                    Ok(()) => {}
                    Err(e) => {
                        let req = self.lock().unwrap().waitingReqs.pop_front();

                        if let Some(req) = req {
                            match req.tx.send(Err(e)) {
                                Err(e) => error!("send response to client fail with error {:?}", e),
                                Ok(_) => (),
                            }
                        }
                    }
                }
            }
        } else {
            self.lock().unwrap().AssignReq(req);
        }
    }
}
