-- Billing schema only

-- Usage Tick - each row represents a billing interval (raw data only)
CREATE TABLE UsageTick (
    id              SERIAL PRIMARY KEY,
    session_id      VARCHAR(64) NOT NULL,
    tenant          VARCHAR NOT NULL,
    caller_tenant   VARCHAR,
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
CREATE INDEX idx_tick_caller_tenant ON UsageTick(caller_tenant, tick_time);
CREATE INDEX idx_tick_session ON UsageTick(session_id);
CREATE INDEX idx_tick_unprocessed ON UsageTick(id) WHERE processed_at IS NULL;

-- Tenant Quota - prepaid credit tracking (USD cents)
CREATE TABLE TenantQuota (
    tenant                 VARCHAR PRIMARY KEY,
    balance_cents          BIGINT DEFAULT 0,          -- Stored explicitly (total_credits - used)
    used_cents             BIGINT DEFAULT 0,          -- Cumulative usage in cents
    inference_used_cents   BIGINT NOT NULL DEFAULT 0, -- Cumulative inference usage in cents
    standby_used_cents     BIGINT NOT NULL DEFAULT 0, -- Cumulative standby usage in cents
    token_used_cents       BIGINT NOT NULL DEFAULT 0, -- Cumulative token usage in cents
    inference_carry_numer  BIGINT NOT NULL DEFAULT 0, -- Exact inference numerator remainder / 3600000
    standby_carry_numer    BIGINT NOT NULL DEFAULT 0, -- Exact standby numerator remainder / 3600000
    token_carry_numer      BIGINT NOT NULL DEFAULT 0, -- Exact token numerator remainder / 1000000
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
    inference_numer  BIGINT NOT NULL DEFAULT 0,
    standby_numer    BIGINT NOT NULL DEFAULT 0,
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

-- ===========================================================================
-- Token-based billing for shared instances. These parallel the GPU-time tables
-- above, but token-shaped: per-request events priced per-million-tokens, keyed
-- on endpoint slug.
-- ===========================================================================

-- Raw per-request token usage. Append-only; PK gives idempotency.
CREATE TABLE TokenUsageEvent (
    gateway_request_id VARCHAR(128) PRIMARY KEY,  -- gateway-minted UUID (dedup key)
    client_request_id  VARCHAR(128),              -- caller X-Request-Id, for support only
    caller_tenant      VARCHAR NOT NULL,          -- paying tenant (direct or OpenRouter)
    model_slug         VARCHAR NOT NULL,          -- inferx/endpoint func name (= request `model`)
    source             VARCHAR(16) NOT NULL,      -- 'direct' | 'openrouter'
    prompt_tokens      BIGINT NOT NULL DEFAULT 0,
    completion_tokens  BIGINT NOT NULL DEFAULT 0,
    cached_tokens      BIGINT NOT NULL DEFAULT 0, -- subset of prompt_tokens
    ts                 TIMESTAMPTZ NOT NULL,      -- gateway request time; picks the rate (no DEFAULT: never insert time)
    gateway_id         BIGINT,
    processed_at       TIMESTAMPTZ                -- NULL until billed; no-rate rows stay NULL and self-heal
);

CREATE INDEX idx_tokenevent_tenant      ON TokenUsageEvent(caller_tenant, ts);
CREATE INDEX idx_tokenevent_model       ON TokenUsageEvent(model_slug, ts);
CREATE INDEX idx_tokenevent_unprocessed ON TokenUsageEvent(ts) WHERE processed_at IS NULL;

-- Per-token price book, keyed on endpoint slug. Effective-dated like BillingRate.
CREATE TABLE TokenRate (
    id                        SERIAL PRIMARY KEY,
    model_slug                VARCHAR,               -- NULL = global default
    cents_per_million_input   BIGINT NOT NULL,
    cents_per_million_output  BIGINT NOT NULL,
    cents_per_million_cached  BIGINT NOT NULL DEFAULT 0,
    effective_from            TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    effective_to              TIMESTAMPTZ,           -- NULL = currently active
    tenant                    VARCHAR,               -- NULL = default; set = per-tenant override
    created_at                TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    added_by                  VARCHAR NOT NULL
);

CREATE INDEX idx_tokenrate_lookup ON TokenRate(model_slug, effective_from);

-- Hourly rollup per tenant x endpoint. Reporting only; the *_numer columns hold
-- exact undivided numerators so invoices sum then divide once.
CREATE TABLE TokenUsageHourly (
    id                 SERIAL PRIMARY KEY,
    tenant             VARCHAR NOT NULL,
    model_slug         VARCHAR NOT NULL,
    hour               TIMESTAMPTZ NOT NULL,
    input_tokens       BIGINT NOT NULL DEFAULT 0,   -- billable input = prompt - cached
    cached_tokens      BIGINT NOT NULL DEFAULT 0,
    output_tokens      BIGINT NOT NULL DEFAULT 0,
    request_count      BIGINT NOT NULL DEFAULT 0,
    input_numer        BIGINT NOT NULL DEFAULT 0,
    output_numer       BIGINT NOT NULL DEFAULT 0,
    cached_numer       BIGINT NOT NULL DEFAULT 0,
    charge_cents       BIGINT NOT NULL DEFAULT 0,   -- display only
    UNIQUE(tenant, model_slug, hour)
);

CREATE INDEX idx_tokenhourly_tenant ON TokenUsageHourly(tenant, hour);

-- Event-time-bound rate lookup, mirroring GetBillingRateCents. Returns the three
-- cents-per-million values, or no row when no rate is active (caller: skip + alert).
CREATE OR REPLACE FUNCTION GetTokenRateCents(
    p_model_slug VARCHAR,
    p_ts TIMESTAMPTZ,
    p_tenant VARCHAR
) RETURNS TABLE (
    cents_per_million_input  BIGINT,
    cents_per_million_output BIGINT,
    cents_per_million_cached BIGINT
)
LANGUAGE SQL
STABLE
AS $$
    SELECT
        r.cents_per_million_input,
        r.cents_per_million_output,
        r.cents_per_million_cached
    FROM TokenRate r
    WHERE (r.model_slug = p_model_slug OR r.model_slug IS NULL)
      AND (r.tenant     = p_tenant     OR r.tenant IS NULL)
      AND r.effective_from <= p_ts
      AND (r.effective_to IS NULL OR p_ts < r.effective_to)
    ORDER BY (r.tenant IS NOT NULL) DESC,
             (r.model_slug IS NOT NULL) DESC,
             r.effective_from DESC
    LIMIT 1;
$$;
