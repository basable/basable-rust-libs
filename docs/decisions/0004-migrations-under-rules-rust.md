# F.4 — migrations under rules_rust

**Status: resolved in Phase 1 (`basable-db`, 2026-09-22), with one revision: the framework DDL is not embedded in the crates but vendored verbatim into the tenant's `db/app/migrations/` by the scaffolder (`framework.sql.tmpl`), so there is ONE ledger and one path; the crate ships the runner and the boot-time `verify`. Under Bazel the SQL files reach the tests as `data`, resolved through `TEST_SRCDIR` (`basable_testkit::runfile`).**

Two migration paths exist by design and must not be confused:

- **The tenant's own schema** is dbmate-format files in one flat
  `db/app/migrations/` (timestamped, exactly like the monorepo's
  `db/state/migrations`), built into a `dbmate_image` with the vendored
  `k8s/infra/dbmate.bzl` macro and run as a Job in apply wave 0
  (`dbmate --wait up`). The app verifies the migration head at boot; it has
  no `migrate` subcommand.
- **The framework's own DDL** (the processing-object envelope, the config
  framework tables) ships INSIDE the crates as embedded SQL — the Go
  migrations verbatim, schema-qualified, dbmate markers removed — and
  `basable-db`'s runner applies it per schema with a per-schema ledger under
  an advisory lock.

The spike is the second path under Bazel: `include_str!` of the embedded
`.sql` files needs `compile_data` on the `rust_library` so the paths resolve
in the rules_rust sandbox, and the per-schema ledger plus advisory lock must
behave with several schemas in one database (one tenant, several
nanoservices). Phase 1 pins both with tests; `basable-testkit` applies the
same files in tests.
