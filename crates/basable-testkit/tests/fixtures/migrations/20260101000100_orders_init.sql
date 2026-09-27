-- migrate:up

-- The schema and role of nanoservice `orders` (the scaffolder's init.sql,
-- rendered by hand for this fixture).

-- The role is cluster-global and two databases may run this migration at
-- once (a test database per test). The existence check is not atomic, so
-- the concurrent loser's CREATE ROLE is caught and the check repeated: the
-- repeat is what makes the winner's committed role visible to the rest of
-- this transaction (its scan of pg_roles accepts the catalog invalidation
-- the winner's commit sent, which the failed CREATE ROLE left a stale
-- negative cache entry behind for).
DO $$
DECLARE
    attempts int := 0;
BEGIN
    LOOP
        IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'nano_orders') THEN
            EXIT;
        END IF;
        BEGIN
            CREATE ROLE nano_orders NOLOGIN NOINHERIT;
            EXIT;
        EXCEPTION WHEN duplicate_object OR unique_violation THEN
            attempts := attempts + 1;
            IF attempts > 50 THEN
                RAISE;
            END IF;
            PERFORM pg_sleep(0.05);
        END;
    END LOOP;
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
ALTER DEFAULT PRIVILEGES IN SCHEMA basable_config GRANT SELECT ON TABLES TO nano_orders;

-- migrate:down
DROP SCHEMA IF EXISTS nano_orders CASCADE;
REVOKE nano_orders FROM app;
DROP ROLE IF EXISTS nano_orders;
