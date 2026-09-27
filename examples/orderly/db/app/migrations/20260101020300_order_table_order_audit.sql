-- migrate:up
SET LOCAL ROLE nano_order;
SET LOCAL search_path = nano_order;

-- Plain table `order_audit` of nanoservice `order`. Only this
-- nanoservice's role can reach it (the Directive §2).
CREATE TABLE order_audit (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- TODO: the columns (mirror them in model.rs).
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

-- dbmate records this version in the same transaction, and the role may
-- not write the ledger.
RESET ROLE;

-- migrate:down
SET LOCAL ROLE nano_order;
SET LOCAL search_path = nano_order;
DROP TABLE IF EXISTS order_audit;
RESET ROLE;
