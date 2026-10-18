-- migrate:up

-- The ledger's tables, in the schema of the nanoservice that owns the
-- ledger. `nano_ledger` is a placeholder: apply this migration with the
-- replacement `nano_ledger` -> `nano_<name>` (basable_db::migrate::apply
-- takes replacements), as the scaffolder renders it for the owning
-- nanoservice. The role creates and owns the tables, like any of its own.
SET LOCAL ROLE nano_ledger;
SET LOCAL search_path = nano_ledger;

-- The write model: one row per commit of one process. The primary key is
-- the compare-and-set (a commit inserts at the highest sequence read plus
-- one; a loser gets a unique violation). Lines are
-- [batch_key, ledger, batch_type, object_id, unit_key, amount], the batch
-- type 1 debit, 2 credit, 3 memo, amounts as decimal strings; snapshot
-- diffs are {object_id, type, full | patch} per object booked. There is no
-- batch table: a batch key is the user's, and every line carries the
-- batch's ledger and type with it, so this table alone checks and reads
-- back. A batch's balance is the sum of its lines; zero is a closed batch.
-- booked_at is the journal's one time: when something happened in the
-- world is a field of the object, in its diff, read through the user's
-- reader. rules_versions is {type_name: version} for the object types
-- whose rules version this journal moved (a type's first booking in the
-- process, a restatement); usually {}. The process's version per type is
-- the fold of these over its journals, which is all a booking reads.
CREATE TABLE journal (
    process_id     UUID NOT NULL,
    sequence       BIGINT NOT NULL CHECK (sequence >= 1),
    booked_at      TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    lines          JSONB NOT NULL CHECK (jsonb_typeof(lines) = 'array'),
    snapshot_diffs JSONB NOT NULL CHECK (jsonb_typeof(snapshot_diffs) = 'array'),
    rules_versions JSONB NOT NULL CHECK (jsonb_typeof(rules_versions) = 'object'),
    PRIMARY KEY (process_id, sequence)
);
-- A batch's lines across every process: the batch sum's index, over the
-- batch keys a journal's lines carry and nothing else, so a key never
-- matches a ledger, a type or a unit that happens to share its value. The
-- query names the same expression:
--   WHERE jsonb_path_query_array(lines, '$[*][0]') @> '[<batch_key>]'
CREATE INDEX idx_journal_batch_keys
    ON journal USING GIN ((jsonb_path_query_array(lines, '$[*][0]')) jsonb_path_ops);

-- An index of the journals' rules_versions: the highest version recorded
-- per process and object type, upserted by the commit's own statement and
-- never read by a booking. The startup restatement's worklist is every row
-- behind the running version.
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
SET LOCAL ROLE nano_ledger;
SET LOCAL search_path = nano_ledger;
DROP TABLE IF EXISTS process_ledger_object_version;
DROP TABLE IF EXISTS journal;
RESET ROLE;
