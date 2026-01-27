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

use std::result::Result as SResult;

use crate::common::*;
use crate::peer_mgr::NA_CONFIG;
use crate::scheduler::scheduler::SnapshotScheduleState;

use chrono::Local;
use serde::{Deserializer, Serializer};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgConnectOptions;
use sqlx::postgres::PgPool;
use sqlx::postgres::PgPoolOptions;
use sqlx::ConnectOptions;
use sqlx::FromRow;
use sqlx::Row;
use std::ops::Deref;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::SystemTime;

use tokio::sync::mpsc;
use tokio::sync::Notify;

lazy_static::lazy_static! {
    pub static ref POD_AUDIT_AGENT: PodAuditAgent = PodAuditAgent::New();
    pub static ref REQ_AUDIT_AGENT: ReqAuditAgent = ReqAuditAgent::New();
    pub static ref GPU_USAGE_AGENT: GpuUsageAuditAgent = GpuUsageAuditAgent::New();
}

#[derive(Debug, Clone)]
pub enum AuditRecord {
    PodAudit(PodAudit),
    SnapshotScheduleAudit(SnapshotScheduleAudit),
}

#[derive(Debug, Clone)]
pub struct PodAudit {
    pub tenant: String,
    pub namespace: String,
    pub fpname: String,
    pub fprevision: i64,
    pub id: String,
    pub podtype: String,
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

    pub tx: mpsc::Sender<AuditRecord>,
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
        let (tx, rx) = mpsc::channel::<AuditRecord>(300);

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

    pub fn AuditPod(&self, msg: PodAudit) {
        self.tx.try_send(AuditRecord::PodAudit(msg)).unwrap();
    }

    pub fn AuditSnapshotSchedule(&self, msg: SnapshotScheduleAudit) {
        self.tx
            .try_send(AuditRecord::SnapshotScheduleAudit(msg))
            .unwrap();
    }

    pub fn Enable() -> bool {
        return NA_CONFIG.auditdbAddr.len() > 0;
    }

    pub async fn HandleMsg(&self, sqlaudit: &SqlAudit, msg: AuditRecord) -> Result<()> {
        match msg {
            AuditRecord::PodAudit(msg) => {
                match sqlaudit
                    .PodAudit(
                        &msg.tenant,
                        &msg.namespace,
                        &msg.fpname,
                        msg.fprevision,
                        &msg.id,
                        &msg.podtype,
                        &msg.nodename,
                        &msg.action,
                        &msg.state,
                    )
                    .await
                {
                    Err(e) => {
                        error!("audit PodAudit fail with {:?}", e);
                    }
                    _ => (),
                }
                if msg.log.len() > 0 {
                    match sqlaudit
                        .AddFailPod(
                            &msg.tenant,
                            &msg.namespace,
                            &msg.fpname,
                            msg.fprevision,
                            &msg.id,
                            &msg.state,
                            &msg.nodename,
                            &msg.log,
                            &msg.exitInfo,
                        )
                        .await
                    {
                        Err(e) => {
                            error!("audit AddFailPod fail with {:?}", e);
                        }
                        _ => (),
                    }
                }
            }
            AuditRecord::SnapshotScheduleAudit(msg) => {
                match sqlaudit.CreateSnapshotScheduleRecord(&msg).await {
                    Err(e) => {
                        error!("audit SnapshotScheduleAudit fail with {:?}", e);
                    }
                    _ => (),
                }
            }
        }

        return Ok(());
    }

    pub async fn Process(&self, mut rx: mpsc::Receiver<AuditRecord>) -> Result<()> {
        let addr = NA_CONFIG.auditdbAddr.clone();
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
                        self.HandleMsg(&sqlaudit, msg).await?;
                    } else {
                        break;
                    }
                }
            }
        }
        unimplemented!()
    }
}

pub struct FuncId {
    pub tenant: String,
    pub namespace: String,
    pub funcname: String,
    pub revision: i64,
}

impl FuncId {
    pub fn New(funcId: &str) -> Result<Self> {
        let parts = funcId.split("/").collect::<Vec<&str>>();
        if parts.len() != 4 {
            return Err(Error::CommonError(format!(
                "FuncId new fail with id {}",
                funcId
            )));
        }

        let revision: i64 = match parts[3].parse() {
            Err(_) => {
                return Err(Error::CommonError(format!(
                    "FuncId new fail with id {} revision invalid {}",
                    funcId, parts[3]
                )));
            }
            Ok(r) => r,
        };

        return Ok(Self {
            tenant: parts[0].to_owned(),
            namespace: parts[1].to_owned(),
            funcname: parts[2].to_owned(),
            revision: revision,
        });
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, FromRow)]
pub struct SnapshotScheduleAudit {
    pub tenant: String,
    pub namespace: String,
    pub funcname: String,
    pub revision: i64,
    pub nodename: String,
    pub state: String,
    pub detail: String,
    // #[serde(with = "crate::audit::datetime_local")]
    pub updatetime: chrono::DateTime<chrono::Utc>,
}

impl SnapshotScheduleAudit {
    pub fn New(funcId: &str, nodename: &str, state: &SnapshotScheduleState) -> Result<Self> {
        let id = FuncId::New(funcId)?;
        let audit = Self {
            tenant: id.tenant,
            namespace: id.namespace,
            funcname: id.funcname,
            revision: id.revision,
            nodename: nodename.to_owned(),
            state: state.StateName(),
            detail: state.Detail(),
            updatetime: SystemTime::now().into(),
        };

        return Ok(audit);
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
        let (tx, rx) = mpsc::channel::<ReqAudit>(300);

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

    pub fn Audit(&self, _msg: ReqAudit) {
        // match self.tx.try_send(msg) {
        //     Ok(()) => (),
        //     Err(e) => {
        //         error!("ReqAuditAgent: fail to send audit log {:?}", &e);
        //     }
        // }
    }

    pub fn Enable() -> bool {
        return NA_CONFIG.auditdbAddr.len() > 0;
    }

    pub async fn Process(&self, mut rx: mpsc::Receiver<ReqAudit>) -> Result<()> {
        let addr = NA_CONFIG.auditdbAddr.clone();
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

/// GPU Usage record for billing
#[derive(Debug, Clone)]
pub struct GpuUsage {
    pub tenant: String,
    pub namespace: String,
    pub funcname: String,
    pub fprevision: i64,
    pub nodename: String,
    pub pod_id: String,

    // GPU resource info
    pub gpu_type: String,
    pub gpu_count: i32,
    pub vram_mb: i64,
    pub total_vram_mb: i64,

    // Usage tracking
    pub usage_type: String, // "request" or "snapshot"
    pub start_time: chrono::DateTime<chrono::Utc>,
    pub end_time: chrono::DateTime<chrono::Utc>,
    pub duration_ms: i64,

    pub is_coldstart: bool,
    pub gateway_id: Option<i64>,
}

#[derive(Debug)]
pub struct GpuUsageAuditAgentInner {
    pub closeNotify: Arc<Notify>,
    pub stop: AtomicBool,
    pub tx: mpsc::Sender<GpuUsage>,
}

#[derive(Clone, Debug)]
pub struct GpuUsageAuditAgent(Arc<GpuUsageAuditAgentInner>);

impl Deref for GpuUsageAuditAgent {
    type Target = Arc<GpuUsageAuditAgentInner>;

    fn deref(&self) -> &Arc<GpuUsageAuditAgentInner> {
        &self.0
    }
}

impl GpuUsageAuditAgent {
    pub fn New() -> Self {
        let (tx, rx) = mpsc::channel::<GpuUsage>(300);

        let inner = GpuUsageAuditAgentInner {
            closeNotify: Arc::new(Notify::new()),
            stop: AtomicBool::new(false),
            tx: tx,
        };

        let agent = GpuUsageAuditAgent(Arc::new(inner));
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

    pub fn Audit(&self, msg: GpuUsage) {
        // Skip if auditdb is not enabled
        if !Self::Enable() {
            return;
        }
        match self.tx.try_send(msg) {
            Ok(()) => (),
            Err(e) => {
                error!("GpuUsageAuditAgent: fail to send audit log {:?}", &e);
            }
        }
    }

    pub fn Enable() -> bool {
        return NA_CONFIG.auditdbAddr.len() > 0;
    }

    pub async fn Process(&self, mut rx: mpsc::Receiver<GpuUsage>) -> Result<()> {
        let addr = NA_CONFIG.auditdbAddr.clone();
        info!("GpuUsageAuditAgent: auditdb address {}", &addr);
        if addr.len() == 0 {
            // auditdb is not enabled
            info!("GpuUsageAuditAgent: auditdb is not enabled");
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
                        match sqlaudit.CreateGpuUsageRecord(&msg).await {
                            Ok(()) => (),
                            Err(e) => {
                                error!("GpuUsageAuditAgent: fail to create record {:?}", &e);
                            }
                        }
                    } else {
                        break;
                    }
                }
            }
        }
        Ok(())
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

    pub async fn CreateGpuUsageRecord(&self, usage: &GpuUsage) -> Result<()> {
        let query = r#"
            INSERT INTO GpuUsage (
                tenant, namespace, funcname, fprevision, nodename, pod_id,
                gpu_type, gpu_count, vram_mb, total_vram_mb,
                usage_type, start_time, end_time, duration_ms,
                is_coldstart, gateway_id
            ) VALUES (
                $1, $2, $3, $4, $5, $6,
                $7, $8, $9, $10,
                $11, $12, $13, $14,
                $15, $16
            )
        "#;
        let _result = sqlx::query(query)
            .bind(&usage.tenant)
            .bind(&usage.namespace)
            .bind(&usage.funcname)
            .bind(usage.fprevision)
            .bind(&usage.nodename)
            .bind(&usage.pod_id)
            .bind(&usage.gpu_type)
            .bind(usage.gpu_count)
            .bind(usage.vram_mb)
            .bind(usage.total_vram_mb)
            .bind(&usage.usage_type)
            .bind(usage.start_time)
            .bind(usage.end_time)
            .bind(usage.duration_ms)
            .bind(usage.is_coldstart)
            .bind(usage.gateway_id)
            .execute(&self.pool)
            .await?;
        return Ok(());
    }

    pub async fn CreateSnapshotScheduleRecord(&self, audit: &SnapshotScheduleAudit) -> Result<()> {
        let query = r#"
                            INSERT INTO SnapshotScheduleAudit 
                                (tenant, namespace, funcname, revision, nodename, state, detail, updatetime)
                            VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())
                            ON CONFLICT (tenant, namespace, funcname, revision, nodename, state)
                            DO UPDATE SET
                                detail = EXCLUDED.detail,
                                updatetime = NOW()
                            "#;
        let _result = sqlx::query(query)
            .bind(&audit.tenant)
            .bind(&audit.namespace)
            .bind(&audit.funcname)
            .bind(audit.revision)
            .bind(&audit.nodename)
            .bind(&audit.state)
            .bind(&audit.detail)
            .execute(&self.pool)
            .await?;
        return Ok(());
    }

    pub async fn ReadSnapshotScheduleRecords(
        &self,
        tenant: &str,
        namespace: &str,
        funcname: &str,
        revision: i64,
    ) -> Result<Vec<SnapshotScheduleAudit>> {
        let rows = sqlx::query(
            r#"
            SELECT tenant, namespace, funcname, revision, nodename,
                   state, detail, updatetime
            FROM SnapshotScheduleAudit
            WHERE tenant = $1
              AND namespace = $2
              AND funcname = $3
              AND revision = $4
            ORDER BY updatetime;
            "#,
        )
        .bind(tenant)
        .bind(namespace)
        .bind(funcname)
        .bind(revision)
        .fetch_all(&self.pool)
        .await?;

        let result = rows
            .into_iter()
            .map(|row| {
                // let datetime: NaiveDateTime = row.get("updatetime");
                // let utctime = DateTime::<Utc>::from_naive_utc_and_offset(datetime, Utc);
                SnapshotScheduleAudit {
                    tenant: row.get("tenant"),
                    namespace: row.get("namespace"),
                    funcname: row.get("funcname"),
                    revision: row.get("revision"),
                    nodename: row.get("nodename"),
                    state: row.get("state"),
                    detail: row.get("detail"),
                    updatetime: row.get("updatetime"), // converts to SystemTime
                }
            })
            .collect();

        Ok(result)
    }

    pub async fn CreatePod(
        &self,
        tenant: &str,
        namespace: &str,
        fpname: &str,
        fprevision: i64,
        id: &str,
        podtype: &str,
        nodename: &str,
        state: &str,
    ) -> Result<()> {
        let query ="insert into Pod (tenant, namespace, fpname, fprevision, id, podtype, nodename, state, updatetime) values \
        ($1, $2, $3, $4, $5, $6, $7, $8, NOW())";
        let _result = sqlx::query(query)
            .bind(tenant)
            .bind(namespace)
            .bind(fpname)
            .bind(fprevision)
            .bind(id)
            .bind(podtype)
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
        podtype: &str,
        nodename: &str,
        action: &str,
        state: &str,
    ) -> Result<()> {
        if action == "create" {
            self.CreatePod(
                tenant, namespace, fpname, fprevision, id, podtype, nodename, state,
            )
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

    pub async fn FuncCount(
        &self,
        tenant: &str,
        namespace: &str,
        fpname: &str,
        fprevision: i64,
        podtype: &str,
        nodename: &str,
    ) -> Result<u64> {
        // Use parameters instead of format!(...)
        let row: (i64,) = sqlx::query_as(
            r#"
                SELECT COUNT(*)
                FROM pod
                WHERE tenant = $1
                AND namespace = $2
                AND fpname = $3
                AND fprevision = $4
                AND podtype = $5
                AND nodename = $6
                "#,
        )
        .bind(tenant)
        .bind(namespace)
        .bind(fpname)
        .bind(fprevision)
        .bind(podtype)
        .bind(nodename)
        .fetch_one(&self.pool)
        .await?;

        // error!(
        //     "SELECT COUNT(*)
        //         FROM pod
        //         WHERE tenant = '{}'
        //         AND namespace = '{}'
        //         AND fpname = '{}'
        //         AND fprevision = {}
        //         AND podtype = '{}'
        //         AND nodename = '{}'",
        //     tenant, namespace, fpname, fprevision, podtype, nodename
        // );

        Ok(row.0 as u64)
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
            "SELECT state, updatetime FROM podaudit where tenant = '{}' and namespace='{}' \
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
        let query = format!("select tenant, namespace, fpname, fprevision, id, state, createtime, nodename, '' as log, exit_info \
         from PodFailLog where tenant = '{}' and namespace = '{}' and fpname = '{}' and fprevision = {}", 
            tenant, namespace, fpname, fprevision);
        let selectQuery = sqlx::query_as::<_, PodFailLog>(&query);
        let logs: Vec<PodFailLog> = match selectQuery.fetch_all(&self.pool).await {
            Err(e) => {
                return Err(e.into());
            }
            Ok(l) => l,
        };
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
        let query = format!("select tenant, namespace, fpname, fprevision, id, state, nodename, createtime, log, exit_info from PodFailLog where tenant = '{}' and namespace = '{}' and fpname = '{}' and fprevision = {} and id = '{}'", 
            tenant, namespace, fpname, fprevision, id);
        let selectQuery = sqlx::query_as::<_, PodFailLog>(&query);
        let log: PodFailLog = match selectQuery.fetch_one(&self.pool).await {
            Ok(l) => l,
            Err(e) => {
                error!("ReadPodFailLog error is {:#?}", &e);
                return Err(e.into());
            }
        };
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
    pub createtime: chrono::DateTime<chrono::Utc>,
    pub log: String,
    pub exit_info: String,
}

#[derive(Serialize, Deserialize, Debug, FromRow)]
pub struct PodAuditLog {
    pub state: String,
    pub updatetime: chrono::DateTime<chrono::Utc>,
}

pub mod datetime_local {
    use super::*;

    pub fn serialize<S>(dt: &DateTime<Utc>, serializer: S) -> SResult<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let local_dt = dt.with_timezone(&Local);
        serializer.serialize_str(&local_dt.to_rfc3339())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> SResult<DateTime<Utc>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let parsed = DateTime::parse_from_rfc3339(&s).map_err(serde::de::Error::custom)?;
        Ok(parsed.with_timezone(&Utc))
    }
}
