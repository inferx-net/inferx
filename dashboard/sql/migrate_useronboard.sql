CREATE TABLE IF NOT EXISTS UserOnboard (
    sub             VARCHAR PRIMARY KEY,    -- Keycloak subject ID (immutable)
    username        VARCHAR NOT NULL,       -- preferred_username at onboard time
    tenant_name     VARCHAR NOT NULL,       -- immutable tenant identifier
    status          VARCHAR NOT NULL DEFAULT 'pending',
    saga_step       INT NOT NULL DEFAULT 0,
    onboarded_at    TIMESTAMP DEFAULT NOW(),
    completed_at    TIMESTAMP,
    CHECK (status IN ('pending', 'complete', 'failed'))
);

CREATE UNIQUE INDEX IF NOT EXISTS useronboard_idx_tenant_name ON UserOnboard (tenant_name);
