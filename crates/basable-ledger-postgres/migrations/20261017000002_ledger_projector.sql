-- migrate:up

-- The optional projector's tables. Apply this migration only when the
-- implementer runs the projector; the engine never reads these tables and
-- a claim sums its batch from the journals.
SET LOCAL ROLE nano_ledger;
SET LOCAL search_path = nano_ledger;

-- The balance of every (batch, unit) the projector has applied.
CREATE TABLE batch_balance (
    batch_key BIGINT NOT NULL,
    unit_key  BIGINT NOT NULL,
    amount    NUMERIC(39, 0) NOT NULL,
    PRIMARY KEY (batch_key, unit_key)
);

-- The last sequence applied per process: the monotonic check. A journal
-- applies only as the stored sequence plus one; at or below is the overlap
-- showing it again; two or more above is a missed journal and an error.
CREATE TABLE projected_process (
    process_id UUID PRIMARY KEY,
    sequence   BIGINT NOT NULL CHECK (sequence >= 1)
);

-- Where each named projector's window starts: the start time of its last
-- run, minus the overlap on the next read.
CREATE TABLE projector_watermark (
    name    TEXT PRIMARY KEY,
    through TIMESTAMPTZ NOT NULL
);

RESET ROLE;

-- migrate:down
SET LOCAL ROLE nano_ledger;
SET LOCAL search_path = nano_ledger;
DROP TABLE IF EXISTS projector_watermark;
DROP TABLE IF EXISTS projected_process;
DROP TABLE IF EXISTS batch_balance;
RESET ROLE;
