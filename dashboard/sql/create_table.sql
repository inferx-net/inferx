

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
-- Billing v4: Money-Based Credits with Independent Standby
-- ============================================================================

-- Usage Tick - each row represents a billing interval (raw data only)
-- DROP TABLE UsageTick;
CREATE TABLE UsageTick (
    id              SERIAL PRIMARY KEY,
    session_id      VARCHAR(64) NOT NULL,
    tenant          VARCHAR NOT NULL,
    namespace       VARCHAR NOT NULL,
    funcname        VARCHAR NOT NULL,
    fprevision      BIGINT NOT NULL,        -- model version
    nodename        VARCHAR,                -- NULL for standby ticks
    pod_id          BIGINT,                 -- NULL for standby ticks
    gateway_id      BIGINT,
    gpu_type        VARCHAR NOT NULL,
    gpu_count       INT NOT NULL,
    vram_mb         BIGINT NOT NULL,
    total_vram_mb   BIGINT NOT NULL,
    tick_time       TIMESTAMPTZ NOT NULL,
    interval_ms     BIGINT NOT NULL,
    tick_type       VARCHAR(16) NOT NULL,   -- 'start', 'periodic', 'final'
    usage_type      VARCHAR(16) NOT NULL,   -- 'request', 'snapshot', or 'standby'
    is_coldstart    BOOLEAN DEFAULT FALSE,
    processed_at    TIMESTAMPTZ             -- NULL = unprocessed, set when billing processed
);

CREATE INDEX idx_tick_tenant ON UsageTick(tenant, tick_time);
CREATE INDEX idx_tick_session ON UsageTick(session_id);
CREATE INDEX idx_tick_unprocessed ON UsageTick(id) WHERE processed_at IS NULL;

-- Tenant Quota - prepaid credit tracking (USD cents)
-- DROP TABLE TenantQuota;
CREATE TABLE TenantQuota (
    tenant                 VARCHAR PRIMARY KEY,
    balance_cents          BIGINT DEFAULT 0,       -- Stored explicitly (total_credits - used)
    used_cents             BIGINT DEFAULT 0,       -- Cumulative usage in cents
    inference_used_cents   BIGINT NOT NULL DEFAULT 0, -- Cumulative inference usage in cents
    standby_used_cents     BIGINT NOT NULL DEFAULT 0, -- Cumulative standby usage in cents
    threshold_cents        BIGINT DEFAULT 0,       -- Disable when remaining < this
    quota_exceeded         BOOLEAN DEFAULT FALSE,
    currency               VARCHAR(3) DEFAULT 'USD'
);

-- Tenant Credit History - audit trail for credits added (USD cents)
-- DROP TABLE TenantCreditHistory;
CREATE TABLE TenantCreditHistory (
    id              SERIAL PRIMARY KEY,
    tenant          VARCHAR NOT NULL,
    amount_cents    BIGINT NOT NULL,
    currency        VARCHAR(3) NOT NULL DEFAULT 'USD',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    note            VARCHAR,
    payment_ref     VARCHAR,
    added_by        VARCHAR
);

CREATE INDEX idx_credit_tenant ON TenantCreditHistory(tenant, created_at);

-- Usage Hourly - tenant-level aggregation (commented out, kept for future reference)
-- Replaced by UsageHourlyByFunc which supports per-function analytics.
-- DROP TABLE UsageHourly;
-- CREATE TABLE UsageHourly (
--     id               SERIAL PRIMARY KEY,
--     tenant           VARCHAR NOT NULL,
--     hour             TIMESTAMPTZ NOT NULL,
--     charge_cents     BIGINT NOT NULL,
--     inference_cents  BIGINT NOT NULL DEFAULT 0,
--     standby_cents    BIGINT NOT NULL DEFAULT 0,
--     inference_ms     BIGINT NOT NULL DEFAULT 0,
--     standby_ms       BIGINT NOT NULL DEFAULT 0,
--     UNIQUE(tenant, hour)
-- );
-- CREATE INDEX idx_hourly_tenant ON UsageHourly(tenant, hour);

-- Per-function hourly aggregation — replaces UsageHourly for all queries
-- DROP TABLE UsageHourlyByFunc;
CREATE TABLE UsageHourlyByFunc (
    id               SERIAL PRIMARY KEY,
    tenant           VARCHAR NOT NULL,
    namespace        VARCHAR NOT NULL,
    funcname         VARCHAR NOT NULL,
    fprevision       BIGINT NOT NULL,
    hour             TIMESTAMPTZ NOT NULL,
    charge_cents     BIGINT NOT NULL,
    inference_cents  BIGINT NOT NULL DEFAULT 0,
    standby_cents    BIGINT NOT NULL DEFAULT 0,
    inference_ms     BIGINT NOT NULL DEFAULT 0,
    standby_ms       BIGINT NOT NULL DEFAULT 0,
    UNIQUE(tenant, namespace, funcname, fprevision, hour)
);

CREATE INDEX idx_hourly_func_tenant ON UsageHourlyByFunc(tenant, hour);
CREATE INDEX idx_hourly_func_ns ON UsageHourlyByFunc(tenant, namespace, hour);
CREATE INDEX idx_hourly_func_model ON UsageHourlyByFunc(tenant, namespace, funcname, hour);

-- Billing Rate - configurable rates with effective dates
-- Enables rate changes and per-tenant pricing without code changes
-- DROP TABLE BillingRate;
CREATE TABLE BillingRate (
    id                  SERIAL PRIMARY KEY,
    usage_type          VARCHAR(16) NOT NULL,   -- 'inference' or 'standby'
    rate_cents_per_hour INT NOT NULL,           -- per GPU-hour
    effective_from      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    effective_to        TIMESTAMPTZ,            -- NULL = currently active
    tenant              VARCHAR,                -- NULL = global default, set = per-tenant override
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    added_by            VARCHAR NOT NULL
);

CREATE INDEX idx_rate_lookup ON BillingRate(usage_type, effective_from);

-- Shared rate lookup used by quota check, hourly aggregation, and analytics.
-- Prefers tenant-specific rate over global rate, and returns 0 when no match.
CREATE OR REPLACE FUNCTION GetBillingRateCents(
    p_usage_type VARCHAR,
    p_tick_time TIMESTAMPTZ,
    p_tenant VARCHAR
) RETURNS INT
LANGUAGE SQL
STABLE
AS $$
SELECT COALESCE((
    SELECT r.rate_cents_per_hour
    FROM BillingRate r
    WHERE r.usage_type = CASE
            WHEN p_usage_type = 'standby' THEN 'standby'
            ELSE 'inference'
        END
      AND r.effective_from <= p_tick_time
      AND (r.effective_to IS NULL OR p_tick_time < r.effective_to)
      AND (r.tenant IS NULL OR r.tenant = p_tenant)
    ORDER BY (r.tenant IS NOT NULL) DESC, r.effective_from DESC
    LIMIT 1
), 0);
$$;

-- Initial global rates
INSERT INTO BillingRate (usage_type, rate_cents_per_hour, effective_from, added_by)
VALUES ('inference', 800, '2025-01-01', 'system'),
       ('standby',   20, '2025-01-01', 'system');
