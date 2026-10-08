# basable-processingobject — the declarative reconciliation framework

The port of the monorepo's `golang/controller/lib/processingobject`:
desired state lives in typed spec rows under a generic envelope, and
stateless multi-replica workers drive the external world to match it, one
exclusively claimed, fenced attempt at a time. The Go package's `CLAUDE.md`
and package doc are the specification; this file says what is the same and
what the type system changed (`docs/porting-notes.md` 13–34 has the list,
81 the in-process wake that replaced the database one; 3, 7–9 and 12 cover
the pieces it leans on in `basable-core`, `basable-db` and the testkit). The Directive (`docs/DIRECTIVE.md` in every tenant repository, `golang/controller/lib/scaffold/directive.md` in the monorepo) is the contract this crate serves.

Two meta-rules govern any change: an invariant the framework cannot
mechanically enforce is a comment, and comments rot — enforcement lives in
the schema, the transaction shape or a check the code performs; and a
public API without a call site does not land (no framework list-all, no
process-wide store, no in-process type registry: a nanoservice binds one
`TypedStore` per type over its own pool and lists through its own tables).

## The eight invariants

Each module names the invariant it implements.

1. **Envelope owns the metadata.**
2. **Spec mutation is one locked transaction.**
3. **Status is writable only under exact claim authority — single
   provenance.**
4. **Claims are exclusive and leased, fenced two ways.**
5. **Completion is one fenced transaction.**
6. **Deletion is one-way.**
7. **Remote I/O never runs inside a database transaction**, while holding an
   envelope lock, or on a pooled connection another transaction is using.
8. **Types are nanoservice-local and nanoservices are logically separated.**

The Directive's section 3 is the full text; section 4 lists what each one
obliges the author of a reconciler to do.

## The envelope

One partitioned table, `basable.processing_object`, owns identity,
desired-state revision, scheduling, deletion intent and claim authority for
every type; each type is a `SMALLINT` key in the `processing_object_type`
registry and a LIST partition `nano_<name>.processing_object_<type>` in the
owning nanoservice's schema. Typed spec/status tables reference the
PARTITION with `ON DELETE CASCADE` (note 9), and every framework statement
targets the partition (note 13). `Meta` (`model.rs`) is the envelope's
read model — public id, the immutable `NamespacedName`, labels,
`generation` / `observed_generation`, `wake_seq`, `phase`, `attempts`,
`last_error`, `deleted_at`, the schedule sentinels. Holding a `Meta`
grants nothing: the claim token is not exposed, and there is no
`claimed_by` column.

## The store

`TypedStore<S, T, A>` (`store.rs`) is the per-type handle.
`TypedStore::bind(&pool, decl)` validates the `ProcessingObjectType` and
probes the database's registry and partition fail-fast, so a missing
migration is a boot failure. The paths:

| Path | Module | Transaction shape |
|---|---|---|
| `create(id, name, &spec, &status, CreateOptions)` | `store.rs` | the only INSERT: envelope + typed spec + typed status in one transaction, `id` client-minted so an ambiguous commit is adopted by retrying the same id (`Error::NameTaken` is the one natural-key violation the framework classifies itself); wakes the store's workers after the commit (an adopting call wakes nobody) |
| `update_spec(&ref, mutate)` | `store_mutate.rs` | invariant 2: lock the envelope, read the committed row, apply the synchronous closure `FnOnce(&mut S, &T) -> Result<(), BoxError>` (the status is the last committed one, read-only — note 16, 34), write the spec through the adapter, advance `generation` and `wake_seq`; wakes after the commit |
| `mark_deleted(&ref)` | `store_mutate.rs` | invariant 6: stamps `deleted_at` and wakes (the idempotent repeat changes nothing and wakes nobody); further intent writes answer `Error::Deleting` |
| `nudge(&ref)` | `store_mutate.rs` | advances `wake_seq`, makes the object due now, wakes |
| `read(&ref)` / `read_many(&ids)` | `store_read.rs` | one `REPEATABLE READ READ ONLY` snapshot per call, envelope and typed rows together |
| `claim_batch(cfg)` | `claim.rs` | invariant 4, below |

The wake is in process (`wake.rs`, note 81). A store's shared inner holds
the wake of every `Worker` currently running on it (registered for the whole
of `run`, removed by a guard on exit), so every clone of one store shares
them; a write that makes an object due signals them all once its
transaction has COMMITTED, never for a rolled-back write, and a nanoservice
cannot signal one by hand. It rests on two facts of the deployment: every
replica runs every worker, and every write to a type happens in a process
running that type's worker, through the one store the nanoservice bound.
A store bound separately for the same type (`Harness::second_store`)
shares nothing and models another process: its writes reach these workers
through the poll, which stays the correctness path. Bind once, clone
everywhere.

## Claims, fenced two ways

`claim_batch` takes disjoint batches across replicas with `FOR UPDATE SKIP
LOCKED`, ordered by how overdue work is with `last_reconciled_at` as the
fairness key; adopting an expired claim bumps that key, so a poison object
rotates to the back. A `Claim<S, T, A>` is the attempt's authority: the
claim-time `object` snapshot, `adopted()`, `config()`, `r#ref()`,
`lease_handle()` and `heartbeat()`. Authority rests on two clocks:

- the DATABASE token and lease (`claim_token`, `lease_expires_at`): once the
  lease expires, a successor replaces the token through ordinary claiming;
  every write of this attempt is `… WHERE id = $1 AND claim_token = $2`,
  and zero rows affected is `Error::Fenced`, permanent for the attempt;
- the LOCAL ownership proof, a `Deadline` (note 3, 24) measured from a
  reading taken BEFORE the claim was sent and extended by each successful
  heartbeat (`LeaseHandle::heartbeat` in `claim.rs`). `require_proof` fails closed with zero I/O
  once it lapses, so a paused process cannot write after its lease was
  stolen even if the database still shows its token.

Only the token is authority; lease expiry never fences the holder by
itself, and the proof only ever revokes. There is deliberately no release:
a claim ends by completion or by lease expiry, so crash, shutdown
mid-attempt and lost completion all converge on one abandonment path.
`Claim` and `LeaseHandle` implement `basable_externaleffect::Owner` (note
47), which is how an effect `dispatch` is bounded by the remaining lease.

## The adapter and outcome contract

A type is `ProcessingObjectType::new(name, key, prefix, adapter)`
(`decl.rs`). The `Adapter<S, T>` trait is a dumb column mapper with five
required callbacks — `insert_spec`, `insert_status`, `read_rows` (batched
by contract, once per claim batch; a requested id missing a typed row is
`Error::Invariant`), `write_spec`, `write_status` — and the optional
`finalize_delete`, run inside the deletion completion. Every callback
receives `&mut Tx<'_>`, the framework's transaction wrapped so that only
`Executor` is implemented: `begin`, `commit` and `rollback` are unreachable
by type (note 14). `write_status` writes every column it owns,
unconditionally: every status write the framework issues is fenced
identically, so the adapter never knows who is writing (the payoff of
invariant 3).

The reconciler returns `Result<Outcome<T>, BoxError>` (note 22); an `Err`
is a `Retry` with no status write. `Outcome<T>` (`outcome.rs`) is an enum
with no zero value (note 15), each `status: Option<T>` meaning "leave the
row as is" when `None`:

- `Outcome::converged(status)`, `converged_after(status, d)`,
  `requeue_now(status)` — success for the observed generation, scheduled
  at `resync`, after `d`, or at once (the scheduling modifiers exist only
  on `Converged`, so a modifier on the wrong decision is unrepresentable);
- `Outcome::retry(status, cause)` — transient; keyed exponential backoff
  (`Backoff::delay(id, generation, attempt)`, bit-identical to Go),
  `attempts` advance, `max_attempts` escalates to `Blocked`;
- `Outcome::blocked(status, cause)` — unsatisfiable until a new generation
  or a nudge; refused for a deleting object;
- `Outcome::delete()` — absence confirmed: the envelope goes, typed rows
  cascade, `finalize_delete` ran first;
- `Outcome::settled(status)` — successful permanent convergence, parked;
  with `deleted_at` set, a retained tombstone.

`claim.complete(out)` (`complete.rs`, invariant 5) consumes the claim
(note 21): one transaction that re-verifies the token under the envelope
lock, writes status under a savepoint (a constraint-rejected row becomes a
loud classified `Retry` instead of a livelock — note 25), settles the
envelope and clears the claim. A superseded or woken completion still
commits its observation but leaves the object due now. An ambiguous
COMMIT is retried once; if the retry finds the landed completion's
signature it adopts it and answers `Completion::Unknown` (note 23, 26),
and no post-completion callback runs. `COMPLETION_TIMEOUT` bounds it.

`complete` also does the completion's wake, after the commit and before
it returns (so before the worker runs `after_complete`, and identically
for a manual drive): a completion that leaves the object due now —
`requeue_now`, superseded or woken, the finalizer-failed retry under a
stale fence — signals the store's workers; one due again after `d` with
`0 < d <= poll_interval` (the claim's `WorkerConfig`) — `converged_after`,
a short `Retry` backoff, a short default resync — arms a one-shot timer
that signals them after `d`, a detached task that holds no parallelism
slot and signals nobody once the worker stopped. Parked (`Blocked`,
`Settled`), deleted and `Unknown` completions arm nothing, nor does a
delay longer than the poll interval: the poll serves it within one
interval, as before. Without the timer `after(5s)` under a 30 s poll ran
up to 30 s late. The settled schedule (`Due`) and the decision (`Wake::
for_due`) are crate-internal.

`claim.write_status(status)` (`writestatus.rs`) is the mid-attempt half of
invariant 3: a fenced whole-row write that settles nothing, for exactly
one pattern completion cannot express — declare-before-I/O, a marker that
must be durable before a remote effect goes out, written by the SAME
attempt that then sends it. Keep ONE working status value; the completion
overwrites the whole row from whatever `T` it is given.

## The worker

`Worker::new(store, WorkerConfig, reconciler, after_complete)` drives one
type on one replica; `run(ctx)` until the context is cancelled, then drains.
Give it a clone of the store the type's writers use: `run` registers the
worker's wake on that store for its whole duration.
`Reconciler::reconcile(&self, ctx, &mut claim) -> impl Future<Output =
Result<Outcome<T>, BoxError>> + Send` and `AfterComplete::after_complete`
are traits with `impl Future` methods (note 27); `NoAfterComplete` is the
no-op. Per attempt (`worker.rs`):

1. a heartbeat pump task on the `LeaseHandle` ticks at `(attempt_timeout +
   LEASE_SLACK) / 3`; a transient error waits for the next tick, a fenced
   heartbeat cancels the attempt (note 30);
2. the pass runs under `catch_unwind` on a context bounded by
   `attempt_timeout`; a panic completes as a loud `Retry` without a stack
   (note 28), a deadline or cancellation drops the pass future (note 29);
3. the pump is aborted and joined, then `complete` runs under
   `COMPLETION_TIMEOUT`, not a detached context (note 32), and does the
   completion's wake once committed;
4. `after_complete` runs on a bounded detached context for a `Committed`
   completion only, its panic contained.

Scheduling is poll-first: the `poll_interval` scan is the correctness
path; the store's in-process wake (above) only shortens latency, and wakes
coalesce into a stored permit (many signals while a scan runs mean one
more scan). The worker holds no database connection of its own. A freed
attempt slot signals this worker alone: that news is local.
`WorkerConfig`'s scheduling fields (`resync`, `backoff`, `max_attempts`,
`attempt_timeout`) MUST be identical on every replica of one logical
worker; `poll_interval`, `batch_size`, `parallelism`,
`after_complete_timeout` are per-replica tuning; a non-empty
`label_selector` defines a logical worker (selectors pairwise disjoint and
jointly covering). Zero means "default" (note 18).

## Errors, and what is deliberately gone

One `Error` enum, matched on by kind (note 17): `NotFound`, `Deleting`,
`NameTaken`, `Invariant`, `Fenced`, `InvalidConfig`, `Mutate`, `Sql { op,
source }`, `CommitUnknown { op, source }` (every store write is safe to
retry after it: a create adopts by id, the rest re-apply). Not to be
resurrected, as in Go: unfenced status writes, cross-type cascades, a
generation-CAS `update_spec`, caller-supplied external ids, `Permanent`
outcomes (`Settled` is the parked success), a process-wide store or type
registry, kill seams on the worker. Gone in both since 2026-10-08 (note
81): the database wake — `pg_notify` on a wake channel, a `LISTEN` per
worker or per process. The admin inspector (`inspect.go`) is the
platform's and has no port here.

## File map

| File | Responsibility |
|---|---|
| `src/lib.rs` | The contract: the crate doc and the eight invariants; re-exports |
| `src/model.rs` | `NamespacedName`, `Ref`, `Phase`, `Meta`, `Row`, `Object`, the schedule sentinels (invariant 1) |
| `src/outcome.rs` | `Outcome<T>`, `Schedule`, the constructors and predicates |
| `src/decl.rs` | `Adapter`, `ProcessingObjectType`, `Backoff`, `WorkerConfig` + `validated` |
| `src/tx.rs` | `Tx`, the statements-only transaction |
| `src/error.rs` | `Error` |
| `src/store.rs` | `TypedStore`, `bind`, `create`, `CreateOptions` |
| `src/store_mutate.rs` | `update_spec`, `mark_deleted`, `nudge` (invariants 2 and 6) |
| `src/store_read.rs` | `read`, `read_many` |
| `src/claim.rs` | `claim_batch`, `Claim`, `LeaseHandle`, `heartbeat`, the local proof, `LEASE_SLACK` (invariant 4) |
| `src/writestatus.rs` | `Claim::write_status` (invariant 3, mid-attempt) |
| `src/complete.rs` | `Claim::complete`, `Completion`, the completion's wake (invariants 3, 5, 6) |
| `src/wake.rs` | The in-process wake: `Wakes` (the store's registered workers, `signal`, the timer), the `Registration` guard, `Due`, `Wake::for_due` — all crate-internal |
| `src/worker.rs` | `Reconciler`, `AfterComplete`, `NoAfterComplete`, `Worker`, `COMPLETION_TIMEOUT` |

The conformance suite lives in `basable-processingobject-testkit`.
