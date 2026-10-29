-- migrate:up
SET LOCAL ROLE nano_order;
SET LOCAL search_path = nano_order;

-- Plain table `order_audit` of nanoservice `order`. Only this
-- nanoservice's role can reach it (the Directive §2).
-- `created_at` and `updated_at` are added when the plan declares none of
-- its own; a declared one is the plan's, without the default.
CREATE TABLE order_audit (
    -- TODO: the columns (mirror them in model.rs).
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
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
