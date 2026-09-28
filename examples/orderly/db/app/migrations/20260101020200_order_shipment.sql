-- migrate:up

-- Processing-object type `shipment` (key 2) of nanoservice
-- `order`: the registry row, the envelope partition INSIDE this
-- nanoservice's schema, and the typed spec/status tables hanging off the
-- partition by composite foreign key. The envelope owns identity,
-- generation, scheduling, deletion intent and claim authority; these tables
-- carry domain columns only (the Directive §3 invariant 1).
--
-- The registry row and the partition need the envelope parent's owner, the
-- `app` login this migration runs as, so they come first; the partition is
-- then handed to the nanoservice role, whose typed tables follow under
-- SET LOCAL ROLE. Every framework statement targets the partition, and the
-- typed tables reference the partition, so the role needs nothing on the
-- parent. RESET ROLE at the end: dbmate records this version in the same
-- transaction, and the role may not write the ledger.

INSERT INTO basable.processing_object_type (key, name) VALUES (2, 'shipment');

CREATE TABLE nano_order.processing_object_shipment
    PARTITION OF basable.processing_object FOR VALUES IN (2)
    WITH (fillfactor = 90);
ALTER TABLE nano_order.processing_object_shipment OWNER TO nano_order;

SET LOCAL ROLE nano_order;
SET LOCAL search_path = nano_order;

-- Desired state only.
CREATE TABLE shipment_spec (
    id UUID PRIMARY KEY,
    processing_object_type_key SMALLINT NOT NULL DEFAULT 2
        CHECK (processing_object_type_key = 2),
    -- TODO: the desired-state columns. Mark identity columns immutable in
    -- the trigger below and mirror every column in type.rs and adapter.rs.
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    FOREIGN KEY (processing_object_type_key, id)
        REFERENCES processing_object_shipment (processing_object_type_key, id)
        ON DELETE CASCADE
);


-- Observed state only, written exclusively under exact claim authority
-- (fenced completions and Claim::write_status). No unfenced writer exists.
CREATE TABLE shipment_status (
    id UUID PRIMARY KEY,
    processing_object_type_key SMALLINT NOT NULL DEFAULT 2
        CHECK (processing_object_type_key = 2),
    -- TODO: the observed-state columns.
    -- Declared-effect slot (uncomment when an adapter uses Strategy::Declared):
    -- effect_operation VARCHAR(64),
    -- effect_key TEXT,
    -- effect_declared_at TIMESTAMPTZ,
    -- effect_detail JSONB,
    -- CHECK ((effect_operation IS NULL) = (effect_key IS NULL) AND (effect_operation IS NULL) = (effect_declared_at IS NULL)),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    FOREIGN KEY (processing_object_type_key, id)
        REFERENCES processing_object_shipment (processing_object_type_key, id)
        ON DELETE CASCADE
);

RESET ROLE;

-- migrate:down
SET LOCAL ROLE nano_order;
SET LOCAL search_path = nano_order;
DROP TABLE IF EXISTS shipment_status;
DROP TABLE IF EXISTS shipment_spec;
RESET ROLE;
DROP TABLE IF EXISTS nano_order.processing_object_shipment;
DELETE FROM basable.processing_object_type WHERE key = 2 AND name = 'shipment';
