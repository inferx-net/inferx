-- Billing schema only

-- Usage Tick - each row represents a billing interval (raw data only)
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
CREATE TABLE TenantQuota (
    tenant                 VARCHAR PRIMARY KEY,
    balance_cents          BIGINT DEFAULT 0,          -- Stored explicitly (total_credits - used)
    used_cents             BIGINT DEFAULT 0,          -- Cumulative usage in cents
    inference_used_cents   BIGINT NOT NULL DEFAULT 0, -- Cumulative inference usage in cents
    standby_used_cents     BIGINT NOT NULL DEFAULT 0, -- Cumulative standby usage in cents
    threshold_cents        BIGINT DEFAULT 0,          -- Disable when remaining < this
    quota_exceeded         BOOLEAN DEFAULT FALSE,
    currency               VARCHAR(3) DEFAULT 'USD'
);

-- Tenant Credit History - audit trail for credits added (USD cents)
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

-- Per-function hourly aggregation
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

-- Initial global rates (idempotent)
INSERT INTO BillingRate (usage_type, rate_cents_per_hour, effective_from, added_by)
SELECT 'inference', 850, '2025-01-01', 'system'
WHERE NOT EXISTS (
    SELECT 1
    FROM BillingRate
    WHERE usage_type = 'inference'
      AND tenant IS NULL
      AND effective_from = '2025-01-01'::timestamptz
);

INSERT INTO BillingRate (usage_type, rate_cents_per_hour, effective_from, added_by)
SELECT 'standby', 10, '2025-01-01', 'system'
WHERE NOT EXISTS (
    SELECT 1
    FROM BillingRate
    WHERE usage_type = 'standby'
      AND tenant IS NULL
      AND effective_from = '2025-01-01'::timestamptz
);
