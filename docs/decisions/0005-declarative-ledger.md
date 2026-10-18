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
- A **batch** is the user's: named by any string, with one **ledger** and
  one **type** (debit, credit or memo), both returned by the object that
  names it. That is everything the framework knows about a batch. Entities,
  subledger types, account classes, GL codes and settle times are the
  user's vocabulary for what a batch means, outside the framework.
- A **position** is the booked balance of one (object, batch, unit): the
  sum of the object's lines, computed from the process's journals on every
  read. No position, balance or booked-state table exists.
- A **journal** is one row per commit of one process: lines
  `[batch_key, object_id, unit_key, amount]` and snapshot diffs
  `{object_id, type, full | patch}` (RFC 7386 merge patches; the first
  sight of an object is its full JSON). Folding a process's diffs rebuilds
  every object at any sequence; the reader applied to the fold is the
  booked state and version.
- **Double entry**: in every desired state and every journal the amounts in
  debit batches sum to the amounts in credit batches per (ledger, unit).
  Memo batches stand outside. A negative amount is a balance below zero in
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
  state's desired state recomputed now. A changed amount in an unchanged
  state is allowed only when that state lists itself as its own next state.
- Batches are named through a recorder, not a store handle, so the desired
  state stays pure; the engine resolves the names in one query. A consumed
  or closed batch rolls to the next epoch under the same id; a batch named
  under another ledger or type rejects.
- A commit locks the batches it writes into `FOR SHARE` and refuses one
  that is no longer open (`CommitOutcome::BatchNotOpen`, retried like a
  lost sequence), so a close waits for every in-flight writer and nothing
  lands in a batch after its close; the claim sums the moment the close
  committed. A snapshot-based horizon wait was tried first and dropped:
  a snapshot lists only the in-progress transactions below its `xmax`, and
  a writer whose transaction id is assigned after the close is not in it.
- A rules version bump restates: every process whose recorded version
  (per process and object type, `record_version`, a converging upsert
  after the commit) is behind is re-evaluated on its folded snapshots and
  the difference appended as a journal with lines and no diffs. Prospective:
  history is never rewritten. A version changes only with a deployment, so
  the new code restates once at startup (`Ledger::restate_behind`, over
  `Store::processes_behind`, the version table's index) and a booking into
  a process still behind restates it first, in the same journal. A replica
  of the previous deployment may not book a process already restated:
  `LedgerError::RulesBehind`, since its older rules would move the
  positions back. A rules change is never rolled back; the old desired
  state comes back as a higher version.

## Not in the write path, by design

- **No balance table, and none shipped.** A position is a sum of lines; a
  claim sums its batch after the close. A queryable `batch_balance` is the
  user's projection off the journal table (a trigger, with the warning that
  it serializes bookings on hot batches, or ClickHouse through change data
  capture), and `basable-ledger-testkit`'s `projection` check is what it
  must pass. A time-window projector was built in `basable-ledger-postgres`
  and removed the same day: it is not the framework's concern.
- **No feed column.** A sequence value is handed out inside the
  transaction, so an auto-increment is neither gapless nor commit-ordered.
- **No account-level limit.** The ledger records reality; a synchronous
  limit is a guarded tier beside it, not the ledger's model.

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

## Phases

1. `basable-ledger` and `basable-ledger-testkit` (done 2026-10-17).
2. `basable-ledger-postgres` and the conformance suite against Postgres
   (done 2026-10-17).
3. Batch lifecycle, epochs and claims behind the row lock (done
   2026-10-17). The optional time-window projector built with it was
   removed on 2026-10-18.
4. The restatement at startup (done 2026-10-17 as an operator's sweep with
   a dry run, reshaped 2026-10-18): `Store::processes_behind` reads the
   version table's index, `Ledger::restate_behind` runs to completion when
   the new deployment starts, `preview_restatement(process)` is the
   per-process dry run, and a ledger on older rules is refused. A process
   that booked a type but crashed between commit and `record_version` is
   not on the worklist; its next booking restates it first.
