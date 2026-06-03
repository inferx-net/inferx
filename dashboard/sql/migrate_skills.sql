CREATE TABLE IF NOT EXISTS SkillTemplate (
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

CREATE TABLE IF NOT EXISTS Skill (
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

CREATE TABLE IF NOT EXISTS SkillRevision (
    revision_id    BIGSERIAL PRIMARY KEY,
    skill_id       BIGINT NOT NULL REFERENCES Skill(skill_id),
    version        INTEGER NOT NULL,
    template_id    BIGINT NOT NULL REFERENCES SkillTemplate(template_id),
    has_cache      BOOLEAN NOT NULL DEFAULT FALSE,
    cache_status   VARCHAR NOT NULL DEFAULT 'none',
    cache_ready_at TIMESTAMP,
    created_at     TIMESTAMP DEFAULT NOW(),
    created_by     VARCHAR NOT NULL,
    UNIQUE(skill_id, version),
    CHECK (cache_status IN ('none', 'pending', 'ready', 'failed'))
);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'fk_active_revision'
    ) THEN
        ALTER TABLE Skill
            ADD CONSTRAINT fk_active_revision
            FOREIGN KEY (active_revision_id) REFERENCES SkillRevision(revision_id);
    END IF;
END$$;

ALTER TABLE Skill ADD COLUMN IF NOT EXISTS description TEXT;

CREATE TABLE IF NOT EXISTS SkillSubscription (
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
        ON DELETE CASCADE,
);

CREATE INDEX IF NOT EXISTS idx_skillsub_tenant
    ON SkillSubscription (subscriber_tenant);

DO $$
DECLARE
    v_constraint_name TEXT;
BEGIN
    SELECT conname INTO v_constraint_name
    FROM pg_constraint
    WHERE conrelid = 'SkillSubscription'::regclass
      AND contype = 'c'
      AND pg_get_constraintdef(oid) LIKE '%tool_alias%';
    IF v_constraint_name IS NOT NULL THEN
        EXECUTE 'ALTER TABLE SkillSubscription DROP CONSTRAINT ' || quote_ident(v_constraint_name);
    END IF;
END$$;
