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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

use http_body_util::Empty;
use hyper::body::{Bytes, Incoming};
use hyper::client::conn::http1::SendRequest;
use hyper::Request;
use hyper::StatusCode;
use hyper_util::rt::TokioIo;

use inferxlib::data_obj::DeltaEvent;

use crate::common::*;
use crate::na::LeaseWorkerResp;
use crate::peer_mgr::IxTcpClient;
use inferxlib::obj_mgr::func_mgr::HttpEndpoint;

use super::func_agent_mgr::{FuncAgent, IxTimestamp, WorkerUpdate};
use super::scheduler_client::SchedulerClient;

pub const FUNCCALL_URL: &str = "http://127.0.0.1/funccall";
pub const RESPONSE_LIMIT: usize = 4 * 1024 * 1024; // 4MB
pub const WORKER_PORT: u16 = 80;

#[derive(Debug, PartialEq, Eq)]
pub enum HttpClientState {
    Fail,
    Success,
}

impl HttpClientState {
    pub fn Fail(&self) -> bool {
        match self {
            Self::Fail => true,
            Self::Success => false,
        }
    }
}

#[derive(Debug)]
pub struct FuncWorkerInner {
    pub closeNotify: Arc<Notify>,
    pub stop: AtomicBool,

    pub workerId: String,
    pub workerName: String,

    pub tenant: String,
    pub namespace: String,
    pub funcname: String,
    pub fprevision: i64,
    pub id: Mutex<String>,

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
        workerId: &str,
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

            workerId: workerId.to_owned(),
            tenant: tenant.to_owned(),
            namespace: namespace.to_owned(),
            funcname: funcname.to_owned(),
            fprevision: fprevision,
            id: Mutex::new("".to_owned()),
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

    pub fn AssignReq(&self, req: FuncClientReq) {
        // error!(
        //     "AssignReq ongoingReqCnt ongoing {:?}/{:?}",
        //     &self.workerId,
        //     self.ongoingReqCnt.load(Ordering::Relaxed)
        // );
        self.ongoingReqCnt.fetch_add(1, Ordering::SeqCst);
        self.reqQueue.try_send(req).unwrap();
    }

    pub fn OngoingReq(&self) -> usize {
        return self.ongoingReqCnt.load(Ordering::SeqCst);
    }

    pub fn EnqEvent(&self, event: DeltaEvent) {
        match self.eventChann.try_send(event) {
            Err(e) => {
                error!(
                    "funcwork {} EnqEvent fail with error {:?}",
                    self.id.lock().unwrap().clone(),
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
        let id = self.id.lock().unwrap().clone();
        return SchedulerClient {}
            .ReturnWorker(
                &self.tenant,
                &self.namespace,
                &self.funcname,
                self.fprevision,
                &id,
                failworker,
            )
            .await;
    }

    // return: (workerId, IPAddr, Keepalive)
    pub async fn LeaseWorker(&self) -> Result<LeaseWorkerResp> {
        return SchedulerClient {}
            .LeaseWorker(
                &self.tenant,
                &self.namespace,
                &self.funcname,
                self.fprevision,
            )
            .await;
    }

    pub fn FinishWorker(&self) {
        self.funcAgent
            .totalSlot
            .fetch_sub(self.parallelLevel, Ordering::SeqCst);
        assert!(
            self.State() == FuncWorkerState::Idle || self.State() == FuncWorkerState::Processing
        );
        let slot = self.AvailableSlot(); // need to dec the current connect when fail
        self.funcAgent.DecrSlot(slot);
        self.SetState(FuncWorkerState::Finish);
    }

    pub async fn Process(
        &self,
        reqQueueRx: mpsc::Receiver<FuncClientReq>,
        _eventQueueRx: mpsc::Receiver<DeltaEvent>,
        idleClientRx: mpsc::Receiver<HttpSender>,
    ) -> Result<()> {
        let tracer = opentelemetry::global::tracer("gateway");
        let mut span = tracer.start("lease");
        let mut reqQueueRx = reqQueueRx;
        self.SetState(FuncWorkerState::Init);
        let resp = match self.LeaseWorker().await {
            Err(e) => {
                span.end();
                // error!(
                //     "Lease worker {} fail with error {:?}",
                //     self.WorkerName(),
                //     &e
                // );
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

                loop {
                    match reqQueueRx.try_recv() {
                        Ok(req) => {
                            match &e {
                                Error::SchedulerErr(s) => {
                                    req.Send(Err(Error::SchedulerErr(s.clone())));
                                }
                                e => {
                                    let err = Err(Error::CommonError(format!("{:?}", e)));
                                    req.Send(err);
                                }
                            }

                            // req.Send(Err(Error::CommonError(format!(
                            //     "fail to run func {:?} with error {:?}",
                            //     self.funcname, &err
                            // ))));
                        }
                        Err(_) => {
                            break;
                        }
                    }
                }

                return Ok(());
            }
            Ok(resp) => resp,
        };

        span.end();

        let id = resp.id;
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

        *self.id.lock().unwrap() = id.clone();
        *self.ipAddr.lock().unwrap() = IpAddress(ipaddr);
        self.keepalive.store(keepalive, Ordering::SeqCst);
        *self.hostIpaddr.lock().unwrap() = IpAddress(hostipaddr);
        *self.hostport.lock().unwrap() = hostport;

        self.connPool
            .Init(&id, IpAddress(ipaddr), IpAddress(hostipaddr), hostport)
            .await;

        let mut idleClientRx = idleClientRx;
        let slots = self.parallelLevel;
        self.funcAgent.IncrSlot(slots);
        self.funcAgent
            .SendWorkerStatusUpdate(WorkerUpdate::Ready(self.clone()));
        self.SetState(FuncWorkerState::Idle);
        loop {
            let state = self.State();
            match state {
                FuncWorkerState::Init | FuncWorkerState::Finish => {
                    unreachable!()
                }
                FuncWorkerState::Idle => {
                    tokio::select! {
                        _ = self.closeNotify.notified() => {
                            self.stop.store(false, Ordering::SeqCst);
                            // we clean all the waiting request
                            //self.StopWorker().await?;
                            return Ok(())
                        }
                        // _e = self.ProbeLiveness() => {
                        //     self.funcAgent.SendWorkerStatusUpdate(WorkerUpdate::WorkerFail(self.clone()));
                        //     break;
                        // }
                        req = reqQueueRx.recv() => {
                            match req {
                                None => {
                                    return Ok(())
                                }
                                Some(mut req) => {
                                    self.SetState(FuncWorkerState::Processing);
                                    let client = match self.NewHttpCallClient().await {
                                        Err(e) => {
                                            error!("Funcworker connect fail with error {:?}", &e);
                                            let err = Error::CommonError(format!("Funcworker connect fail with error {:?}", &e));
                                            req.Send(Err(err));
                                            self.FinishWorker();
                                            self.funcAgent.SendWorkerStatusUpdate(WorkerUpdate::WorkerFail((self.clone(), e)));
                                            break;
                                        }
                                        Ok(c) => c,
                                    };
                                    req.keepalive = self.keepalive.swap(true, Ordering::SeqCst);
                                    req.Send(Ok(client));
                                }
                            }
                        }
                        _ = tokio::time::sleep(Duration::from_millis(self.keepaliveTime)) => {
                            self.FinishWorker();
                            self.funcAgent.SendWorkerStatusUpdate(WorkerUpdate::IdleTimeout(self.clone()));
                            break;
                        }

                    }
                }
                FuncWorkerState::Processing => {
                    tokio::select! {
                        _ = self.closeNotify.notified() => {
                            self.stop.store(false, Ordering::SeqCst);
                            //self.StopWorker().await?;
                            return Ok(())
                        }
                        req = reqQueueRx.recv() => {
                            match req {
                                None => {
                                    return Ok(())
                                }
                                Some(req) => {
                                    let client = match self.NewHttpCallClient().await {
                                        Err(e) => {
                                            error!("Funcworker connect fail with error {:?}", &e);
                                            let err = Error::CommonError(format!("Funcworker connect fail with error {:?}", &e));
                                            req.Send(Err(err));
                                            self.FinishWorker();
                                            self.funcAgent.SendWorkerStatusUpdate(WorkerUpdate::WorkerFail((self.clone(), e)));
                                            break;
                                        }
                                        Ok(c) => c,
                                    };
                                    req.Send(Ok(client));
                                }
                            }
                        }
                        httpstate = idleClientRx.recv() => {
                            match httpstate {
                                None => {
                                    return Ok(())
                                }
                                Some(sender) => {
                                    let state = sender.HttpState();
                                    if state == HttpClientState::Fail {
                                        if self.failCount.fetch_add(1, Ordering::SeqCst) == 3 { // fail 3 times
                                            self.FinishWorker();
                                            self.funcAgent.SendWorkerStatusUpdate(WorkerUpdate::WorkerFail((self.clone(), Error::CommonError(format!("Http fail")))));
                                            break;
                                        }
                                    } else {
                                        // clear failure count
                                        self.failCount.store(0, Ordering::Relaxed);
                                    }

                                    self.funcAgent.IncrSlot(1);
                                    let cnt = self.ongoingReqCnt.fetch_sub(1, Ordering::SeqCst);
                                    if cnt == 1 {
                                        self.SetState(FuncWorkerState::Idle);
                                    }

                                    self.funcAgent.dataNotify.notify_waiters();
                                    self.connPool.ReturnSender(sender).await;
                                }
                            }
                        }
                    }
                }
            }
        }

        return Ok(());
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
            "{}/{}/{}/{}/{:?}",
            &self.tenant,
            &self.namespace,
            &self.funcname,
            &self.fprevision,
            self.id.lock()
        );
    }
}

#[derive(Debug)]
pub struct ConnectionPoolInner {
    pub tenant: String,
    pub namespace: String,
    pub funcname: String,
    pub revision: i64,
    pub id: Mutex<String>,
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
            id: Mutex::new("".to_owned()),
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

    pub async fn Init(&self, id: &str, ipaddr: IpAddress, hostipaddr: IpAddress, hostport: u16) {
        *self.ipAddr.lock().unwrap() = ipaddr;
        *self.id.lock().unwrap() = id.to_owned();
        *self.hostIpaddr.lock().unwrap() = hostipaddr;
        *self.hostport.lock().unwrap() = hostport;
        // let mut joinset = self.joinset.lock().await;
        // for _i in 0..self.queueLen - 1 {
        //     let clone = self.clone();
        //     joinset.spawn(async move { clone.NewHttpCallClient().await });
        // }
    }

    pub fn FuncName(&self) -> String {
        let id = self.id.lock().unwrap().clone();
        return format!(
            "{}/{}/{}/{}/{}",
            &self.tenant, &self.namespace, &self.funcname, self.revision, &id
        );
    }

    pub async fn ReturnSender(&self, sender: HttpSender) {
        let state = sender.HttpState();
        if state == HttpClientState::Fail {
            return;
        }

        self.senders.lock().await.push(sender);
    }

    pub async fn GetConnect(&self) -> Result<QHttpCallClient> {
        match self.senders.lock().await.pop() {
            Some(sender) => {
                self.reuseConn.fetch_add(1, Ordering::Relaxed);
                return Ok(QHttpCallClient::New(self.finishQueue.clone(), sender));
            }
            None => (),
        }

        self.newConn.fetch_add(1, Ordering::Relaxed);
        match self.NewHttpCallClient().await {
            Err(Error::CommonError(str)) => {
                return Err(Error::CommonError(format!(
                    "Socket fail: {} {}",
                    self.FuncName(),
                    str
                )));
            }
            Ok(c) => Ok(c),
            Err(e) => return Err(e),
        }
    }

    pub async fn NewHttpCallClient(&self) -> Result<QHttpCallClient> {
        let stream = self.ConnectPod().await?;
        let sender = HttpSender::New(stream).await?;
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
    sender: SendRequest<axum::body::Body>,
    pub fail: Arc<AtomicBool>,
}

impl HttpSender {
    pub async fn New(stream: TcpStream) -> Result<Self> {
        let io = TokioIo::new(stream);
        let (sender, conn) = hyper::client::conn::http1::handshake(io).await?;
        let fail = Arc::new(AtomicBool::new(false));
        let failclone = fail.clone();
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                error!("QHttpCallClient::Error in connection: {}", e);
                failclone.store(true, Ordering::SeqCst);
            }
        });

        let sender = HttpSender {
            sender: sender,
            fail: Arc::new(AtomicBool::new(false)),
        };

        return Ok(sender);
    }

    pub async fn Send(&mut self, req: Request<axum::body::Body>) -> Result<Response<Incoming>> {
        let res = self.sender.send_request(req).await;
        match res {
            Err(e) => {
                self.fail.store(true, Ordering::SeqCst);
                return Err(Error::CommonError(format!(
                    "QHttpCallClient::Error in sending: {}",
                    e
                )));
            }
            Ok(r) => {
                let status = r.status();
                if status != StatusCode::OK {
                    self.fail.store(true, Ordering::SeqCst);
                }
                return Ok(r);
            }
        }
    }

    pub fn HttpState(&self) -> HttpClientState {
        let state = if self.fail.load(Ordering::Relaxed) {
            HttpClientState::Fail
        } else {
            HttpClientState::Success
        };
        return state;
    }

    pub fn SetState(&self, state: HttpClientState) {
        let fail = state.Fail();
        self.fail.store(fail, Ordering::SeqCst);
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
        return Self {
            finishQueue: finishQueue,
            sender: Some(sender),
        };
    }

    pub async fn Send(&mut self, req: Request<axum::body::Body>) -> Result<Response<Incoming>> {
        return self.sender.as_mut().unwrap().Send(req).await;
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
