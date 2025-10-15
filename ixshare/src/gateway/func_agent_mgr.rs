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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use inferxlib::data_obj::ObjRef;
use inferxlib::obj_mgr::funcpolicy_mgr::{FuncPolicy, FuncPolicySpec};
use inferxlib::resource::DEFAULT_PARALLEL_LEVEL;
use once_cell::sync::OnceCell;
use tokio::sync::Semaphore;
use tokio::sync::{mpsc, Notify};
use tokio::sync::{oneshot, Mutex as TMutex};
use tokio::time;

use crate::audit::SqlAudit;
use crate::common::*;
use crate::gateway::scheduler_client::SchedulerClient;
use crate::scheduler::scheduler_handler::GetClient;
use inferxlib::obj_mgr::func_mgr::*;

use super::func_worker::*;
use super::gw_obj_repo::GwObjRepo;

pub static GW_OBJREPO: OnceCell<GwObjRepo> = OnceCell::new();

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

    let namespaceStore = NamespaceStore::New(&GATEWAY_CONFIG.etcdAddrs.to_vec()).await?;

    let addr = GATEWAY_CONFIG.auditdbAddr.clone();
    if addr.len() == 0 {
        // auditdb is not enabled
        return Ok(());
    }

    let sqlaudit = SqlAudit::New(&addr).await?;
    let client = GetClient().await?;

    let objRepo = GwObjRepo::New(GATEWAY_CONFIG.stateSvcAddrs.to_vec())
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

    GW_OBJREPO.set(objRepo.clone()).unwrap();

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
        agent.EnqReq(tenant, namespace, funcname, tx)?;
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

#[derive(Debug, PartialEq)]
pub enum WaitState {
    WaitReq,
    WaitSlot,
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

    pub reqQueue: ClientReqQueue,
    pub slots: Arc<Semaphore>,

    pub reqQueueTx: mpsc::Sender<FuncClientReq>,
    pub workerStateUpdateTx: mpsc::Sender<WorkerUpdate>,
    pub availableSlot: usize,
    pub totalSlot: usize,
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

    pub fn NextReqId(&mut self) -> u64 {
        self.nextReqId += 1;
        return self.nextReqId;
    }

    pub fn TotalSlot(&self) -> usize {
        return self.totalSlot + self.startingSlot;
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
        let reqQueue = ClientReqQueue::New(1000, 30_000);
        let (rtx, rrx) = mpsc::channel(1000);
        let (wtx, wrx) = mpsc::channel(1000);
        let inner = FuncAgentInner {
            closeNotify: Arc::new(Notify::new()),
            stop: AtomicBool::new(false),
            tenant: func.tenant.clone(),
            namespace: func.namespace.clone(),
            funcName: func.name.to_owned(),
            funcVersion: func.Version(),
            func: func.clone(),
            reqQueueTx: rtx,
            workerStateUpdateTx: wtx,
            totalSlot: 0,
            availableSlot: 0,
            startingSlot: 0,
            workers: BTreeMap::new(),
            nextWorkerId: 0,
            nextReqId: 0,

            reqQueue: reqQueue,
            slots: Arc::new(Semaphore::new(0)),
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
    ) -> Result<()> {
        let funcReq = FuncClientReq {
            reqId: self.lock().unwrap().NextReqId(),
            tenant: tenant.to_owned(),
            namespace: namespace.to_owned(),
            funcName: funcname.to_owned(),
            keepalive: true,
            enqueueTime: std::time::Instant::now(),
            tx: tx,
        };
        match self.lock().unwrap().reqQueueTx.try_send(funcReq) {
            Err(_e) => {
                return Err(Error::CommonError("FuncAgent Queue is full".to_owned()));
            }
            Ok(_) => return Ok(()),
        }
    }

    pub async fn Close(&self) {
        let closeNotify = self.lock().unwrap().closeNotify.clone();
        closeNotify.notify_one();
    }

    pub fn FuncKey(&self) -> String {
        let inner = self.lock().unwrap();
        return inner.Key();
    }

    pub async fn AssignReqs(
        &self,
        reqQueue: &ClientReqQueue,
        workers: &Vec<FuncWorker>,
    ) -> WaitState {
        if reqQueue.Count().await == 0 {
            return WaitState::WaitReq;
        }

        if self.lock().unwrap().availableSlot == 0 {
            return WaitState::WaitSlot;
        }
        for worker in workers {
            while worker.AvailableSlot() > 0 {
                let req = match reqQueue.TryRecv().await {
                    None => return WaitState::WaitReq,
                    Some(req) => req,
                };
                // let total = self.lock().unwrap().totalSlot;
                // error!(
                //     "worker.AvailableSlot() {}/{}/{}",
                //     worker.AvailableSlot(),
                //     self.lock().unwrap().availableSlot,
                //     total
                // );
                worker.AssignReq(req);
                self.DecrSlot(1);
            }
        }

        return WaitState::WaitSlot;
    }

    pub async fn NeedNewWorker(&self, reqQueue: &ClientReqQueue) -> bool {
        let reqCnt = reqQueue.Count().await;
        let totalSlotCnt = self.lock().unwrap().TotalSlot();
        if reqCnt as f64 >= 1.5 * totalSlotCnt as f64 {
            return true;
        }

        return false;
    }

    pub async fn Process(
        &self,
        reqQueueRx: mpsc::Receiver<FuncClientReq>,
        workerStateUpdateRx: mpsc::Receiver<WorkerUpdate>,
    ) -> Result<()> {
        let mut reqQueueRx = reqQueueRx;
        let mut workerStateUpdateRx = workerStateUpdateRx;

        let closeNotify = self.lock().unwrap().closeNotify.clone();
        let reqQueue = self.lock().unwrap().reqQueue.clone();
        let throttle = Throttle::New(2, 2, Duration::from_secs(1)).await;

        loop {
            let workers: Vec<FuncWorker> = self.lock().unwrap().workers.values().cloned().collect();
            let state = self.AssignReqs(&reqQueue, &workers).await;
            if state == WaitState::WaitSlot {
                if self.NeedNewWorker(&reqQueue).await {
                    if throttle.TryAcquire().await {
                        match self.NewWorker().await {
                            Ok(()) => {}
                            Err(e) => {
                                error!(
                                    "Func Create New Worker fail {:?} for {}",
                                    e,
                                    self.lock().unwrap().Key()
                                )
                            }
                        }
                    }
                }
            }

            // let key = self.lock().unwrap().Key();
            tokio::select! {
                _ = closeNotify.notified() => {
                    self.lock().unwrap().stop.store(false, Ordering::SeqCst);
                    break;
                }
                workReq = reqQueueRx.recv() => {
                    if let Some(req) = workReq {
                        reqQueue.Send(req).await;
                        // self.ProcessReq(req).await;
                    } else {
                        unreachable!("FuncAgent::Process reqQueueRx closed");
                    }
                }
                stateUpdate = workerStateUpdateRx.recv() => {
                    if let Some(update) = stateUpdate {
                        match update {
                            WorkerUpdate::Ready(worker) => {
                                let slot = worker.ReadySlot();
                                let oldslot = worker.contributeSlot.swap(slot, Ordering::SeqCst);
                                assert!(oldslot == 0);

                                error!("worker ready {}...", worker.WorkerName());

                                worker.SetState(FuncWorkerState::Idle);
                            }
                            WorkerUpdate::RequestDone(_worker) => {
                                // self.IncrSlot(1);
                            }
                            WorkerUpdate::WorkerFail((worker, e)) => {
                                let workerId = worker.workerId.clone();
                                error!("ReturnWorker WorkerUpdate::WorkerFail ... e {:?}", e);
                                worker.ReturnWorker().await.ok();


                                let agentCount = {
                                    let mut agent = self.lock().unwrap();

                                    agent.workers.remove(&workerId);
                                    agent.workers.len()
                                };

                                if agentCount == 0 {
                                    let error = format!("{:?}", e);
                                    loop {
                                        let req = match reqQueue.TryRecv().await {
                                            None => break,
                                            Some(req) => req,
                                        };

                                        req.Send(Err(Error::CommonError(error.clone())));
                                    }
                                }
                            }
                            WorkerUpdate::IdleTimeout(worker) => {
                                // there is race condition there might be new request coming after work idle timeout and before funcagent return the worker
                                // need to check whether the work get new requests.
                                if worker.OngoingReq() == 0 {
                                    let slot = worker.contributeSlot.swap(0, Ordering::SeqCst);
                                    error!("ReturnWorker WorkerUpdate::IdleTimeout 1 ... id {}", worker.workerId);
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

    pub fn ParallelLevel(&self) -> Result<usize> {
        let tenant = self.lock().unwrap().tenant.clone();
        let mut level = self.FuncPolicy(&tenant)?.parallel;
        if level == 0 {
            level = DEFAULT_PARALLEL_LEVEL;
        }

        return Ok(level);
    }

    pub fn FuncPolicy(&self, tenant: &str) -> Result<FuncPolicySpec> {
        match &self.lock().unwrap().func.object.spec.policy {
            ObjRef::Obj(p) => return Ok(p.clone()),
            ObjRef::Link(l) => {
                if l.objType != FuncPolicy::KEY {
                    return Err(Error::CommonError(format!(
                        "GetFuncPolicy for func {} fail invalic link type {}",
                        self.FuncKey(),
                        &l.objType
                    )));
                }

                let obj =
                    GW_OBJREPO
                        .get()
                        .unwrap()
                        .funcpolicyMgr
                        .Get(tenant, &l.namespace, &l.name)?;

                return Ok(obj.object);
            }
        }
    }

    pub async fn NewWorker(&self) -> Result<()> {
        let keepaliveTime = 200; // 200 millisecond self.lock().unwrap().func.spec.keepalivePolicy.keepaliveTime;

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

        let parallelLevel = self.ParallelLevel()?;
        self.lock().unwrap().startingSlot += parallelLevel;
        match FuncWorker::New(
            &workderId,
            &tenant,
            &namespace,
            &funcname,
            fprevision,
            parallelLevel,
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
        // error!(
        //     "IncrSlot cnt {} {} available {}",
        //     l.Key(),
        //     cnt,
        //     l.availableSlot
        // );
        l.availableSlot += cnt;
        return l.availableSlot;
    }

    pub fn DecrSlot(&self, cnt: usize) -> usize {
        let mut l = self.lock().unwrap();
        // error!(
        //     "DecrSlot cnt {} {} available {}",
        //     l.Key(),
        //     cnt,
        //     l.availableSlot
        // );
        l.availableSlot -= cnt;
        return l.availableSlot;
    }
}

#[derive(Debug)]
pub struct ClientReqQueuInner {
    pub queue: TMutex<VecDeque<FuncClientReq>>,
    pub queueLen: usize,
    pub timeout: u64, // ms
    pub notify: Arc<Notify>,
}

#[derive(Debug, Clone)]
pub struct ClientReqQueue(Arc<ClientReqQueuInner>);

impl Deref for ClientReqQueue {
    type Target = Arc<ClientReqQueuInner>;

    fn deref(&self) -> &Arc<ClientReqQueuInner> {
        &self.0
    }
}

impl ClientReqQueue {
    pub fn New(queueLen: usize, timeout: u64) -> Self {
        let inner = ClientReqQueuInner {
            queue: TMutex::new(VecDeque::new()),
            queueLen: queueLen,
            timeout: timeout,
            notify: Arc::new(Notify::new()),
        };

        return Self(Arc::new(inner));
    }

    pub async fn Count(&self) -> usize {
        return self.queue.lock().await.len();
    }

    pub async fn CleanTimeout(&self) {
        let mut q = self.queue.lock().await;
        loop {
            match q.front() {
                None => break,
                Some(first) => {
                    if first.enqueueTime.elapsed().as_millis() as u64 > self.timeout {
                        let item = q.pop_front().unwrap();
                        item.tx.send(Err(Error::Timeout)).ok();
                    } else {
                        break;
                    }
                }
            }
        }
    }

    pub async fn Send(&self, req: FuncClientReq) {
        self.CleanTimeout().await;
        let mut q = self.queue.lock().await;
        if q.len() >= self.queueLen {
            req.tx.send(Err(Error::QueueFull)).unwrap();
            return;
        }

        q.push_back(req);
        if q.len() == 1 {
            self.notify.notify_waiters();
        }
    }

    pub async fn TryRecv(&self) -> Option<FuncClientReq> {
        self.CleanTimeout().await;
        return self.queue.lock().await.pop_front();
    }

    pub async fn Recv(&self) -> FuncClientReq {
        self.CleanTimeout().await;
        loop {
            match self.queue.lock().await.pop_front() {
                Some(r) => return r,
                None => (),
            }
            self.notify.notified().await;
        }
    }

    pub async fn Wait(&self) {
        self.CleanTimeout().await;
        loop {
            {
                let q = self.queue.lock().await;
                if q.len() > 0 {
                    return;
                }
            }

            self.notify.notified().await;
        }
    }
}

#[derive(Debug, Default)]
pub struct ProcessSlotInner {
    pub slots: AtomicUsize,
    pub notify: Notify,
}

#[derive(Debug, Default, Clone)]
pub struct ProcessSlot(Arc<ProcessSlotInner>);

impl Deref for ProcessSlot {
    type Target = Arc<ProcessSlotInner>;

    fn deref(&self) -> &Arc<ProcessSlotInner> {
        &self.0
    }
}

impl ProcessSlot {
    pub fn Dec(&self) {
        self.DecBy(1);
    }

    pub fn Inc(&self) {
        self.IncBy(1);
    }

    pub fn IncBy(&self, count: usize) {
        self.slots.fetch_add(count, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn DecBy(&self, count: usize) {
        self.slots.fetch_sub(count, Ordering::SeqCst);
    }

    pub fn Count(&self) -> usize {
        return self.slots.load(Ordering::SeqCst);
    }

    pub async fn Wait(&self) {
        self.notify.notified().await;
    }
}

#[derive(Debug)]
pub struct ThrottleInner {
    tokens: TMutex<usize>,
    capacity: usize,
    refill_interval: Duration,
    notify: Notify,
}

#[derive(Debug, Clone)]
pub struct Throttle(Arc<ThrottleInner>);

impl Deref for Throttle {
    type Target = Arc<ThrottleInner>;

    fn deref(&self) -> &Arc<ThrottleInner> {
        &self.0
    }
}

impl Throttle {
    pub async fn New(initial: usize, capacity: usize, refill_interval: Duration) -> Self {
        let throttle = Self(Arc::new(ThrottleInner {
            tokens: TMutex::new(initial.min(capacity)),
            capacity,
            refill_interval,
            notify: Notify::new(),
        }));

        // Start background refill task
        Self::Refill(throttle.clone()).await;
        throttle
    }

    async fn Refill(throttle: Self) {
        tokio::spawn(async move {
            let mut ticker = time::interval(throttle.refill_interval);
            loop {
                ticker.tick().await;
                let mut tokens = throttle.tokens.lock().await;
                if *tokens < throttle.capacity {
                    *tokens += 1;
                    throttle.notify.notify_one();
                }
            }
        });
    }

    /// Acquire a token (wait if none available)
    pub async fn Acquire(&self) {
        loop {
            {
                let mut tokens = self.tokens.lock().await;
                if *tokens > 0 {
                    *tokens -= 1;
                    return;
                }
            }
            self.notify.notified().await;
        }
    }

    /// Try to acquire a token immediately (non-blocking)
    pub async fn TryAcquire(&self) -> bool {
        let mut tokens = self.tokens.lock().await;
        if *tokens > 0 {
            *tokens -= 1;
            true
        } else {
            false
        }
    }
}
