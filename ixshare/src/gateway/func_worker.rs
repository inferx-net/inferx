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
// limitations under the Licens

use core::ops::Deref;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::response::Response;
use opentelemetry::global::ObjectSafeSpan;
use opentelemetry::trace::Tracer;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Notify};
use tokio::sync::{oneshot, Mutex as TMutex};
use tokio::task::JoinSet;
use tokio::time;
use tokio::time::Duration;

use once_cell::sync::Lazy;
use std::collections::HashSet;

use http_body_util::Empty;
use hyper::body::{Bytes, Incoming};
use hyper::client::conn::http1::SendRequest;
use hyper::Request;
use hyper::StatusCode;
use hyper_util::rt::TokioIo;

use inferxlib::data_obj::DeltaEvent;

use crate::common::*;
use crate::gateway::metrics::{FunccallLabels, GATEWAY_METRICS};
use crate::na::LeaseWorkerResp;
use crate::peer_mgr::IxTcpClient;
use inferxlib::obj_mgr::func_mgr::HttpEndpoint;

use super::func_agent_mgr::{FuncAgent, IxTimestamp, WorkerUpdate};
use super::scheduler_client::SCHEDULER_CLIENT;

pub const FUNCCALL_URL: &str = "http://127.0.0.1/funccall";
pub const RESPONSE_LIMIT: usize = 4 * 1024 * 1024; // 4MB
pub const WORKER_PORT: u16 = 80;

pub static RETRYABLE_HTTP_STATUS: Lazy<HashSet<u16>> = Lazy::new(|| {
    [
        408, // Request Timeout
        429, // Too Many Requests
        500, // Internal Server Error
        502, // Bad Gateway
        503, // Service Unavailable
        504, // Gateway Timeout
    ]
    .into_iter()
    .collect()
});

#[derive(Debug, PartialEq, Eq)]
pub enum HttpClientState {
    Fail = 0isize,
    Clear,
    Success,
}

impl HttpClientState {
    pub fn Fail(&self) -> bool {
        match self {
            Self::Fail => true,
            Self::Clear => true,
            Self::Success => false,
        }
    }

    fn FromIsize(value: isize) -> HttpClientState {
        match value {
            0 => HttpClientState::Fail,
            1 => HttpClientState::Clear,
            2 => HttpClientState::Success,
            _ => unreachable!(),
        }
    }
}

#[derive(Debug, Default)]
pub struct PerfStat {
    pub newConn: AtomicU64,
    pub reuseConn: AtomicU64,
    pub connTime: AtomicU64,
    pub readQueueTime: AtomicU64,
    pub processTime: AtomicU64,
}

#[derive(Debug)]
pub struct FuncWorkerInner {
    pub closeNotify: Arc<Notify>,
    pub stop: AtomicBool,

    pub workerId: isize,
    pub workerName: String,

    pub tenant: String,
    pub namespace: String,
    pub funcname: String,
    pub fprevision: i64,
    pub id: AtomicIsize,

    pub ipAddr: Mutex<IpAddress>,
    pub hostIpaddr: Mutex<IpAddress>,
    pub hostport: Mutex<u16>,
    pub endpoint: HttpEndpoint,
    pub keepalive: AtomicBool, // is this a new worker and a keepalive worker

    pub parallelLevel: usize,
    pub keepaliveTime: u64,
    pub ongoingReqCnt: AtomicUsize,

    pub reqQueue: mpsc::Sender<FuncClientReq>,
    pub finishQueue: mpsc::Sender<HttpSender>,
    pub eventChann: mpsc::Sender<DeltaEvent>,
    pub funcClientCnt: AtomicUsize,
    pub funcAgent: FuncAgent,

    pub state: Mutex<FuncWorkerState>,

    pub connPool: ConnectionPool,
    pub failCount: AtomicUsize,
    pub perfStat: PerfStat,
}

impl Drop for FuncWorkerInner {
    fn drop(&mut self) {
        // error!(
        //     "FuncWorkerInner {}/{}/{} drop ...",
        //     self.tenant, self.namespace, self.funcname
        // );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FuncWorkerState {
    // the new pod's state is not ready
    Init,
    // the worker is ready to process any requests and no running request
    Idle,
    // the worker is processing a request
    Processing,
    Finish,
}

#[derive(Debug, Clone)]
pub struct FuncWorker(Arc<FuncWorkerInner>);

impl Deref for FuncWorker {
    type Target = Arc<FuncWorkerInner>;

    fn deref(&self) -> &Arc<FuncWorkerInner> {
        &self.0
    }
}

impl FuncWorker {
    pub async fn New(
        workerId: isize,
        tenant: &str,
        namespace: &str,
        funcname: &str,
        fprevision: i64,
        parallelLeve: usize,
        keepaliveTime: u64,
        endpoint: HttpEndpoint,
        funcAgent: &FuncAgent,
    ) -> Result<Self> {
        let (tx, rx) = mpsc::channel::<FuncClientReq>(parallelLeve * 2);
        let (finishTx, finishRx) = mpsc::channel::<HttpSender>(parallelLeve * 2);
        let (etx, erx) = mpsc::channel(parallelLeve * 2);

        let connectPool = ConnectionPool::New(
            tenant,
            namespace,
            funcname,
            fprevision,
            endpoint.clone(),
            finishTx.clone(),
        );

        let inner = FuncWorkerInner {
            closeNotify: Arc::new(Notify::new()),
            stop: AtomicBool::new(false),

            workerId: workerId,
            tenant: tenant.to_owned(),
            namespace: namespace.to_owned(),
            funcname: funcname.to_owned(),
            fprevision: fprevision,
            id: AtomicIsize::new(-1),
            workerName: "".to_owned(), // todo: remove this

            ipAddr: Mutex::new(IpAddress::default()),
            endpoint: endpoint.clone(),
            keepalive: AtomicBool::new(false),
            hostIpaddr: Mutex::new(IpAddress::default()),
            hostport: Mutex::new(0),

            parallelLevel: parallelLeve,
            keepaliveTime,
            ongoingReqCnt: AtomicUsize::new(0),

            reqQueue: tx,
            finishQueue: finishTx,
            eventChann: etx,
            funcClientCnt: AtomicUsize::new(0),
            funcAgent: funcAgent.clone(),
            state: Mutex::new(FuncWorkerState::Init),
            connPool: connectPool,
            failCount: AtomicUsize::new(0),
            perfStat: PerfStat::default(),
        };

        let worker = Self(Arc::new(inner));

        let clone = worker.clone();
        tokio::spawn(async move {
            clone.Process(rx, erx, finishRx).await.unwrap();
        });

        return Ok(worker);
    }

    pub fn State(&self) -> FuncWorkerState {
        return self.state.lock().unwrap().clone();
    }

    pub fn SetState(&self, state: FuncWorkerState) {
        *self.state.lock().unwrap() = state;
    }

    pub async fn Close(&self) {
        let closeNotify = self.closeNotify.clone();
        closeNotify.notify_one();
    }

    pub fn ReadySlot(&self) -> usize {
        return self.parallelLevel;
    }

    pub fn AvailableSlot(&self) -> usize {
        let state = self.State();
        // error!(
        //     "AvailableSlot state {:?}/{:?}/{}/{}",
        //     &self.workerId,
        //     state,
        //     self.parallelLevel,
        //     self.ongoingReqCnt.load(Ordering::SeqCst)
        // );
        if state == FuncWorkerState::Idle || state == FuncWorkerState::Processing {
            return self.parallelLevel - self.ongoingReqCnt.load(Ordering::SeqCst);
        } else {
            return 0;
        }
    }

    pub fn OngoingReq(&self) -> usize {
        return self.ongoingReqCnt.load(Ordering::SeqCst);
    }

    pub fn EnqEvent(&self, event: DeltaEvent) {
        match self.eventChann.try_send(event) {
            Err(e) => {
                error!(
                    "funcwork {} EnqEvent fail with error {:?}",
                    self.id.load(Ordering::Relaxed),
                    e
                );
            }
            Ok(()) => (),
        }
    }

    pub async fn ReturnWorker(&self, failworker: bool) -> Result<()> {
        // error!(
        //     "Return worker newconn {:?} resueconn {}",
        //     self.connPool.newConn.load(Ordering::Relaxed),
        //     self.connPool.reuseConn.load(Ordering::Relaxed)
        // );

        // info!(
        //     "return worker {:?} the perf {:#?}",
        //     self.WorkerName(),
        //     &self.perfStat
        // );

        let id = self.id.load(Ordering::Relaxed);
        return SCHEDULER_CLIENT
            .ReturnWorker(
                &self.tenant,
                &self.namespace,
                &self.funcname,
                self.fprevision,
                &format!("{}", id),
                failworker,
            )
            .await;
    }

    // return: (workerId, IPAddr, Keepalive)
    pub async fn LeaseWorker(&self) -> Result<LeaseWorkerResp> {
        return SCHEDULER_CLIENT
            .LeaseWorker(
                &self.tenant,
                &self.namespace,
                &self.funcname,
                self.fprevision,
            )
            .await;
    }

    pub async fn FinishWorker(&self) {
        self.funcAgent
            .totalSlot
            .fetch_sub(self.parallelLevel, Ordering::SeqCst);
        assert!(
            self.State() == FuncWorkerState::Idle || self.State() == FuncWorkerState::Processing
        );
        let slot = self.AvailableSlot(); // need to dec the current connect when fail
        self.funcAgent.DecrSlot(slot);
        self.funcAgent
            .activeReqCnt
            .fetch_sub(self.ongoingReqCnt.load(Ordering::SeqCst), Ordering::SeqCst);
        // self.PrintCounts().await;
        self.SetState(FuncWorkerState::Finish);
    }

    pub fn WorkerId(&self) -> isize {
        return self.workerId;
    }

    pub async fn PrintCounts(&self) {
        let activeReqCnt = self.funcAgent.ActiveReqCnt();
        let ongoingReqCnt = self.OngoingReq();
        let waitReqCnt = self.funcAgent.reqQueue.Count().await;
        error!(
            "PrintCounts activeReqCnt {}, ongoingReqCnt {} waitReqCnt {} workercount {}",
            activeReqCnt,
            ongoingReqCnt,
            waitReqCnt,
            self.funcAgent.workers.lock().unwrap().len()
        );

        if self.funcAgent.workers.lock().unwrap().len() == 1 {
            assert!(activeReqCnt == waitReqCnt);
        }
    }

    // return is_fail
    pub async fn HandleReturn(&self, sender: HttpSender) -> bool {
        self.funcAgent.activeReqCnt.fetch_sub(1, Ordering::SeqCst);
        let ongoingReqCnt = self.ongoingReqCnt.fetch_sub(1, Ordering::AcqRel);
        if sender.Fail() {
            if self.failCount.fetch_add(1, Ordering::AcqRel) == 3 {
                // fail 3 times
                error!("Pod failed 3 times: {:?}", self.WorkerName());
                self.FinishWorker().await;
                self.funcAgent
                    .SendWorkerStatusUpdate(WorkerUpdate::WorkerFail((
                        self.clone(),
                        Error::CommonError(format!("Http fail")),
                    )));
                return true;
            }
        } else {
            // clear failure count
            self.failCount.store(0, Ordering::Relaxed);
        }

        self.funcAgent.IncrSlot(1);
        if ongoingReqCnt == 1 {
            self.SetState(FuncWorkerState::Idle);
        }

        let gap = sender.startTime.lock().unwrap().elapsed().as_micros() as u64;

        self.perfStat.processTime.fetch_add(gap, Ordering::SeqCst);

        self.connPool.ReturnSender(sender).await;

        return false;
    }

    pub fn Clean(&self) {
        let ongoingReqCnt = self.ongoingReqCnt.load(Ordering::SeqCst);
        self.funcAgent
            .activeReqCnt
            .fetch_sub(ongoingReqCnt, Ordering::SeqCst);
    }

    pub async fn Process(
        &self,
        _reqQueueRx: mpsc::Receiver<FuncClientReq>,
        _eventQueueRx: mpsc::Receiver<DeltaEvent>,
        idleClientRx: mpsc::Receiver<HttpSender>,
    ) -> Result<()> {
        let tracer = opentelemetry::global::tracer("gateway");
        let mut span = tracer.start("lease");
        self.SetState(FuncWorkerState::Init);
        let start = std::time::Instant::now();
        let resp = match self.LeaseWorker().await {
            Err(e) => {
                span.end();
                self.funcAgent
                    .startingSlot
                    .fetch_sub(self.parallelLevel, Ordering::SeqCst);
                match &e {
                    Error::SchedulerErr(s) => {
                        self.funcAgent
                            .SendWorkerStatusUpdate(WorkerUpdate::WorkerLeaseFail((
                                self.clone(),
                                Error::SchedulerErr(s.clone()),
                            )));
                    }
                    e => {
                        let err = Error::CommonError(format!("{:?}", e));
                        self.funcAgent
                            .SendWorkerStatusUpdate(WorkerUpdate::WorkerLeaseFail((
                                self.clone(),
                                err,
                            )));
                    }
                }

                return Ok(());
            }
            Ok(resp) => resp,
        };

        let labels = FunccallLabels {
            tenant: self.tenant.clone(),
            namespace: self.namespace.clone(),
            funcname: self.funcname.clone(),
            status: StatusCode::OK.as_u16(),
        };
        let leaseLatency = start.elapsed().as_millis();
        if !resp.keepalive {
            error!("cold start latency {:?}/{}", &labels, leaseLatency);
            GATEWAY_METRICS
                .lock()
                .await
                .funccallCsTtft
                .get_or_create(&labels)
                .observe(leaseLatency as f64 / 1000.0);
        }

        span.end();

        let id: isize = resp.id.parse().unwrap();
        let ipaddr = resp.ipaddr;
        let keepalive = resp.keepalive;
        let hostipaddr = resp.hostipaddr;
        let hostport = resp.hostport as u16;

        self.funcAgent
            .startingSlot
            .fetch_sub(self.parallelLevel, Ordering::SeqCst);
        self.funcAgent
            .totalSlot
            .fetch_add(self.parallelLevel, Ordering::SeqCst);

        self.id.store(id, Ordering::SeqCst);
        *self.ipAddr.lock().unwrap() = IpAddress(ipaddr);
        self.keepalive.store(keepalive, Ordering::SeqCst);
        *self.hostIpaddr.lock().unwrap() = IpAddress(hostipaddr);
        *self.hostport.lock().unwrap() = hostport;

        self.connPool
            .Init(id, IpAddress(ipaddr), IpAddress(hostipaddr), hostport)
            .await;

        let mut idleClientRx = idleClientRx;
        let slots = self.parallelLevel;
        self.funcAgent.IncrSlot(slots);
        self.funcAgent
            .SendWorkerStatusUpdate(WorkerUpdate::Ready(self.clone()));
        self.SetState(FuncWorkerState::Processing);
        let reqQueue = self.funcAgent.reqQueue.clone();

        let clone = self.clone();
        defer!(clone.Clean());

        loop {
            let isScaleInWorker =
                self.workerId == self.funcAgent.scaleInWorkerId.load(Ordering::Relaxed);

            let state = self.State();
            match state {
                FuncWorkerState::Init | FuncWorkerState::Finish => {
                    error!("Get unexpected state {:?}", state);
                    unreachable!()
                }
                FuncWorkerState::Idle => {
                    // error!(
                    //     "funcworker return 1 to idle {} isScaleInWorker {}",
                    //     &self.WorkerName(),
                    //     isScaleInWorker
                    // );

                    // let workername = self.WorkerName();
                    // defer! {
                    //     error!(
                    //         "funcworker return 2 to idle {} isScaleInWorker {}",
                    //         &workername,
                    //         isScaleInWorker
                    //     );
                    // };
                    let mut interval =
                        tokio::time::interval(std::time::Duration::from_millis(self.keepaliveTime));
                    interval.tick().await;
                    if isScaleInWorker {
                        loop {
                            tokio::select! {
                                _ = self.closeNotify.notified() => {
                                    self.stop.store(false, Ordering::SeqCst);
                                    return Ok(())
                                }
                                _ = tokio::time::sleep(Duration::from_millis(1)) => {
                                    if self.funcAgent.NeedLastWorker() {
                                        // error!("scalein worker 1 timeout {}/{:?}", self.WorkerName(), self.keepaliveTime);
                                        // self.PrintCounts().await;
                                        self.SetState(FuncWorkerState::Processing);
                                        break;
                                    }
                                }
                                _ = interval.tick() => {
                                    self.FinishWorker().await;
                                    self.funcAgent.SendWorkerStatusUpdate(WorkerUpdate::IdleTimeout(self.clone()));
                                    return Ok(())
                                }

                            }
                        }
                    } else {
                        tokio::select! {
                            _ = self.closeNotify.notified() => {
                                self.stop.store(false, Ordering::SeqCst);
                                // we clean all the waiting request
                                //self.StopWorker().await?;
                                return Ok(())
                            }
                            _ = reqQueue.WaitReq() => {
                                self.SetState(FuncWorkerState::Processing);
                            }
                            _ = interval.tick() => {
                                self.FinishWorker().await;
                                self.funcAgent.SendWorkerStatusUpdate(WorkerUpdate::IdleTimeout(self.clone()));
                                return Ok(())
                            }

                        }
                    }
                }
                FuncWorkerState::Processing => {
                    let cnt = self.funcAgent.parallelLeve.load(Ordering::Relaxed)
                        - self.ongoingReqCnt.load(Ordering::Relaxed);

                    if cnt > 0 {
                        let reqs = reqQueue.TryRecvBatch(cnt).await;
                        for req in reqs {
                            self.ongoingReqCnt.fetch_add(1, Ordering::SeqCst);
                            let client = match self.NewHttpCallClient().await {
                                Err(e) => {
                                    error!("Funcworker connect fail with error {:?}", &e);
                                    let err = Error::CommonError(format!(
                                        "Funcworker connect fail with error {:?}",
                                        &e
                                    ));
                                    req.Send(Err(err));
                                    self.FinishWorker().await;
                                    self.funcAgent.SendWorkerStatusUpdate(
                                        WorkerUpdate::WorkerFail((self.clone(), e)),
                                    );
                                    return Ok(());
                                }
                                Ok(c) => c,
                            };
                            req.Send(Ok(client));
                        }
                    }

                    if self.ongoingReqCnt.load(Ordering::Relaxed) == 0 {
                        self.SetState(FuncWorkerState::Idle);
                    } else if self.funcAgent.parallelLeve.load(Ordering::Relaxed)
                        > self.ongoingReqCnt.load(Ordering::Relaxed)
                    {
                        tokio::select! {
                            _ = self.closeNotify.notified() => {
                                self.stop.store(false, Ordering::SeqCst);
                                //self.StopWorker().await?;
                                return Ok(())
                            }
                            _ = reqQueue.WaitReq() => {
                            }
                            httpstate = idleClientRx.recv() => {
                                match httpstate {
                                    None => {
                                        return Ok(())
                                    }
                                    Some(sender) => {
                                        if self.HandleReturn(sender).await {
                                            return Ok(());
                                        }

                                        while let Ok(sender) = idleClientRx.try_recv() {
                                            if self.HandleReturn(sender).await {
                                                return Ok(());
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        tokio::select! {
                            _ = self.closeNotify.notified() => {
                                self.stop.store(false, Ordering::SeqCst);
                                //self.StopWorker().await?;
                                return Ok(())
                            }
                            httpstate = idleClientRx.recv() => {
                                match httpstate {
                                    None => {
                                        return Ok(())
                                    }
                                    Some(sender) => {
                                        if self.HandleReturn(sender).await {
                                            return Ok(());
                                        }

                                        while let Ok(sender) = idleClientRx.try_recv() {
                                            if self.HandleReturn(sender).await {
                                                return Ok(());
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    pub async fn NewHttpCallClient(&self) -> Result<QHttpCallClient> {
        let client = self.connPool.GetConnect().await;
        return client;
    }

    pub fn PodNamespace(&self) -> String {
        return format!("{}/{}", &self.tenant, &self.namespace);
    }

    pub fn WorkerName(&self) -> String {
        return format!(
            "{}/{}/{}/{}/{:?}/{}",
            &self.tenant,
            &self.namespace,
            &self.funcname,
            &self.fprevision,
            self.id.load(Ordering::Relaxed),
            self.workerId
        );
    }
}

#[derive(Debug)]
pub struct ConnectionPoolInner {
    pub tenant: String,
    pub namespace: String,
    pub funcname: String,
    pub revision: i64,
    pub id: AtomicIsize,
    pub ipAddr: Mutex<IpAddress>,
    pub hostIpaddr: Mutex<IpAddress>,
    pub hostport: Mutex<u16>,
    pub endpoint: HttpEndpoint,
    pub finishQueue: mpsc::Sender<HttpSender>,
    pub joinset: TMutex<JoinSet<Result<QHttpCallClient>>>,
    pub senders: TMutex<Vec<HttpSender>>,
    pub newConn: AtomicUsize,
    pub reuseConn: AtomicUsize,
}

#[derive(Debug, Clone)]
pub struct ConnectionPool(Arc<ConnectionPoolInner>);

impl Deref for ConnectionPool {
    type Target = Arc<ConnectionPoolInner>;

    fn deref(&self) -> &Arc<ConnectionPoolInner> {
        &self.0
    }
}

impl ConnectionPool {
    pub fn New(
        tenant: &str,
        namespace: &str,
        funcname: &str,
        revision: i64,
        endpoint: HttpEndpoint,
        finishQueue: mpsc::Sender<HttpSender>,
    ) -> Self {
        let joinset = JoinSet::new();
        let inner = ConnectionPoolInner {
            tenant: tenant.to_owned(),
            namespace: namespace.to_owned(),
            funcname: funcname.to_owned(),
            revision: revision,
            id: AtomicIsize::new(-1),
            ipAddr: Mutex::new(IpAddress::default()),
            hostIpaddr: Mutex::new(Default::default()),
            hostport: Mutex::new(0),
            endpoint: endpoint,
            finishQueue: finishQueue,
            joinset: TMutex::new(joinset),
            senders: TMutex::new(Vec::new()),
            reuseConn: AtomicUsize::new(0),
            newConn: AtomicUsize::new(0),
        };

        let pool = Self(Arc::new(inner));

        return pool;
    }

    pub async fn Clear(&self) {
        self.senders.lock().await.clear();
    }

    pub async fn Init(&self, id: isize, ipaddr: IpAddress, hostipaddr: IpAddress, hostport: u16) {
        *self.ipAddr.lock().unwrap() = ipaddr;
        self.id.store(id, Ordering::SeqCst);
        *self.hostIpaddr.lock().unwrap() = hostipaddr;
        *self.hostport.lock().unwrap() = hostport;
        // let mut joinset = self.joinset.lock().await;
        // for _i in 0..self.queueLen - 1 {
        //     let clone = self.clone();
        //     joinset.spawn(async move { clone.NewHttpCallClient().await });
        // }
    }

    pub fn PodName(&self) -> String {
        let id = self.id.load(Ordering::Relaxed);
        return format!(
            "{}/{}/{}/{}/{}",
            &self.tenant, &self.namespace, &self.funcname, self.revision, id
        );
    }

    pub async fn ReturnSender(&self, sender: HttpSender) {
        if sender.Fail() || sender.Close() {
            return;
        }

        self.senders.lock().await.push(sender);
    }

    pub async fn GetConnect(&self) -> Result<QHttpCallClient> {
        loop {
            match self.senders.lock().await.pop() {
                Some(sender) => {
                    if sender.Close() || sender.Fail() {
                        continue;
                    }
                    self.reuseConn.fetch_add(1, Ordering::Relaxed);
                    return Ok(QHttpCallClient::New(self.finishQueue.clone(), sender));
                }
                None => {
                    break;
                }
            }
        }

        self.newConn.fetch_add(1, Ordering::Relaxed);
        match self.NewHttpCallClient().await {
            Err(Error::CommonError(str)) => {
                return Err(Error::CommonError(format!(
                    "Socket fail: {} {}",
                    self.PodName(),
                    str
                )));
            }
            Ok(c) => Ok(c),
            Err(e) => return Err(e),
        }
    }

    pub async fn NewHttpCallClient(&self) -> Result<QHttpCallClient> {
        let stream = self.ConnectPod().await?;
        let sender = HttpSender::New(&self.PodName(), stream).await?;
        let client = QHttpCallClient::New(self.finishQueue.clone(), sender);
        return Ok(client);
    }

    pub async fn ConnectPod(&self) -> Result<TcpStream> {
        for _ in 0..10 {
            match self.TryConnectPod(self.endpoint.port).await {
                Err(e) => {
                    error!(
                        "connectpod error {:?} for pod {}/{:?}",
                        e, &self.funcname, &self.id
                    );
                }
                Ok(s) => return Ok(s),
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        return Err(Error::CommonError(format!(
            "ConnectionPool FuncWorker::ConnectPod timeout"
        )));
    }

    pub async fn TryConnectPod(&self, port: u16) -> Result<TcpStream> {
        let hostip = *self.hostIpaddr.lock().unwrap();
        let hostport = *self.hostport.lock().unwrap();
        let dstIp = self.ipAddr.lock().unwrap().0;

        let tcpclient = IxTcpClient {
            hostIp: hostip.0,
            hostPort: hostport,
            tenant: self.tenant.clone(),
            namespace: self.namespace.clone(),
            dstIp: dstIp,
            dstPort: port,
            srcIp: 0x01020304,
            srcPort: 123,
        };

        return tcpclient.Connect().await;
    }
}

#[derive(Debug)]
pub struct HttpResponse {
    pub status: StatusCode,
    pub response: String,
}

#[derive(Debug)]
pub struct FuncClientReq {
    pub tenant: String,
    pub namespace: String,
    pub funcName: String,
    pub keepalive: bool,
    pub enqueueTime: IxTimestamp,
    pub timeout: u64,
    pub tx: oneshot::Sender<Result<(QHttpCallClient, bool)>>,
}

impl FuncClientReq {
    pub fn Send(self, client: Result<QHttpCallClient>) {
        let _ = match client {
            Err(e) => self.tx.send(Err(e)),
            Ok(client) => self.tx.send(Ok((client, self.keepalive))),
        };
    }
}

#[derive(Debug)]
pub struct QHttpClient {
    sender: SendRequest<Empty<Bytes>>,
}

impl QHttpClient {
    pub async fn New(stream: TcpStream) -> Result<Self> {
        let io = TokioIo::new(stream);
        let (sender, conn) = hyper::client::conn::http1::handshake(io).await?;
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                error!("QHttpClient::Error in connection: {}", e);
            }
        });
        return Ok(Self { sender: sender });
    }

    pub async fn Send(
        &mut self,
        req: Request<Empty<Bytes>>,
        timeout: u64,
    ) -> Result<Response<Incoming>> {
        if timeout == 0 {
            let res = self.sender.send_request(req).await;
            match res {
                Err(e) => {
                    return Err(Error::CommonError(format!(
                        "QHttpClient::Error in Send1: {}",
                        e
                    )))
                }
                Ok(r) => return Ok(r),
            }
        } else {
            tokio::select! {
                res = self.sender.send_request(req) => {
                    match res {
                        Err(e) => return Err(Error::CommonError(format!("QHttpClient::Error in Send2: {}", e))),
                        Ok(r) => return Ok(r)
                    }
                }
                _ = time::sleep(Duration::from_millis(timeout)) => {
                    return Err(Error::CommonError(format!("QHttpClient::Error in Send3: timeout")));
                }
            }
        }
    }
}

#[derive(Debug)]
pub struct HttpSender {
    pub podname: String,
    sender: SendRequest<axum::body::Body>,
    pub fail: Arc<AtomicBool>,
    pub close: Arc<AtomicBool>,
    pub reuse: Arc<AtomicUsize>,
    pub startTime: Mutex<std::time::Instant>,
}

impl HttpSender {
    pub async fn New(podname: &str, stream: TcpStream) -> Result<Self> {
        let io = TokioIo::new(stream);
        let (sender, conn) = hyper::client::conn::http1::handshake(io).await?;
        let fail = Arc::new(AtomicBool::new(false));
        let failclone = fail.clone();
        let close = Arc::new(AtomicBool::new(false));
        let closeclone = close.clone();
        tokio::spawn(async move {
            match conn.await {
                Err(e) => {
                    error!("QHttpCallClient::Error in connection: {}", e);
                    failclone.store(true, Ordering::SeqCst);
                }
                Ok(()) => {
                    closeclone.store(true, Ordering::SeqCst);
                }
            }
        });

        let sender = HttpSender {
            podname: podname.to_owned(),
            sender: sender,
            fail: fail,
            close: close,
            reuse: Arc::new(AtomicUsize::new(1)),
            startTime: Mutex::new(std::time::Instant::now()),
        };

        return Ok(sender);
    }

    pub fn ResetTime(&self) {
        *self.startTime.lock().unwrap() = std::time::Instant::now();
    }

    pub async fn Send(&mut self, req: Request<axum::body::Body>) -> Result<Response<Incoming>> {
        let now = std::time::Instant::now();
        tokio::select! {
            res = self.sender.send_request(req) => {
                match res {
                    Err(e) => {
                        error!("HttpSender fail for pod {} with error {:?}", &self.podname, e);
                        self.fail.store(true, Ordering::SeqCst);

                        return Err(Error::CommonError(format!(
                            "QHttpCallClient::Error take {} ms reuse {} in sending: {}/{}/{}",
                            now.elapsed().as_millis(),
                            self.reuse.load(Ordering::Relaxed),
                            e.is_canceled(),
                            e.is_closed(),
                            e
                        )));
                    }
                    Ok(r) => {
                        let status = r.status();
                        if RETRYABLE_HTTP_STATUS.contains(&(status.as_u16())) {
                            error!("HttpSender fail for pod {} with error response {:?}", &self.podname, &status);
                            self.fail.store(true, Ordering::SeqCst);
                        }
                        return Ok(r);
                    }
                }
            }
            // _ = tokio::time::sleep(Duration::from_millis(10000)) => {
            //     self.fail.store(HttpClientState::Fail as usize, Ordering::SeqCst);
            //     return Err(Error::CommonError(format!(
            //         "QHttpCallClient::Error IxTimeout take {} ms in sending",
            //         now.elapsed().as_millis()
            //     )));
            // }

        }
    }

    pub fn Fail(&self) -> bool {
        return self.fail.load(Ordering::Acquire);
    }

    pub fn Close(&self) -> bool {
        return self.close.load(Ordering::Acquire);
    }
}

#[derive(Debug)]
pub struct QHttpCallClient {
    pub finishQueue: mpsc::Sender<HttpSender>,
    pub sender: Option<HttpSender>,
}

impl Drop for QHttpCallClient {
    fn drop(&mut self) {
        let sender = self.sender.take().unwrap();
        match self.finishQueue.try_send(sender) {
            Err(_e) => {
                //error!("QHttpCallClient send fail with error {:?}", _e);
            }
            Ok(()) => (),
        }
    }
}

impl QHttpCallClient {
    pub fn New(finishQueue: mpsc::Sender<HttpSender>, sender: HttpSender) -> Self {
        sender.reuse.fetch_add(1, Ordering::Relaxed);
        sender.ResetTime();
        return Self {
            finishQueue: finishQueue,
            sender: Some(sender),
        };
    }

    pub async fn Send(&mut self, req: Request<axum::body::Body>) -> Result<Response<Incoming>> {
        return self.sender.as_mut().unwrap().Send(req).await;
    }

    pub fn PodName(&self) -> String {
        match &self.sender {
            None => "unknown".to_owned(),
            Some(s) => s.podname.clone(),
        }
    }
}

#[derive(Debug)]
pub struct QHttpCallClientDirect {
    sender: SendRequest<axum::body::Body>,
    pub fail: AtomicBool,
}

impl QHttpCallClientDirect {
    pub async fn New(stream: TcpStream) -> Result<Self> {
        let io = TokioIo::new(stream);
        let (sender, conn) = hyper::client::conn::http1::handshake(io).await?;
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                error!("QHttpCallClientDirect::Error in connection: {}", e);
            }
            // error!("QHttpCallClient exiting fd {}", fd);
        });
        return Ok(Self {
            sender: sender,
            fail: AtomicBool::new(false),
        });
    }

    pub async fn Send(&mut self, req: Request<axum::body::Body>) -> Result<Response<Incoming>> {
        tokio::select! {
            res = self.sender.send_request(req) => {
                match res {
                    Err(e) => {
                        self.fail.store(true, Ordering::SeqCst);
                        return Err(Error::CommonError(format!("QHttpCallClientDirect::Error in Send: {}", e)));
                    }
                    Ok(r) => return Ok(r)
                }
            }
        }
    }
}
