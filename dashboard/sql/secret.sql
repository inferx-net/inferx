--DROP TABLE ApiKey;
CREATE TABLE Apikey (
    key_id              BIGSERIAL PRIMARY KEY,
    apikey              VARCHAR NOT NULL UNIQUE,     -- raw API key (first version)
    username            VARCHAR NOT NULL,
    keyname             VARCHAR NOT NULL,
    scope               VARCHAR NOT NULL DEFAULT 'inference',
    restrict_tenant     VARCHAR,
    restrict_namespace  VARCHAR,
    createtime          TIMESTAMP DEFAULT NOW(),
    expires_at          TIMESTAMP,
    revoked_at          TIMESTAMP,
    revoked_by          VARCHAR,
    revoke_reason       VARCHAR,
    CHECK (scope IN ('full', 'inference', 'read')),
    CHECK (restrict_namespace IS NULL OR restrict_tenant IS NOT NULL)
);

CREATE UNIQUE INDEX apikey_idx_username_keyname ON Apikey (username, keyname);
CREATE INDEX apikey_idx_username ON Apikey (username);
CREATE INDEX apikey_idx_active ON Apikey (revoked_at, expires_at);

CREATE TABLE UserRole (
    username        VARCHAR NOT NULL,
    rolename       VARCHAR NOT NULL,
    PRIMARY KEY(username, rolename)
);

CREATE INDEX userrole_idx_rolename ON UserRole (rolename);
