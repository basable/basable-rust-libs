# basable-testkit — a database per test, and one lost COMMIT

The port of the monorepo's `golang/test/testdb.go` and of the
processing-object testkit's `commitfault.go`, for a tenant's tests and the
frameworks' own suites alike. A test asks for a fresh database on the
Postgres `TEST_DATABASE_URL` names, owned by an `app` login the harness
creates (CNPG creates it in production), with the tenant's dbmate
migrations applied as that login exactly as the migration Job applies them.
Without the variable the harness answers `None` and the test returns, which
is how a unit-test run stays green on a machine without Postgres while the
CI's Postgres service makes every suite run (`docs/porting-notes.md` 11–12).

The Directive (`docs/DIRECTIVE.md` in every tenant repository,
`golang/controller/lib/scaffold/directive.md` in the monorepo) is the
contract this crate serves: its tests are where a nanoservice proves its
ownership and its commit-ambiguity handling against a real database.

## The surface

| Item | What it is |
|---|---|
| `TestDb::from_env()` | A database with the tenant's migrations (`db/app/migrations` in the runfiles, or `MIGRATIONS_DIR_VAR`) applied, or `None` when `DATABASE_URL_VAR` (`TEST_DATABASE_URL`) is unset. A setup failure panics: it is a test |
| `TestDb::from_env_with_migrations(path)` | The same with an explicit migrations directory (the frameworks' fixture) |
| `TestDb::create(url, migrations)` | The explicit form: creates the database, the `app` login if the server lacks it, and applies `migrations` as that login; `TestDbError::{Url, Admin, Connect, Migrate}` |
| `name()`, `migrator()`, `migrator_pool()`, `superuser()`, `app_options()` | The database's name; the `app` login as a `PgPool` or as the typed `MigratorPool`; the admin login; the `app` connect options for a pool a test opens itself (through a proxy, say) |
| `nano_pool::<N: Stateful>()` | The pool the production binary would hold for nanoservice `N`: `SET ROLE nano_<name>`, `search_path` pinned |
| `finish()` | Closes the pools and drops the database. Drop a `PgListener` first: it holds a pool connection, and `close()` waits for it |
| `runfile(rel)` | A path under the Bazel runfiles (`$TEST_SRCDIR/$TEST_WORKSPACE/<rel>`), else relative to the crate's manifest directory or the working directory; `data = [...]` on the test target is what puts the files there |
| `CommitFaultProxy::start(upstream)`, `for_options(options) -> (proxy, options through the proxy)`, `addr()`, `arm(mode)`, `is_armed()` | A loopback TCP proxy in front of Postgres that relays bytes unchanged until armed; the first COMMIT after that, on whichever connection carries it, faults once and the proxy disarms. `for_options` takes `app_options()` and returns the connect options pointed at the proxy |
| `CommitFault::{Applied, RolledBack}` | The two ambiguities the wire cannot tell apart: the COMMIT lands and the acknowledgement is dropped, or the COMMIT is rewritten to a `SELECT` of the same length and the session closed with the transaction open |

## Rules

- **`TEST_DATABASE_URL`, not testcontainers.** The tenant template's CI
  provides a Postgres service and its tests skip without the variable; the
  libs follow the same contract so one harness serves both. Locally it is
  the monorepo's `test_postgres` container on port 29000.
- **The URL's login creates roles and databases.** The harness creates the
  `app` login once per server (a `DO` block, skipped when it exists) with a
  fixed password, because the server is a test container, and a database
  per test as that admin login.
- **One database per test, never shared.** The name is derived per test
  and truncated to Postgres's 63-character limit; suites may run in
  parallel against one server.
- **A fault is armed, not configured.** `arm` once, drive the one commit
  that must be ambiguous, assert on what landed; the proxy disarms itself
  so the rest of the test runs clean.

## What the tests pin

- `tests/ownership.rs` (plan B3, pinned): the `orders` role runs every
  framework statement shape on its own partition; it is refused elsewhere
  (another nanoservice's tables, the envelope parent); the migrator owns
  the framework and nothing of a nanoservice; a pool for a missing role
  fails at boot, not on the first query.
- `tests/migrate.rs`: the ledger holds every version and a rerun applies
  nothing; a failing migration rolls back and leaves no ledger row;
  `replacements` rewrite the SQL before it runs; a unique violation is
  recognised by SQLSTATE.
- `tests/commitfault.rs`: an applied fault lands the row and loses the
  ack; a rolled-back fault loses the row and the ack alike.
- `tests/fixtures/migrations`: the scaffolder's templates rendered for two
  nanoservices, `orders` (one processing-object type, one config type) and
  `inventory` (one plain table), on top of the framework migration;
  `tests/common/mod.rs` has their `Nanoservice` markers and `fixture_db()`.

## Porting notes

`docs/porting-notes.md` 11 (`TEST_DATABASE_URL`), 12 (the proxy instead of
a dial hook), and 7–10 for the ownership shape the suites pin.

## File map

| File | Responsibility |
|---|---|
| `src/testdb.rs` | `TestDb`, `TestDbError`, `DATABASE_URL_VAR`, `MIGRATIONS_DIR_VAR`, `runfile`, the `app` login and database-name helpers |
| `src/commitfault.rs` | `CommitFault`, `CommitFaultProxy`, the relay and the COMMIT / ReadyForQuery scanners |
