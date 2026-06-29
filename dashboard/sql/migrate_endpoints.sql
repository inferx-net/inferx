CREATE TABLE IF NOT EXISTS Endpoints (
    slug                    VARCHAR PRIMARY KEY,
    func_revision           BIGINT NOT NULL,
    brief_intro             TEXT,
    detailed_intro          TEXT,
    cs_ttft                 TEXT,
    recommended_use_cases   JSONB NOT NULL DEFAULT '[]'::jsonb,
    tags                    JSONB NOT NULL DEFAULT '[]'::jsonb,
    provider                VARCHAR,
    parameter_count_b       NUMERIC(10,2),
    context_length          BIGINT,
    concurrency             NUMERIC(10,2),
    last_published_at       TIMESTAMPTZ,
    last_published_by       VARCHAR,
    updatetime              TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE Endpoints
    ADD COLUMN IF NOT EXISTS cs_ttft TEXT,
    ADD COLUMN IF NOT EXISTS concurrency NUMERIC(10,2);

ALTER TABLE Endpoints
    DROP COLUMN IF EXISTS max_token_length;

-- OpenRouter provider-listing metadata (§4.3.1). Catalog metadata that the flat
-- /v1/models adapter emits to OpenRouter. Reuses context_length/provider/
-- parameter_count_b/tags already on the table; adds the remaining required and
-- optional OpenRouter fields. Modality columns are nullable with NO default:
-- NULL means "not set" and ListOnOpenRouter refuses to list such a row rather
-- than silently defaulting to ["text"] and mis-advertising capabilities.
ALTER TABLE Endpoints
    ADD COLUMN IF NOT EXISTS or_name           VARCHAR,  -- OpenRouter display `name` (operator-authored)
    ADD COLUMN IF NOT EXISTS hugging_face_id   VARCHAR,
    ADD COLUMN IF NOT EXISTS quantization      VARCHAR,
    ADD COLUMN IF NOT EXISTS input_modalities  JSONB,    -- nullable: NULL = "not set"
    ADD COLUMN IF NOT EXISTS output_modalities JSONB,    -- (do NOT default to ["text"])
    ADD COLUMN IF NOT EXISTS max_output_length BIGINT,
    ADD COLUMN IF NOT EXISTS pricing           JSONB,    -- rate card: prompt/completion or up-to-2 tiers (§4.4)
    ADD COLUMN IF NOT EXISTS discount_to_user  NUMERIC,  -- optional OpenRouter discount; NULL = none
    ADD COLUMN IF NOT EXISTS supported_sampling_parameters JSONB,
    ADD COLUMN IF NOT EXISTS supported_features            JSONB,
    ADD COLUMN IF NOT EXISTS openrouter_slug   VARCHAR,  -- canonical attach, auto-resolved (§4.1.1)
    ADD COLUMN IF NOT EXISTS or_slug_override  VARCHAR;  -- operator-pinned slug; wins over auto-resolve

-- OpenRouter listing lifecycle + audit (§4.3.2).
-- or_listed     - InferX-side: is this row emitted into /v1/models at all?
-- or_is_ready   - OpenRouter-side `is_ready`: the graceful take-offline control.
-- or_deprecation_date - optional planned-sunset signal.
ALTER TABLE Endpoints
    ADD COLUMN IF NOT EXISTS or_listed           BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS or_is_ready         BOOLEAN,
    ADD COLUMN IF NOT EXISTS or_deprecation_date DATE,
    ADD COLUMN IF NOT EXISTS or_listed_at        TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS or_listed_by        VARCHAR;

CREATE INDEX IF NOT EXISTS idx_endpoints_tags ON Endpoints USING GIN (tags);
CREATE INDEX IF NOT EXISTS idx_endpoints_or_listed ON Endpoints (or_listed) WHERE or_listed;

CREATE OR REPLACE FUNCTION set_updatetime()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updatetime = now();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS endpoints_updatetime ON Endpoints;
CREATE TRIGGER endpoints_updatetime BEFORE UPDATE ON Endpoints
FOR EACH ROW EXECUTE FUNCTION set_updatetime();
