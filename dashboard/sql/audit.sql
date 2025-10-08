DROP DATABASE auditdb;
CREATE DATABASE auditdb;

\c auditdb;

DROP TABLE Pod;
CREATE TABLE Pod (
    tenant          VARCHAR NOT NULL,
    namespace       VARCHAR NOT NULL,
    fpname          VARCHAR NOT NULL,
    fprevision      bigint,
    id              VARCHAR NOT NULL,
    nodename        VARCHAR NOT NULL,
    state           VARCHAR NOT NULL,
    updatetime      TIMESTAMP,
    PRIMARY KEY(tenant, namespace, fpname, fprevision, id)
);

DROP TABLE PodAudit;
CREATE TABLE PodAudit (
    tenant          VARCHAR NOT NULL,
    namespace       VARCHAR NOT NULL,
    fpname          VARCHAR NOT NULL,
    fprevision      bigint,
    id              VARCHAR NOT NULL,
    nodename        VARCHAR NOT NULL,
    action          VARCHAR NOT NULL,
    state           VARCHAR NOT NULL,
    updatetime      TIMESTAMP,
    PRIMARY KEY(tenant, namespace, fpname, fprevision, id, updatetime)
);

DROP TABLE PodFailLog;
CREATE TABLE PodFailLog (
    tenant          VARCHAR NOT NULL,
    namespace       VARCHAR NOT NULL,
    fpname          VARCHAR NOT NULL,
    fprevision      bigint,
    id              VARCHAR NOT NULL,
    state           VARCHAR NOT NULL,
    nodename        VARCHAR NOT NULL,
    createtime      TIMESTAMP,
    log             VARCHAR NOT NULL,
    exit_info       VARCHAR NOT NULL,
    PRIMARY KEY(tenant, namespace, fpname, fprevision, id)
);

DROP TABLE FuncState;
CREATE TABLE FuncState (
    tenant          VARCHAR NOT NULL,
    namespace       VARCHAR NOT NULL,
    fpname          VARCHAR NOT NULL,
    fprevision      bigint,
    state           VARCHAR NOT NULL,
    updatetime      TIMESTAMP,
    PRIMARY KEY(tenant, namespace, fpname, fprevision, updatetime)
);

DROP TABLE SnapshotSchedule;
CREATE TABLE SnapshotSchedule (
    tenant          VARCHAR NOT NULL,
    namespace       VARCHAR NOT NULL,
    funcname        VARCHAR NOT NULL,
    revision        bigint,
    nodename        VARCHAR NOT NULL,
    state           VARCHAR NOT NULL,
    detail          VARCHAR NOT NULL,
    updatetime      TIMESTAMP,
    PRIMARY KEY(tenant, namespace, funcname, revision, nodename)
);

DROP TABLE ReqAudit;
CREATE TABLE ReqAudit (
    seqid           SERIAL PRIMARY KEY, 
    podkey          VARCHAR NOT NULL,
    audittime       TIMESTAMP,
    keepalive       bool,
    ttft            int,            -- Time to First Token
    latency         int
);

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


CREATE USER audit_user WITH PASSWORD '123456';
GRANT ALL ON ALL TABLES IN SCHEMA public to audit_user;
GRANT USAGE ON SEQUENCE reqaudit_seqid_seq TO audit_user;

-- https://stackoverflow.com/questions/18664074/getting-error-peer-authentication-failed-for-user-postgres-when-trying-to-ge

