-- migrate:up
SET LOCAL ROLE nano_catalog;
SET LOCAL search_path = nano_catalog;

-- Plain table `product` of nanoservice `catalog`. Only this
-- nanoservice's role can reach it (the Directive §2).
CREATE TABLE product (
    id UUID NOT NULL PRIMARY KEY,
    sku TEXT NOT NULL UNIQUE,
    price_cents BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

CREATE INDEX idx_product_sku ON product (sku);

-- dbmate records this version in the same transaction, and the role may
-- not write the ledger.
RESET ROLE;

-- migrate:down
SET LOCAL ROLE nano_catalog;
SET LOCAL search_path = nano_catalog;
DROP TABLE IF EXISTS product;
RESET ROLE;
