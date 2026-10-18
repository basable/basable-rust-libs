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
-- [batch_key, object_id, unit_key, amount], amounts as decimal strings;
-- snapshot diffs are {object_id, type, full | patch} per object booked.
CREATE TABLE journal (
    process_id     UUID NOT NULL,
    sequence       BIGINT NOT NULL CHECK (sequence >= 1),
    event_time     TIMESTAMPTZ NOT NULL,
    booked_at      TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    lines          JSONB NOT NULL CHECK (jsonb_typeof(lines) = 'array'),
    snapshot_diffs JSONB NOT NULL CHECK (jsonb_typeof(snapshot_diffs) = 'array'),
    PRIMARY KEY (process_id, sequence)
);
-- A batch's lines across every process: `lines @> '[[<batch_key>]]'`.
CREATE INDEX idx_journal_lines ON journal USING GIN (lines jsonb_path_ops);

-- One physical batch per (batch_id, epoch). The framework knows of a batch
-- only its ledger and its type (1 debit, 2 credit, 3 memo); what the id
-- means is the user's. Status: 1 open, 2 closed, 3 claimed, 4 consumed.
-- A commit locks the batches it writes into FOR SHARE and refuses one that
-- is not open, so the close's UPDATE waits for every in-flight writer and
-- nothing lands in a batch after its close.
CREATE TABLE batch (
    batch_key  BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    batch_id   TEXT NOT NULL,
    epoch      INTEGER NOT NULL CHECK (epoch >= 1),
    ledger     BIGINT NOT NULL,
    batch_type SMALLINT NOT NULL CHECK (batch_type IN (1, 2, 3)),
    status     SMALLINT NOT NULL DEFAULT 1 CHECK (status IN (1, 2, 3, 4)),
    closed_at  TIMESTAMPTZ,
    UNIQUE (batch_id, epoch)
);

-- What a claim found in a batch, per unit, recorded with the status change
-- to claimed in one transaction so a claimant that crashes afterwards reads
-- the same amounts again.
CREATE TABLE batch_claim (
    batch_key BIGINT NOT NULL REFERENCES batch (batch_key),
    unit_key  BIGINT NOT NULL,
    amount    NUMERIC(39, 0) NOT NULL,
    PRIMARY KEY (batch_key, unit_key)
);

-- The desired_state version that last evaluated an object type in a
-- process; the restatement sweep's worklist is every row behind the
-- running version.
CREATE TABLE process_accounting_object_version (
    process_id UUID NOT NULL,
    type_name  TEXT NOT NULL,
    version    INTEGER NOT NULL,
    PRIMARY KEY (process_id, type_name)
);
CREATE INDEX idx_process_accounting_object_version_sweep
    ON process_accounting_object_version (type_name, version);

RESET ROLE;

-- migrate:down
SET LOCAL ROLE nano_ledger;
SET LOCAL search_path = nano_ledger;
DROP TABLE IF EXISTS process_accounting_object_version;
DROP TABLE IF EXISTS batch_claim;
DROP TABLE IF EXISTS batch;
DROP TABLE IF EXISTS journal;
RESET ROLE;
