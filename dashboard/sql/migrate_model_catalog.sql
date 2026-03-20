CREATE TABLE IF NOT EXISTS ModelCatalogEntry (
    id                    BIGSERIAL PRIMARY KEY,
    slug                  VARCHAR NOT NULL UNIQUE,
    display_name          VARCHAR NOT NULL,
    provider              VARCHAR NOT NULL,
    model_type            VARCHAR NOT NULL,
    brief_intro           TEXT NOT NULL,
    detailed_intro        TEXT NOT NULL DEFAULT '',
    source_kind           VARCHAR NOT NULL DEFAULT 'huggingface',
    source_model_id       VARCHAR NOT NULL UNIQUE,
    parameter_count_b     NUMERIC(10,2),
    is_moe                BOOLEAN NOT NULL DEFAULT false,
    tags                  JSONB NOT NULL DEFAULT '[]'::jsonb,
    recommended_use_cases JSONB NOT NULL DEFAULT '[]'::jsonb,
    default_func_spec     JSONB NOT NULL,
    is_active             BOOLEAN NOT NULL DEFAULT true,
    catalog_version       INTEGER NOT NULL DEFAULT 1,
    createtime            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updatetime            TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_mce_provider ON ModelCatalogEntry (provider);
CREATE INDEX IF NOT EXISTS idx_mce_type ON ModelCatalogEntry (model_type);
CREATE INDEX IF NOT EXISTS idx_mce_active ON ModelCatalogEntry (is_active);
CREATE INDEX IF NOT EXISTS idx_mce_tags ON ModelCatalogEntry USING GIN (tags);

CREATE OR REPLACE FUNCTION set_updatetime()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updatetime = now();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS mce_updatetime ON ModelCatalogEntry;
CREATE TRIGGER mce_updatetime BEFORE UPDATE ON ModelCatalogEntry
FOR EACH ROW EXECUTE FUNCTION set_updatetime();
