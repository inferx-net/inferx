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
    microcents_per_million_input   BIGINT NOT NULL,
    microcents_per_million_output  BIGINT NOT NULL,
    microcents_per_million_cached  BIGINT NOT NULL DEFAULT 0,
    effective_from            TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    effective_to              TIMESTAMPTZ,
    tenant                    VARCHAR,
    created_at                TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    added_by                  VARCHAR NOT NULL
);

ALTER TABLE TokenRate
  ADD COLUMN IF NOT EXISTS microcents_per_million_input BIGINT,
  ADD COLUMN IF NOT EXISTS microcents_per_million_output BIGINT,
  ADD COLUMN IF NOT EXISTS microcents_per_million_cached BIGINT;

DO $$
BEGIN
  IF EXISTS (
    SELECT 1
    FROM information_schema.columns
    WHERE table_name = 'tokenrate'
      AND column_name = 'cents_per_million_input'
  ) THEN
    EXECUTE $sql$
      UPDATE TokenRate
      SET
        microcents_per_million_input = COALESCE(microcents_per_million_input, cents_per_million_input * 1000000),
        microcents_per_million_output = COALESCE(microcents_per_million_output, cents_per_million_output * 1000000),
        microcents_per_million_cached = COALESCE(microcents_per_million_cached, cents_per_million_cached * 1000000)
      WHERE
        microcents_per_million_input IS NULL
        OR microcents_per_million_output IS NULL
        OR microcents_per_million_cached IS NULL
    $sql$;
  END IF;
END $$;

DO $$
BEGIN
  IF EXISTS (
    SELECT 1
    FROM information_schema.columns
    WHERE table_name = 'tokenrate'
      AND column_name = 'cents_per_million_input'
  ) THEN
    EXECUTE $sql$
      ALTER TABLE TokenRate
        ALTER COLUMN cents_per_million_input SET DEFAULT 0,
        ALTER COLUMN cents_per_million_output SET DEFAULT 0,
        ALTER COLUMN cents_per_million_cached SET DEFAULT 0
    $sql$;
  END IF;
END $$;

ALTER TABLE TokenRate
  ALTER COLUMN microcents_per_million_input SET NOT NULL,
  ALTER COLUMN microcents_per_million_output SET NOT NULL,
  ALTER COLUMN microcents_per_million_cached SET NOT NULL,
  ALTER COLUMN microcents_per_million_cached SET DEFAULT 0;

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
    input_numer        NUMERIC NOT NULL DEFAULT 0,
    output_numer       NUMERIC NOT NULL DEFAULT 0,
    cached_numer       NUMERIC NOT NULL DEFAULT 0,
    charge_cents       BIGINT NOT NULL DEFAULT 0,
    UNIQUE(tenant, model_slug, hour)
);

ALTER TABLE TokenUsageHourly
  ALTER COLUMN input_numer TYPE NUMERIC USING input_numer::numeric,
  ALTER COLUMN output_numer TYPE NUMERIC USING output_numer::numeric,
  ALTER COLUMN cached_numer TYPE NUMERIC USING cached_numer::numeric;

CREATE INDEX IF NOT EXISTS idx_tokenhourly_tenant ON TokenUsageHourly(tenant, hour);

CREATE OR REPLACE FUNCTION GetTokenRateMicrocents(
    p_model_slug VARCHAR,
    p_ts TIMESTAMPTZ,
    p_tenant VARCHAR
) RETURNS TABLE (
    microcents_per_million_input  BIGINT,
    microcents_per_million_output BIGINT,
    microcents_per_million_cached BIGINT
)
LANGUAGE SQL
STABLE
AS $$
    SELECT
        r.microcents_per_million_input,
        r.microcents_per_million_output,
        r.microcents_per_million_cached
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
