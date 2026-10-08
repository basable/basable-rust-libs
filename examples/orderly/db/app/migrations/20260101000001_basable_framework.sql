-- migrate:up

-- The basable framework schemas, vendored verbatim from the crates at
-- 0.2.0. Never edit an applied migration; the frameworks
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
-- Declarative configuration framework (basable-config): the monorepo's
-- config framework verbatim, in the basable_config schema. Storage is
-- temporal class-table inheritance: a base `configuration_object` table
-- whose live rows carry an open system_period, plus a `_history` parent
-- holding closed versions; each live table INHERITS its `_history` table, so
-- the parent returns the full history and the child only current rows. The
-- BEFORE ROW `versioning()` trigger maintains the periods; history is
-- written ONLY by the trigger, never by application code. Every config type
-- adds one subtype table pair in its own migration.
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION basable_config.versioning() RETURNS TRIGGER AS $versioning$
DECLARE
    sys_period text := TG_ARGV[0];
    history_table text := TG_ARGV[1];
    ignore_unchanged_values bool := COALESCE(TG_ARGV[3]::bool, false);
    common_columns text[];
    time_stamp_to_use timestamptz := current_timestamp;
    range_lower timestamptz;
    manipulate jsonb;
BEGIN
    IF TG_WHEN != 'BEFORE' OR TG_LEVEL != 'ROW' THEN
        RAISE EXCEPTION 'versioning() must be fired BEFORE ROW';
    END IF;
    IF TG_OP NOT IN ('INSERT', 'UPDATE', 'DELETE') THEN
        RAISE EXCEPTION 'versioning() must be fired for INSERT/UPDATE/DELETE';
    END IF;

    -- Skip no-op updates: nothing changed -> no history row, no write.
    IF ignore_unchanged_values AND TG_OP = 'UPDATE' AND NEW IS NOT DISTINCT FROM OLD THEN
        RETURN NULL;
    END IF;

    IF TG_OP = 'UPDATE' OR TG_OP = 'DELETE' THEN
        EXECUTE format('SELECT lower($1.%I)', sys_period) USING OLD INTO range_lower;
        IF range_lower IS NULL THEN
            range_lower := time_stamp_to_use;
        END IF;

        -- Columns common to the live table and its history table, minus the
        -- system_period column (which the trigger sets explicitly).
        SELECT array_agg(quote_ident(attname)) INTO common_columns
        FROM (
            SELECT a.attname
            FROM pg_attribute a
            WHERE a.attrelid = TG_RELID AND a.attnum > 0 AND NOT a.attisdropped AND a.attname <> sys_period
            INTERSECT
            SELECT a.attname
            FROM pg_attribute a
            WHERE a.attrelid = history_table::regclass AND a.attnum > 0 AND NOT a.attisdropped AND a.attname <> sys_period
        ) s;

        EXECUTE format(
            'INSERT INTO %s (%s, %I) VALUES (%s, tstzrange($2, $3, ''[)''))',
            history_table,
            array_to_string(common_columns, ','),
            sys_period,
            '$1.' || array_to_string(common_columns, ',$1.')
        ) USING OLD, range_lower, time_stamp_to_use;
    END IF;

    IF TG_OP = 'INSERT' OR TG_OP = 'UPDATE' THEN
        manipulate := jsonb_set('{}'::jsonb, ('{' || sys_period || '}')::text[],
            to_jsonb(tstzrange(time_stamp_to_use, null, '[)')));
        RETURN jsonb_populate_record(NEW, manipulate);
    END IF;

    RETURN OLD;
END;
$versioning$ LANGUAGE plpgsql SECURITY DEFINER SET search_path = basable_config, pg_catalog;

-- Type discriminator lookup. Each config type registers its SMALLINT id in
-- its own migration; the id must match its TypeInfo in the nanoservice's
-- config.rs.
CREATE TABLE basable_config.configuration_object_type (
    id   SMALLINT PRIMARY KEY,
    name VARCHAR NOT NULL UNIQUE
);

-- Base table (temporal, via INHERITS).
CREATE TABLE basable_config.configuration_object_history (
    id            UUID NOT NULL,
    system_period TSTZRANGE NOT NULL DEFAULT tstzrange(current_timestamp, NULL),
    external_id   VARCHAR NOT NULL,
    configuration_object_type_id SMALLINT NOT NULL REFERENCES basable_config.configuration_object_type(id),
    name          VARCHAR NOT NULL,
    namespace_id  UUID NOT NULL,                 -- = id for namespace objects (root scope self-reference)
    -- Loader-managed objects carry the label basable.com/managed-by=config
    -- (stamped by the loader); runtime-written ones carry =runtime.
    labels        JSONB                          -- string => string map
);
CREATE UNIQUE INDEX configuration_object_history_lookup
    ON basable_config.configuration_object_history (id, lower(system_period),
        coalesce(upper(system_period), 'infinity') DESC);

CREATE TABLE basable_config.configuration_object (
    PRIMARY KEY (id),
    FOREIGN KEY (configuration_object_type_id) REFERENCES basable_config.configuration_object_type(id),
    -- Containment: deleting a namespace cascade-deletes every object in it
    -- (and, via the subtype id FKs, their subtype rows).
    FOREIGN KEY (namespace_id) REFERENCES basable_config.configuration_object(id) ON DELETE CASCADE
) INHERITS (basable_config.configuration_object_history);

ALTER TABLE basable_config.configuration_object ALTER COLUMN id SET DEFAULT gen_random_uuid();

CREATE UNIQUE INDEX configuration_object_external_id ON basable_config.configuration_object(external_id);
-- Natural key for namespaced objects. Namespace objects self-reference
-- (namespace_id = id, distinct per object) so this composite does not enforce
-- their name uniqueness — that is done by the global partial index below.
CREATE UNIQUE INDEX configuration_object_type_ns_name
    ON basable_config.configuration_object(configuration_object_type_id, namespace_id, name);

CREATE TRIGGER versioning_trigger
    BEFORE INSERT OR UPDATE OR DELETE ON basable_config.configuration_object
    FOR EACH ROW EXECUTE PROCEDURE basable_config.versioning('system_period', 'basable_config.configuration_object_history', true, true);

-- NamespaceConfiguration (type id 1). The root scope object; referenceable by
-- its plain name. Its identity (name) lives on configuration_object; the
-- subtype table holds the display name.
INSERT INTO basable_config.configuration_object_type (id, name) VALUES (1, 'NamespaceConfiguration');

CREATE TABLE basable_config.namespace_configuration_history (
    id            UUID NOT NULL,
    system_period TSTZRANGE NOT NULL DEFAULT tstzrange(current_timestamp, NULL),
    display_name  VARCHAR
);
CREATE UNIQUE INDEX namespace_configuration_history_lookup
    ON basable_config.namespace_configuration_history (id, lower(system_period),
        coalesce(upper(system_period), 'infinity') DESC);

CREATE TABLE basable_config.namespace_configuration (
    PRIMARY KEY (id),
    FOREIGN KEY (id) REFERENCES basable_config.configuration_object(id) ON DELETE CASCADE
) INHERITS (basable_config.namespace_configuration_history);

CREATE TRIGGER versioning_trigger
    BEFORE INSERT OR UPDATE OR DELETE ON basable_config.namespace_configuration
    FOR EACH ROW EXECUTE PROCEDURE basable_config.versioning('system_period', 'basable_config.namespace_configuration_history', true, true);

-- Global namespace-name uniqueness: namespace objects self-reference, so the
-- (type, namespace_id, name) key does not cover them.
CREATE UNIQUE INDEX configuration_object_namespace_name
    ON basable_config.configuration_object(name) WHERE configuration_object_type_id = 1;

-- ---------------------------------------------------------------------------
-- The app login: CNPG creates it as the database owner; every nanoservice
-- role is granted to it WITH SET so each pool can SET ROLE to its own.
-- ---------------------------------------------------------------------------
GRANT USAGE ON SCHEMA basable TO app;
GRANT SELECT ON basable.processing_object_type TO app;

-- migrate:down
DROP SCHEMA IF EXISTS basable_config CASCADE;
DROP SCHEMA IF EXISTS basable CASCADE;
