-- migrate:up
SET LOCAL ROLE nano_inventory;
SET LOCAL search_path = nano_inventory;

-- Plain table `item` of nanoservice `inventory`.
CREATE TABLE item (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    sku TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

CREATE UNIQUE INDEX idx_item_sku ON item (sku);
RESET ROLE;

-- migrate:down
SET LOCAL ROLE nano_inventory;
SET LOCAL search_path = nano_inventory;
DROP TABLE IF EXISTS item;
RESET ROLE;
