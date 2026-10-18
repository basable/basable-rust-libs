# basable-ledger-postgres — the ledger's tables, its Postgres store and the batch lifecycle

The `Store` of `basable-ledger` over Postgres, in the schema of the
nanoservice that owns the ledger (`basable_db::NanoPool`, `SET ROLE` and
`search_path` pinned, so table names are unqualified), plus what only a
database can do: the batch lifecycle behind a row lock. The specification
is `docs/decisions/0005-declarative-ledger.md`.

## The surface

| Item | What it is |
|---|---|
| `MIGRATION`, `MIGRATION_FILE`, `SCHEMA_PLACEHOLDER`, `migrations()` | The dbmate migration that creates `journal`, `batch`, `batch_claim` and `process_accounting_object_version`, with `nano_ledger` as the schema and role placeholder. Apply it through `basable_db::migrate::apply` with the replacement `("nano_ledger", "nano_<name>")` |
| `PgStore::new(pool)` | The store: `read` (every journal of a process, the batch rows its lines name, the recorded versions), `resolve` (the highest epoch per batch id), `commit` (new batches with `ON CONFLICT (batch_id, epoch) DO NOTHING`; the existing batches written into locked `FOR SHARE` and checked open, else `BatchNotOpen`; the journal at the next sequence, a unique violation on the primary key being `SequenceTaken`), `record_version` (an upsert that never moves backwards), `batch_sum` (a containment query on the lines index), `processes_behind` (the startup restatement's worklist, from the version table's index) |
| `BatchLifecycle::new(pool)` | `status`, `close` (open to closed; the `UPDATE` waits for every commit holding the row `FOR SHARE`), `claim` (closed to claimed: sums the batch's lines per unit and records them in `batch_claim` in one transaction; a second claim reads the record), `consume` (claimed to consumed; the batch never changes again, the engine rolls the next line to a new epoch); `Claim`, `LifecycleError::{Backend, Unknown, WrongStatus, Corrupt}` |

## The tables

- `journal (process_id, sequence) PK`: `event_time`, `booked_at`
  (`clock_timestamp()` at insert), `lines` jsonb
  `[[batch_key, object_id, unit_key, amount], …]`, `snapshot_diffs` jsonb.
  A GIN index on `lines` (`jsonb_path_ops`) answers "this batch's lines" as
  `lines @> '[[<key>]]'`.
- `batch (batch_key identity PK, batch_id, epoch, ledger, batch_type,
  status, closed_at)`, `UNIQUE (batch_id, epoch)`. Type 1 debit, 2 credit,
  3 memo; status 1 open, 2 closed, 3 claimed, 4 consumed.
- `batch_claim (batch_key, unit_key) PK`, `amount NUMERIC(39,0)`: what a
  claim found, written with the status change.
- `process_accounting_object_version (process_id, type_name) PK`,
  `version`, indexed on `(type_name, version)` as the startup
  restatement's worklist.

No balance table: a position is a sum of lines, and so is a claim.

## Rules

- **The primary key is the compare-and-set.** No row is updated per
  booking, no advisory lock. A loser rolls back (its new batch rows with it)
  and the engine reads again.
- **The one lock is a shared row lock on the batches a commit writes
  into.** Share locks do not contend with each other, so bookings into one
  batch stay parallel; only a close waits, once, and after it nothing can
  land in the batch. A commit that finds a batch closed returns
  `BatchNotOpen` and the engine resolves the next epoch.
- **Amounts travel as strings** inside the jsonb and are summed as
  `numeric` in SQL, so an `i128` never meets a `bigint`.
- **sqlx's runtime API only**, never the query macros; `basable_db`'s
  SQLSTATE helpers classify a unique violation.
- **Nothing in the write path maintains a balance, and this crate ships
  no balance table.** `batch_sum` computes one on demand for a claim. A
  queryable `batch_balance` is the user's projection off the journal
  table, and `basable_ledger_testkit::projection::check_agrees` is the
  check it must pass.

## A balance projection by trigger, and its warning

A user may keep a `batch_balance` of their own in the write transaction
with a trigger: `AFTER INSERT ON journal FOR EACH STATEMENT`, `REFERENCING
NEW TABLE AS inserted`, one set-based `INSERT INTO batch_balance
(batch_key, unit_key, amount) SELECT (l->>0)::bigint, (l->>2)::bigint,
(l->>3)::numeric FROM inserted, jsonb_array_elements (inserted.lines) l ON
CONFLICT (batch_key, unit_key) DO UPDATE SET amount = batch_balance.amount
+ excluded.amount`. Always consistent, no process to run.

**Warning: this puts a balance back into the write path. Every booking that
touches a shared batch (this month's revenue, a bank clearing batch) updates
the same `batch_balance` row and waits for the previous commit on it, so
bookings serialize on the hot batches. Invisible at a tenant's volume; a
bottleneck at thousands of transactions per second, which is exactly what
the batch model exists to avoid. Beyond that, feed ClickHouse through
change data capture on the journal table.**

## What the tests pin

`src/lib.rs`: the migration parses. Against `TEST_DATABASE_URL`, skipped
without it:

- `tests/postgres.rs`: the conformance suite of `basable-ledger-testkit`;
  the invoice and payment fixture on real rows, replays recognised, the
  batch sum spanning the process; a rules bump restating with one appended
  journal; both COMMIT faults of `CommitFaultProxy` surfacing as a store
  error with the retry answering `AlreadyBooked` or `Booked` and exactly
  one journal in the table; Postgres and the in-memory store agreeing on a
  history.
- `tests/lifecycle.rs`: close, claim (recorded, idempotent), consume, and
  the engine rolling a consumed batch to epoch 2; a writer holding the row
  `FOR SHARE` makes the close wait, and a booking that finds its batch
  closed at commit books into the next epoch.
- `tests/restatement.rs`: after a rules bump, `preview_restatement` of one
  process books nothing, `restate_behind` restates every process behind
  from the version table's index and the run after it finds nothing, a
  version row exists only for a type the process booked, and a ledger on
  the older rules is refused with `LedgerError::RulesBehind`.
