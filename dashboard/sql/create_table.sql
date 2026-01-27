

--DROP TABLE Pod;
CREATE TABLE Pod (
    tenant          VARCHAR NOT NULL,
    namespace       VARCHAR NOT NULL,
    fpname          VARCHAR NOT NULL,
    fprevision      bigint,
    id              VARCHAR NOT NULL,
    podtype         VARCHAR NOT NULL,
    nodename        VARCHAR NOT NULL,
    state           VARCHAR NOT NULL,
    updatetime      TIMESTAMPTZ,
    PRIMARY KEY(tenant, namespace, fpname, fprevision, podtype, nodename, id)
);

--DROP TABLE PodAudit;
CREATE TABLE PodAudit (
    tenant          VARCHAR NOT NULL,
    namespace       VARCHAR NOT NULL,
    fpname          VARCHAR NOT NULL,
    fprevision      bigint,
    id              VARCHAR NOT NULL,
    nodename        VARCHAR NOT NULL,
    action          VARCHAR NOT NULL,
    state           VARCHAR NOT NULL,
    updatetime      TIMESTAMPTZ,
    PRIMARY KEY(tenant, namespace, fpname, fprevision, id, updatetime)
);

--DROP TABLE PodFailLog;
CREATE TABLE PodFailLog (
    tenant          VARCHAR NOT NULL,
    namespace       VARCHAR NOT NULL,
    fpname          VARCHAR NOT NULL,
    fprevision      bigint,
    id              VARCHAR NOT NULL,
    state           VARCHAR NOT NULL,
    nodename        VARCHAR NOT NULL,
    createtime      TIMESTAMPTZ,
    log             VARCHAR NOT NULL,
    exit_info       VARCHAR NOT NULL,
    PRIMARY KEY(tenant, namespace, fpname, fprevision, id)
);

--DROP TABLE FuncState;
CREATE TABLE FuncState (
    tenant          VARCHAR NOT NULL,
    namespace       VARCHAR NOT NULL,
    fpname          VARCHAR NOT NULL,
    fprevision      bigint,
    state           VARCHAR NOT NULL,
    updatetime      TIMESTAMPTZ,
    PRIMARY KEY(tenant, namespace, fpname, fprevision, updatetime)
);

--DROP TABLE ReqAudit;
CREATE TABLE ReqAudit (
    seqid           SERIAL PRIMARY KEY, 
    podkey          VARCHAR NOT NULL,
    audittime       TIMESTAMP,
    keepalive       bool,
    ttft            int,            -- Time to First Token
    latency         int
);

-- DROP TABLE SnapshotScheduleAudit;
CREATE TABLE SnapshotScheduleAudit (
    tenant          VARCHAR NOT NULL,
    namespace       VARCHAR NOT NULL,
    funcname        VARCHAR NOT NULL,
    revision        bigint,
    nodename        VARCHAR NOT NULL,
    state           VARCHAR NOT NULL,
    detail          VARCHAR NOT NULL,
    updatetime      TIMESTAMPTZ,
    PRIMARY KEY(tenant, namespace, funcname, revision, nodename, state)
);

-- CREATE INDEX idx_snapshot_audit
-- ON SnapshotScheduleAudit (tenant, namespace, funcname, revision, nodename);

CREATE OR REPLACE FUNCTION notification_trigger() RETURNS TRIGGER AS 
$$
BEGIN
    PERFORM pg_notify('ReqAudit_insert', 
            to_json(NEW)::TEXT
    );
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE TRIGGER capture_change_trigger AFTER INSERT OR UPDATE OR DELETE ON ReqAudit
FOR EACH ROW EXECUTE FUNCTION notification_trigger();


-- CREATE USER audit_user WITH PASSWORD '123456';
-- GRANT ALL ON ALL TABLES IN SCHEMA public to audit_user;
-- GRANT USAGE ON SEQUENCE reqaudit_seqid_seq TO audit_user;

-- https://stackoverflow.com/questions/18664074/getting-error-peer-authentication-failed-for-user-postgres-when-trying-to-ge

-- GPU Usage for Billing
-- DROP TABLE GpuUsage;
CREATE TABLE GpuUsage (
    seqid           SERIAL PRIMARY KEY,
    tenant          VARCHAR NOT NULL,
    namespace       VARCHAR NOT NULL,
    funcname        VARCHAR NOT NULL,
    fprevision      BIGINT NOT NULL,
    nodename        VARCHAR NOT NULL,
    pod_id          VARCHAR NOT NULL,

    -- GPU resource (for billing calculation)
    gpu_type        VARCHAR NOT NULL,       -- GPU type from node (e.g., "A100", "H100")
    gpu_count       INT NOT NULL,           -- Model's requested GPU count
    vram_mb         BIGINT NOT NULL,        -- Model's required vRAM in MB
    total_vram_mb   BIGINT NOT NULL,        -- Node's total vRAM per GPU in MB

    -- Usage tracking
    usage_type      VARCHAR(16) NOT NULL,   -- 'request' or 'snapshot'
    start_time      TIMESTAMPTZ NOT NULL,
    end_time        TIMESTAMPTZ NOT NULL,
    duration_ms     BIGINT NOT NULL,

    is_coldstart    BOOLEAN DEFAULT FALSE,
    gateway_id      BIGINT
);

-- Index for billing queries by tenant and time range
CREATE INDEX idx_gpu_usage_billing ON GpuUsage (tenant, start_time);

-- Index for per-function analysis
CREATE INDEX idx_gpu_usage_func ON GpuUsage (tenant, namespace, funcname, start_time);

