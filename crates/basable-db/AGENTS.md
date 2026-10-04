# basable-db — Postgres for nanoservices sharing one database

The port of the monorepo's `golang/lib/db` (the pool, `ReleaseListenConn`)
and of the test harness's dbmate runner, shaped for a tenant project where
several nanoservices share ONE database: each owns a schema and a role, and
the Directive's table-ownership rule (§2) is a Postgres permission error
rather than a convention (plan B3). `docs/porting-notes.md` 7–10 record the
shape that was settled.

The Directive (`docs/DIRECTIVE.md` in every tenant repository,
`golang/controller/lib/scaffold/directive.md` in the monorepo) is the
contract this crate serves.

## The surface

| Item | What it is |
|---|---|
| `Nanoservice` | A marker trait on a unit type: `const NAME: &'static str`, the crate's snake_case name as declared in `routing.yaml`, which follows `basable_core::names::validate_type_name` |
| `Stateful: Nanoservice` | The marker of a nanoservice that owns data; `role()` and `schema()` are both `nano_<NAME>`. A pool exists only for a `Stateful` nanoservice, so asking for one on a nanoservice that owns nothing is a compile error |
| `NanoPool<N: Stateful>` | The pool of one stateful nanoservice: `connect(options, config)`, `nanoservice()`, `pool()`, `close()`; derefs to `PgPool` and `&pool` is an `Executor`. Every connection is the `app` login switched to the nanoservice's role with `search_path` pinned, in `after_connect` |
| `MigratorPool` | The `app` login itself, which owns every schema: `connect`, `pool`, `close`. A distinct type so it cannot be handed to nanoservice code; the testkit and local bootstrap are its only users |
| `PoolConfig` | `max_connections` 8, `min_connections` 0, `acquire_timeout` 10 s, `max_lifetime` 1 h, `idle_timeout` 30 min: six stateful nanoservices at eight connections leave room under the template's `max_connections = 100` for Kratos, the migration Job and CNPG |
| `migrate` | The dbmate-format runner: `parse`, `read_dir`, `apply(pool, migrations, replacements)` (one transaction per file unless `transaction:false`, the `schema_migrations` ledger dbmate itself writes), `applied_versions`, `verify(pool, expected)`; `Migration`, `MigrateError::{Io, Malformed, DuplicateVersion, Sql, Ledger, Missing}` |
| `sqlstate` | `of`, `is_class`, `is_unique_violation`, `is_integrity_violation`, `is_insufficient_privilege`, `is_serialization_failure`, `is_undefined_table` |
| `begin_snapshot` | A `REPEATABLE READ READ ONLY` transaction, how the processing-object read model reads a batch so spec and status agree with the envelope rows |
| `release_listen_conn` | Returns a connection that ran `LISTEN` to its pool with its subscriptions cleared, or discards it when the `UNLISTEN` fails or times out |

## Rules

- **One `app` login, `SET ROLE` per pool.** CNPG mints the one login;
  every `nano_<name>` role has `app` as a member `WITH SET TRUE, INHERIT
  FALSE`. `NanoPool::connect` proves the switch on a direct connection
  first, so a missing role or a login that is not a member fails at boot
  with the database's error, not as a pool timeout on the first query.
- **`RESET ROLE` is the one escape the type cannot close.** Nothing in the
  frameworks issues it and no lint reads a SQL string; per-role logins
  would close it and are a `connect` variant away.
- **The migrator owns the schema; the role owns what is in it.** A
  nanoservice's init migration creates `nano_x` as `app`, grants `USAGE,
  CREATE` to the role, and the role creates and owns its tables. Unswitched,
  `app` is refused on them like anyone else.
- **A role-switching migration ends with `RESET ROLE`.** dbmate writes the
  ledger row in the migration's own transaction after the file's
  statements; the runner here does the same on purpose, so a file that
  leaves `SET LOCAL ROLE` in force fails in the tests where the Job would.
- **Production migrations run in the dbmate Job.** The runner is for tests
  and local bootstrap; the app only calls `migrate::verify` at boot to check
  the ledger holds every version it embeds (`basable-app`'s
  `expect_migrations`).
- **Errors are `sqlx::Error` or `MigrateError`.** Nothing is boxed and
  nothing is walked.

## What the tests pin

`src/migrate.rs` parses the up and down blocks, a missing down block and
the transaction opt-out, and refuses what dbmate would; `src/pool.rs` pins
identifier quoting; `src/nanoservice.rs` pins that role and schema follow
the name. The database-backed suites live in `basable-testkit`
(`tests/ownership.rs`, `tests/migrate.rs`), because they need the harness
this crate's pools are opened through.

## Porting notes

`docs/porting-notes.md` 7 (one login, `SET ROLE` per pool), 8 (schema
ownership), 9 (typed tables reference the partition), 10 (`RESET ROLE`
at the end of a switching migration); 63 for why `release_listen_conn`
has no caller in the bus any more.

## File map

| File | Responsibility |
|---|---|
| `src/nanoservice.rs` | `Nanoservice`, `Stateful` |
| `src/pool.rs` | `PoolConfig`, `NanoPool`, `MigratorPool`, the `Executor` impl, the role switch |
| `src/migrate.rs` | `Migration`, `MigrateError`, `parse`, `read_dir`, `apply`, `applied_versions`, `verify` |
| `src/sqlstate.rs` | The SQLSTATE predicates |
| `src/snapshot.rs` | `begin_snapshot` |
| `src/listen.rs` | `release_listen_conn` |
