-- migrate:up

-- The reserved conformance type (key 32000, the top of the SMALLINT range no
-- production type uses): the registry row and the partition as the migrator,
-- handed to the nanoservice role, then the typed tables as that role. The
-- shape of a real type's migration; the typed tables reference the
-- PARTITION.
--
-- conformance_status carries a CHECK (provisioned_widgets >= 0): a
-- completion whose reconciler returns a negative status hits it, and the
-- framework's status savepoint converts the class-23 violation into a loud
-- Retry — the seam the status-constraint conformance test drives.
--
-- conformance_archive is deliberately NOT foreign-keyed to the envelope: it
-- is the durable teardown tombstone finalize_delete writes, and it must
-- outlive the deleted object.

INSERT INTO basable.processing_object_type (key, name) VALUES (32000, 'conformance');

CREATE TABLE nano_conformance.processing_object_conformance
    PARTITION OF basable.processing_object FOR VALUES IN (32000)
    WITH (fillfactor = 90);
ALTER TABLE nano_conformance.processing_object_conformance OWNER TO nano_conformance;

SET LOCAL ROLE nano_conformance;
SET LOCAL search_path = nano_conformance;

CREATE TABLE conformance_spec (
    id UUID PRIMARY KEY,
    processing_object_type_key SMALLINT NOT NULL DEFAULT 32000
        CHECK (processing_object_type_key = 32000),
    widgets INTEGER NOT NULL CHECK (widgets >= 0),
    content TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    FOREIGN KEY (processing_object_type_key, id)
        REFERENCES processing_object_conformance (processing_object_type_key, id)
        ON DELETE CASCADE
);

CREATE TABLE conformance_status (
    id UUID PRIMARY KEY,
    processing_object_type_key SMALLINT NOT NULL DEFAULT 32000
        CHECK (processing_object_type_key = 32000),
    provisioned_widgets INTEGER NOT NULL CHECK (provisioned_widgets >= 0),
    external_id TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    FOREIGN KEY (processing_object_type_key, id)
        REFERENCES processing_object_conformance (processing_object_type_key, id)
        ON DELETE CASCADE
);

CREATE TABLE conformance_archive (
    id UUID PRIMARY KEY,
    widgets INTEGER NOT NULL,
    content TEXT NOT NULL,
    provisioned_widgets INTEGER NOT NULL,
    external_id TEXT NOT NULL,
    archived_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
RESET ROLE;

-- migrate:down
SET LOCAL ROLE nano_conformance;
SET LOCAL search_path = nano_conformance;
DROP TABLE IF EXISTS conformance_archive;
DROP TABLE IF EXISTS conformance_status;
DROP TABLE IF EXISTS conformance_spec;
RESET ROLE;
DROP TABLE IF EXISTS nano_conformance.processing_object_conformance;
DELETE FROM basable.processing_object_type WHERE key = 32000 AND name = 'conformance';
