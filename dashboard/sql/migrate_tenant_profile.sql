CREATE EXTENSION IF NOT EXISTS citext;

CREATE TABLE IF NOT EXISTS TenantProfile (
    tenant_name     VARCHAR PRIMARY KEY,
    sub             VARCHAR NOT NULL UNIQUE, -- Keycloak subject ID (immutable)
    display_name    VARCHAR,                 -- full name from JWT, NULL until synced
    email           CITEXT NOT NULL,         -- normalized email, '' until synced
    company_name    VARCHAR,
    created_at      TIMESTAMP DEFAULT NOW(),
    updated_at      TIMESTAMP DEFAULT NOW()
);

-- Seed existing onboarded tenants. display_name/email are filled lazily on next login sync.
INSERT INTO TenantProfile (tenant_name, sub, display_name, email)
SELECT tenant_name, sub, username, ''
  FROM UserOnboard
 WHERE status = 'complete'
ON CONFLICT (sub) DO NOTHING;
