-- migrate:up

-- The first follow-up to the framework migration: the envelope page's
-- index. `TypedStore::list` (basable-processingobject, porting note 97;
-- Go's `InspectPage`) reads one type's partition newest first by
-- `(created_at, id)`, bounded by the keyset cursor
-- `(created_at, id) < ($at, $id)`. This partitioned index gives every
-- partition, present and future, a child the unfiltered page and its bound
-- ride instead of sorting the partition. An applied migration is never
-- edited, so the index is a file of its own; a project scaffolded before it
-- adds this file to db/app/migrations unchanged (the dbmate image and the
-- binary's MIGRATIONS list are globs over that directory).

CREATE INDEX idx_processing_object_created
    ON basable.processing_object (created_at DESC, id DESC);

-- migrate:down
DROP INDEX IF EXISTS basable.idx_processing_object_created;
