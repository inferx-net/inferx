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

CREATE TABLE UserProfile (
    sub             VARCHAR PRIMARY KEY,     -- Keycloak subject ID (immutable)
    default_tenant  VARCHAR,                 -- user's default tenant context; NULL if unset
    created_at      TIMESTAMP DEFAULT NOW(),
    updated_at      TIMESTAMP DEFAULT NOW()
);

CREATE TABLE CatalogModel (
    id                    BIGSERIAL PRIMARY KEY,
    slug                  VARCHAR NOT NULL UNIQUE,
    display_name          VARCHAR NOT NULL,
    provider              VARCHAR NOT NULL,
    modality              VARCHAR NOT NULL,
    brief_intro           TEXT NOT NULL,
    detailed_intro        TEXT NOT NULL DEFAULT '',
    source_kind           VARCHAR NOT NULL DEFAULT 'huggingface',
    source_model_id       VARCHAR NOT NULL,
    parameter_count_b     NUMERIC(10,2),
    is_moe                BOOLEAN NOT NULL DEFAULT false,
    tags                  JSONB NOT NULL DEFAULT '[]'::jsonb,
    recommended_use_cases JSONB NOT NULL DEFAULT '[]'::jsonb,
    default_func_spec     JSONB NOT NULL,
    is_active             BOOLEAN NOT NULL DEFAULT false,
    catalog_version       INTEGER NOT NULL DEFAULT 1,
    createtime            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updatetime            TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_mce_provider ON CatalogModel (provider);
CREATE INDEX idx_mce_modality ON CatalogModel (modality);
CREATE INDEX idx_mce_active ON CatalogModel (is_active);
CREATE INDEX idx_mce_source_model_id ON CatalogModel (source_model_id);
CREATE INDEX idx_mce_tags ON CatalogModel USING GIN (tags);

CREATE OR REPLACE FUNCTION set_updatetime()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updatetime = now();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER mce_updatetime BEFORE UPDATE ON CatalogModel
FOR EACH ROW EXECUTE FUNCTION set_updatetime();

CREATE TABLE Endpoints (
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

CREATE INDEX idx_endpoints_tags ON Endpoints USING GIN (tags);

CREATE TRIGGER endpoints_updatetime BEFORE UPDATE ON Endpoints
FOR EACH ROW EXECUTE FUNCTION set_updatetime();

CREATE TABLE functions (
    id           BIGSERIAL PRIMARY KEY,
    tenant       VARCHAR NOT NULL,
    namespace    VARCHAR NOT NULL,
    name         VARCHAR NOT NULL,
    preset       VARCHAR NOT NULL,
    catalog      VARCHAR NOT NULL,
    token_length INTEGER NOT NULL,
    kb_token_count INTEGER,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant, namespace, name)
);

CREATE INDEX idx_functions_tenant_namespace ON functions (tenant, namespace);

CREATE TABLE SkillTemplate (
    template_id         BIGSERIAL PRIMARY KEY,
    tenant              VARCHAR NOT NULL,
    namespace           VARCHAR NOT NULL,
    display_name        VARCHAR NOT NULL,
    description         TEXT,
    func_tenant         VARCHAR NOT NULL,
    func_namespace      VARCHAR NOT NULL,
    normal_funcname     VARCHAR NOT NULL,
    producer_funcname   VARCHAR,
    producer_revision   BIGINT,
    consumer_funcname   VARCHAR,
    consumer_revision   BIGINT,
    is_active           BOOLEAN NOT NULL DEFAULT TRUE,
    created_at          TIMESTAMP DEFAULT NOW(),
    UNIQUE(tenant, namespace, display_name),
    CHECK ((producer_funcname IS NULL) = (producer_revision IS NULL)),
    CHECK ((consumer_funcname IS NULL) = (consumer_revision IS NULL))
);

CREATE TABLE Skill (
    skill_id                   BIGSERIAL PRIMARY KEY,
    owner_tenant               VARCHAR NOT NULL,
    owner_namespace            VARCHAR NOT NULL,
    skillname                  VARCHAR NOT NULL,
    description                TEXT,
    serving_mode               VARCHAR NOT NULL DEFAULT 'dedicated',
    earning_type               VARCHAR NOT NULL DEFAULT 'free',
    user_price_microcents      INTEGER,
    gpu_billing_target         VARCHAR NOT NULL DEFAULT 'caller',
    inferx_revenue_share_pct   NUMERIC(5,2) NOT NULL DEFAULT 0.0,
    active_revision_id         BIGINT,
    is_published               BOOLEAN NOT NULL DEFAULT FALSE,
    published_at               TIMESTAMP,
    published_by               VARCHAR,
    UNIQUE(owner_tenant, owner_namespace, skillname),
    CHECK (serving_mode IN ('shared', 'dedicated')),
    CHECK (earning_type IN ('free', 'per_run')),
    CHECK (earning_type = 'free' OR user_price_microcents IS NOT NULL),
    CHECK (gpu_billing_target IN ('owner', 'caller'))
);

CREATE TABLE SkillRevision (
    revision_id    BIGSERIAL PRIMARY KEY,
    skill_id       BIGINT NOT NULL REFERENCES Skill(skill_id),
    version        INTEGER NOT NULL,
    template_id    BIGINT NOT NULL REFERENCES SkillTemplate(template_id),
    has_cache               BOOLEAN NOT NULL DEFAULT FALSE,
    cache_status            VARCHAR NOT NULL DEFAULT 'none',
    cache_ready_at          TIMESTAMP,
    allowed_child_skilleps  TEXT[] DEFAULT NULL,
    created_at              TIMESTAMP DEFAULT NOW(),
    created_by              VARCHAR NOT NULL,
    UNIQUE(skill_id, version),
    CHECK (cache_status IN ('none', 'pending', 'ready', 'failed'))
);

ALTER TABLE Skill
    ADD CONSTRAINT fk_active_revision
    FOREIGN KEY (active_revision_id) REFERENCES SkillRevision(revision_id);

CREATE TABLE SkillSubscription (
    subscription_id   BIGSERIAL PRIMARY KEY,
    subscriber_tenant VARCHAR NOT NULL,
    owner_tenant      VARCHAR NOT NULL,
    owner_namespace   VARCHAR NOT NULL,
    skillname         VARCHAR NOT NULL,
    tool_alias        VARCHAR NOT NULL,
    subscribed_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    subscribed_by     VARCHAR NOT NULL,
    UNIQUE(subscriber_tenant, owner_tenant, owner_namespace, skillname),
    UNIQUE(subscriber_tenant, tool_alias),
    FOREIGN KEY (owner_tenant, owner_namespace, skillname)
        REFERENCES Skill (owner_tenant, owner_namespace, skillname)
        ON DELETE CASCADE
);

CREATE INDEX idx_skillsub_tenant
    ON SkillSubscription (subscriber_tenant);
