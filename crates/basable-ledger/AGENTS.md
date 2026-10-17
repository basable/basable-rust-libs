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
| `Accounting<O>` | The rules of one type, no `self`: `VERSION` (bump on a desired-state or classification change), `validate_state_change(current) -> &[State]` (listing the current state allows an amount change in it; a terminal state lists nothing), `desired_state(state, &O, &mut Batches) -> Desired`, `validate_balances(state, &O, &Balances, &Desired)` |
| `BatchAccount` | Whatever names a batch: `ledger()` and `batch_type()` (`Debit`, `Credit`, `Memo`). All the framework knows about a batch; what it means is the user's |
| `Unit` | `unit_key() -> UnitKey(u32)`, what a line carries and what the engine balances by |
| `Batches` | The recorder a desired state names batches through: `open(account, id)` reuses the open batch or has the commit create it, `existing(account, id)` reuses only. Returns a `BatchRef`; two accounts on one id is a `BatchConflict` and a rejection |
| `Desired`, `balance(batch, unit, amount)`, `desired![...]` | The positions of a state: signed amounts in the column of each batch's type. Duplicates of one key are summed |
| `Balances`, `ObjectBalances` | What `validate_balances` and `Ledger::balances` see: per object, per batch id and unit, summed over the batch's epochs, computed from the journals |
| `Ledger<S>`, `Builder`, `Request`, `Outcome` | `Ledger::builder(store).register::<O, A>()...build()`; `book(Request)` books any mix of registered types across processes, one `Outcome` per object: `Booked`, `AlreadyBooked`, `Stale`, `Rejected(reason)`. `balances(process)`, `restate(process)`, `preview_restatement(process)` |
| `Store` | The persistence interface: `read`, `resolve`, `commit`, `record_version`, `batch_sum`; `ProcessRead`, `BatchRow`, `Commit`, `CommitLine`, `LineBatch`, `NewBatch`, `CommitOutcome::{Committed, SequenceTaken}` |
| `InMemoryStore` | The reference store, for tests |
| `Journal`, `Line`, `SnapshotDiff` | The stored shape: lines `[batch_key, object_id, unit_key, amount]`, diffs `{object_id, type, full | patch}` |
| `jsonpatch::{diff, apply}` | RFC 7386 merge patch, how diffs are made and folded |
| `Rejection`, `StoreError`, `LedgerError` | Hand-written errors, no `thiserror` |

## How a call books (per process)

1. **Read** every journal of the process (one range scan), fold each
   object's snapshot diffs into its current JSON and apply the reader: the
   booked state and version are what the reader returns on the fold. Sum
   the lines per (object, batch id, unit) into positions. The highest
   sequence is the process version.
2. **Restate** first, if any type in question has a recorded version behind
   `Accounting::VERSION`: re-evaluate every object of that type on its
   folded snapshot and queue the delta.
3. **Per submitted object**, in order: a version at or below the booked one
   is `AlreadyBooked` or `Stale`. Otherwise the next state must be in
   `validate_state_change(current)`; an unchanged state with changed
   amounts needs the current state to list itself. Evaluate the desired
   state, check it balances (left equals right per ledger and unit, memo
   outside), run `validate_balances` against the process as the call
   leaves it so far, and queue the delta against the object's booked
   positions. A booked position the desired state no longer names closes
   through the same batch id.
4. **One rejection rejects the process's part of the call**, naming the
   object and the reason; other processes in the call are unaffected.
5. **Resolve** every batch id named: an open batch is used, a consumed or
   closed one rolls to the next epoch under the same id, a missing one is
   created (or rejects under `existing`), a batch that exists under another
   ledger or type rejects.
6. **Commit** one journal at `sequence + 1` with every delta line and one
   snapshot diff per booked object (full the first time, a merge patch
   afterwards). The primary key on (process, sequence) is the
   compare-and-set; a lost race is retried, `DEFAULT_MAX_ATTEMPTS` times.
7. **Record** the rules version per type that was missing or behind, as a
   separate converging upsert after the commit.

## Rules

- **The engine is pure; only the store is async.** `engine.rs` takes a
  `ProcessRead` and the submissions and returns a `Commit`; nothing in it
  reads a clock. A restatement's event time is the booking time, passed in.
- **Journals are append-only.** An adjustment, a transition and a
  restatement are all new lines that bring the booked positions to the
  desired state. Nothing is updated or deleted.
- **No balance in the write path.** Positions are sums of lines computed
  on read. The engine never reads a balance table; batch balances are the
  user's projection (`basable-ledger-postgres` offers an optional one).
- **Amounts are signed `i128`** in the column of the batch's type. A
  negative amount is a balance below zero in its column: allowed, because
  clearing an open item across objects is exactly that (a payment holding
  minus the receivable on the invoice's batch), and reported when it ages.
- **The desired state reads only the object.** A price is a field the
  producer computed and sent; a rule change cannot re-price as a side
  effect, which is what makes restatement by re-evaluation sound.
- **Rejections name the object.** A `Rejection` message starts with
  `object <id>` wherever one is to blame.

## What the tests pin

Unit tests here: the merge patch round trip, the recorder's dedup and
conflicts, the wire forms of lines, diffs and amounts, the batch type and
status integers. The behaviour is pinned in `basable-ledger-testkit`:
`tests/scenario.rs` (the invoice lifecycle, duplicates and stale snapshots,
disallowed changes, frozen amounts, the balance check, a payment clearing an
invoice across objects, a rejected object rejecting its process, a rules
bump restating by one appended journal, a consumed batch rolling to the
next epoch, memo batches), `tests/walk.rs` (random histories: every journal
balances, path independence, replay books nothing) and
`tests/conformance_memory.rs` (the store suite over `InMemoryStore`).
