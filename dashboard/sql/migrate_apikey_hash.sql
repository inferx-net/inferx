-- API key clear-key/scoping migration for existing deployments.
-- Run during maintenance window before starting upgraded gateway binaries.

ALTER TABLE Apikey
    ADD COLUMN IF NOT EXISTS key_id BIGSERIAL,
    ADD COLUMN IF NOT EXISTS restrict_tenant VARCHAR,
    ADD COLUMN IF NOT EXISTS restrict_namespace VARCHAR,
    ADD COLUMN IF NOT EXISTS expires_at TIMESTAMP,
    ADD COLUMN IF NOT EXISTS revoked_at TIMESTAMP,
    ADD COLUMN IF NOT EXISTS revoked_by VARCHAR,
    ADD COLUMN IF NOT EXISTS revoke_reason VARCHAR;

-- Rename legacy scope column once; do not keep both names.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_name = 'apikey' AND column_name = 'scope'
    ) AND NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_name = 'apikey' AND column_name = 'access_level'
    ) THEN
        ALTER TABLE Apikey RENAME COLUMN scope TO access_level;
    END IF;
END$$;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_name = 'apikey' AND column_name = 'access_level'
    ) THEN
        RAISE EXCEPTION 'Column access_level is missing. Expected legacy column scope to exist for rename.';
    END IF;
END$$;

-- Drop hash-era artifacts if present.
DROP INDEX IF EXISTS apikey_idx_hash;
DROP INDEX IF EXISTS apikey_idx_prefix;
ALTER TABLE Apikey DROP COLUMN IF EXISTS apikey_hash;
ALTER TABLE Apikey DROP COLUMN IF EXISTS apikey_prefix;
ALTER TABLE Apikey DROP COLUMN IF EXISTS restrict_functions;

-- Keep legacy keys behavior (full privileges) after migration.
UPDATE Apikey
SET access_level = 'full'
WHERE access_level IS NULL OR btrim(access_level) = '';

DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM Apikey
        WHERE restrict_tenant IS NULL OR btrim(restrict_tenant) = ''
    ) THEN
        RAISE EXCEPTION 'Cannot enforce required API key tenant: found rows with NULL/empty restrict_tenant. Update or delete those keys before migration.';
    END IF;
END$$;

ALTER TABLE Apikey
    ALTER COLUMN apikey SET NOT NULL,
    ALTER COLUMN access_level SET NOT NULL,
    ALTER COLUMN restrict_tenant SET NOT NULL;

ALTER TABLE Apikey
    DROP CONSTRAINT IF EXISTS apikey_scope_check,
    DROP CONSTRAINT IF EXISTS apikey_access_level_check;

ALTER TABLE Apikey
    ADD CONSTRAINT apikey_access_level_check CHECK (access_level IN ('full', 'inference', 'read'));

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

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'apikey_tenant_required'
    ) THEN
        ALTER TABLE Apikey
            ADD CONSTRAINT apikey_tenant_required
            CHECK (btrim(restrict_tenant) <> '');
    END IF;
END$$;

ALTER TABLE Apikey DROP CONSTRAINT IF EXISTS apikey_funcs_requires_ns;

CREATE UNIQUE INDEX IF NOT EXISTS apikey_idx_key ON Apikey (apikey);
CREATE UNIQUE INDEX IF NOT EXISTS apikey_idx_username_keyname ON Apikey (username, keyname);
CREATE INDEX IF NOT EXISTS apikey_idx_username ON Apikey (username);
CREATE INDEX IF NOT EXISTS apikey_idx_tenant ON Apikey (restrict_tenant);

-- New key creation in upgraded gateway defaults to inference.
ALTER TABLE Apikey ALTER COLUMN access_level SET DEFAULT 'inference';
