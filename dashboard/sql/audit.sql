-- Audit schema only (no billing tables)

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

CREATE TABLE FuncState (
    tenant          VARCHAR NOT NULL,
    namespace       VARCHAR NOT NULL,
    fpname          VARCHAR NOT NULL,
    fprevision      bigint,
    state           VARCHAR NOT NULL,
    updatetime      TIMESTAMPTZ,
    PRIMARY KEY(tenant, namespace, fpname, fprevision, updatetime)
);

CREATE TABLE ReqAudit (
    seqid           SERIAL PRIMARY KEY,
    podkey          VARCHAR NOT NULL,
    audittime       TIMESTAMP,
    keepalive       bool,
    ttft            int,            -- Time to First Token
    latency         int
);

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

CREATE OR REPLACE FUNCTION notification_trigger() RETURNS TRIGGER AS
$$
BEGIN
    PERFORM pg_notify('ReqAudit_insert', to_json(NEW)::TEXT);
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE TRIGGER capture_change_trigger AFTER INSERT OR UPDATE OR DELETE ON ReqAudit
FOR EACH ROW EXECUTE FUNCTION notification_trigger();
