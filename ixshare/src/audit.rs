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

use crate::common::*;
use crate::node_config::NODE_CONFIG;

use serde::{Deserialize, Serialize};
use sqlx::postgres::PgConnectOptions;
use sqlx::postgres::PgPool;
use sqlx::postgres::PgPoolOptions;
use sqlx::ConnectOptions;
use sqlx::FromRow;
use std::ops::Deref;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::sync::Notify;

lazy_static::lazy_static! {
    pub static ref POD_AUDIT_AGENT: PodAuditAgent = PodAuditAgent::New();
    pub static ref REQ_AUDIT_AGENT: ReqAuditAgent = ReqAuditAgent::New();
}

#[derive(Debug, Clone)]
pub struct PodAudit {
    pub tenant: String,
    pub namespace: String,
    pub fpname: String,
    pub fprevision: i64,
    pub id: String,
    pub nodename: String,
    pub action: String,
    pub state: String,
    pub log: String,
    pub exitInfo: String,
}

#[derive(Debug)]
pub struct PodAuditAgentInner {
    pub closeNotify: Arc<Notify>,
    pub stop: AtomicBool,

    pub tx: mpsc::Sender<PodAudit>,
}

#[derive(Clone, Debug)]
pub struct PodAuditAgent(Arc<PodAuditAgentInner>);

impl Deref for PodAuditAgent {
    type Target = Arc<PodAuditAgentInner>;

    fn deref(&self) -> &Arc<PodAuditAgentInner> {
        &self.0
    }
}

impl PodAuditAgent {
    pub fn New() -> Self {
        let (tx, rx) = mpsc::channel::<PodAudit>(300);

        let inner = PodAuditAgentInner {
            closeNotify: Arc::new(Notify::new()),
            stop: AtomicBool::new(false),
            tx: tx,
        };

        let agent = PodAuditAgent(Arc::new(inner));
        let agent1 = agent.clone();

        tokio::spawn(async move {
            agent1.Process(rx).await.unwrap();
        });

        return agent;
    }

    pub fn Close(&self) -> Result<()> {
        self.closeNotify.notify_one();
        return Ok(());
    }

    pub fn Audit(&self, msg: PodAudit) {
        self.tx.try_send(msg).unwrap();
    }

    pub fn Enable() -> bool {
        return NODE_CONFIG.auditdbAddr.len() > 0;
    }

    pub async fn Process(&self, mut rx: mpsc::Receiver<PodAudit>) -> Result<()> {
        let addr = NODE_CONFIG.auditdbAddr.clone();
        if addr.len() == 0 {
            // auditdb is not enabled
            return Ok(());
        }
        let sqlaudit = SqlAudit::New(&addr).await?;

        loop {
            tokio::select! {
                _ = self.closeNotify.notified() => {
                    self.stop.store(false, Ordering::SeqCst);
                    break;
                }
                msg = rx.recv() => {
                    if let Some(msg) = msg {
                        match sqlaudit.PodAudit(&msg.tenant, &msg.namespace, &msg.fpname, msg.fprevision, &msg.id, &msg.nodename, &msg.action, &msg.state).await {
                            Err(e)=> {
                                error!("audit PodAudit fail with {:?}", e);
                            }
                            _ => ()
                        }
                        if msg.log.len() > 0 {
                            match sqlaudit.AddFailPod(&msg.tenant, &msg.namespace, &msg.fpname, msg.fprevision, &msg.id, &msg.state, &msg.nodename, &msg.log, &msg.exitInfo).await {
                                Err(e) => {
                                    error!("audit AddFailPod fail with {:?}", e);
                                }
                                _ => ()
                            }
                        }
                    } else {
                        break;
                    }
                }
            }
        }
        unimplemented!()
    }
}

#[derive(Debug, Clone)]
pub struct ReqAudit {
    pub tenant: String,
    pub namespace: String,
    pub fpname: String,
    pub keepalive: bool,
    pub ttft: i32,
    pub latency: i32,
}

#[derive(Debug)]
pub struct ReqAuditAgentInner {
    pub closeNotify: Arc<Notify>,
    pub stop: AtomicBool,

    pub tx: mpsc::Sender<ReqAudit>,
}

#[derive(Clone, Debug)]
pub struct ReqAuditAgent(Arc<ReqAuditAgentInner>);

impl Deref for ReqAuditAgent {
    type Target = Arc<ReqAuditAgentInner>;

    fn deref(&self) -> &Arc<ReqAuditAgentInner> {
        &self.0
    }
}

impl ReqAuditAgent {
    pub fn New() -> Self {
        let (tx, rx) = mpsc::channel::<ReqAudit>(30);

        let inner = ReqAuditAgentInner {
            closeNotify: Arc::new(Notify::new()),
            stop: AtomicBool::new(false),
            tx: tx,
        };

        let agent = ReqAuditAgent(Arc::new(inner));
        let agent1 = agent.clone();

        tokio::spawn(async move {
            agent1.Process(rx).await.unwrap();
        });

        return agent;
    }

    pub fn Close(&self) -> Result<()> {
        self.closeNotify.notify_one();
        return Ok(());
    }

    pub fn Audit(&self, msg: ReqAudit) {
        self.tx.try_send(msg).unwrap();
    }

    pub fn Enable() -> bool {
        return NODE_CONFIG.auditdbAddr.len() > 0;
    }

    pub async fn Process(&self, mut rx: mpsc::Receiver<ReqAudit>) -> Result<()> {
        let addr = NODE_CONFIG.auditdbAddr.clone();
        if addr.len() == 0 {
            // auditdb is not enabled
            return Ok(());
        }
        let sqlaudit = SqlAudit::New(&addr).await?;

        loop {
            tokio::select! {
                _ = self.closeNotify.notified() => {
                    self.stop.store(false, Ordering::SeqCst);
                    break;
                }
                msg = rx.recv() => {
                    if let Some(msg) = msg {
                        sqlaudit.CreateReqAuditRecord(&msg.tenant, &msg.namespace, &msg.fpname, msg.keepalive, msg.ttft, msg.latency).await?;
                    } else {
                        break;
                    }
                }
            }
        }
        unimplemented!()
    }
}

#[derive(Debug, Clone)]
pub struct SqlAudit {
    pub pool: PgPool,
}

impl SqlAudit {
    pub async fn New(sqlSvcAddr: &str) -> Result<Self> {
        let url_parts = url::Url::parse(sqlSvcAddr).expect("Failed to parse URL");
        let username = url_parts.username();
        let password = url_parts.password().unwrap_or("");
        let host = url_parts.host_str().unwrap_or("localhost");
        let port = url_parts.port().unwrap_or(5432);
        let database = url_parts.path().trim_start_matches('/');

        let options = PgConnectOptions::new()
            .host(host)
            .port(port)
            .username(username)
            .password(password)
            .database(database);

        options.clone().disable_statement_logging();

        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;
        return Ok(Self { pool: pool });
    }

    pub async fn CreateReqAuditRecord(
        &self,
        tenant: &str,
        namespace: &str,
        fpname: &str,
        keepalive: bool,
        ttf: i32,
        latency: i32,
    ) -> Result<()> {
        let podkey = format!("{}/{}/{}", tenant, namespace, fpname);
        let query = "insert into ReqAudit (podkey, audittime, keepalive, ttft, latency) values \
            ($1, NOW(), $2, $3, $4)";
        let _result = sqlx::query(query)
            .bind(podkey)
            .bind(keepalive)
            .bind(ttf)
            .bind(latency)
            .execute(&self.pool)
            .await?;
        return Ok(());
    }

    pub async fn CreatePod(
        &self,
        tenant: &str,
        namespace: &str,
        fpname: &str,
        fprevision: i64,
        id: &str,
        nodename: &str,
        state: &str,
    ) -> Result<()> {
        let query ="insert into Pod (tenant, namespace, fpname, fprevision, id, nodename, state, updatetime) values \
        ($1, $2, $3, $4, $5, $6, $7, NOW())";
        let _result = sqlx::query(query)
            .bind(tenant)
            .bind(namespace)
            .bind(fpname)
            .bind(fprevision)
            .bind(id)
            .bind(nodename)
            .bind(state)
            .execute(&self.pool)
            .await?;
        return Ok(());
    }

    pub async fn AddFailPod(
        &self,
        tenant: &str,
        namespace: &str,
        fpname: &str,
        fprevision: i64,
        id: &str,
        state: &str,
        nodename: &str,
        log: &str,
        exitInfo: &str,
    ) -> Result<()> {
        const MAX_VARCHAR_LEN: usize = 65535;
        let log = if log.len() >= MAX_VARCHAR_LEN {
            let start = log.len() - MAX_VARCHAR_LEN;
            log[start..].to_owned()
        } else {
            log.to_owned()
        };

        let query ="insert into PodFailLog (tenant, namespace, fpname, fprevision, id, nodename, state, log, exit_info, createtime) values \
        ($1, $2, $3, $4, $5, $6, $7, $8, $9, NOW())";
        let _result = sqlx::query(query)
            .bind(tenant)
            .bind(namespace)
            .bind(fpname)
            .bind(fprevision)
            .bind(id)
            .bind(nodename)
            .bind(state)
            .bind(log)
            .bind(exitInfo)
            .execute(&self.pool)
            .await?;

        return Ok(());
    }

    pub async fn UpdatePod(
        &self,
        tenant: &str,
        namespace: &str,
        fpname: &str,
        fprevision: i64,
        id: &str,
        state: &str,
    ) -> Result<()> {
        let query ="update Pod set state = $1, updatetime=Now() where tenant = $2 and namespace=$3 and fpname=$4 and fprevision=$5 and id=$6";

        let _result = sqlx::query(query)
            .bind(state)
            .bind(tenant)
            .bind(namespace)
            .bind(fpname)
            .bind(fprevision)
            .bind(id)
            .execute(&self.pool)
            .await?;
        return Ok(());
    }

    pub async fn Audit(
        &self,
        tenant: &str,
        namespace: &str,
        fpname: &str,
        fprevision: i64,
        id: &str,
        nodename: &str,
        action: &str,
        state: &str,
    ) -> Result<()> {
        let query ="insert into PodAudit (tenant, namespace, fpname, fprevision, id, nodename, action, state, updatetime) values \
            ($1, $2, $3, $4, $5, $6, $7, $8, NOW())";

        let _result = sqlx::query(query)
            .bind(tenant)
            .bind(namespace)
            .bind(fpname)
            .bind(fprevision)
            .bind(id)
            .bind(nodename)
            .bind(action)
            .bind(state)
            .execute(&self.pool)
            .await?;

        return Ok(());
    }

    pub async fn PodAudit(
        &self,
        tenant: &str,
        namespace: &str,
        fpname: &str,
        fprevision: i64,
        id: &str,
        nodename: &str,
        action: &str,
        state: &str,
    ) -> Result<()> {
        if action == "create" {
            self.CreatePod(tenant, namespace, fpname, fprevision, id, nodename, state)
                .await?;
        } else {
            self.UpdatePod(tenant, namespace, fpname, fprevision, id, state)
                .await?;
        }

        self.Audit(
            tenant, namespace, fpname, fprevision, id, nodename, action, state,
        )
        .await?;
        return Ok(());
    }

    pub async fn ReadPodAudit(
        &self,
        tenant: &str,
        namespace: &str,
        fpname: &str,
        fprevision: i64,
        id: &str,
    ) -> Result<Vec<PodAuditLog>> {
        let query = format!(
            "SELECT state, to_char(updatetime, 'YYYY/MM/DD HH24:MI:SS') as updatetime FROM podaudit where tenant = '{}' and namespace='{}' \
         and fpname='{}' and fprevision='{}' and id='{}' order by updatetime;",
            tenant, namespace, fpname, fprevision, id
        );
        let selectQuery = sqlx::query_as::<_, PodAuditLog>(&query);
        let logs: Vec<PodAuditLog> = selectQuery.fetch_all(&self.pool).await?;
        return Ok(logs);
    }

    pub async fn ReadPodFailLogs(
        &self,
        tenant: &str,
        namespace: &str,
        fpname: &str,
        fprevision: i64,
    ) -> Result<Vec<PodFailLog>> {
        let query = format!("select tenant, namespace, fpname, fprevision, id, state, nodename, '' as log, exit_info \
         from PodFailLog where tenant = '{}' and namespace = '{}' and fpname = '{}' and fprevision = {}", 
            tenant, namespace, fpname, fprevision);
        let selectQuery = sqlx::query_as::<_, PodFailLog>(&query);
        let logs: Vec<PodFailLog> = selectQuery.fetch_all(&self.pool).await?;
        return Ok(logs);
    }

    pub async fn ReadPodFailLog(
        &self,
        tenant: &str,
        namespace: &str,
        fpname: &str,
        fprevision: i64,
        id: &str,
    ) -> Result<PodFailLog> {
        let query = format!("select tenant, namespace, fpname, fprevision, id, state, nodename, log, exit_info from PodFailLog where tenant = '{}' and namespace = '{}' and fpname = '{}' and fprevision = {} and id = '{}'", 
            tenant, namespace, fpname, fprevision, id);
        let selectQuery = sqlx::query_as::<_, PodFailLog>(&query);
        let log: PodFailLog = selectQuery.fetch_one(&self.pool).await?;
        return Ok(log);
    }
}

#[derive(Serialize, Deserialize, Debug, FromRow)]
pub struct PodFailLog {
    pub tenant: String,
    pub namespace: String,
    pub fpname: String,
    pub fprevision: i64,
    pub id: String,
    pub state: String,
    pub nodename: String,
    // #[serde(skip_serializing)]
    // pub createtime: sqlx::types::time::OffsetDateTime,
    pub log: String,
    pub exit_info: String,
}

#[derive(Serialize, Deserialize, Debug, FromRow)]
pub struct PodAuditLog {
    pub state: String,
    pub updatetime: String,
}
