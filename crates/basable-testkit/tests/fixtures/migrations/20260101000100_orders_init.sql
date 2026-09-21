-- migrate:up

-- The schema and role of nanoservice `orders` (the scaffolder's init.sql,
-- rendered by hand for this fixture).

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'nano_orders') THEN
        CREATE ROLE nano_orders NOLOGIN NOINHERIT;
    END IF;
END $$;

GRANT nano_orders TO app WITH SET TRUE, INHERIT FALSE;

-- The schema belongs to the app login (the migrator, which also owns the
-- envelope parent and creates a type's partition in here); the role may
-- create in it, and everything the role creates is the role's. The app
-- login gets no right on those tables: unswitched, it is refused like any
-- other role.
CREATE SCHEMA IF NOT EXISTS nano_orders;
GRANT USAGE, CREATE ON SCHEMA nano_orders TO nano_orders;
ALTER ROLE nano_orders SET search_path = nano_orders;

GRANT USAGE ON SCHEMA basable TO nano_orders;
GRANT SELECT ON basable.processing_object_type TO nano_orders;
GRANT USAGE ON SCHEMA basable_config TO nano_orders;
GRANT SELECT ON ALL TABLES IN SCHEMA basable_config TO nano_orders;

-- migrate:down
DROP SCHEMA IF EXISTS nano_orders CASCADE;
REVOKE nano_orders FROM app;
DROP ROLE IF EXISTS nano_orders;
