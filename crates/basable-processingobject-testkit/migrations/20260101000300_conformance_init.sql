-- migrate:up

-- The schema and role of the reserved conformance nanoservice: the
-- scaffolder's init.sql shape, applied by the testkit on top of the tenant
-- fixture (the framework migration must have run). Test-only; never part of
-- a production tree.

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'nano_conformance') THEN
        CREATE ROLE nano_conformance NOLOGIN NOINHERIT;
    END IF;
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
