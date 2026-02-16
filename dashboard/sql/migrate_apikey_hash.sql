-- API key clear-key/scoping migration for existing deployments.
-- Run during maintenance window before starting upgraded gateway binaries.

ALTER TABLE Apikey
    ADD COLUMN IF NOT EXISTS key_id BIGSERIAL,
    ADD COLUMN IF NOT EXISTS scope VARCHAR NOT NULL DEFAULT 'full',
    ADD COLUMN IF NOT EXISTS restrict_tenant VARCHAR,
    ADD COLUMN IF NOT EXISTS restrict_namespace VARCHAR,
    ADD COLUMN IF NOT EXISTS expires_at TIMESTAMP,
    ADD COLUMN IF NOT EXISTS revoked_at TIMESTAMP,
    ADD COLUMN IF NOT EXISTS revoked_by VARCHAR,
    ADD COLUMN IF NOT EXISTS revoke_reason VARCHAR;

-- Drop hash-era artifacts if present.
DROP INDEX IF EXISTS apikey_idx_hash;
DROP INDEX IF EXISTS apikey_idx_prefix;
ALTER TABLE Apikey DROP COLUMN IF EXISTS apikey_hash;
ALTER TABLE Apikey DROP COLUMN IF EXISTS apikey_prefix;
ALTER TABLE Apikey DROP COLUMN IF EXISTS restrict_functions;

-- Keep legacy keys behavior (full privileges) after migration.
UPDATE Apikey
SET scope = 'full'
WHERE scope IS NULL OR scope = '';

ALTER TABLE Apikey
    ALTER COLUMN apikey SET NOT NULL;

ALTER TABLE Apikey
    DROP CONSTRAINT IF EXISTS apikey_scope_check;

ALTER TABLE Apikey
    ADD CONSTRAINT apikey_scope_check CHECK (scope IN ('full', 'inference', 'read'));

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'apikey_ns_requires_tenant'
    ) THEN
        ALTER TABLE Apikey
            ADD CONSTRAINT apikey_ns_requires_tenant
            CHECK (restrict_namespace IS NULL OR restrict_tenant IS NOT NULL);
    END IF;
END$$;
ALTER TABLE Apikey DROP CONSTRAINT IF EXISTS apikey_funcs_requires_ns;

CREATE UNIQUE INDEX IF NOT EXISTS apikey_idx_key ON Apikey (apikey);
CREATE UNIQUE INDEX IF NOT EXISTS apikey_idx_username_keyname ON Apikey (username, keyname);
CREATE INDEX IF NOT EXISTS apikey_idx_username ON Apikey (username);

-- New key creation in upgraded gateway defaults to inference.
ALTER TABLE Apikey ALTER COLUMN scope SET DEFAULT 'inference';
