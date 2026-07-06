-- Token-based billing migration: new token tables + TenantQuota carry columns.
-- Idempotent; safe to re-run.
\set ON_ERROR_STOP on

ALTER TABLE TenantQuota
  ADD COLUMN IF NOT EXISTS token_used_cents  BIGINT NOT NULL DEFAULT 0,
  ADD COLUMN IF NOT EXISTS token_carry_numer BIGINT NOT NULL DEFAULT 0;

CREATE TABLE IF NOT EXISTS TokenUsageEvent (
    gateway_request_id VARCHAR(128) PRIMARY KEY,
    client_request_id  VARCHAR(128),
    caller_tenant      VARCHAR NOT NULL,
    model_slug         VARCHAR NOT NULL,
    source             VARCHAR(16) NOT NULL,
    prompt_tokens      BIGINT NOT NULL DEFAULT 0,
    completion_tokens  BIGINT NOT NULL DEFAULT 0,
    cached_tokens      BIGINT NOT NULL DEFAULT 0,
    ts                 TIMESTAMPTZ NOT NULL,
    gateway_id         BIGINT,
    processed_at       TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_tokenevent_tenant      ON TokenUsageEvent(caller_tenant, ts);
CREATE INDEX IF NOT EXISTS idx_tokenevent_model       ON TokenUsageEvent(model_slug, ts);
CREATE INDEX IF NOT EXISTS idx_tokenevent_unprocessed ON TokenUsageEvent(ts) WHERE processed_at IS NULL;

CREATE TABLE IF NOT EXISTS TokenRate (
    id                        SERIAL PRIMARY KEY,
    model_slug                VARCHAR,
    cents_per_million_input   BIGINT NOT NULL,
    cents_per_million_output  BIGINT NOT NULL,
    cents_per_million_cached  BIGINT NOT NULL DEFAULT 0,
    effective_from            TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    effective_to              TIMESTAMPTZ,
    tenant                    VARCHAR,
    created_at                TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    added_by                  VARCHAR NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_tokenrate_lookup ON TokenRate(model_slug, effective_from);

CREATE TABLE IF NOT EXISTS TokenUsageHourly (
    id                 SERIAL PRIMARY KEY,
    tenant             VARCHAR NOT NULL,
    model_slug         VARCHAR NOT NULL,
    hour               TIMESTAMPTZ NOT NULL,
    input_tokens       BIGINT NOT NULL DEFAULT 0,
    cached_tokens      BIGINT NOT NULL DEFAULT 0,
    output_tokens      BIGINT NOT NULL DEFAULT 0,
    request_count      BIGINT NOT NULL DEFAULT 0,
    input_numer        BIGINT NOT NULL DEFAULT 0,
    output_numer       BIGINT NOT NULL DEFAULT 0,
    cached_numer       BIGINT NOT NULL DEFAULT 0,
    charge_cents       BIGINT NOT NULL DEFAULT 0,
    UNIQUE(tenant, model_slug, hour)
);

CREATE INDEX IF NOT EXISTS idx_tokenhourly_tenant ON TokenUsageHourly(tenant, hour);

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
