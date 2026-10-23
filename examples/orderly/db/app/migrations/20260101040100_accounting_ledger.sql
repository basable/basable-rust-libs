-- migrate:up

-- Vendored from basable-ledger-postgres 0.7.0 (its `MIGRATION`
-- constant, with `nano_ledger` replaced by this nanoservice's schema and role).
-- The ledger owned by nanoservice `accounting`: its tables live in
-- nano_accounting like every table it owns (the Directive §2, §11).

SET LOCAL ROLE nano_accounting;
SET LOCAL search_path = nano_accounting;

-- The write model: one row per commit of one process. The primary key is
-- the compare-and-set (a commit inserts at the highest sequence read plus
-- one; a loser gets a unique violation). Lines are grouped by object, so
-- an object id is written once:
--   {"<object_id>": [[batch_key, unit_key, amount], ...], ...}
-- amounts as decimal strings; snapshot diffs are {object_id, type, full |
-- patch} per object booked. A line carries the batch's key only. The
-- batch's ledger and type are yours, in a batches table of your own
-- (batch_key, ledger, batch_type and whatever else you keep about a
-- batch), found by the key, or encoded in the key; the engine has them at
-- booking time from the desired state and the framework keeps no batch
-- table. A batch's balance is the sum of its lines; zero is a closed
-- batch. booked_at is the journal's one time: when something happened in
-- the world is a field of the object, in its diff, read through the user's
-- reader. This table is all a booking reads.
CREATE TABLE journal (
    process_id     UUID NOT NULL,
    sequence       BIGINT NOT NULL CHECK (sequence >= 1),
    booked_at      TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    lines          JSONB NOT NULL CHECK (jsonb_typeof(lines) = 'object'),
    snapshot_diffs JSONB NOT NULL CHECK (jsonb_typeof(snapshot_diffs) = 'array'),
    PRIMARY KEY (process_id, sequence)
);
-- A batch's lines across every process: the batch sum's index, over the
-- batch keys a journal's lines carry, under every object, and nothing
-- else, so a key never matches a unit or an amount that happens to share
-- its value. The query names the same expression:
--   WHERE jsonb_path_query_array(lines, '$.*[*][0]') @> '[<batch_key>]'
CREATE INDEX idx_journal_batch_keys
    ON journal USING GIN ((jsonb_path_query_array(lines, '$.*[*][0]')) jsonb_path_ops);

-- The rules version each object type of a process was last evaluated
-- under, one row per process and type, written by the commit's own
-- statement (latest wins) and never read by a booking. The startup
-- restatement's worklist is every row behind the running version; a
-- restatement that finds nothing to book moves the row on its own.
CREATE TABLE process_ledger_object_version (
    process_id UUID NOT NULL,
    type_name  TEXT NOT NULL,
    version    INTEGER NOT NULL,
    PRIMARY KEY (process_id, type_name)
);
CREATE INDEX idx_process_ledger_object_version_behind
    ON process_ledger_object_version (type_name, version);

RESET ROLE;

-- migrate:down
SET LOCAL ROLE nano_accounting;
SET LOCAL search_path = nano_accounting;
DROP TABLE IF EXISTS process_ledger_object_version;
DROP TABLE IF EXISTS journal;
RESET ROLE;
