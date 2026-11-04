# 0005: the declarative ledger

Settled with the user on 2026-10-17, from the monorepo's
`docs/todo/declarative-ledger-plan.md` (its §3 to §7), which stays the
longer record with the alternatives weighed. This is the specification the
`basable-ledger` crates implement; a change to the model is a change here
first.

## The model

- A **process** is the business process every object of a flow shares,
  `process_id`; an **object** is one business object within it,
  `object_id`, optionally with a `parent_id` in the same process. Processes
  stay small (a payment with its captures, an invoice with its lines); the
  objects of a process serialize.
- An object has **one accounting state**. Its **desired state** is the set
  of positions that must exist while it is in that state: each a batch, a
  unit and a signed amount. The user writes it as code, a pure function of
  (state, object), under a hardcoded rules **version**.
- A **batch** is the user's: one value of the user's `Batch` enum, whose
  variant is the kind of batch and whose fields say which one
  (`Batch::Payout(merchant, week)`). The value returns a **primary key**
  (what identifies the batch), one **ledger** and one **type** (debit, credit
  or memo). A batch also has a **key**, the bigint a stored line carries
  alone (since 2026-10-20; until then every line carried the key, ledger and
  type).
- Each kind of batch has a **table of the user's**, one row per batch: the
  primary key's columns, a unique `batch_key`, `ledger`, `batch_type` and
  whatever else the user keeps. Entities, subledger types, account classes,
  GL codes and periods are the user's vocabulary for what a batch means,
  outside the framework. The framework keeps no batch table and knows no
  column of these: it reaches them only through `find` and `ensure`, methods
  on the value, which the scaffold renders from the kinds of batch the
  manifest lists.
- **One sequence** allocates `batch_key` over every batch table, as the
  default of each, because a line carries the key alone and the same number
  in two tables would be one batch. It hands out identity, not order, so its
  gaps and commit order do not matter.
- **A row is for good.** Its primary key, `batch_key`, `ledger` and
  `batch_type` never change and it is never deleted: the next evaluation
  would allocate a new key and book the money across to it. So a key means
  one ledger and one type for good. A table holding a batch in two ledgers
  has the ledger in its primary key, so the same thing in both books is two
  batches with two keys.
- A new row holds what the value carries, and `ensure` never updates an
  existing one, so two values with one primary key and different other
  columns are one batch. A column the value does not carry is nullable or
  defaulted, and the owner fills it later.
- A batch is **closed** when a booking brings its balance to zero: a claim
  (a VAT return filed for what the month's batch held, a payout for a
  clearing batch) is an object of the user's whose desired state holds
  minus the batch's sum on that batch and the same amount on the batch the
  money moves to, booked like any other. Nothing marks a batch. A line
  landing in it afterwards is a remainder the next claim takes; whatever a
  sum (`Store::batch_sum`) missed is in the next sum.
- A **position** is the booked balance of one (object, batch, unit): the
  sum of the object's lines, computed from the process's journals on every
  read. No position, balance or booked-state table exists.
- A **journal** is one row per commit of one process: lines grouped by
  object, `{object_id: [[batch_key, unit_key, amount], ...]}`, and
  snapshot diffs
  `{object_id, type, full | patch}` (RFC 7386 merge patches; the first
  sight of an object is its full JSON). Folding a process's diffs rebuilds
  every object at any sequence; the reader applied to the fold is the
  booked state and version.
- **Double entry**: in every desired state and every journal the amounts in
  debit batches sum to the amounts in credit batches per (ledger, unit).
  The engine checks each desired state; a journal is the difference of
  balanced states under keys that each mean one ledger and one type, so it
  balances by construction and is not checked (a line carries no type to
  check it by). Memo batches stand outside. A negative amount is a balance below zero in
  its column, allowed (an open item clearing across objects is exactly
  that) and reported when it ages.

## The engine

- The consistency boundary is the process. Its version is the highest
  journal sequence; the commit is the insert of the next journal, and the
  primary key (process_id, sequence) is the compare-and-set. A lost race is
  read again and recomputed, inside the library.
- A call carries any mix of registered types across processes; the objects
  of one process book atomically, in one journal, in the order submitted,
  or not at all. Outcomes are per object: booked, already booked (an equal
  version), stale (a lower one), rejected (with the reason, which names the
  object; one rejection rejects the process's part of the call).
- The diff is always against the booked positions, never against the old
  state's desired state recomputed now. Every booking first re-evaluates
  every current object of the process under the running rules and books
  the difference, so the engine never needs to know whether rules changed:
  a submission's own delta, computed after that, is the object's own
  change. A changed amount in an unchanged state is allowed only when that
  state lists itself as its own next state.
- The desired state names batches by value and stays pure: it reads and
  writes nothing, and the same object under the same rules always names the
  same batches. The engine resolves them in two steps, both inside the
  retry above.
- **Before the plan**, one `find` per distinct primary key reads the rows
  that exist. The plan compares by key. A primary key with no row takes a
  placeholder, which is sound because it has nothing booked: a line only
  ever carries the key of a row already committed. So a booking makes one
  more read than the journals: one lookup per distinct batch, which small
  processes keep small.
- **Between the plan and the commit**, `ensure` creates the row of each new
  batch a line lands in: `INSERT ... ON CONFLICT DO NOTHING`, committed on its
  own, then a read. The real keys replace the placeholders. A concurrent first
  use of one batch waits for that one statement, which no stalled client can
  hold open, and takes the same key; once the row exists nothing waits.
  `ensure` never touches a row that exists, so `ON CONFLICT DO UPDATE` is ruled
  out.
- **The commit** is one statement, on every booking: the journal and the
  version index. No booking holds a transaction across round trips.
- A rejected booking, a batch named with nothing to book and
  `preview_restatement` leave no row; a preview only reads, and reports a batch
  with no row as new. A lost sequence race, a crash between the row and the
  journal, or a value refused after an earlier row landed can leave a row with
  nothing booked on it. It stays, as every row does, and sums to nothing; the
  next booking on that batch finds it. So no row means nothing booked, and a
  row does not mean something was.
- `restate_behind` books the same way, so a rule change may name a new batch
  for objects booked long ago.
- `find` and `ensure` return the row's ledger and type with its key, and a
  value whose row says otherwise rejects, across calls as well as within
  one. One key named under two ledgers or types was only caught within a
  call between 2026-10-20 and this change.
- A rules version bump restates. A process converges to the running rules
  whenever it is touched, so what a bump needs is the processes nobody
  touches: `Ledger::restate_behind` runs once when the new deployment
  starts, over `Store::processes_behind`, the version index that lists
  every process whose last evaluation of a type was under an older number,
  and restates each by appending a journal with lines and no diffs (or by
  moving the index alone when nothing differs). The index is one row per
  process and object type, written by the commit's own statement for every
  type in the process, latest wins, and never read by a booking: two
  tables written per process, one read. Prospective: history is never
  rewritten. A replica of the previous deployment still booking during the
  rollout books under its rules and re-indexes the process at its version,
  so the pass takes it again; nothing is refused, and the book settles when
  the old replica is gone. (A per-process version read on every booking, a
  `RulesBehind` refusal and a `rules_versions` column on the journal were
  each built and removed on 2026-10-18.)

## Not in the write path, by design

- **No batch table in the framework, no batch status.** The rows are the
  user's and hold identity, never state: a batch opens with its first line
  and is closed when a booking brings it to zero. The 2026-10-17
  build's `batch` and `batch_claim` tables, string ids with epochs, the
  `Batches` recorder, the open-closed-claimed-consumed lifecycle and the
  `FOR SHARE` lock in the commit were removed on 2026-10-18: a closed
  period in bookkeeping is closed and a late entry is booked in the open
  period, which is this shape; epochs were a distributed-systems idea, not
  an accounting one.
- **No balance table, and none shipped.** A position is a sum of lines; a
  claim sums its batch and books what it took. A queryable `batch_balance`
  is the user's projection off the journal table (a trigger, with the
  warning that it serializes bookings on hot batches, or ClickHouse through
  change data capture), and `basable-ledger-testkit`'s `projection` check
  is what it must pass. A zero balance is a closed batch and should have no
  row: the trigger deletes what it brought to zero, a `SummingMergeTree`
  drops it on merge. A time-window projector was built in
  `basable-ledger-postgres` and removed the same day: it is not the
  framework's concern. TigerBeetle was weighed as a store on 2026-11-03: its
  fixed 128-byte records hold no snapshot diffs or version index, so it could
  only be such a projection, applied after the Postgres commit, and a
  production cluster is six dedicated machines. Not until a measurement asks.
- **No feed column.** A sequence value is handed out inside the
  transaction, so an auto-increment is neither gapless nor commit-ordered.
- **No account-level limit.** The ledger records reality; a synchronous
  limit is a guarded tier beside it, not the ledger's model.

## What else was weighed for a batch's key (2026-11-03)

- **A hash of the batch's name** (the template and the test fixtures until
  now; it began as the fixture's stand-in for the user's table). It needs no
  table, but two names can land on one number and silently merge into one
  batch, and FNV-1a collisions can be crafted where a name holds text a
  customer chose. Removed: every kind of batch has a table.
- **An allocated key as a field of the object**, filled by the producer or
  the owner's `Book` handler. The startup restatement runs on stored
  snapshots with no handler, so a rule change naming a new batch for old
  objects would first need those snapshots migrated.
- **Creating the row inside `desired_state`.** It runs for every object of
  the process on every booking, again after a lost race, on bookings that end
  up rejected and in a preview that promises to book nothing, and positions
  would depend on a table instead of on the object alone.
- **`desired_state` returning a closure that upserts.** A closure has no key
  until it has run, so the plan could not compare or sort without one write
  per position per booking, and it needs a Postgres transaction where the
  core crate has no `sqlx` and the property tests have no database. A value
  with a method allows the same SQL on any table and stays comparable.
- **Recording the primary key in the journal** the first time a process uses a
  batch, so a repeat booking finds its keys in the fold and makes no lookup. It
  saves a few primary-key reads outside any transaction, for a second record
  of the mapping, a bigger row and a backfill. Declined until a measurement asks;
  it could then be a cache in front of `find`.
- **One mapping table in the framework** (primary key, `batch_key`, ledger,
  type). One counter would hold by construction and no SQL would be
  generated. Declined to keep typed key columns per kind and the 2026-10-18
  decision that the framework keeps no batch table.
- **The row in the journal's transaction** (0.10.0, replaced 2026-11-04). A
  booking that failed left no row, and a first use took one durable commit
  instead of two. But that transaction spanned client round trips: a first
  writer stalled between its insert and its commit (a throttled pod, a partition,
  a lost node, where only TCP keepalive frees it) held up every booking naming
  that batch, because `find` still saw no row, and each waiter held a pool
  connection, so requests that never named the batch starved too. Nothing
  bounded it. Speed did not decide this: on a laptop with a simulated 0.5 ms
  round trip, a booking that was a first use took 5.07 ms in one transaction and
  3.33 ms row first on a cheap commit, and 11.78 ms and 15.17 ms on a real flush,
  while bookings on one batch shared by 16 clients stayed within 5% either way.
  Empty rows are the price, and they sum to nothing.

## Deviations the Rust shape made

- The rules version is `Accounting::VERSION`, an associated constant, not a
  field `desired_state` returns: the startup restatement needs the running
  version before it has any object to evaluate, and one number in one
  place cannot drift.
- Rejections are per object through `Outcome`; a restatement, having no
  object to report on, surfaces them as `LedgerError::Rejected`.
- Type names in snapshot diffs are the `LedgerObject::TYPE` strings, not
  registry integers; a registry is an optimisation the first store does not
  need.
- The user's enum is `Batch` (`Account` until 2026-11-03, in `batches.rs`,
  `accounts.rs` until then) and implements `LedgerBatch` (`BatchAccount`
  until then), beside `LedgerObject`: `primary_key()`, `ledger()` and
  `batch_type()`. The crate's old `Batch` struct becomes internal. `Accounting`
  keeps its name, since it holds the rules. `PrimaryKey` is an associated type
  that is `Ord` and holds the key columns alone, and the engine and
  `InMemoryStore` sort, dedupe and allocate by it, so tests and Postgres agree
  on what one batch is.
- `find(&self, conn)` and `ensure(&self, tx)` are `PgBatch`, in
  `basable-ledger-postgres`, so the core crate stays free of `sqlx`. Each
  variant knows its own table and key values, so neither takes a parameter.
  One owner has one `Batch` enum, which `Accounting`, `Desired` and `Ledger`
  carry as a type (`Accounting::Batch`, `Store::Batch`). The Postgres suites
  moved inside that crate, since only it may implement `PgBatch` for the
  testkit's fixture.
- In the manifest a kind of batch has one ledger, so a second book is a second
  kind. Having the ledger in a table's primary key stays open to a hand-written
  `PgBatch`.

## Phases

1. `basable-ledger` and `basable-ledger-testkit` (done 2026-10-17).
2. `basable-ledger-postgres` and the conformance suite against Postgres
   (done 2026-10-17).
3. Batch lifecycle, epochs and claims behind a row lock (done
   2026-10-17, removed 2026-10-18 together with the batch table: a batch
   closes by a booking to zero, above). The optional time-window projector
   built with it was removed the same day.
4. The restatement at startup (done 2026-10-17 as an operator's sweep with
   a dry run, reshaped 2026-10-18): `Store::processes_behind` reads the
   version index, `Ledger::restate_behind` runs to completion when the new
   deployment starts, `preview_restatement(process)` is the per-process dry
   run. Later that day the engine stopped reading any version: every
   booking re-evaluates its whole process, the index is written by the
   commit's statement, and an older replica's booking converges at the next
   run instead of being refused.
5. Batches resolved at booking time (done 2026-11-04, released in 0.10.0 with
   the rows in the journal's transaction, moved ahead of it the same day): the
   three crates, the conformance suite, the Go port in the monorepo's
   `lib/ledger`, and the scaffold (the manifest's kinds of batch, the tables,
   the one sequence, `find` and `ensure`, the Directive's §11). A
   project with journals under hashed keys seeds one row per batch, with that
   key, before it upgrades; otherwise the first evaluation allocates a new key
   and books the money across to it.
