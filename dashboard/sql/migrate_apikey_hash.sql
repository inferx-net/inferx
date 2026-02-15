-- API key hash/scoping migration for existing deployments.
-- Run during maintenance window before starting upgraded gateway binaries.

CREATE EXTENSION IF NOT EXISTS pgcrypto;

ALTER TABLE Apikey
    ADD COLUMN IF NOT EXISTS key_id BIGSERIAL,
    ADD COLUMN IF NOT EXISTS apikey_hash CHAR(64),
    ADD COLUMN IF NOT EXISTS apikey_prefix VARCHAR(16),
    ADD COLUMN IF NOT EXISTS scope VARCHAR NOT NULL DEFAULT 'full',
    ADD COLUMN IF NOT EXISTS restrict_tenant VARCHAR,
    ADD COLUMN IF NOT EXISTS restrict_namespace VARCHAR,
    ADD COLUMN IF NOT EXISTS restrict_functions VARCHAR[],
    ADD COLUMN IF NOT EXISTS expires_at TIMESTAMP,
    ADD COLUMN IF NOT EXISTS revoked_at TIMESTAMP,
    ADD COLUMN IF NOT EXISTS revoked_by VARCHAR,
    ADD COLUMN IF NOT EXISTS revoke_reason VARCHAR;

-- Backfill legacy rows where raw API keys were stored.
UPDATE Apikey
SET
    apikey_hash = encode(digest(apikey, 'sha256'), 'hex'),
    apikey_prefix = 'ix_' || substring(regexp_replace(apikey, '[^0-9a-fA-F]', '', 'g') from 1 for 12)
WHERE apikey IS NOT NULL
  AND (apikey_hash IS NULL OR apikey_hash = '');

-- Ensure non-empty display prefix even for unexpected legacy formats.
UPDATE Apikey
SET apikey_prefix = 'ix_000000000000'
WHERE apikey_prefix IS NULL OR apikey_prefix = 'ix_';

-- Keep legacy keys behavior (full privileges) after backfill.
UPDATE Apikey
SET scope = 'full'
WHERE scope IS NULL OR scope = '';

ALTER TABLE Apikey
    ALTER COLUMN apikey_hash SET NOT NULL,
    ALTER COLUMN apikey_prefix SET NOT NULL;

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

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'apikey_funcs_requires_ns'
    ) THEN
        ALTER TABLE Apikey
            ADD CONSTRAINT apikey_funcs_requires_ns
            CHECK (restrict_functions IS NULL OR restrict_namespace IS NOT NULL);
    END IF;
END$$;

CREATE UNIQUE INDEX IF NOT EXISTS apikey_idx_hash ON Apikey (apikey_hash);
CREATE UNIQUE INDEX IF NOT EXISTS apikey_idx_username_keyname ON Apikey (username, keyname);
CREATE INDEX IF NOT EXISTS apikey_idx_prefix ON Apikey (apikey_prefix);
CREATE INDEX IF NOT EXISTS apikey_idx_username ON Apikey (username);

-- New key creation in upgraded gateway defaults to inference.
ALTER TABLE Apikey ALTER COLUMN scope SET DEFAULT 'inference';
