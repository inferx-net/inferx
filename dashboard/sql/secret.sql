CREATE EXTENSION IF NOT EXISTS citext;

--DROP TABLE ApiKey;
CREATE TABLE Apikey (
    key_id              BIGSERIAL PRIMARY KEY,
    apikey              VARCHAR NOT NULL UNIQUE,     -- raw API key (first version)
    username            VARCHAR NOT NULL,
    keyname             VARCHAR NOT NULL,
    access_level        VARCHAR NOT NULL DEFAULT 'inference',
    restrict_tenant     VARCHAR NOT NULL,
    restrict_namespace  VARCHAR,
    createtime          TIMESTAMP DEFAULT NOW(),
    expires_at          TIMESTAMP,
    revoked_at          TIMESTAMP,
    revoked_by          VARCHAR,
    revoke_reason       VARCHAR,
    CHECK (access_level IN ('full', 'inference', 'read')),
    CHECK (btrim(restrict_tenant) <> ''),
    CHECK (restrict_namespace IS NULL OR restrict_tenant IS NOT NULL)
);

CREATE UNIQUE INDEX apikey_idx_username_keyname ON Apikey (username, keyname);
CREATE INDEX apikey_idx_username ON Apikey (username);
CREATE INDEX apikey_idx_tenant ON Apikey (restrict_tenant);
CREATE INDEX apikey_idx_active ON Apikey (revoked_at, expires_at);

CREATE TABLE UserRole (
    username        VARCHAR NOT NULL,
    rolename       VARCHAR NOT NULL,
    PRIMARY KEY(username, rolename)
);

CREATE INDEX userrole_idx_rolename ON UserRole (rolename);

CREATE TABLE UserOnboard (
    sub             VARCHAR PRIMARY KEY,    -- Keycloak subject ID (immutable)
    username        VARCHAR NOT NULL,       -- preferred_username at onboard time
    tenant_name     VARCHAR NOT NULL,       -- immutable tenant identifier
    status          VARCHAR NOT NULL DEFAULT 'pending',
    saga_step       INT NOT NULL DEFAULT 0,
    onboarded_at    TIMESTAMP DEFAULT NOW(),
    completed_at    TIMESTAMP,
    CHECK (status IN ('pending', 'complete', 'failed'))
);

CREATE UNIQUE INDEX useronboard_idx_tenant_name ON UserOnboard (tenant_name);

CREATE TABLE TenantProfile (
    tenant_name     VARCHAR PRIMARY KEY,
    sub             VARCHAR NOT NULL UNIQUE, -- Keycloak subject ID (immutable)
    display_name    VARCHAR,                 -- full name from JWT, NULL until synced
    email           CITEXT NOT NULL,         -- normalized email, '' until synced
    company_name    VARCHAR,
    created_at      TIMESTAMP DEFAULT NOW(),
    updated_at      TIMESTAMP DEFAULT NOW()
);

CREATE TABLE ModelCatalogEntry (
    id                    BIGSERIAL PRIMARY KEY,
    slug                  VARCHAR NOT NULL UNIQUE,
    display_name          VARCHAR NOT NULL,
    provider              VARCHAR NOT NULL,
    model_type            VARCHAR NOT NULL,
    brief_intro           TEXT NOT NULL,
    detailed_intro        TEXT NOT NULL DEFAULT '',
    source_kind           VARCHAR NOT NULL DEFAULT 'huggingface',
    source_model_id       VARCHAR NOT NULL UNIQUE,
    parameter_count_b     NUMERIC(10,2),
    is_moe                BOOLEAN NOT NULL DEFAULT false,
    tags                  JSONB NOT NULL DEFAULT '[]'::jsonb,
    recommended_use_cases JSONB NOT NULL DEFAULT '[]'::jsonb,
    default_func_spec     JSONB NOT NULL,
    is_active             BOOLEAN NOT NULL DEFAULT true,
    catalog_version       INTEGER NOT NULL DEFAULT 1,
    createtime            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updatetime            TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_mce_provider ON ModelCatalogEntry (provider);
CREATE INDEX idx_mce_type ON ModelCatalogEntry (model_type);
CREATE INDEX idx_mce_active ON ModelCatalogEntry (is_active);
CREATE INDEX idx_mce_tags ON ModelCatalogEntry USING GIN (tags);

CREATE OR REPLACE FUNCTION set_updatetime()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updatetime = now();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER mce_updatetime BEFORE UPDATE ON ModelCatalogEntry
FOR EACH ROW EXECUTE FUNCTION set_updatetime();
