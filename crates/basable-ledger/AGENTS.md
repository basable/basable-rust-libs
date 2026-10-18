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
| `LedgerObject` | The reader on the user's own type: `TYPE`, `State` (a `Copy + Eq + Hash` enum), `process_id`, `object_id`, `parent_id`, `version`, `state() -> State`. `Serialize + DeserializeOwned`: the serde form is the stored form |
| `Accounting<O>` | The rules of one type, no `self`: `VERSION` (bump on a desired-state or classification change), `validate_state_change(current) -> &[State]` (listing the current state allows an amount change in it; a terminal state lists nothing), `desired_state(state, &O) -> Desired`, `validate_balances(state, &O, &Balances, &Desired)` |
| `BatchAccount` | Whatever names a batch: `batch_key()`, `ledger()` and `batch_type()` (`Debit`, `Credit`, `Memo`). All the framework knows about a batch. There is no batch table: the key is the user's, allocated in a table of their own or encoded or hashed from what names the batch, and a batch is closed when a booking brings its balance to zero. Implemented for `&A` and for `Batch` |
| `Batch`, `BatchKey`, `LedgerKey`, `BatchType`, `UnitKey` | A batch as a line names it (key, ledger, type; `Batch::of(&account)`), and the keys. `BatchKey: From<&impl BatchAccount>`, so a balance lookup takes a key or an account |
| `Unit` | `unit_key() -> UnitKey(u32)`, what a line carries and what the engine balances by |
| `Desired`, `balance(account, unit, amount)`, `desired![...]` | The positions of a state: signed amounts in the column of each batch's type. Duplicates of one key are summed |
| `Balances`, `ObjectBalances` | What `validate_balances` and `Ledger::balances` see: per object, per batch key and unit, computed from the journals; `batch(key or &account, unit)` over the process, `batch_account(key)` the ledger and type the process booked the key under |
| `Ledger<S>`, `Builder`, `Request`, `Outcome` | `Ledger::builder(store).register::<O, A>()...build()`; `book(Request)` books any mix of registered types across processes, one `Outcome` per object: `Booked`, `AlreadyBooked`, `Stale`, `Rejected(reason)`. `balances(process)`, `restate(process)`, `preview_restatement(process)` (the per-process dry run: the net deltas per batch key and unit); `restate_behind()`, called once when a deployment starts, restates every process the store's version index lists behind the running rules of a registered type and reports `RestatementReport { processes, objects, lines }` |
| `Store` | The persistence interface: `read` (a process's journals, the one read a booking makes), `commit` (the journal row and, in the same statement, the version index row of every type in the process, latest wins), `record_versions` (the index alone, for a restatement with nothing to book), `batch_sum`, `processes_behind` (the startup restatement's worklist, from the index); `Commit`, `CommitOutcome::{Committed, SequenceTaken}` |
| `InMemoryStore` | The reference store, for tests |
| `Journal`, `Line`, `SnapshotDiff` | The stored shape: `(process_id, sequence, booked_at, lines, snapshot_diffs)`, lines `[batch_key, ledger, batch_type, object_id, unit_key, amount]` (the type 1 debit, 2 credit, 3 memo), diffs `{object_id, type, full | patch}` |
| `jsonpatch::{diff, apply}` | RFC 7386 merge patch, how diffs are made and folded |
| `Rejection`, `StoreError`, `LedgerError` | Hand-written errors, no `thiserror` |

## How a call books (per process)

1. **Read** every journal of the process (one range scan), fold each
   object's snapshot diffs into its current JSON and apply the reader: the
   booked state and version are what the reader returns on the fold. Sum
   the lines per (object, batch key, unit) into positions; each line
   carries its batch's ledger and type, and one key under two of them is a
   corrupt store. The highest sequence is the process version. Nothing
   else is read: the engine knows no recorded rules version.
2. **Re-evaluate every current object** under the running rules and queue
   the difference between its desired state and its booked positions.
   Under unchanged rules that is nothing, by path independence; after a
   rules change it is the restatement, booked ahead of the submissions so
   that a submission's own delta below is the object's own change.
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
   afterwards). In the same statement the store upserts, for every type
   present in the process, the running `VERSION` into its version index,
   latest wins: two tables written per process, one read. The primary key
   on (process, sequence) is the compare-and-set; a lost race is retried,
   `DEFAULT_MAX_ATTEMPTS` times. A restatement that finds nothing to book
   writes no journal and moves the index alone (`record_versions`).

A rules version changes only with a deployment, so the new code calls
`restate_behind()` once when it starts: it pages through `processes_behind`
per registered type, the processes whose indexed version is below the
running one, and restates each (lines, no diffs, one journal; or the index
alone when nothing differs) until nothing is listed. Before serving or
alongside it is the caller's choice; any number of replicas may run it at
once. A replica of the previous deployment still booking during the rollout
books under its own rules and indexes the process at its own version, so
the pass takes that process again; the book settles when the old replica is
gone, and a process it touched last is taken at the next startup. A rules
change is never refused and never special-cased: a process converges to
the running rules whenever it is touched. A version row exists only for a
type the process booked.

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
  reads a clock. The store stamps `booked_at`, the journal's one time; when
  something happened in the world is in the object's own fields, in the
  diffs.
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
a booking by an older replica converging at the next run) and
`tests/conformance_memory.rs` (the
store suite over `InMemoryStore`).
