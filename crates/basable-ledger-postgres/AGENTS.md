# basable-ledger-postgres — the ledger's tables and its Postgres store

The `Store` of `basable-ledger` over Postgres, in the schema of the
nanoservice that owns the ledger (`basable_db::NanoPool`, `SET ROLE` and
`search_path` pinned, so table names are unqualified). The specification
is `docs/decisions/0005-declarative-ledger.md`.

## The surface

| Item | What it is |
|---|---|
| `MIGRATION`, `MIGRATION_FILE`, `SCHEMA_PLACEHOLDER`, `migrations()` | The dbmate migration that creates `journal` and `process_ledger_object_version`, with `nano_ledger` as the schema and role placeholder. Apply it through `basable_db::migrate::apply` with the replacement `("nano_ledger", "nano_<name>")` |
| `PgStore::new(pool)` | The store: `read` (every journal of a process, the one read a booking makes), `commit` (one statement: a CTE inserts the journal at the next sequence and upserts the version index row of every type in the process, latest wins; a unique violation on the primary key fails the statement as a whole, `SequenceTaken`), `record_versions` (the index alone), `batch_sum` (over the expression index on the lines' batch keys), `processes_behind` (the startup restatement's worklist, from the version index) |

## The tables

- `journal (process_id, sequence) PK`: `booked_at` (`clock_timestamp()` at
  insert, the journal's one time; an object's own instants are in its
  diffs), `lines` jsonb
  `[[batch_key, ledger, batch_type, object_id, unit_key, amount], …]`
  (type 1 debit, 2 credit, 3 memo; amounts as decimal strings),
  `snapshot_diffs` jsonb. `idx_journal_batch_keys` is a GIN index on the
  expression `jsonb_path_query_array(lines, '$[*][0]')`, the batch keys of
  a journal's lines and nothing else, so `... @> '[<key>]'` finds a batch's
  journals and a small key never matches a ledger, a type or a unit that
  shares its value.
- `process_ledger_object_version (process_id, type_name) PK`, `version`:
  the rules version each type of a process was last evaluated under,
  upserted by the commit's own statement (latest wins) and never read by a
  booking; indexed on `(type_name, version)` as the startup restatement's
  worklist.

No batch table: a batch key is the user's, and every line carries the
batch's ledger and type with it. No balance table: a position is a sum of
lines, and so is a claim. A batch at zero is a closed batch.

## Rules

- **The primary key is the compare-and-set.** The commit is one
  autocommitted statement, a CTE that inserts the journal and upserts the
  version index rows: two tables written, no transaction to open, no second
  round trip, no lock of any kind. A loser's statement fails as a whole and
  the engine reads again. A booking reads the journal table and nothing
  else.
- **Amounts travel as strings** inside the jsonb and are summed as
  `numeric` in SQL, so an `i128` never meets a `bigint`.
- **The batch sum names the index's expression.** The `WHERE` of
  `batch_sum` is `jsonb_path_query_array(j.lines, '$[*][0]') @> $1` with
  `$1 = [key]`, then `(l->>0)::bigint = $2` on the unnested lines; change
  the index and the query together.
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
(batch_key, unit_key, amount) SELECT (l->>0)::bigint, (l->>4)::bigint,
(l->>5)::numeric FROM inserted, jsonb_array_elements(inserted.lines) l ON
CONFLICT (batch_key, unit_key) DO UPDATE SET amount = batch_balance.amount
+ excluded.amount`, followed by `DELETE FROM batch_balance WHERE amount =
0` over the keys it touched: a zero balance is a closed batch, and the
table should hold only the open ones. Always consistent, no process to run.

**Warning: this puts a balance back into the write path. Every booking that
touches a shared batch (this month's revenue, a bank clearing batch) updates
the same `batch_balance` row and waits for the previous commit on it, so
bookings serialize on the hot batches. Invisible at a tenant's volume; a
bottleneck at thousands of transactions per second, which is exactly what
the batch model exists to avoid. Beyond that, feed ClickHouse through
change data capture on the journal table: a `SummingMergeTree` per (batch
key, unit) drops a row whose summed columns are all zero when it merges.**

## What the tests pin

`src/lib.rs`: the migration parses and creates no batch table. Against
`TEST_DATABASE_URL`, skipped without it:

- `tests/postgres.rs`: the conformance suite of `basable-ledger-testkit`;
  the invoice and payment fixture on real rows (both types indexed at the
  version they were evaluated under), replays recognised (which
  is what repairs a commit whose acknowledgement was lost: the retry reads
  the row and answers `AlreadyBooked`), the batch sum spanning the
  process; a month's VAT batch closed by a VAT return booking it to zero
  and a late correction leaving the remainder; a rules bump restating with
  one appended journal; Postgres and the in-memory store agreeing on a
  history.
- `tests/restatement.rs`: after a rules bump, `preview_restatement` of one
  process books nothing, `restate_behind` restates every process behind
  from the version index and the run after it finds nothing, a version row
  exists only for a type the process booked, and a booking by a replica on
  the older rules is accepted, indexed at its version and taken again by
  the next run.
