-- migrate:up

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'nano_inventory') THEN
        CREATE ROLE nano_inventory NOLOGIN NOINHERIT;
    END IF;
END $$;

GRANT nano_inventory TO app WITH SET TRUE, INHERIT FALSE;

-- The schema belongs to the app login (the migrator, which also owns the
-- envelope parent and creates a type's partition in here); the role may
-- create in it, and everything the role creates is the role's. The app
-- login gets no right on those tables: unswitched, it is refused like any
-- other role.
CREATE SCHEMA IF NOT EXISTS nano_inventory;
GRANT USAGE, CREATE ON SCHEMA nano_inventory TO nano_inventory;
ALTER ROLE nano_inventory SET search_path = nano_inventory;

GRANT USAGE ON SCHEMA basable TO nano_inventory;
GRANT SELECT ON basable.processing_object_type TO nano_inventory;
GRANT USAGE ON SCHEMA basable_config TO nano_inventory;
GRANT SELECT ON ALL TABLES IN SCHEMA basable_config TO nano_inventory;

-- migrate:down
DROP SCHEMA IF EXISTS nano_inventory CASCADE;
REVOKE nano_inventory FROM app;
DROP ROLE IF EXISTS nano_inventory;
