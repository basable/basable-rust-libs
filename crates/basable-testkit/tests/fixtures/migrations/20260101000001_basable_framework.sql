-- migrate:up

-- The basable framework schemas, vendored verbatim from the crates at
-- 0.1.0. Never edit an applied migration; the frameworks
-- ship their own follow-ups with the crate version.
--
-- `basable` holds the processing-object type registry and the envelope
-- parent every nanoservice's type partitions; `basable_config` holds the
-- declarative configuration framework. Each stateful nanoservice gets its
-- own schema and NOLOGIN role in its init migration; the CNPG-created `app`
-- login is a member of every nanoservice role (SET ROLE per pool), so a
-- cross-schema query is a permission error.

REVOKE ALL ON SCHEMA public FROM PUBLIC;

CREATE SCHEMA IF NOT EXISTS basable;
CREATE SCHEMA IF NOT EXISTS basable_config;

-- ---------------------------------------------------------------------------
-- Processing objects: registry + envelope parent (partitioned per type).
-- ---------------------------------------------------------------------------
CREATE TABLE basable.processing_object_type (
    key  SMALLINT PRIMARY KEY,
    name VARCHAR(64) NOT NULL UNIQUE
);

CREATE TABLE basable.processing_object (
    id UUID NOT NULL DEFAULT gen_random_uuid(),
    processing_object_type_key SMALLINT NOT NULL
        REFERENCES basable.processing_object_type (key),
    external_id VARCHAR(255) NOT NULL,
    name VARCHAR(255) NOT NULL,
    namespace UUID NOT NULL,
    CONSTRAINT ck_processing_object_name_nonempty CHECK (name <> ''),
    labels JSONB,
    CONSTRAINT ck_processing_object_labels_object
        CHECK (labels IS NULL OR jsonb_typeof(labels) = 'object'),
    generation BIGINT NOT NULL DEFAULT 1 CHECK (generation >= 1),
    generation_changed_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    observed_generation BIGINT NOT NULL DEFAULT 0,
    CONSTRAINT ck_processing_object_gen_order
        CHECK (observed_generation >= 0 AND observed_generation <= generation),
    phase VARCHAR(16) NOT NULL DEFAULT 'pending'
        CHECK (phase IN ('pending', 'retrying', 'converged', 'blocked')),
    deleted_at TIMESTAMPTZ,
    last_error TEXT CHECK (octet_length(last_error) <= 4096),
    claim_token UUID,
    claimed_at TIMESTAMPTZ,
    lease_expires_at TIMESTAMPTZ,
    CONSTRAINT ck_processing_object_lease_coherent CHECK (
        (claim_token IS NULL AND claimed_at IS NULL AND lease_expires_at IS NULL)
        OR
        (claim_token IS NOT NULL AND claimed_at IS NOT NULL AND lease_expires_at IS NOT NULL
            AND lease_expires_at > claimed_at)
    ),
    next_reconcile_at TIMESTAMPTZ NOT NULL DEFAULT '1970-01-01 00:00:00+00',
    wake_seq BIGINT NOT NULL DEFAULT 0 CHECK (wake_seq >= 0),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    last_reconciled_at TIMESTAMPTZ,
    due_at TIMESTAMPTZ GENERATED ALWAYS AS (
        CASE
            WHEN observed_generation < generation THEN '1970-01-01 00:00:00+00'::timestamptz
            ELSE next_reconcile_at
        END
    ) STORED,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (processing_object_type_key, id)
) PARTITION BY LIST (processing_object_type_key);

CREATE INDEX idx_processing_object_scan
    ON basable.processing_object (due_at, last_reconciled_at ASC NULLS FIRST, id)
    WHERE due_at < '2900-01-01 00:00:00+00';

CREATE UNIQUE INDEX uq_processing_object_namespace_name
    ON basable.processing_object (processing_object_type_key, namespace, name)
    WHERE deleted_at IS NULL;

CREATE INDEX idx_processing_object_labels
    ON basable.processing_object USING GIN (labels jsonb_path_ops)
    WHERE labels IS NOT NULL;

-- ---------------------------------------------------------------------------
-- Declarative configuration framework (basable-config).
-- ---------------------------------------------------------------------------
CREATE TABLE basable_config.configuration_type (
    id     SMALLINT PRIMARY KEY,
    name   VARCHAR(64) NOT NULL UNIQUE,
    prefix VARCHAR(8) NOT NULL UNIQUE
);

CREATE TABLE basable_config.configuration_object (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    type_id SMALLINT NOT NULL REFERENCES basable_config.configuration_type (id),
    namespace_id UUID,
    name VARCHAR(255) NOT NULL CHECK (name <> ''),
    labels JSONB NOT NULL DEFAULT '{}'::jsonb,
    spec JSONB NOT NULL,
    version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (type_id, namespace_id, name)
);

CREATE TABLE basable_config.configuration_object_history (
    id UUID NOT NULL,
    version BIGINT NOT NULL,
    spec JSONB NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (id, version)
);

INSERT INTO basable_config.configuration_type (id, name, prefix) VALUES (1, 'namespace', 'ns');

-- The seed's own namespace (config/base/*.json objects live under it).
INSERT INTO basable_config.configuration_object (id, type_id, namespace_id, name, spec)
VALUES ('00000000-0000-0000-0000-000000000001', 1, NULL, 'default', '{}'::jsonb);

-- ---------------------------------------------------------------------------
-- The app login: CNPG creates it as the database owner; every nanoservice
-- role is granted to it WITH SET so each pool can SET ROLE to its own.
-- ---------------------------------------------------------------------------
GRANT USAGE ON SCHEMA basable TO app;
GRANT SELECT ON basable.processing_object_type TO app;
GRANT USAGE ON SCHEMA basable_config TO app;
GRANT SELECT ON ALL TABLES IN SCHEMA basable_config TO app;
GRANT INSERT, UPDATE, DELETE ON basable_config.configuration_object TO app;
GRANT INSERT ON basable_config.configuration_object_history TO app;

-- migrate:down
DROP SCHEMA IF EXISTS basable_config CASCADE;
DROP SCHEMA IF EXISTS basable CASCADE;
