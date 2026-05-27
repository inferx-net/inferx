ALTER TABLE UsageTick
    ADD COLUMN IF NOT EXISTS caller_tenant VARCHAR;

CREATE INDEX IF NOT EXISTS idx_tick_caller_tenant
    ON UsageTick(caller_tenant, tick_time);
