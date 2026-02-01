

--DROP TABLE Pod;
CREATE TABLE Pod (
    tenant          VARCHAR NOT NULL,
    namespace       VARCHAR NOT NULL,
    fpname          VARCHAR NOT NULL,
    fprevision      bigint,
    id              VARCHAR NOT NULL,
    podtype         VARCHAR NOT NULL,
    nodename        VARCHAR NOT NULL,
    state           VARCHAR NOT NULL,
    updatetime      TIMESTAMPTZ,
    PRIMARY KEY(tenant, namespace, fpname, fprevision, podtype, nodename, id)
);

--DROP TABLE PodAudit;
CREATE TABLE PodAudit (
    tenant          VARCHAR NOT NULL,
    namespace       VARCHAR NOT NULL,
    fpname          VARCHAR NOT NULL,
    fprevision      bigint,
    id              VARCHAR NOT NULL,
    nodename        VARCHAR NOT NULL,
    action          VARCHAR NOT NULL,
    state           VARCHAR NOT NULL,
    updatetime      TIMESTAMPTZ,
    PRIMARY KEY(tenant, namespace, fpname, fprevision, id, updatetime)
);

--DROP TABLE PodFailLog;
CREATE TABLE PodFailLog (
    tenant          VARCHAR NOT NULL,
    namespace       VARCHAR NOT NULL,
    fpname          VARCHAR NOT NULL,
    fprevision      bigint,
    id              VARCHAR NOT NULL,
    state           VARCHAR NOT NULL,
    nodename        VARCHAR NOT NULL,
    createtime      TIMESTAMPTZ,
    log             VARCHAR NOT NULL,
    exit_info       VARCHAR NOT NULL,
    PRIMARY KEY(tenant, namespace, fpname, fprevision, id)
);

--DROP TABLE FuncState;
CREATE TABLE FuncState (
    tenant          VARCHAR NOT NULL,
    namespace       VARCHAR NOT NULL,
    fpname          VARCHAR NOT NULL,
    fprevision      bigint,
    state           VARCHAR NOT NULL,
    updatetime      TIMESTAMPTZ,
    PRIMARY KEY(tenant, namespace, fpname, fprevision, updatetime)
);

--DROP TABLE ReqAudit;
CREATE TABLE ReqAudit (
    seqid           SERIAL PRIMARY KEY, 
    podkey          VARCHAR NOT NULL,
    audittime       TIMESTAMP,
    keepalive       bool,
    ttft            int,            -- Time to First Token
    latency         int
);

-- DROP TABLE SnapshotScheduleAudit;
CREATE TABLE SnapshotScheduleAudit (
    tenant          VARCHAR NOT NULL,
    namespace       VARCHAR NOT NULL,
    funcname        VARCHAR NOT NULL,
    revision        bigint,
    nodename        VARCHAR NOT NULL,
    state           VARCHAR NOT NULL,
    detail          VARCHAR NOT NULL,
    updatetime      TIMESTAMPTZ,
    PRIMARY KEY(tenant, namespace, funcname, revision, nodename, state)
);

-- CREATE INDEX idx_snapshot_audit
-- ON SnapshotScheduleAudit (tenant, namespace, funcname, revision, nodename);

CREATE OR REPLACE FUNCTION notification_trigger() RETURNS TRIGGER AS 
$$
BEGIN
    PERFORM pg_notify('ReqAudit_insert', 
            to_json(NEW)::TEXT
    );
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE TRIGGER capture_change_trigger AFTER INSERT OR UPDATE OR DELETE ON ReqAudit
FOR EACH ROW EXECUTE FUNCTION notification_trigger();


-- CREATE USER audit_user WITH PASSWORD '123456';
-- GRANT ALL ON ALL TABLES IN SCHEMA public to audit_user;
-- GRANT USAGE ON SEQUENCE reqaudit_seqid_seq TO audit_user;

-- https://stackoverflow.com/questions/18664074/getting-error-peer-authentication-failed-for-user-postgres-when-trying-to-ge

-- ============================================================================
-- GPU Quota & Billing (Periodic Tick)
-- ============================================================================

-- GPU Usage Tick - each row represents a billing interval (raw data only)
-- DROP TABLE GpuUsageTick;
CREATE TABLE GpuUsageTick (
    id              SERIAL PRIMARY KEY,
    session_id      VARCHAR(64) NOT NULL,
    tenant          VARCHAR NOT NULL,
    namespace       VARCHAR NOT NULL,
    funcname        VARCHAR NOT NULL,
    nodename        VARCHAR NOT NULL,
    pod_id          VARCHAR NOT NULL,
    gateway_id      BIGINT,
    gpu_type        VARCHAR NOT NULL,
    gpu_count       INT NOT NULL,
    vram_mb         BIGINT NOT NULL,
    total_vram_mb   BIGINT NOT NULL,
    tick_time       TIMESTAMPTZ NOT NULL,
    interval_ms     BIGINT NOT NULL,
    tick_type       VARCHAR(16) NOT NULL,   -- 'start', 'periodic', 'final'
    usage_type      VARCHAR(16) NOT NULL,   -- 'request' or 'snapshot'
    is_coldstart    BOOLEAN DEFAULT FALSE,
    processed_at    TIMESTAMPTZ             -- NULL = unprocessed, set when billing processed
);

CREATE INDEX idx_tick_tenant ON GpuUsageTick(tenant, tick_time);
CREATE INDEX idx_tick_session ON GpuUsageTick(session_id);
CREATE INDEX idx_tick_unprocessed ON GpuUsageTick(id) WHERE processed_at IS NULL;

-- Tenant Quota - prepaid credit tracking
-- DROP TABLE TenantQuota;
CREATE TABLE TenantQuota (
    tenant                 VARCHAR PRIMARY KEY,
    used_ms                BIGINT DEFAULT 0,       -- Cumulative usage (from processed GpuUsageTick)
    threshold_ms           BIGINT DEFAULT 0,       -- Disable when remaining < this
    quota_exceeded         BOOLEAN DEFAULT FALSE
);
-- Note: credits are calculated as SUM(TenantCreditHistory.amount_ms) each time (rare, no race)

-- Tenant Credit History - audit trail for credits added
-- DROP TABLE TenantCreditHistory;
CREATE TABLE TenantCreditHistory (
    id              SERIAL PRIMARY KEY,
    tenant          VARCHAR NOT NULL,
    amount_ms       BIGINT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    note            VARCHAR,
    payment_ref     VARCHAR,
    added_by        VARCHAR
);

CREATE INDEX idx_credit_tenant ON TenantCreditHistory(tenant, created_at);

-- GPU Usage Hourly - pre-aggregated for dashboard queries
-- DROP TABLE GpuUsageHourly;
CREATE TABLE GpuUsageHourly (
    id              SERIAL PRIMARY KEY,
    tenant          VARCHAR NOT NULL,
    hour            TIMESTAMPTZ NOT NULL,
    usage_ms        BIGINT NOT NULL,
    UNIQUE(tenant, hour)
);

CREATE INDEX idx_hourly_tenant ON GpuUsageHourly(tenant, hour);
