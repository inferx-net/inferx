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
use std::collections::HashMap;
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
    pub static ref USAGE_TICK_AGENT: UsageTickAuditAgent = UsageTickAuditAgent::New();
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

/// Usage Tick record for billing (v4: supports request, snapshot, and standby)
pub struct UsageTick {
    pub session_id: String,
    pub tenant: String,
    pub namespace: String,
    pub funcname: String,
    pub fprevision: i64,            // model version
    pub nodename: Option<String>,   // NULL for standby ticks
    pub pod_id: Option<i64>,        // NULL for standby ticks
    pub gateway_id: Option<i64>,

    // GPU resource info
    pub gpu_type: String,
    pub gpu_count: i32,
    pub vram_mb: i64,
    pub total_vram_mb: i64,

    // Tick info
    pub tick_time: DateTime<Utc>,  // Time when interval was calculated
    pub interval_ms: i64,
    pub tick_type: String,  // "start", "periodic", "final"
    pub usage_type: String, // "request", "snapshot", or "standby"
    pub is_coldstart: bool,
}

// ============================================================================
// GPU Usage Tick Audit Agent
// ============================================================================

#[derive(Debug)]
pub struct UsageTickAuditAgentInner {
    pub closeNotify: Arc<Notify>,
    pub stop: AtomicBool,
    pub tx: mpsc::Sender<UsageTick>,
}

#[derive(Clone, Debug)]
pub struct UsageTickAuditAgent(Arc<UsageTickAuditAgentInner>);

impl Deref for UsageTickAuditAgent {
    type Target = Arc<UsageTickAuditAgentInner>;

    fn deref(&self) -> &Arc<UsageTickAuditAgentInner> {
        &self.0
    }
}

impl UsageTickAuditAgent {
    pub fn New() -> Self {
        let (tx, rx) = mpsc::channel::<UsageTick>(1000);

        let inner = UsageTickAuditAgentInner {
            closeNotify: Arc::new(Notify::new()),
            stop: AtomicBool::new(false),
            tx: tx,
        };

        let agent = UsageTickAuditAgent(Arc::new(inner));
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

    pub fn Audit(&self, tick: UsageTick) {
        // Skip if auditdb is not enabled
        if !Self::Enable() {
            return;
        }
        match self.tx.try_send(tick) {
            Ok(()) => (),
            Err(e) => {
                error!("UsageTickAuditAgent: fail to send tick {:?}", &e);
            }
        }
    }

    pub fn Enable() -> bool {
        return NA_CONFIG.billingdbAddr.len() > 0;
    }

    pub async fn Process(&self, mut rx: mpsc::Receiver<UsageTick>) -> Result<()> {
        let addr = NA_CONFIG.billingdbAddr.clone();
        info!("UsageTickAuditAgent: billingdb address {}", &addr);
        if addr.len() == 0 {
            info!("UsageTickAuditAgent: billingdb is not enabled");
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
                    if let Some(tick) = msg {
                        match sqlaudit.CreateUsageTickRecord(&tick).await {
                            Ok(()) => (),
                            Err(e) => {
                                error!("UsageTickAuditAgent: fail to create tick record {:?}", &e);
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

#[derive(Serialize, Deserialize, Debug, FromRow)]
pub struct TenantCreditHistoryRecord {
    pub id: i64,
    pub amount_cents: i64,
    pub currency: String,
    pub note: Option<String>,
    pub payment_ref: Option<String>,
    pub added_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Debug, FromRow)]
pub struct BillingCreditHistoryRecord {
    pub id: i64,
    pub tenant: String,
    pub amount_cents: i64,
    pub currency: String,
    pub note: Option<String>,
    pub payment_ref: Option<String>,
    pub added_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Debug, FromRow)]
pub struct BillingRateHistoryRecord {
    pub id: i64,
    pub usage_type: String,
    pub rate_cents_per_hour: i32,
    pub effective_from: DateTime<Utc>,
    pub effective_to: Option<DateTime<Utc>>,
    pub tenant: Option<String>,
    pub created_at: DateTime<Utc>,
    pub added_by: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
pub struct TenantBillingAdminSummary {
    pub tenant: String,
    pub balance_cents: i64,
    pub used_cents: i64,
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

    /// Create usage tick record (v4: supports request, snapshot, and standby)
    pub async fn CreateUsageTickRecord(&self, tick: &UsageTick) -> Result<()> {
        let query = r#"
            INSERT INTO UsageTick (
                session_id, tenant, namespace, funcname, fprevision, nodename, pod_id, gateway_id,
                gpu_type, gpu_count, vram_mb, total_vram_mb,
                tick_time, interval_ms, tick_type, usage_type, is_coldstart
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8,
                $9, $10, $11, $12,
                $13, $14, $15, $16, $17
            )
        "#;
        let _result = sqlx::query(query)
            .bind(&tick.session_id)
            .bind(&tick.tenant)
            .bind(&tick.namespace)
            .bind(&tick.funcname)
            .bind(tick.fprevision)
            .bind(&tick.nodename)
            .bind(&tick.pod_id)
            .bind(tick.gateway_id)
            .bind(&tick.gpu_type)
            .bind(tick.gpu_count)
            .bind(tick.vram_mb)
            .bind(tick.total_vram_mb)
            .bind(tick.tick_time)
            .bind(tick.interval_ms)
            .bind(&tick.tick_type)
            .bind(&tick.usage_type)
            .bind(tick.is_coldstart)
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

    /// Add credits to tenant in cents (insert into TenantCreditHistory)
    pub async fn AddTenantCredit(
        &self,
        tenant: &str,
        amount_cents: i64,
        currency: &str,
        note: Option<&str>,
        payment_ref: Option<&str>,
        added_by: Option<&str>,
    ) -> Result<i64> {
        let query = r#"
            INSERT INTO TenantCreditHistory (tenant, amount_cents, currency, note, payment_ref, added_by)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id
        "#;
        let row: (i32,) = sqlx::query_as(query)
            .bind(tenant)
            .bind(amount_cents)
            .bind(currency)
            .bind(note)
            .bind(payment_ref)
            .bind(added_by)
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0 as i64)
    }

    /// Check whether a tenant credit record already exists for the given payment reference.
    pub async fn HasTenantCreditPaymentRef(&self, tenant: &str, payment_ref: &str) -> Result<bool> {
        let row: (bool,) = sqlx::query_as(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM TenantCreditHistory
                WHERE tenant = $1 AND payment_ref = $2
            )
            "#,
        )
        .bind(tenant)
        .bind(payment_ref)
        .fetch_one(&self.pool)
        .await?;

        Ok(row.0)
    }

    /// Add a billing rate row.
    pub async fn AddBillingRate(
        &self,
        usage_type: &str,
        rate_cents_per_hour: i32,
        effective_from: DateTime<Utc>,
        effective_to: Option<DateTime<Utc>>,
        tenant: Option<&str>,
        added_by: Option<&str>,
    ) -> Result<i64> {
        let query = r#"
            INSERT INTO BillingRate (usage_type, rate_cents_per_hour, effective_from, effective_to, tenant, added_by)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id
        "#;
        let row: (i32,) = sqlx::query_as(query)
            .bind(usage_type)
            .bind(rate_cents_per_hour)
            .bind(effective_from)
            .bind(effective_to)
            .bind(tenant)
            .bind(added_by)
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0 as i64)
    }

    /// List billing rate history with optional scope/tenant filtering and pagination.
    pub async fn ListBillingRateHistory(
        &self,
        scope: &str,
        tenant: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<BillingRateHistoryRecord>, i64)> {
        let records: Vec<BillingRateHistoryRecord> = sqlx::query_as(
            r#"
            SELECT
                id::bigint as id,
                usage_type,
                rate_cents_per_hour,
                effective_from,
                effective_to,
                tenant,
                created_at,
                added_by
            FROM BillingRate
            WHERE
                ($1 = 'all')
                OR ($1 = 'global' AND tenant IS NULL)
                OR ($1 = 'tenant' AND tenant IS NOT NULL AND ($2 IS NULL OR tenant = $2))
            ORDER BY created_at DESC, id DESC
            LIMIT $3 OFFSET $4
            "#,
        )
        .bind(scope)
        .bind(tenant)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        let total: (i64,) = sqlx::query_as(
            r#"
            SELECT COUNT(*)::bigint
            FROM BillingRate
            WHERE
                ($1 = 'all')
                OR ($1 = 'global' AND tenant IS NULL)
                OR ($1 = 'tenant' AND tenant IS NOT NULL AND ($2 IS NULL OR tenant = $2))
            "#,
        )
        .bind(scope)
        .bind(tenant)
        .fetch_one(&self.pool)
        .await?;

        Ok((records, total.0))
    }

    /// List tenant credit history with pagination
    pub async fn ListTenantCreditHistory(
        &self,
        tenant: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<TenantCreditHistoryRecord>, i64)> {
        let records: Vec<TenantCreditHistoryRecord> = sqlx::query_as(
            r#"
            SELECT
                id::bigint as id,
                amount_cents,
                currency,
                note,
                payment_ref,
                added_by,
                created_at
            FROM TenantCreditHistory
            WHERE tenant = $1
            ORDER BY created_at DESC, id DESC
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(tenant)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        let total: (i64,) = sqlx::query_as(
            r#"
            SELECT COUNT(*)::bigint
            FROM TenantCreditHistory
            WHERE tenant = $1
            "#,
        )
        .bind(tenant)
        .fetch_one(&self.pool)
        .await?;

        Ok((records, total.0))
    }

    /// List credit history across all tenants (or a specific tenant when provided).
    pub async fn ListBillingCreditHistory(
        &self,
        tenant: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<BillingCreditHistoryRecord>, i64)> {
        let records: Vec<BillingCreditHistoryRecord> = sqlx::query_as(
            r#"
            SELECT
                id::bigint as id,
                tenant,
                amount_cents,
                currency,
                note,
                payment_ref,
                added_by,
                created_at
            FROM TenantCreditHistory
            WHERE ($1::varchar IS NULL OR tenant = $1)
            ORDER BY created_at DESC, id DESC
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(tenant)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        let total: (i64,) = sqlx::query_as(
            r#"
            SELECT COUNT(*)::bigint
            FROM TenantCreditHistory
            WHERE ($1::varchar IS NULL OR tenant = $1)
            "#,
        )
        .bind(tenant)
        .fetch_one(&self.pool)
        .await?;

        Ok((records, total.0))
    }

    /// Recalculate quota_exceeded for tenant based on credits and used_cents.
    pub async fn RecalculateTenantQuota(&self, tenant: &str) -> Result<bool> {
        // First, log current values for debugging
        let debug_row: (i64, i64, i64, bool) = sqlx::query_as(
            r#"
            SELECT
                COALESCE((SELECT SUM(amount_cents) FROM TenantCreditHistory WHERE tenant = $1), 0)::bigint AS total_credits,
                COALESCE(q.used_cents, 0)::bigint AS used_cents,
                COALESCE(q.threshold_cents, 0)::bigint AS threshold_cents,
                COALESCE(q.quota_exceeded, false) AS quota_exceeded
            FROM TenantQuota q
            WHERE q.tenant = $1
            "#
        )
            .bind(tenant)
            .fetch_optional(&self.pool)
            .await?
            .unwrap_or((0, 0, 0, false));
        error!(
            "RecalculateTenantQuota: tenant={}, total_credits_cents={}, used_cents={}, threshold_cents={}, old_quota_exceeded={}",
            tenant, debug_row.0, debug_row.1, debug_row.2, debug_row.3
        );

        // Step 1: Ensure TenantQuota row exists (INSERT if not exists)
        // This must be a separate statement because PostgreSQL CTEs operate on the same snapshot,
        // so an INSERT in a CTE isn't visible to an UPDATE in the same statement.
        sqlx::query(
            r#"
            INSERT INTO TenantQuota (tenant)
            VALUES ($1)
            ON CONFLICT (tenant) DO NOTHING
            "#
        )
            .bind(tenant)
            .execute(&self.pool)
            .await?;

        // Step 2: Update balance_cents and quota_exceeded based on current credits and usage
        let query = r#"
            WITH credits AS (
                SELECT COALESCE(SUM(amount_cents), 0)::bigint AS total_credits
                FROM TenantCreditHistory
                WHERE tenant = $1
            )
            UPDATE TenantQuota q
            SET
                balance_cents = COALESCE(c.total_credits, 0) - q.used_cents,
                quota_exceeded =
                    (COALESCE(c.total_credits, 0) - q.used_cents) <= q.threshold_cents
            FROM credits c
            WHERE q.tenant = $1
            RETURNING q.quota_exceeded
        "#;
        let row: (bool,) = sqlx::query_as(query)
            .bind(tenant)
            .fetch_one(&self.pool)
            .await?;
        error!(
            "RecalculateTenantQuota: tenant={}, new_quota_exceeded={}",
            tenant, row.0
        );
        Ok(row.0)
    }

    /// Get tenant credit balance in cents (total credits - total usage)
    pub async fn GetTenantCreditBalance(&self, tenant: &str) -> Result<i64> {
        let row: (i64, i64) = sqlx::query_as(
            r#"
            SELECT
                COALESCE((SELECT SUM(amount_cents) FROM TenantCreditHistory WHERE tenant = $1), 0)::bigint AS total_credits,
                COALESCE((SELECT used_cents FROM TenantQuota WHERE tenant = $1), 0)::bigint AS used_cents
            "#
        )
            .bind(tenant)
            .fetch_one(&self.pool)
            .await?;
        let total_credits = row.0;
        let used_cents = row.1;

        let balance = total_credits - used_cents;
        error!(
            "GetTenantCreditBalance: tenant={}, total_credits_cents={}, used_cents={}, balance_cents={}",
            tenant, total_credits, used_cents, balance
        );

        Ok(balance)
    }

    pub async fn GetTenantBillingSummariesByTenantNames(
        &self,
        tenant_names: &[String],
    ) -> Result<HashMap<String, TenantBillingAdminSummary>> {
        if tenant_names.is_empty() {
            return Ok(HashMap::new());
        }

        let names = tenant_names.to_vec();
        let rows = sqlx::query_as::<_, TenantBillingAdminSummary>(
            r#"
            WITH input_tenants AS (
                SELECT DISTINCT unnest($1::text[]) AS tenant
            ),
            credit_totals AS (
                SELECT
                    tenant,
                    COALESCE(SUM(amount_cents), 0)::bigint AS total_credits
                FROM TenantCreditHistory
                WHERE tenant = ANY($1)
                GROUP BY tenant
            )
            SELECT
                i.tenant,
                (COALESCE(c.total_credits, 0)::bigint - COALESCE(q.used_cents, 0)::bigint) AS balance_cents,
                COALESCE(q.used_cents, 0)::bigint AS used_cents
            FROM input_tenants i
            LEFT JOIN credit_totals c ON c.tenant = i.tenant
            LEFT JOIN TenantQuota q ON q.tenant = i.tenant
            "#
        )
        .bind(names)
        .fetch_all(&self.pool)
        .await?;

        let mut map = HashMap::new();
        for row in rows {
            map.insert(row.tenant.clone(), row);
        }
        Ok(map)
    }

    /// Get tenant billing summary in cents
    /// Returns: (balance, used, threshold, quota_exceeded, total_credits, currency, inference_cents, standby_cents, inference_ms, standby_ms)
    pub async fn GetTenantBillingSummary(
        &self,
        tenant: &str,
    ) -> Result<(i64, i64, i64, bool, i64, String, i64, i64, i64, i64)> {
        let row: (i64, i64, i64, bool, String, i64, i64) = sqlx::query_as(
            r#"
            SELECT
                COALESCE((SELECT SUM(amount_cents) FROM TenantCreditHistory WHERE tenant = $1), 0)::bigint AS total_credits,
                COALESCE(q.used_cents, 0)::bigint AS used_cents,
                COALESCE(q.threshold_cents, 0)::bigint AS threshold_cents,
                COALESCE(q.quota_exceeded, false) AS quota_exceeded,
                COALESCE(q.currency, 'USD') AS currency,
                COALESCE(q.inference_used_cents, 0)::bigint AS inference_used_cents,
                COALESCE(q.standby_used_cents, 0)::bigint AS standby_used_cents
            FROM TenantQuota q
            WHERE q.tenant = $1
            "#,
        )
            .bind(tenant)
            .fetch_optional(&self.pool)
            .await?
            .unwrap_or((0, 0, 0, false, "USD".to_string(), 0, 0));

        let total_credits_cents = row.0;
        let used_cents = row.1;
        let threshold_cents = row.2;
        let quota_exceeded = row.3;
        let currency = row.4;
        let inference_used_cents = row.5;
        let standby_used_cents = row.6;
        let balance_cents = total_credits_cents - used_cents;

        // Keep cost split aligned with used_cents from TenantQuota.
        // Hours are sourced from UsageHourlyByFunc (rolled up across all functions).
        let period_ms: (i64, i64) = sqlx::query_as(
            r#"
            SELECT
                COALESCE(SUM(inference_ms), 0)::bigint,
                COALESCE(SUM(standby_ms), 0)::bigint
            FROM UsageHourlyByFunc
            WHERE tenant = $1
            "#,
        )
            .bind(tenant)
            .fetch_one(&self.pool)
            .await
            .unwrap_or((0, 0));

        Ok((balance_cents, used_cents, threshold_cents, quota_exceeded, total_credits_cents, currency,
            inference_used_cents, standby_used_cents, period_ms.0, period_ms.1))
    }

    /// Get tenant hourly usage for the last N hours (v4: returns cost + time breakdown)
    pub async fn GetTenantHourlyUsage(
        &self,
        tenant: &str,
        hours: i32,
    ) -> Result<Vec<(DateTime<Utc>, i64, i64, i64, i64, i64)>> {
        // Returns: (hour, charge_cents, inference_cents, standby_cents, inference_ms, standby_ms)
        let records: Vec<(DateTime<Utc>, i64, i64, i64, i64, i64)> = sqlx::query_as(
            r#"
            SELECT
                hour,
                SUM(charge_cents)::bigint AS charge_cents,
                SUM(inference_cents)::bigint AS inference_cents,
                SUM(standby_cents)::bigint AS standby_cents,
                SUM(inference_ms)::bigint AS inference_ms,
                SUM(standby_ms)::bigint AS standby_ms
            FROM UsageHourlyByFunc
            WHERE tenant = $1
              AND hour >= NOW() - ($2 || ' hours')::interval
            GROUP BY hour
            ORDER BY hour ASC
            "#,
        )
            .bind(tenant)
            .bind(hours)
            .fetch_all(&self.pool)
            .await?;

        Ok(records)
    }

    /// Get namespace hourly usage for a given tenant + namespace
    /// Returns: (hour, charge_cents, inference_cents, standby_cents, inference_ms, standby_ms)
    pub async fn GetNamespaceHourlyUsage(
        &self,
        tenant: &str,
        namespace: &str,
        hours: i32,
    ) -> Result<Vec<(DateTime<Utc>, i64, i64, i64, i64, i64)>> {
        let records: Vec<(DateTime<Utc>, i64, i64, i64, i64, i64)> = sqlx::query_as(
            r#"
            SELECT
                hour,
                SUM(charge_cents)::bigint AS charge_cents,
                SUM(inference_cents)::bigint AS inference_cents,
                SUM(standby_cents)::bigint AS standby_cents,
                SUM(inference_ms)::bigint AS inference_ms,
                SUM(standby_ms)::bigint AS standby_ms
            FROM UsageHourlyByFunc
            WHERE tenant = $1
              AND namespace = $2
              AND hour >= NOW() - ($3 || ' hours')::interval
            GROUP BY hour
            ORDER BY hour ASC
            "#,
        )
            .bind(tenant)
            .bind(namespace)
            .bind(hours)
            .fetch_all(&self.pool)
            .await?;

        Ok(records)
    }

    /// Get func hourly usage for a given tenant + namespace + funcname
    /// Returns: (hour, charge_cents, inference_cents, standby_cents, inference_ms, standby_ms)
    pub async fn GetFuncHourlyUsage(
        &self,
        tenant: &str,
        namespace: &str,
        funcname: &str,
        hours: i32,
    ) -> Result<Vec<(DateTime<Utc>, i64, i64, i64, i64, i64)>> {
        let records: Vec<(DateTime<Utc>, i64, i64, i64, i64, i64)> = sqlx::query_as(
            r#"
            SELECT
                hour,
                SUM(charge_cents)::bigint AS charge_cents,
                SUM(inference_cents)::bigint AS inference_cents,
                SUM(standby_cents)::bigint AS standby_cents,
                SUM(inference_ms)::bigint AS inference_ms,
                SUM(standby_ms)::bigint AS standby_ms
            FROM UsageHourlyByFunc
            WHERE tenant = $1
              AND namespace = $2
              AND funcname = $3
              AND hour >= NOW() - ($4 || ' hours')::interval
            GROUP BY hour
            ORDER BY hour ASC
            "#,
        )
            .bind(tenant)
            .bind(namespace)
            .bind(funcname)
            .bind(hours)
            .fetch_all(&self.pool)
            .await?;

        Ok(records)
    }

    /// Get namespace hourly usage for a UTC time range [start, end]
    /// Returns: (hour, charge_cents, inference_cents, standby_cents, inference_ms, standby_ms)
    pub async fn GetNamespaceHourlyUsageRange(
        &self,
        tenant: &str,
        namespace: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<(DateTime<Utc>, i64, i64, i64, i64, i64)>> {
        let records: Vec<(DateTime<Utc>, i64, i64, i64, i64, i64)> = sqlx::query_as(
            r#"
            WITH
            params AS (
                SELECT
                    $1::varchar AS tenant,
                    $2::varchar AS namespace,
                    $3::timestamptz AS start_hour,
                    $4::timestamptz AS end_hour,
                    (date_trunc('hour', NOW()) - INTERVAL '23 hours') AS recent_start_hour
            ),
            historical AS (
                SELECT
                    h.hour,
                    SUM(h.charge_cents)::bigint AS charge_cents,
                    SUM(h.inference_cents)::bigint AS inference_cents,
                    SUM(h.standby_cents)::bigint AS standby_cents,
                    SUM(h.inference_ms)::bigint AS inference_ms,
                    SUM(h.standby_ms)::bigint AS standby_ms
                FROM UsageHourlyByFunc h
                JOIN params p ON true
                WHERE h.tenant = p.tenant
                  AND h.namespace = p.namespace
                  AND h.hour >= p.start_hour
                  AND h.hour <= p.end_hour
                  AND h.hour < p.recent_start_hour
                GROUP BY h.hour
            ),
            recent_ticks AS (
                SELECT
                    date_trunc('hour', t.tick_time) AS hour,
                    t.usage_type,
                    t.interval_ms,
                    t.gpu_count,
                    GetBillingRateCents(t.usage_type, t.tick_time, t.tenant)::numeric AS rate_cents_per_hour
                FROM UsageTick t
                JOIN params p ON true
                WHERE t.tenant = p.tenant
                  AND t.namespace = p.namespace
                  AND t.tick_time >= GREATEST(p.start_hour, p.recent_start_hour)
                  AND t.tick_time < LEAST(p.end_hour + INTERVAL '1 hour', NOW())
            ),
            recent AS (
                SELECT
                    hour,
                    (SUM(gpu_count::numeric * interval_ms::numeric * rate_cents_per_hour) / 3600000)::bigint AS charge_cents,
                    (SUM(CASE WHEN usage_type != 'standby'
                        THEN gpu_count::numeric * interval_ms::numeric * rate_cents_per_hour
                        ELSE 0
                    END) / 3600000)::bigint AS inference_cents,
                    (SUM(CASE WHEN usage_type = 'standby'
                        THEN gpu_count::numeric * interval_ms::numeric * rate_cents_per_hour
                        ELSE 0
                    END) / 3600000)::bigint AS standby_cents,
                    SUM(CASE WHEN usage_type != 'standby'
                        THEN interval_ms::bigint * gpu_count::bigint
                        ELSE 0
                    END)::bigint AS inference_ms,
                    SUM(CASE WHEN usage_type = 'standby'
                        THEN interval_ms::bigint * gpu_count::bigint
                        ELSE 0
                    END)::bigint AS standby_ms
                FROM recent_ticks
                GROUP BY hour
            )
            SELECT
                hour,
                charge_cents,
                inference_cents,
                standby_cents,
                inference_ms,
                standby_ms
            FROM historical
            UNION ALL
            SELECT
                hour,
                charge_cents,
                inference_cents,
                standby_cents,
                inference_ms,
                standby_ms
            FROM recent
            ORDER BY hour ASC
            "#,
        )
            .bind(tenant)
            .bind(namespace)
            .bind(start)
            .bind(end)
            .fetch_all(&self.pool)
            .await?;

        Ok(records)
    }

    /// Get model hourly usage for a UTC time range [start, end]
    /// Returns: (hour, charge_cents, inference_cents, standby_cents, inference_ms, standby_ms)
    pub async fn GetFuncHourlyUsageRange(
        &self,
        tenant: &str,
        namespace: &str,
        funcname: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<(DateTime<Utc>, i64, i64, i64, i64, i64)>> {
        let records: Vec<(DateTime<Utc>, i64, i64, i64, i64, i64)> = sqlx::query_as(
            r#"
            WITH
            params AS (
                SELECT
                    $1::varchar AS tenant,
                    $2::varchar AS namespace,
                    $3::varchar AS funcname,
                    $4::timestamptz AS start_hour,
                    $5::timestamptz AS end_hour,
                    (date_trunc('hour', NOW()) - INTERVAL '23 hours') AS recent_start_hour
            ),
            historical AS (
                SELECT
                    h.hour,
                    SUM(h.charge_cents)::bigint AS charge_cents,
                    SUM(h.inference_cents)::bigint AS inference_cents,
                    SUM(h.standby_cents)::bigint AS standby_cents,
                    SUM(h.inference_ms)::bigint AS inference_ms,
                    SUM(h.standby_ms)::bigint AS standby_ms
                FROM UsageHourlyByFunc h
                JOIN params p ON true
                WHERE h.tenant = p.tenant
                  AND h.namespace = p.namespace
                  AND h.funcname = p.funcname
                  AND h.hour >= p.start_hour
                  AND h.hour <= p.end_hour
                  AND h.hour < p.recent_start_hour
                GROUP BY h.hour
            ),
            recent_ticks AS (
                SELECT
                    date_trunc('hour', t.tick_time) AS hour,
                    t.usage_type,
                    t.interval_ms,
                    t.gpu_count,
                    GetBillingRateCents(t.usage_type, t.tick_time, t.tenant)::numeric AS rate_cents_per_hour
                FROM UsageTick t
                JOIN params p ON true
                WHERE t.tenant = p.tenant
                  AND t.namespace = p.namespace
                  AND t.funcname = p.funcname
                  AND t.tick_time >= GREATEST(p.start_hour, p.recent_start_hour)
                  AND t.tick_time < LEAST(p.end_hour + INTERVAL '1 hour', NOW())
            ),
            recent AS (
                SELECT
                    hour,
                    (SUM(gpu_count::numeric * interval_ms::numeric * rate_cents_per_hour) / 3600000)::bigint AS charge_cents,
                    (SUM(CASE WHEN usage_type != 'standby'
                        THEN gpu_count::numeric * interval_ms::numeric * rate_cents_per_hour
                        ELSE 0
                    END) / 3600000)::bigint AS inference_cents,
                    (SUM(CASE WHEN usage_type = 'standby'
                        THEN gpu_count::numeric * interval_ms::numeric * rate_cents_per_hour
                        ELSE 0
                    END) / 3600000)::bigint AS standby_cents,
                    SUM(CASE WHEN usage_type != 'standby'
                        THEN interval_ms::bigint * gpu_count::bigint
                        ELSE 0
                    END)::bigint AS inference_ms,
                    SUM(CASE WHEN usage_type = 'standby'
                        THEN interval_ms::bigint * gpu_count::bigint
                        ELSE 0
                    END)::bigint AS standby_ms
                FROM recent_ticks
                GROUP BY hour
            )
            SELECT
                hour,
                charge_cents,
                inference_cents,
                standby_cents,
                inference_ms,
                standby_ms
            FROM historical
            UNION ALL
            SELECT
                hour,
                charge_cents,
                inference_cents,
                standby_cents,
                inference_ms,
                standby_ms
            FROM recent
            ORDER BY hour ASC
            "#,
        )
            .bind(tenant)
            .bind(namespace)
            .bind(funcname)
            .bind(start)
            .bind(end)
            .fetch_all(&self.pool)
            .await?;

        Ok(records)
    }

    /// Get tenant hourly usage for a UTC time range [start, end] (v4: returns cost + time breakdown)
    pub async fn GetTenantHourlyUsageRange(
        &self,
        tenant: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<(DateTime<Utc>, i64, i64, i64, i64, i64)>> {
        let records: Vec<(DateTime<Utc>, i64, i64, i64, i64, i64)> = sqlx::query_as(
            r#"
            WITH
            params AS (
                SELECT
                    $1::varchar AS tenant,
                    $2::timestamptz AS start_hour,
                    $3::timestamptz AS end_hour,
                    (date_trunc('hour', NOW()) - INTERVAL '23 hours') AS recent_start_hour
            ),
            historical AS (
                SELECT
                    h.hour,
                    SUM(h.charge_cents)::bigint AS charge_cents,
                    SUM(h.inference_cents)::bigint AS inference_cents,
                    SUM(h.standby_cents)::bigint AS standby_cents,
                    SUM(h.inference_ms)::bigint AS inference_ms,
                    SUM(h.standby_ms)::bigint AS standby_ms
                FROM UsageHourlyByFunc h
                JOIN params p ON true
                WHERE h.tenant = p.tenant
                  AND h.hour >= p.start_hour
                  AND h.hour <= p.end_hour
                  AND h.hour < p.recent_start_hour
                GROUP BY h.hour
            ),
            recent_ticks AS (
                SELECT
                    date_trunc('hour', t.tick_time) AS hour,
                    t.usage_type,
                    t.interval_ms,
                    t.gpu_count,
                    GetBillingRateCents(t.usage_type, t.tick_time, t.tenant)::numeric AS rate_cents_per_hour
                FROM UsageTick t
                JOIN params p ON true
                WHERE t.tenant = p.tenant
                  AND t.tick_time >= GREATEST(p.start_hour, p.recent_start_hour)
                  AND t.tick_time < LEAST(p.end_hour + INTERVAL '1 hour', NOW())
            ),
            recent AS (
                SELECT
                    hour,
                    (SUM(gpu_count::numeric * interval_ms::numeric * rate_cents_per_hour) / 3600000)::bigint AS charge_cents,
                    (SUM(CASE WHEN usage_type != 'standby'
                        THEN gpu_count::numeric * interval_ms::numeric * rate_cents_per_hour
                        ELSE 0
                    END) / 3600000)::bigint AS inference_cents,
                    (SUM(CASE WHEN usage_type = 'standby'
                        THEN gpu_count::numeric * interval_ms::numeric * rate_cents_per_hour
                        ELSE 0
                    END) / 3600000)::bigint AS standby_cents,
                    SUM(CASE WHEN usage_type != 'standby'
                        THEN interval_ms::bigint * gpu_count::bigint
                        ELSE 0
                    END)::bigint AS inference_ms,
                    SUM(CASE WHEN usage_type = 'standby'
                        THEN interval_ms::bigint * gpu_count::bigint
                        ELSE 0
                    END)::bigint AS standby_ms
                FROM recent_ticks
                GROUP BY hour
            )
            SELECT
                hour,
                charge_cents,
                inference_cents,
                standby_cents,
                inference_ms,
                standby_ms
            FROM historical
            UNION ALL
            SELECT
                hour,
                charge_cents,
                inference_cents,
                standby_cents,
                inference_ms,
                standby_ms
            FROM recent
            ORDER BY hour ASC
            "#,
        )
            .bind(tenant)
            .bind(start)
            .bind(end)
            .fetch_all(&self.pool)
            .await?;

        Ok(records)
    }

    /// Get tenant usage by model as a full list sorted by total cost (charge_cents), descending.
    /// Returns:
    /// (
    ///   items[(funcname, namespace, inference_ms, standby_ms, inference_cents, standby_cents, charge_cents)],
    ///   total(inference_ms, standby_ms, inference_cents, standby_cents, charge_cents),
    /// )
    pub async fn GetTenantUsageByModel(
        &self,
        tenant: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<(
        Vec<(String, String, i64, i64, i64, i64, i64)>,
        (i64, i64, i64, i64, i64),
    )> {
        let items: Vec<(String, String, i64, i64, i64, i64, i64)> = sqlx::query_as(
            r#"
            WITH
            params AS (
                SELECT
                    $1::varchar AS tenant,
                    $2::timestamptz AS start_hour,
                    $3::timestamptz AS end_hour,
                    date_trunc('hour', NOW()) - INTERVAL '3 hours' AS recent_start
            ),
            historical AS (
                SELECT
                    h.funcname,
                    h.namespace,
                    h.inference_ms,
                    h.standby_ms,
                    h.inference_cents,
                    h.standby_cents,
                    h.charge_cents
                FROM UsageHourlyByFunc h
                JOIN params p ON true
                WHERE h.tenant = p.tenant
                  AND h.hour >= p.start_hour
                  AND h.hour <= p.end_hour
                  AND h.hour < p.recent_start
            ),
            recent_ticks AS (
                SELECT
                    t.funcname,
                    t.namespace,
                    t.usage_type,
                    t.interval_ms::numeric * t.gpu_count::numeric AS gpu_ms,
                    GetBillingRateCents(t.usage_type, t.tick_time, t.tenant)::numeric AS rate_cents_per_hour
                FROM UsageTick t
                JOIN params p ON true
                WHERE t.tenant = p.tenant
                  AND t.tick_time >= GREATEST(p.start_hour, p.recent_start)
                  AND t.tick_time < LEAST(p.end_hour + INTERVAL '1 hour', NOW())
            ),
            combined AS (
                SELECT
                    funcname,
                    namespace,
                    inference_ms,
                    standby_ms,
                    inference_cents,
                    standby_cents,
                    charge_cents
                FROM historical
                UNION ALL
                SELECT
                    funcname,
                    namespace,
                    SUM(CASE WHEN usage_type != 'standby' THEN gpu_ms ELSE 0 END)::bigint AS inference_ms,
                    SUM(CASE WHEN usage_type = 'standby' THEN gpu_ms ELSE 0 END)::bigint AS standby_ms,
                    (SUM(CASE WHEN usage_type != 'standby'
                        THEN gpu_ms * rate_cents_per_hour
                        ELSE 0
                    END) / 3600000)::bigint AS inference_cents,
                    (SUM(CASE WHEN usage_type = 'standby'
                        THEN gpu_ms * rate_cents_per_hour
                        ELSE 0
                    END) / 3600000)::bigint AS standby_cents,
                    (SUM(gpu_ms * rate_cents_per_hour) / 3600000)::bigint AS charge_cents
                FROM recent_ticks
                GROUP BY funcname, namespace
            ),
            grouped AS (
                SELECT
                    funcname,
                    namespace,
                    SUM(inference_ms)::bigint AS inference_ms,
                    SUM(standby_ms)::bigint AS standby_ms,
                    SUM(inference_cents)::bigint AS inference_cents,
                    SUM(standby_cents)::bigint AS standby_cents,
                    SUM(charge_cents)::bigint AS charge_cents
                FROM combined
                GROUP BY funcname, namespace
            )
            SELECT
                funcname,
                namespace,
                inference_ms,
                standby_ms,
                inference_cents,
                standby_cents,
                charge_cents
            FROM grouped
            ORDER BY
                charge_cents DESC,
                funcname ASC,
                namespace ASC
            "#,
        )
        .bind(tenant)
        .bind(start)
        .bind(end)
        .fetch_all(&self.pool)
        .await?;

        let mut total: (i64, i64, i64, i64, i64) = (0, 0, 0, 0, 0);

        for row in &items {
            total.0 += row.2;
            total.1 += row.3;
            total.2 += row.4;
            total.3 += row.5;
            total.4 += row.6;
        }

        Ok((items, total))
    }

    /// Get tenant usage by namespace as a full list sorted by total cost (charge_cents), descending.
    /// Returns:
    /// (
    ///   items[(namespace, inference_ms, standby_ms, inference_cents, standby_cents, charge_cents)],
    ///   total(inference_ms, standby_ms, inference_cents, standby_cents, charge_cents),
    /// )
    pub async fn GetTenantUsageByNamespace(
        &self,
        tenant: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<(
        Vec<(String, i64, i64, i64, i64, i64)>,
        (i64, i64, i64, i64, i64),
    )> {
        let items: Vec<(String, i64, i64, i64, i64, i64)> = sqlx::query_as(
            r#"
            WITH
            params AS (
                SELECT
                    $1::varchar AS tenant,
                    $2::timestamptz AS start_hour,
                    $3::timestamptz AS end_hour,
                    date_trunc('hour', NOW()) - INTERVAL '3 hours' AS recent_start
            ),
            historical AS (
                SELECT
                    h.namespace,
                    h.inference_ms,
                    h.standby_ms,
                    h.inference_cents,
                    h.standby_cents,
                    h.charge_cents
                FROM UsageHourlyByFunc h
                JOIN params p ON true
                WHERE h.tenant = p.tenant
                  AND h.hour >= p.start_hour
                  AND h.hour <= p.end_hour
                  AND h.hour < p.recent_start
            ),
            recent_ticks AS (
                SELECT
                    t.namespace,
                    t.usage_type,
                    t.interval_ms::numeric * t.gpu_count::numeric AS gpu_ms,
                    GetBillingRateCents(t.usage_type, t.tick_time, t.tenant)::numeric AS rate_cents_per_hour
                FROM UsageTick t
                JOIN params p ON true
                WHERE t.tenant = p.tenant
                  AND t.tick_time >= GREATEST(p.start_hour, p.recent_start)
                  AND t.tick_time < LEAST(p.end_hour + INTERVAL '1 hour', NOW())
            ),
            combined AS (
                SELECT
                    namespace,
                    inference_ms,
                    standby_ms,
                    inference_cents,
                    standby_cents,
                    charge_cents
                FROM historical
                UNION ALL
                SELECT
                    namespace,
                    SUM(CASE WHEN usage_type != 'standby' THEN gpu_ms ELSE 0 END)::bigint AS inference_ms,
                    SUM(CASE WHEN usage_type = 'standby' THEN gpu_ms ELSE 0 END)::bigint AS standby_ms,
                    (SUM(CASE WHEN usage_type != 'standby'
                        THEN gpu_ms * rate_cents_per_hour
                        ELSE 0
                    END) / 3600000)::bigint AS inference_cents,
                    (SUM(CASE WHEN usage_type = 'standby'
                        THEN gpu_ms * rate_cents_per_hour
                        ELSE 0
                    END) / 3600000)::bigint AS standby_cents,
                    (SUM(gpu_ms * rate_cents_per_hour) / 3600000)::bigint AS charge_cents
                FROM recent_ticks
                GROUP BY namespace
            ),
            grouped AS (
                SELECT
                    namespace,
                    SUM(inference_ms)::bigint AS inference_ms,
                    SUM(standby_ms)::bigint AS standby_ms,
                    SUM(inference_cents)::bigint AS inference_cents,
                    SUM(standby_cents)::bigint AS standby_cents,
                    SUM(charge_cents)::bigint AS charge_cents
                FROM combined
                GROUP BY namespace
            )
            SELECT
                namespace,
                inference_ms,
                standby_ms,
                inference_cents,
                standby_cents,
                charge_cents
            FROM grouped
            ORDER BY
                charge_cents DESC,
                namespace ASC
            "#,
        )
        .bind(tenant)
        .bind(start)
        .bind(end)
        .fetch_all(&self.pool)
        .await?;

        let mut total: (i64, i64, i64, i64, i64) = (0, 0, 0, 0, 0);

        for row in &items {
            total.0 += row.1;
            total.1 += row.2;
            total.2 += row.3;
            total.3 += row.4;
            total.4 += row.5;
        }

        Ok((items, total))
    }

    /// Get tenant analytics summary for summary cards
    /// Returns: (total_ms, total_cents, top_model_name, top_model_ms, top_namespace, top_namespace_ms, peak_hour, peak_hour_ms)
    pub async fn GetTenantAnalyticsSummary(
        &self,
        tenant: &str,
        hours: i32,
    ) -> Result<(i64, i64, Option<String>, i64, Option<String>, i64, Option<DateTime<Utc>>, i64)> {
        let row: (i64, i64, Option<String>, i64, Option<String>, i64, Option<DateTime<Utc>>, i64) = sqlx::query_as(
            r#"
            WITH
            params AS (
                SELECT
                    $1::varchar AS tenant,
                    NOW() - ($2 || ' hours')::interval AS cutoff,
                    date_trunc('hour', NOW()) - INTERVAL '3 hours' AS recent_start
            ),
            historical AS (
                SELECT h.funcname, h.namespace, h.hour, h.inference_ms, h.inference_cents
                FROM UsageHourlyByFunc h
                JOIN params p ON true
                WHERE h.tenant = p.tenant
                  AND h.hour >= p.cutoff
                  AND h.hour < p.recent_start
            ),
            recent_ticks AS (
                SELECT
                    t.funcname,
                    t.namespace,
                    date_trunc('hour', t.tick_time) AS hour,
                    t.interval_ms::numeric * t.gpu_count::numeric AS gpu_ms,
                    t.gpu_count::numeric * t.interval_ms::numeric
                        * GetBillingRateCents(t.usage_type, t.tick_time, t.tenant)::numeric
                        AS raw_cost
                FROM UsageTick t
                JOIN params p ON true
                WHERE t.tenant = p.tenant
                  AND t.usage_type != 'standby'
                  AND t.tick_time >= GREATEST(p.cutoff, p.recent_start)
                  AND t.tick_time < NOW()
            ),
            combined AS (
                SELECT funcname, namespace, hour, inference_ms, inference_cents
                FROM historical
                UNION ALL
                SELECT funcname, namespace, hour,
                       SUM(gpu_ms)::bigint AS inference_ms,
                       (SUM(raw_cost) / 3600000)::bigint AS inference_cents
                FROM recent_ticks
                GROUP BY funcname, namespace, hour
            ),
            period_usage AS (
                SELECT
                    funcname,
                    namespace,
                    hour,
                    SUM(inference_ms)::bigint AS inference_ms,
                    SUM(inference_cents)::bigint AS charge_cents
                FROM combined
                GROUP BY funcname, namespace, hour
            ),
            total AS (
                SELECT
                    COALESCE(SUM(inference_ms), 0)::bigint AS total_ms,
                    COALESCE(SUM(charge_cents), 0)::bigint AS total_cents
                FROM period_usage
            ),
            top_model AS (
                SELECT funcname, SUM(inference_ms)::bigint AS inference_ms
                FROM period_usage
                GROUP BY funcname
                -- Rank by inference GPU usage first (higher is better).
                ORDER BY inference_ms DESC, funcname ASC
                LIMIT 1
            ),
            top_namespace AS (
                SELECT namespace, SUM(inference_ms)::bigint AS inference_ms
                FROM period_usage
                GROUP BY namespace
                ORDER BY inference_ms DESC, namespace ASC
                LIMIT 1
            ),
            peak_hour AS (
                SELECT hour, SUM(inference_ms)::bigint AS inference_ms
                FROM period_usage
                GROUP BY hour
                ORDER BY inference_ms DESC, hour ASC
                LIMIT 1
            )
            SELECT
                t.total_ms,
                t.total_cents,
                tm.funcname AS top_model_name,
                COALESCE(tm.inference_ms, 0)::bigint AS top_model_ms,
                tn.namespace AS top_namespace_name,
                COALESCE(tn.inference_ms, 0)::bigint AS top_namespace_ms,
                ph.hour AS peak_hour,
                COALESCE(ph.inference_ms, 0)::bigint AS peak_hour_ms
            FROM total t
            LEFT JOIN top_model tm ON true
            LEFT JOIN top_namespace tn ON true
            LEFT JOIN peak_hour ph ON true
            "#,
        )
        .bind(tenant)
        .bind(hours)
        .fetch_one(&self.pool)
        .await?;

        Ok(row)
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
