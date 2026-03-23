DO $$
BEGIN
    IF to_regclass('catalogmodel') IS NULL THEN
        IF to_regclass('modelcatalog') IS NOT NULL THEN
            EXECUTE 'ALTER TABLE ModelCatalog RENAME TO CatalogModel';
        ELSIF to_regclass('modelcatalogentry') IS NOT NULL THEN
            EXECUTE 'ALTER TABLE ModelCatalogEntry RENAME TO CatalogModel';
        END IF;
    END IF;
END;
$$;

CREATE TABLE IF NOT EXISTS CatalogModel (
    id                    BIGSERIAL PRIMARY KEY,
    slug                  VARCHAR NOT NULL UNIQUE,
    display_name          VARCHAR NOT NULL,
    provider              VARCHAR NOT NULL,
    modality              VARCHAR NOT NULL,
    brief_intro           TEXT NOT NULL,
    detailed_intro        TEXT NOT NULL DEFAULT '',
    source_kind           VARCHAR NOT NULL DEFAULT 'huggingface',
    source_model_id       VARCHAR NOT NULL,
    parameter_count_b     NUMERIC(10,2),
    is_moe                BOOLEAN NOT NULL DEFAULT false,
    tags                  JSONB NOT NULL DEFAULT '[]'::jsonb,
    recommended_use_cases JSONB NOT NULL DEFAULT '[]'::jsonb,
    default_func_spec     JSONB NOT NULL,
    is_active             BOOLEAN NOT NULL DEFAULT false,
    catalog_version       INTEGER NOT NULL DEFAULT 1,
    createtime            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updatetime            TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE CatalogModel
ALTER COLUMN is_active SET DEFAULT false;

CREATE INDEX IF NOT EXISTS idx_mce_provider ON CatalogModel (provider);
CREATE INDEX IF NOT EXISTS idx_mce_modality ON CatalogModel (modality);
CREATE INDEX IF NOT EXISTS idx_mce_active ON CatalogModel (is_active);
CREATE INDEX IF NOT EXISTS idx_mce_source_model_id ON CatalogModel (source_model_id);
CREATE INDEX IF NOT EXISTS idx_mce_tags ON CatalogModel USING GIN (tags);

CREATE OR REPLACE FUNCTION set_updatetime()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updatetime = now();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

ALTER TABLE CatalogModel
DROP CONSTRAINT IF EXISTS modelcatalogentry_source_model_id_key;

ALTER TABLE CatalogModel
DROP CONSTRAINT IF EXISTS modelcatalog_source_model_id_key;

ALTER TABLE CatalogModel
DROP CONSTRAINT IF EXISTS catalogmodel_source_model_id_key;

DROP TRIGGER IF EXISTS mce_updatetime ON CatalogModel;
CREATE TRIGGER mce_updatetime BEFORE UPDATE ON CatalogModel
FOR EACH ROW EXECUTE FUNCTION set_updatetime();
