-- migrate:up

-- The batch tables of the ledger owned by nanoservice `accounting`,
-- rendered from owns.ledger.batches: one table per kind of batch, one row per
-- batch, and ONE sequence allocating batch_key over all of them, because a
-- stored line carries that key alone (the Directive §11). A row lands in the
-- commit that first books on the batch, and it is for good.

SET LOCAL ROLE nano_accounting;
SET LOCAL search_path = nano_accounting;

CREATE SEQUENCE batch_key_seq;

CREATE TABLE receivable_batch (
    order_id UUID NOT NULL,
    batch_key BIGINT NOT NULL UNIQUE DEFAULT nextval('batch_key_seq'),
    ledger INTEGER NOT NULL,
    batch_type SMALLINT NOT NULL,
    PRIMARY KEY (order_id)
);

CREATE TABLE cash_batch (
    month TEXT NOT NULL,
    batch_key BIGINT NOT NULL UNIQUE DEFAULT nextval('batch_key_seq'),
    ledger INTEGER NOT NULL,
    batch_type SMALLINT NOT NULL,
    PRIMARY KEY (month)
);

CREATE TABLE revenue_batch (
    month TEXT NOT NULL,
    batch_key BIGINT NOT NULL UNIQUE DEFAULT nextval('batch_key_seq'),
    ledger INTEGER NOT NULL,
    batch_type SMALLINT NOT NULL,
    PRIMARY KEY (month)
);

RESET ROLE;

-- migrate:down
SET LOCAL ROLE nano_accounting;
SET LOCAL search_path = nano_accounting;
DROP TABLE IF EXISTS receivable_batch;
DROP TABLE IF EXISTS cash_batch;
DROP TABLE IF EXISTS revenue_batch;
DROP SEQUENCE IF EXISTS batch_key_seq;
RESET ROLE;
