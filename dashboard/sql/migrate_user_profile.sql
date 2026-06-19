CREATE TABLE IF NOT EXISTS UserProfile (
    sub             VARCHAR PRIMARY KEY,     -- Keycloak subject ID (immutable)
    default_tenant  VARCHAR,                 -- user's default tenant context; NULL if unset
    created_at      TIMESTAMP DEFAULT NOW(),
    updated_at      TIMESTAMP DEFAULT NOW()
);

-- Step 1: backfill from completed onboards (authoritative source).
INSERT INTO UserProfile (sub, default_tenant)
SELECT o.sub, o.tenant_name
  FROM UserOnboard o
 WHERE o.status = 'complete'
ON CONFLICT (sub) DO NOTHING;

-- InferX admin initialization:
-- If the InferX admin user went through onboarding, step 1 above covers them.
-- Otherwise, insert their UserProfile manually:
--   INSERT INTO UserProfile (sub, default_tenant) VALUES ('<admin-sub>', '<home-tenant>')
--   ON CONFLICT (sub) DO UPDATE SET default_tenant = EXCLUDED.default_tenant, updated_at = NOW();
