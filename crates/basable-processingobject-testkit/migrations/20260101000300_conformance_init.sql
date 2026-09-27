-- migrate:up

-- The schema and role of the reserved conformance nanoservice: the
-- scaffolder's init.sql shape, applied by the testkit on top of the tenant
-- fixture (the framework migration must have run). Test-only; never part of
-- a production tree.

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
        IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'nano_conformance') THEN
            EXIT;
        END IF;
        BEGIN
            CREATE ROLE nano_conformance NOLOGIN NOINHERIT;
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

GRANT nano_conformance TO app WITH SET TRUE, INHERIT FALSE;

CREATE SCHEMA IF NOT EXISTS nano_conformance;
GRANT USAGE, CREATE ON SCHEMA nano_conformance TO nano_conformance;
ALTER ROLE nano_conformance SET search_path = nano_conformance;

GRANT USAGE ON SCHEMA basable TO nano_conformance;
GRANT SELECT ON basable.processing_object_type TO nano_conformance;

-- migrate:down
DROP SCHEMA IF EXISTS nano_conformance CASCADE;
REVOKE nano_conformance FROM app;
DROP ROLE IF EXISTS nano_conformance;
