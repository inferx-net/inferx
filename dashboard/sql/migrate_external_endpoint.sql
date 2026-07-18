-- External OpenAI-compatible endpoints proxied by the gateway: a Postgres table
-- mirrored in full into gateway memory, write-through on admin writes.

-- Dead provenance column: external rows have no backing func, so allow NULL.
ALTER TABLE Endpoints ALTER COLUMN func_revision DROP NOT NULL;

CREATE TABLE IF NOT EXISTS ExternalEndpoint (
    slug                VARCHAR PRIMARY KEY,             -- = model name on /endpoints/v1
    base_url            VARCHAR NOT NULL,                -- e.g. https://api.provider.com/v1
    upstream_model      VARCHAR NOT NULL,                -- model name sent to the provider
    provider_api_key    VARCHAR NOT NULL,                -- outbound key gateway sends to provider (plaintext; matches Apikey posture)
    published           BOOLEAN NOT NULL DEFAULT false,  -- direct-path gate (replaces funcstatus.published)
    max_concurrency     INTEGER NOT NULL DEFAULT -1,     -- per-endpoint in-flight cap (-1 = unlimited, gate skipped; 0 = reject all)
    createtime          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updatetime          TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_published_by   VARCHAR,
    CHECK (btrim(slug) <> ''),
    CHECK (btrim(base_url) <> '')
);

-- Idempotent add for existing installs (the CREATE TABLE above is a no-op once the
-- table exists). -1 = unlimited; 0 = reject all; N > 0 caps concurrent upstream
-- requests per slug.
ALTER TABLE ExternalEndpoint ADD COLUMN IF NOT EXISTS max_concurrency INTEGER NOT NULL DEFAULT -1;

-- Sentinel flip: 0 used to mean unlimited. Rewrite existing rows to the new
-- sentinel BEFORE the default changes, so no endpoint silently becomes reject-all.
-- Ordered after the ADD COLUMN so fresh installs (already -1) are a no-op.
UPDATE ExternalEndpoint SET max_concurrency = -1 WHERE max_concurrency = 0;
ALTER TABLE ExternalEndpoint ALTER COLUMN max_concurrency SET DEFAULT -1;

ALTER TABLE ExternalEndpoint DROP CONSTRAINT IF EXISTS external_endpoint_max_concurrency_check;
ALTER TABLE ExternalEndpoint ADD CONSTRAINT external_endpoint_max_concurrency_check
    CHECK (max_concurrency >= -1);

CREATE OR REPLACE FUNCTION set_updatetime()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updatetime = now();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS external_endpoint_updatetime ON ExternalEndpoint;
CREATE TRIGGER external_endpoint_updatetime BEFORE UPDATE ON ExternalEndpoint
FOR EACH ROW EXECUTE FUNCTION set_updatetime();
