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
    createtime          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updatetime          TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_published_by   VARCHAR,
    CHECK (btrim(slug) <> ''),
    CHECK (btrim(base_url) <> '')
);

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
