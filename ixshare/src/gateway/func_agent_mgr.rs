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
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use inferxlib::data_obj::ObjRef;
use inferxlib::obj_mgr::funcpolicy_mgr::{FuncPolicy, FuncPolicySpec, ScaleOutPolicy};
use once_cell::sync::OnceCell;
use serde_json::json;
use tokio::sync::{mpsc, Notify};
use tokio::sync::{oneshot, Mutex as TMutex};
use tokio::time;

use crate::audit::SqlAudit;
use crate::common::*;
use crate::gateway::scheduler_client::{LeasedWorker, SCHEDULER_CLIENT};
use crate::scheduler::scheduler_handler::GetClient;
use inferxlib::obj_mgr::func_mgr::*;

use super::func_worker::*;
use super::gw_obj_repo::GwObjRepo;

pub static GW_OBJREPO: OnceCell<GwObjRepo> = OnceCell::new();

static TIMESTAMP_COUNT: AtomicU64 = AtomicU64::new(0);
lazy_static::lazy_static! {
    pub static ref TIMESTAMP_START: std::time::Instant = std::time::Instant::now();
}

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

    let auditdbAddr = GATEWAY_CONFIG.auditdbAddr.clone();
    if auditdbAddr.len() == 0 {
        // auditdb is not enabled
        return Ok(());
    }

    let billingdbAddr = GATEWAY_CONFIG.billingdbAddr.clone();
    if billingdbAddr.len() == 0 {
        return Err(Error::CommonError(
            "GatewaySvc: billingdb address is not configured".to_string(),
        ));
    }

    let sqlaudit = SqlAudit::New(&auditdbAddr).await?;
    let sqlbilling = SqlAudit::New(&billingdbAddr).await?;
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
        sqlBilling: sqlbilling,
        client: client,
    };

    GW_OBJREPO.set(objRepo.clone()).unwrap();

    let handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(2000));
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    tokio::select! {
                        _ = SCHEDULER_CLIENT.RefreshGateway() => (),
                        _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
                    }
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
        func: &Function,
        timeout: u64,
        timestamp: IxTimestamp,
    ) -> Result<(QHttpCallClient, bool)> {
        let agent = {
            let funcId = func.Id();
            let mut inner = self.agents.lock().unwrap();
            match inner.get(&funcId) {
                Some(agent) => agent.clone(),
                None => {
                    let agent = FuncAgent::New(func);
                    inner.insert(funcId, agent.clone());
                    agent
                }
            }
        };
        let (tx, rx) = oneshot::channel();
        agent.EnqReq(tenant, namespace, funcname, timestamp, timeout, tx)?;
        match rx.await {
            Err(e) => {
                return Err(Error::CommonError(format!(
                    "funcworker fail ... {} {:?}",
                    func.Id(),
                    e
                )));
            }
            Ok(res) => match res {
                Err(e) => {
                    return Err(e);
                }
                Ok((client, keepalive)) => {
                    return Ok((client, keepalive));
                }
            },
        };
    }

    pub async fn DebugInfo(&self) -> serde_json::Value {
        let agents: Vec<FuncAgent> = {
            let lock = self.agents.lock().unwrap();
            lock.values().cloned().collect()
        };

        let mut infos = Vec::new();
        for agent in agents {
            infos.push(agent.DebugInfo().await);
        }

        json!({ "funcs": infos })
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
    WorkerLeaseFail((FuncWorker, Error)),
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

    pub scaleoutPolicy: Mutex<ScaleOutPolicy>,
    pub scaleinTimeout: AtomicU64,
    pub parallelLeve: AtomicUsize,

    pub reqQueue: ClientReqQueue,
    pub dataNotify: Arc<Notify>,

    pub reqQueueTx: mpsc::Sender<FuncClientReq>,
    pub workerStateUpdateTx: mpsc::Sender<WorkerUpdate>,

    pub totalSlot: Arc<AtomicUsize>,
    pub startingSlot: Arc<AtomicUsize>,
    pub activeReqCnt: AtomicUsize,

    pub workers: Mutex<BTreeMap<isize, FuncWorker>>,
    pub nextWorkerId: AtomicIsize,
    pub scaleInWorkerId: AtomicIsize,
}

impl FuncAgentInner {
    pub fn Key(&self) -> String {
        return format!("{}/{}/{}", &self.tenant, &self.namespace, &self.funcName);
    }

    pub fn AddWorker(&self, worker: &FuncWorker) {
        self.workers
            .lock()
            .unwrap()
            .insert(worker.WorkerId(), worker.clone());

        self.scaleInWorkerId
            .store(self.ScaleInWorkerId(), Ordering::SeqCst);
    }

    pub fn RemoveWorker(&self, worker: &FuncWorker) {
        self.workers.lock().unwrap().remove(&worker.WorkerId());
        self.scaleInWorkerId
            .store(self.ScaleInWorkerId(), Ordering::SeqCst);
    }

    pub fn NextWorkerId(&self) -> isize {
        return self.nextWorkerId.fetch_add(1, Ordering::SeqCst) + 1;
    }

    pub fn ScaleInWorkerId(&self) -> isize {
        let lock = self.workers.lock().unwrap();
        if lock.len() <= 1 {
            return -1;
        }

        if let Some(workerId) = lock.keys().next() {
            return *workerId;
        } else {
            return -1;
        }
    }
}

#[derive(Debug, Clone)]
pub struct FuncAgent(Arc<FuncAgentInner>);

impl Deref for FuncAgent {
    type Target = Arc<FuncAgentInner>;

    fn deref(&self) -> &Arc<FuncAgentInner> {
        &self.0
    }
}

impl FuncAgent {
    pub fn New(func: &Function) -> Self {
        let policy = GW_OBJREPO.get().unwrap().FuncPolicy(func);

        let queueLen = policy.queueLen;

        let reqQueue = ClientReqQueue::New(queueLen);
        let (rtx, rrx) = mpsc::channel(1000);
        let (wtx, wrx) = mpsc::channel(100);
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
            totalSlot: Arc::new(AtomicUsize::new(0)),
            startingSlot: Arc::new(AtomicUsize::new(0)),
            activeReqCnt: AtomicUsize::new(0),
            workers: Mutex::new(BTreeMap::new()),
            nextWorkerId: AtomicIsize::new(0),
            scaleInWorkerId: AtomicIsize::new(-1),

            reqQueue: reqQueue,
            dataNotify: Arc::new(Notify::new()),

            scaleoutPolicy: Mutex::new(policy.scaleoutPolicy),
            scaleinTimeout: AtomicU64::new((policy.scaleinTimeout * 1000.0) as u64),
            parallelLeve: AtomicUsize::new(policy.parallel),
        };

        let ret = Self(Arc::new(inner));

        let clone = ret.clone();
        tokio::spawn(async move {
            clone.Process(rrx, wrx).await.unwrap();
        });

        return ret;
    }

    pub fn TotalSlot(&self) -> usize {
        // let totalSlot = self.totalSlot.load(Ordering::SeqCst);
        // return totalSlot + self.startingSlot.load(Ordering::SeqCst);
        return self.workers.lock().unwrap().len() * self.ParallelLevel();
    }

    pub fn UpdatePolicy(&self) {
        let policy = GW_OBJREPO.get().unwrap().FuncPolicy(&self.func);

        *self.scaleoutPolicy.lock().unwrap() = policy.scaleoutPolicy;
        self.scaleinTimeout
            .store((policy.scaleinTimeout * 1000.0) as u64, Ordering::Relaxed);
        self.parallelLeve.store(policy.parallel, Ordering::Relaxed);
    }

    pub fn EnqReq(
        &self,
        tenant: &str,
        namespace: &str,
        funcname: &str,
        timestamp: IxTimestamp,
        timeout: u64,
        tx: oneshot::Sender<Result<(QHttpCallClient, bool)>>,
    ) -> Result<()> {
        let funcReq = FuncClientReq {
            tenant: tenant.to_owned(),
            namespace: namespace.to_owned(),
            funcName: funcname.to_owned(),
            keepalive: true,
            timeout: timeout,
            enqueueTime: timestamp,
            tx: tx,
        };
        match self.reqQueueTx.try_send(funcReq) {
            Err(_e) => {
                return Err(Error::CommonError("FuncAgent Queue is full".to_owned()));
            }
            Ok(_) => return Ok(()),
        }
    }

    pub async fn Close(&self) {
        let closeNotify = self.closeNotify.clone();
        closeNotify.notify_one();
    }

    pub fn FuncKey(&self) -> String {
        return self.Key();
    }

    pub async fn DebugInfo(&self) -> serde_json::Value {
        let workers: Vec<FuncWorker> = {
            let lock = self.workers.lock().unwrap();
            lock.values().cloned().collect()
        };

        let mut worker_infos = Vec::new();
        for worker in workers {
            worker_infos.push(worker.DebugInfo());
        }

        let wait_cnt = self.reqQueue.Count().await;

        json!({
            "funcKey": self.FuncKey(),
            "activeReqCnt": self.activeReqCnt.load(Ordering::Relaxed),
            "waitReqCnt": wait_cnt,
            "parallelLevel": self.ParallelLevel(),
            "totalSlot": self.totalSlot.load(Ordering::Relaxed),
            "startingSlot": self.startingSlot.load(Ordering::Relaxed),
            "scaleInWorkerId": self.scaleInWorkerId.load(Ordering::Relaxed),
            "nextWorkerId": self.nextWorkerId.load(Ordering::Relaxed),
            "workers": worker_infos,
        })
    }

    pub async fn NeedNewWorker(&self) -> bool {
        let reqCnt = self.activeReqCnt.load(Ordering::Relaxed);
        let totalSlotCnt = self.TotalSlot();
        let waitReqcnt = self.reqQueue.Count().await;
        match &*self.scaleoutPolicy.lock().unwrap() {
            ScaleOutPolicy::WaitQueueRatio(ratio) => {
                if reqCnt as f64 > (1.0 + ratio.waitRatio) * totalSlotCnt as f64 {
                    trace!(
                        "NeedNewWorker the reqcnt is {}, totalSlotCnt {} reqcnt {}",
                        reqCnt,
                        totalSlotCnt,
                        waitReqcnt
                    );

                    return true;
                }
            }
        }

        return false;
    }

    pub fn NeedLastWorker(&self) -> bool {
        let reqCnt = self.activeReqCnt.load(Ordering::Relaxed);
        let totalSlotCnt = self.TotalSlot() - self.ParallelLevel();
        return reqCnt > totalSlotCnt;
    }

    pub fn ActiveReqCnt(&self) -> usize {
        return self.activeReqCnt.load(Ordering::Relaxed);
    }

    pub async fn WaitReqCnt(&self) -> usize {
        return self.reqQueue.Count().await;
    }

    pub async fn Process(
        &self,
        reqQueueRx: mpsc::Receiver<FuncClientReq>,
        workerStateUpdateRx: mpsc::Receiver<WorkerUpdate>,
    ) -> Result<()> {
        let mut reqQueueRx = reqQueueRx;
        let mut workerStateUpdateRx = workerStateUpdateRx;

        let closeNotify = self.closeNotify.clone();
        let reqQueue = self.reqQueue.clone();
        let throttle = Throttle::New(2, 2, Duration::from_secs(1)).await;
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(2000));

        loop {
            let timeoutReqCnt = reqQueue.CleanTimeout().await;
            self.activeReqCnt.fetch_sub(timeoutReqCnt, Ordering::SeqCst);
            if self.NeedNewWorker().await {
                if throttle.TryAcquire().await {
                    trace!(
                        "create new worker {} activereqcnt {} waitreqcnt {}",
                        self.FuncKey(),
                        self.ActiveReqCnt(),
                        self.WaitReqCnt().await
                    );
                    match self.NewWorker().await {
                        Ok(()) => {}
                        Err(e) => {
                            error!("Func Create New Worker fail {:?} for {}", e, self.Key())
                        }
                    }
                }
            }

            // let key = self.lock().unwrap().Key();
            tokio::select! {
                _ = closeNotify.notified() => {
                    self.stop.store(false, Ordering::SeqCst);
                    break;
                }
                workReq = reqQueueRx.recv() => {
                    if let Some(req) = workReq {
                        if reqQueue.Send(req).await {
                            self.activeReqCnt.fetch_add(1, Ordering::SeqCst);
                        }
                        // self.ProcessReq(req).await;
                    } else {
                        unreachable!("FuncAgent::Process reqQueueRx closed");
                    }

                    loop {
                        match reqQueueRx.try_recv() {
                            Err(_) => break,
                            Ok(req) => {
                                if reqQueue.Send(req).await {
                                    self.activeReqCnt.fetch_add(1, Ordering::SeqCst);
                                }
                            }
                        }
                    }
                }
                _ = interval.tick() => {
                    // keepalive for each 500 ms
                    self.UpdatePolicy();
                }
                // trigger timeout checkout
                _ = time::sleep(Duration::from_millis(10)) => {}
                stateUpdate = workerStateUpdateRx.recv() => {
                    if let Some(update) = stateUpdate {
                        match update {
                            WorkerUpdate::Ready(_worker) => {
                                // worker.SetState(FuncWorkerState::Processing);
                            }
                            WorkerUpdate::WorkerFail((worker, e)) => {
                                // Remove from local tracking immediately to prevent reuse
                                self.RemoveWorker(&worker);

                                // Spawn background retry to return worker to scheduler
                                Self::spawn_return_worker_retry(worker.clone(), true);

                                trace!("Spawned background retry for WorkerFail: {}/{:?}/{:?}", worker.WorkerName(), worker.id, e);
                            }
                            WorkerUpdate::IdleTimeout(worker) => {
                                // Remove from local tracking immediately to prevent reuse
                                self.RemoveWorker(&worker);

                                // Spawn background retry to return worker to scheduler
                                Self::spawn_return_worker_retry(worker.clone(), false);

                                trace!("Spawned background retry for IdleTimeout: {}/{:?}", worker.WorkerName(), worker.id);
                            }
                            WorkerUpdate::WorkerLeaseFail((worker, e)) => {
                                error!("Worker lease fail, worker: {}, error: {:?}", worker.WorkerName(), e);
                                self.RemoveWorker(&worker);
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

    pub fn ParallelLevel(&self) -> usize {
        return self.parallelLeve.load(Ordering::Relaxed);
    }

    pub fn FuncPolicy(&self, tenant: &str) -> Result<FuncPolicySpec> {
        // if there is funcpolicy with same name, will override the current one
        match GW_OBJREPO.get().unwrap().funcpolicyMgr.Get(
            &self.func.tenant,
            &self.func.namespace,
            &self.func.name,
        ) {
            Err(_) => (),
            Ok(p) => return Ok(p.object),
        }

        match &self.func.object.spec.policy {
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
                    match GW_OBJREPO
                        .get()
                        .unwrap()
                        .funcpolicyMgr
                        .Get(tenant, &l.namespace, &l.name)
                    {
                        Err(e) => {
                            error!(
                                "can't get funcPolicy {}/{}/{}, fallback to default error {:?}",
                                tenant, &l.namespace, &l.name, e
                            );
                            FuncPolicySpec::default()
                        }
                        Ok(p) => p.object,
                    };

                return Ok(obj);
            }
        }
    }

    pub async fn NewWorker(&self) -> Result<()> {
        let keepaliveTime = self.scaleinTimeout.load(Ordering::Relaxed);

        let workerId = self.NextWorkerId();

        let tenant;
        let namespace;
        let funcname;
        let fprevision;
        let endpoint;
        {
            tenant = self.tenant.clone();
            namespace = self.namespace.clone();
            funcname = self.funcName.clone();
            fprevision = self.funcVersion;
            endpoint = self.func.object.spec.endpoint.clone();
        }

        let parallelLevel = self.ParallelLevel();
        self.startingSlot.fetch_add(parallelLevel, Ordering::SeqCst);
        match FuncWorker::New(
            workerId,
            &tenant,
            &namespace,
            &funcname,
            fprevision,
            parallelLevel,
            10.max(keepaliveTime), // keepalive must be larger than 10 ms
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
                self.AddWorker(&worker);
                return Ok(());
            }
        };
    }

    pub fn SendWorkerStatusUpdate(&self, update: WorkerUpdate) {
        let statusUpdateTx = self.workerStateUpdateTx.clone();
        statusUpdateTx.try_send(update).unwrap();
    }

    fn spawn_return_worker_retry(worker: FuncWorker, failworker: bool) {
        tokio::spawn(async move {
            let mut retry_count = 0;
            let max_retries = 10;
            let mut backoff = std::time::Duration::from_millis(500);
            let mut success = false;
            let pod_id = worker.id.load(Ordering::Relaxed);

            loop {
                match worker.ReturnWorker(failworker).await {
                    Ok(()) => {
                        trace!(
                        "Worker {}/{}/{} (podId: {}) successfully returned to scheduler after {} retries",
                        worker.tenant,
                        worker.namespace,
                        worker.funcname,
                        pod_id,
                        retry_count
                    );
                        success = true;
                        break;
                    }
                    Err(e) => {
                        let error_str = format!("{:?}", e);

                        // If pod doesn't exist on scheduler, no point retrying
                        if error_str.contains("NotExist") {
                            error!(
                                "ReturnWorker failed for {}/{}/{} (podId: {}): pod doesn't exist on scheduler, stopping retry",
                                worker.tenant, worker.namespace, worker.funcname, pod_id
                            );
                            break;
                        }

                        retry_count += 1;
                        if retry_count >= max_retries {
                            error!(
                                "ReturnWorker exceeded max retries ({}) for {}/{}/{} (podId: {}). Last error: {:?}",
                                max_retries, worker.tenant, worker.namespace, worker.funcname, pod_id, e
                            );
                            break;
                        }
                        error!(
                            "ReturnWorker failed (attempt {}/{}): {:?}, retrying in {:?}",
                            retry_count, max_retries, e, backoff
                        );
                        tokio::time::sleep(backoff).await;
                        backoff = std::cmp::min(backoff * 2, std::time::Duration::from_secs(5));
                    }
                }
            }

            let lw = LeasedWorker {
                tenant: worker.tenant.clone(),
                namespace: worker.namespace.clone(),
                funcname: worker.funcname.clone(),
                fprevision: worker.fprevision,
                id: worker.id.load(Ordering::Relaxed).to_string(),
            };
            let removed = SCHEDULER_CLIENT.leasedWorkers.lock().await.remove(&lw);
            if removed && !success {
                info!(
                    "Removed worker {}/{} in cleanup after hitting retry cap",
                    worker.funcname, lw.id
                );
            }
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Copy)]
pub struct IxTimestamp {
    pub count: u64,
    pub ms: u64,
}

impl Default for IxTimestamp {
    fn default() -> Self {
        return Self::New();
    }
}

impl IxTimestamp {
    pub fn New() -> Self {
        return Self {
            count: TIMESTAMP_COUNT.fetch_add(1, Ordering::SeqCst),
            ms: TIMESTAMP_START.elapsed().as_millis() as u64,
        };
    }

    pub fn Elapsed(&self) -> u64 {
        return TIMESTAMP_START.elapsed().as_millis() as u64 - self.ms;
    }
}

#[derive(Debug)]
pub struct ClientReqQueuInner {
    // pub reqQueue: TMutex<BTreeMap<IxTimestamp, FuncClientReq>>,
    pub reqQueue: TMutex<VecDeque<FuncClientReq>>,
    pub queueLen: AtomicUsize,
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
    pub fn New(queueLen: usize) -> Self {
        let inner = ClientReqQueuInner {
            reqQueue: TMutex::new(VecDeque::new()),
            // reqQueue: TMutex::new(BTreeMap::new()),
            queueLen: AtomicUsize::new(queueLen),
            notify: Arc::new(Notify::new()),
        };

        return Self(Arc::new(inner));
    }

    pub async fn Count(&self) -> usize {
        return self.reqQueue.lock().await.len();
    }

    pub async fn WaitReq(&self) {
        loop {
            let notified = self.notify.notified();
            if !self.reqQueue.lock().await.is_empty() {
                return;
            }
            notified.await;
        }
    }

    pub async fn Recv(&self) -> FuncClientReq {
        loop {
            let notified = self.notify.notified();
            // match self.reqQueue.lock().await.pop_first() {
            match self.reqQueue.lock().await.pop_back() {
                None => (),
                Some(r) => return r,
            }
            notified.await;
        }
    }

    pub async fn CleanTimeout(&self) -> usize {
        let mut q = self.reqQueue.lock().await;
        let mut count = 0;
        loop {
            match q.back() {
                None => break,
                Some(first) => {
                    let timeout = first.timeout;
                    if first.enqueueTime.Elapsed() as u64 > timeout {
                        let item = q.pop_back().unwrap();
                        count += 1;
                        item.tx.send(Err(Error::Timeout(timeout))).ok();
                    } else {
                        break;
                    }
                }
            }
        }

        return count;
    }

    pub async fn Send(&self, req: FuncClientReq) -> bool {
        let mut q = self.reqQueue.lock().await;
        if q.len() >= self.queueLen.load(Ordering::Relaxed) {
            req.tx.send(Err(Error::QueueFull)).unwrap();
            return false;
        }

        // q.insert(req.enqueueTime.clone(), req);
        q.push_front(req);
        if q.len() == 1 {
            // self.notify.notify_waiters();
            self.notify.notify_one();
        }

        return true;
    }

    pub async fn TryRecvBatch(&self, cnt: usize) -> Vec<FuncClientReq> {
        // self.CleanTimeout().await;
        let mut q = self.reqQueue.lock().await;
        let mut v = Vec::new();
        for _i in 0..cnt {
            match q.pop_back() {
                None => return v,
                Some(req) => v.push(req),
            }
        }

        return v;
    }

    pub async fn TryRecv(&self) -> Option<FuncClientReq> {
        // self.CleanTimeout().await;
        match self.reqQueue.lock().await.pop_back() {
            None => return None,
            Some(v) => return Some(v),
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
