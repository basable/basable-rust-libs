-- migrate:up

-- Processing-object type `order` (key 1) of nanoservice `orders`: the
-- scaffolder's type migration, rendered by hand for this fixture.
--
-- The registry row and the partition need the envelope parent's owner (the
-- app login the migration runs as), so they come first; the partition is
-- then handed to the nanoservice role, whose typed tables follow under
-- SET LOCAL ROLE. The typed tables reference the PARTITION, not the parent:
-- the same rows, and the role needs nothing on the parent. RESET ROLE at
-- the end: dbmate records the version in this same transaction, and the
-- role may not write the ledger.

INSERT INTO basable.processing_object_type (key, name) VALUES (1, 'order');

CREATE TABLE nano_orders.processing_object_order
    PARTITION OF basable.processing_object FOR VALUES IN (1)
    WITH (fillfactor = 90);
ALTER TABLE nano_orders.processing_object_order OWNER TO nano_orders;

SET LOCAL ROLE nano_orders;
SET LOCAL search_path = nano_orders;

CREATE TABLE order_spec (
    id UUID PRIMARY KEY,
    processing_object_type_key SMALLINT NOT NULL DEFAULT 1
        CHECK (processing_object_type_key = 1),
    customer TEXT NOT NULL, -- immutable (trigger below)
    lines INTEGER NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    FOREIGN KEY (processing_object_type_key, id)
        REFERENCES processing_object_order (processing_object_type_key, id)
        ON DELETE CASCADE
);

CREATE FUNCTION reject_order_spec_identity_change()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.customer IS DISTINCT FROM OLD.customer THEN
        RAISE EXCEPTION 'order identity is immutable (attempted change on %)', OLD.id
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER trg_order_spec_identity_immutable
    BEFORE UPDATE ON order_spec
    FOR EACH ROW EXECUTE FUNCTION reject_order_spec_identity_change();

CREATE TABLE order_status (
    id UUID PRIMARY KEY,
    processing_object_type_key SMALLINT NOT NULL DEFAULT 1
        CHECK (processing_object_type_key = 1),
    phase TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    FOREIGN KEY (processing_object_type_key, id)
        REFERENCES processing_object_order (processing_object_type_key, id)
        ON DELETE CASCADE
);
RESET ROLE;

-- migrate:down
SET LOCAL ROLE nano_orders;
SET LOCAL search_path = nano_orders;
DROP TABLE IF EXISTS order_status;
DROP TABLE IF EXISTS order_spec;
DROP FUNCTION IF EXISTS reject_order_spec_identity_change();
RESET ROLE;
DROP TABLE IF EXISTS nano_orders.processing_object_order;
DELETE FROM basable.processing_object_type WHERE key = 1 AND name = 'order';
