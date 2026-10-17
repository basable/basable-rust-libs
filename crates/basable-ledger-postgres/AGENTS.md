# basable-ledger-postgres — the ledger's tables and its Postgres store

The `Store` of `basable-ledger` over Postgres, in the schema of the
nanoservice that owns the ledger (`basable_db::NanoPool`, `SET ROLE` and
`search_path` pinned, so table names are unqualified). The specification is
`docs/decisions/0005-declarative-ledger.md`; this crate is the persistence
half of it.

## The surface

| Item | What it is |
|---|---|
| `MIGRATION`, `MIGRATION_FILE`, `SCHEMA_PLACEHOLDER`, `migrations()` | The one dbmate migration that creates `journal`, `batch` and `process_accounting_object_version`, with `nano_ledger` as the schema and role placeholder. Apply it through `basable_db::migrate::apply` with the replacement `("nano_ledger", "nano_<name>")`; the scaffolder renders the same for a tenant's owning nanoservice |
| `PgStore::new(pool)` | The store over the owning nanoservice's `PgPool`: `read` (every journal of a process, the batch rows its lines name, the recorded versions), `resolve` (the highest epoch per batch id), `commit` (the batches to create with `ON CONFLICT (batch_id, epoch) DO NOTHING`, then the journal at the next sequence; a unique violation on the primary key is `SequenceTaken`), `record_version` (an upsert that never moves backwards), `batch_sum` (a containment query on the lines index) |

## The tables

- `journal (process_id, sequence) PK`: `event_time`, `booked_at`
  (`clock_timestamp()` at insert, the optional projector's window), `lines`
  jsonb `[[batch_key, object_id, unit_key, amount], …]`, `snapshot_diffs`
  jsonb. A GIN index on `lines` (`jsonb_path_ops`) answers "this batch's
  lines" as `lines @> '[[<key>]]'`; an index on `booked_at` serves the
  projector.
- `batch (batch_key identity PK, batch_id, epoch, ledger, batch_type,
  status, closed_at)`, `UNIQUE (batch_id, epoch)`. Type 1 debit, 2 credit,
  3 memo; status 1 open, 2 closed, 3 claimed, 4 consumed.
- `process_accounting_object_version (process_id, type_name) PK`,
  `version`, with an index on `(type_name, version)` as the sweep's
  worklist.

## Rules

- **The primary key is the compare-and-set.** No row is updated per
  booking, no lock, no advisory lock. A loser rolls back (its new batch rows
  with it) and the engine reads again.
- **Amounts travel as strings** inside the jsonb and are summed as
  `numeric` in SQL (`SUM((l->>3)::numeric)::text`), so an `i128` never meets
  a `bigint`.
- **sqlx's runtime API only**, never the query macros; `basable_db`'s
  SQLSTATE helpers classify a unique violation.
- **Nothing here maintains a balance.** `batch_sum` computes one on demand
  for a claim; the optional projector (phase 3) is a separate component the
  implementer starts or not.

## What the tests pin

`src/lib.rs`: the embedded migration parses and carries the placeholder.
`tests/postgres.rs`, against `TEST_DATABASE_URL` and skipped without it:
the conformance suite of `basable-ledger-testkit` passes; the invoice and
payment fixture books through the engine on real rows, with replays
recognised and the batch sum spanning the process; a rules bump restates
with one appended journal and the version recorded; a lost COMMIT
acknowledgement (both faults of `CommitFaultProxy`) surfaces as a store
error and the retry answers `AlreadyBooked` or `Booked` with exactly one
journal in the table; and Postgres and the in-memory store agree on a
history, outcome for outcome and balance for balance.
