# basable-ledger — journals derived from object state

A declarative double-entry ledger. The user declares, per object type, the
accounting states an object can be in, the positions that must exist while
it is in each state (its **desired state**), and which state changes are
allowed. The engine compares what is booked for an object with the desired
state its current snapshot asks for and appends the difference as one
balanced journal. Corrections, amount changes and rule changes all fall out
of the same comparison: nothing is ever written per event, and nothing is
ever edited.

This crate has no Go original. The specification is
`docs/decisions/0005-declarative-ledger.md` in this repository, settled
with the user on 2026-10-17 from the monorepo's
`docs/todo/declarative-ledger-plan.md`; `docs/porting-notes.md` 87 records
that there is nothing to keep in sync.

## The surface

| Item | What it is |
|---|---|
| `LedgerObject` | The reader on the user's own type: `TYPE`, `State` (a `Copy + Eq + Hash` enum), `process_id`, `object_id`, `parent_id`, `version`, `changed_at`, `state() -> (State, entered_at)`. `Serialize + DeserializeOwned`: the serde form is the stored form |
| `Accounting<O>` | The rules of one type, no `self`: `VERSION` (bump on a desired-state or classification change), `validate_state_change(current) -> &[State]` (listing the current state allows an amount change in it; a terminal state lists nothing), `desired_state(state, &O) -> Desired`, `validate_balances(state, &O, &Balances, &Desired)` |
| `BatchAccount` | Whatever names a batch: `batch_key()`, `ledger()` and `batch_type()` (`Debit`, `Credit`, `Memo`). All the framework knows about a batch. There is no batch table: the key is the user's, allocated in a table of their own or encoded or hashed from what names the batch, and a batch is closed when a booking brings its balance to zero. Implemented for `&A` and for `Batch` |
| `Batch`, `BatchKey`, `LedgerKey`, `BatchType`, `UnitKey` | A batch as a line names it (key, ledger, type; `Batch::of(&account)`), and the keys. `BatchKey: From<&impl BatchAccount>`, so a balance lookup takes a key or an account |
| `Unit` | `unit_key() -> UnitKey(u32)`, what a line carries and what the engine balances by |
| `Desired`, `balance(account, unit, amount)`, `desired![...]` | The positions of a state: signed amounts in the column of each batch's type. Duplicates of one key are summed |
| `Balances`, `ObjectBalances` | What `validate_balances` and `Ledger::balances` see: per object, per batch key and unit, computed from the journals; `batch(key or &account, unit)` over the process, `batch_account(key)` the ledger and type the process booked the key under |
| `Ledger<S>`, `Builder`, `Request`, `Outcome` | `Ledger::builder(store).register::<O, A>()...build()`; `book(Request)` books any mix of registered types across processes, one `Outcome` per object: `Booked`, `AlreadyBooked`, `Stale`, `Rejected(reason)`. `balances(process)`, `restate(process)`, `preview_restatement(process)` (the per-process dry run: the net deltas per batch key and unit); `restate_behind()`, called once when a deployment starts, restates every process behind the running rules of any registered type and reports `RestatementReport { processes, objects, lines, versions_recorded }` |
| `Store` | The persistence interface: `read`, `commit`, `record_version`, `batch_sum`, `processes_behind` (the startup restatement's worklist); `ProcessRead`, `Commit`, `CommitOutcome::{Committed, SequenceTaken}` |
| `InMemoryStore` | The reference store, for tests |
| `Journal`, `Line`, `SnapshotDiff` | The stored shape: lines `[batch_key, ledger, batch_type, object_id, unit_key, amount]` (the type 1 debit, 2 credit, 3 memo), diffs `{object_id, type, full | patch}` |
| `jsonpatch::{diff, apply}` | RFC 7386 merge patch, how diffs are made and folded |
| `Rejection`, `StoreError`, `LedgerError` | Hand-written errors, no `thiserror` |

## How a call books (per process)

1. **Read** every journal of the process (one range scan), fold each
   object's snapshot diffs into its current JSON and apply the reader: the
   booked state and version are what the reader returns on the fold. Sum
   the lines per (object, batch key, unit) into positions; each line
   carries its batch's ledger and type, and one key under two of them is a
   corrupt store. The highest sequence is the process version.
2. **Restate** first, if any type in question has a recorded version behind
   `Accounting::VERSION`: re-evaluate every object of that type on its
   folded snapshot and queue the delta.
3. **Per submitted object**, in order: a version at or below the booked one
   is `AlreadyBooked` or `Stale`. Otherwise the next state must be in
   `validate_state_change(current)`; an unchanged state with changed
   amounts needs the current state to list itself. Evaluate the desired
   state, reject a batch key the process has booked under another ledger
   or type, check it balances (left equals right per ledger and unit, memo
   outside), run `validate_balances` against the process as the call
   leaves it so far, and queue the delta against the object's booked
   positions. A booked position the desired state no longer names closes
   through the same batch, whose ledger and type the booked lines carry.
4. **One rejection rejects the process's part of the call**, naming the
   object and the reason; other processes in the call are unaffected.
5. **Commit** one journal at `sequence + 1` with every delta line and one
   snapshot diff per booked object (full the first time, a merge patch
   afterwards). The primary key on (process, sequence) is the
   compare-and-set; a lost race is retried, `DEFAULT_MAX_ATTEMPTS` times.
6. **Record** the rules version per type that was missing or behind, as a
   separate converging upsert after the commit.

A rules version changes only with a deployment, so the new code calls
`restate_behind()` once when it starts: it pages through `processes_behind`
per registered type and restates each process (lines, no diffs, one
journal) until nothing is behind. Before serving or alongside it is the
caller's choice; any number of replicas may run it at once. While the new
deployment restates, a replica of the previous one is still booking: a
booking whose running `VERSION` is below the process's recorded version
fails with `LedgerError::RulesBehind` rather than moving the restated
positions back, and the caller retries once the old replica is gone. A
rules change is therefore never rolled back; the old desired state comes
back as a higher version. A process that booked a type but crashed between
its commit and `record_version` has no row to find; its next booking
restates it. A version row exists only for a type the process booked.

## How a batch closes

By a booking, not a status. A claim (a VAT return filed for a month, the
payout of a clearing batch) is an object of the user's whose desired state
holds minus the batch's sum on that batch and the same amount on the batch
the money moves to, booked like any other object; the producer read the sum
with `Store::batch_sum` and put it on the object. The batch is then at
zero, which is what closed means. A line landing in it afterwards (a late
correction) is a nonzero remainder, visible as an open item and taken by
the next claim; whatever a sum missed is in the next sum, since the claim's
own lines are part of it. No epoch, no lock, nothing marked.

## Rules

- **The engine is pure; only the store is async.** `engine.rs` takes a
  `ProcessRead` and the submissions and returns a `Commit`; nothing in it
  reads a clock. A restatement's event time is the booking time, passed in.
- **Journals are append-only.** An adjustment, a transition and a
  restatement are all new lines that bring the booked positions to the
  desired state. Nothing is updated or deleted.
- **No balance in the write path, no batch table anywhere.** Positions are
  sums of lines computed on read; a batch's balance is `batch_sum` over
  every process; a queryable balance table is the user's projection.
- **Amounts are signed `i128`** in the column of the batch's type. A
  negative amount is a balance below zero in its column: allowed, because
  clearing an open item across objects is exactly that (a payment holding
  minus the receivable on the invoice's batch), and reported when it ages.
- **The desired state reads only the object.** A price is a field the
  producer computed and sent, a batch key is a field or a function of one;
  a rule change cannot re-price as a side effect, which is what makes
  restatement by re-evaluation sound.
- **Rejections name the object.** A `Rejection` message starts with
  `object <id>` wherever one is to blame.

## What the tests pin

Unit tests here: the merge patch round trip, the wire forms of lines (key,
ledger, type, object, unit, amount), diffs and amounts, the batch type
integer and a type outside 1 to 3 refused, a key from whatever names a
batch. The behaviour is pinned in `basable-ledger-testkit`:
`tests/scenario.rs` (the invoice lifecycle, duplicates and stale snapshots,
disallowed changes, frozen amounts, the balance check, a payment clearing an
invoice across objects, a rejected object rejecting its process, a rules
bump restating by one appended journal, a batch closed by a VAT return
booking it to zero with a late correction leaving the remainder, memo
batches), `tests/walk.rs` (random histories: every journal balances, path
independence, replay books nothing), `tests/restatement.rs` (the startup
restatement over every process behind, the run after it finding nothing,
the older-rules replica refused) and `tests/conformance_memory.rs` (the
store suite over `InMemoryStore`).
