CREATE TABLE IF NOT EXISTS functions (
    id           BIGSERIAL PRIMARY KEY,
    tenant       VARCHAR NOT NULL,
    namespace    VARCHAR NOT NULL,
    name         VARCHAR NOT NULL,
    preset       VARCHAR NOT NULL,
    catalog      VARCHAR NOT NULL,
    token_length INTEGER NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant, namespace, name)
);

CREATE INDEX IF NOT EXISTS idx_functions_tenant_namespace
ON functions (tenant, namespace);
