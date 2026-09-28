# The Directive

This document is the constitution of a basable nanoservice application. It
is authored once in `basable-rust-libs` (`docs/DIRECTIVE.md`), versioned with
the crates, vendored verbatim into every project the scaffolder renders, and
loaded verbatim into the agent's system prompt. Where code and this document
disagree, this document is right and the code is a bug. It is the same
contract the platform's own Go controller runs on; the Rust crates make more
of it mechanical (§9), but none of it optional.

## 1. The declarative model

An application is ONE binary running as several identical replicas over ONE
Postgres database, made of N **nanoservices**. A nanoservice is a crate under
`nanoservices/` that owns a set of things, and what it owns is what it is:

- **0..N processing-object types.** A lifecycle the system drives: desired
  state in a typed `<type>_spec` table hanging off the shared
  `basable.processing_object` envelope, observed state in a typed
  `<type>_status` table, and a stateless multi-replica `Worker` running a
  `Reconciler` that drives the world to match, one exclusively claimed,
  fenced attempt at a time. The framework owns identity, the desired-state
  generation, scheduling, retry, deletion intent and claim authority; the
  nanoservice owns the two typed tables, an `Adapter` mapping their columns,
  and the reconcile pass.
- **0..N plain tables** in the nanoservice's own schema, queried through its
  own `repository.rs`.
- **0..N config types**: declarative catalogs a human edits as JSON under
  `config/base/` and the app loads at boot; the system never creates them.
- **0..N external providers**, each call through exactly one external-effect
  adapter (§6).
- **0..N schedules** (a ticker worker the nanoservice owns) and **0..N
  webhooks** (a raw route that verifies a signature and makes one messenger
  send).

There is no label to pick. **The one question, asked per lifecycle, not per
nanoservice:** is there a multi-step lifecycle to track here — waiting,
polling a third party until it finishes, scheduled work, driving another
system to match? If NO, the nanoservice is a plain executor over whatever
tables and idempotent external calls it has, and a processing-object type
would be over-built. If YES, that lifecycle is a processing-object type with
a spec table, a status table, a reconciler and a worker. A nanoservice may
own several types and plain tables at once; one that owns nothing but an
adapter is complete as it is. Prefer the cheapest thing that fits, and let a
nanoservice start with no type and grow one when a genuine multi-step need
appears — that is a cheaper mistake to fix than unneeded state-machine
machinery built up front.

Config catalogs versus processing objects: a config type is edited by a
person and loaded; a processing object is created by the system and
converged. Something both people and the system change is two things — a
catalog the system reads and an object the system owns.

Every replica runs every nanoservice. Nothing in memory survives a restart
or is trusted across replicas: state is in Postgres, under claim fences.

## 2. Table ownership

A nanoservice never touches another nanoservice's tables — reads included.
Every table has exactly one owner, the nanoservice whose migrations create it
in its own schema, and only that nanoservice's code queries or mutates it.
Cross-nanoservice data access is a messenger request answered by the owner
from its own tables. If no route exists, add one in `routing.yaml`; never
join the table. The database enforces this (§9: one schema and one role per
stateful nanoservice), so a cross-schema query is a `permission denied`, not
a code-review note.

## 3. The eight invariants

1. **Envelope owns the metadata.** Identity, generation, scheduling, deletion
   intent and claim authority live in the envelope; typed spec/status tables
   carry domain columns only and cascade from it.
2. **Spec mutation is one locked transaction.** Every accepted mutation locks
   the envelope first and, in that transaction, advances the generation,
   stamps `generation_changed_at`, advances `wake_seq`, resets retry state,
   re-arms scheduling and publishes a wake. Newer intent is visible to every
   fence before any successor could act on the old spec.
3. **Status is writable only under exact claim authority — single
   provenance.** The writing transaction locks the envelope and verifies the
   attempt's claim token, generation and wake sequence before the adapter
   write. There is no unfenced status-write path, so the adapter writes every
   column it owns, unconditionally — no provenance flags, no conditional
   column guards, no monotonic merges. Two fenced paths exist, both writing
   the whole row through the one adapter callback: the **completion**
   (invariant 5) and **`Claim::write_status`**, a mid-attempt write that
   settles nothing on the envelope and exists for exactly one pattern
   completion cannot express — declare-before-I/O: a marker that must be
   durable *before* a remote effect goes out, written by the *same attempt*
   that then sends it. Input that arrives without a claim (user activity,
   provider observations, webhooks) belongs in a table owned by that writer,
   or wakes the object through `nudge` so a claimed pass observes it. A
   fenced-out attempt records nothing in status, ever.
4. **Claims are exclusive and leased, fenced two ways.** A per-attempt token,
   a lease heartbeats extend, and expiry that makes the row claimable by a
   successor. A holder also self-fences on a local monotonic ownership proof
   measured from before the claim was sent and checked against both clocks,
   so a paused process cannot write after its lease was stolen even if the
   database still shows its token. Only the token is authority; lease expiry
   never fences the holder by itself, and the local proof only ever revokes.
5. **Completion is one fenced transaction.** Typed status and envelope
   scheduling commit together or not at all. A completion that lost a
   generation or wake race still commits its observation but leaves the
   object due now. An ambiguous commit is retried once; if the retry cannot
   identify which outcome landed, the landed transaction is adopted,
   `Completion::Unknown` is reported, and no post-completion callback runs.
6. **Deletion is one-way.** `mark_deleted` stamps `deleted_at`. After
   confirming external absence a reconciler returns `Outcome::Delete` to
   remove the envelope (typed rows cascade) or explicitly `Settled` to keep a
   successful tombstone. A teardown that has not confirmed absence stays loud
   — surfaced by age, not by retry count — and cannot park via `Blocked`.
7. **Remote I/O never runs inside a database transaction**, while holding an
   envelope lock, or on a pooled connection another transaction is using.
   Fences bracket effects; they never span them. A messenger send is remote
   I/O under this rule.
8. **Types are nanoservice-local and nanoservices are logically separated.**
   A type belongs to exactly one nanoservice; the framework offers no
   cross-type write path — no cascades, no derived-intent writes into another
   type's rows, no claim authority that spans types. A nanoservice owning
   several types writes each through its own `TypedStore`. Across
   nanoservices the messenger is the only channel. Consistency comes from
   **generations, not shared authority**: a message carries the sender's
   generation, and the receiver persists the last accepted value and applies
   only strictly-increasing ones, so a late call from a stale pass can never
   overwrite newer intent.

## 4. What each invariant obliges the author to do

1. Domain columns only in typed tables; adapter callbacks never touch
   envelope columns.
2. Every intent write goes through `TypedStore::create`, `update_spec`,
   `mark_deleted` or `nudge` — never through `repository.rs` SQL.
3. No unfenced status writer, and you must not add one. Input that arrives
   without a claim goes into a side table owned by that writer plus a
   `nudge`, never into `<type>_status`.
4. Policy discipline: `WorkerConfig`'s `resync`, `backoff`, `max_attempts`
   and `attempt_timeout` identical on every replica — replicas that disagree
   steal each other's live leases.
5. Return exactly one `Outcome` per pass and let the framework commit typed
   status and envelope scheduling together; never persist status yourself.
6. A teardown returns `Delete`/`Settled` only on confirmed absence — never
   because a cascade was *accepted*.
7. Adapter callbacks are database-only; provider calls and messenger sends
   happen in the reconcile pass between fences.
8. Cross-nanoservice consistency is generation fencing over the messenger,
   never shared authority (§7).

A design that cannot be squared with an invariant is a framework change, not
a nanoservice change: stop and re-plan rather than working around it.

## 5. The adapter and outcome contract

A type is declared as a `ProcessingObjectType<S, T>` (registry name, positive
`TYPE_KEY`, lowercase `PUBLIC_ID_PREFIX`) plus an `Adapter<S, T>` — five
callbacks (`insert_spec`, `insert_status`, `read_rows`, `write_spec`,
`write_status`) and one optional (`finalize_delete`). Every callback runs
inside a framework-owned transaction that has already locked the envelope,
receives a restricted `Tx` (statements only — no commit, rollback or
savepoint), is database-only, and never touches envelope columns.
`read_rows` is batched by contract. **The adapter is a dumb column mapper**:
`write_status` takes a whole `T` and writes every column unconditionally;
every column present in `write_spec` is present in `read_rows` and vice
versa, so a column can never be silently lost.

The reconcile pass is **level-triggered**: read the whole claim-time
snapshot (`claim.object` — `meta`, `spec`, `status`) and do whatever that
state still requires, rather than reacting to what changed. Check
`meta.deleting()` first and return; re-check external state before acting
(check-then-act, never fire-and-forget); return exactly one `Outcome<T>`:

- `Converged { status }` — success for the observed generation, rescheduled
  at `resync`; `.after(d)` for a known wait, `.requeue_now()` to checkpoint
  and continue.
- `Retry { status, cause }` — transient; keyed exponential backoff, may
  escalate to `Blocked` at `max_attempts`.
- `Blocked { status, cause }` — this generation is unsatisfiable; parked
  until a new generation or `nudge`.
- `Settled { status }` — successful terminal park (also the retained
  tombstone when `deleted_at` is set).
- `Delete` — deleting object, external absence confirmed.

`status: None` means the pass observed nothing and status is left untouched.
Besides the final outcome there is exactly one other status write a pass may
make: `claim.write_status(status)` — fenced, mid-attempt, settling nothing —
used only immediately before a remote effect that is not look-before-act safe
(§6, `Declared`). Keep one working status value (`claim.object.status`); a
status rebuilt from a stale copy erases the marker. `WorkerConfig` is per-type
policy from one constructor, not per-call tuning. There is no stuck-row
budget of your own: `max_attempts` (0 = unbounded) escalates a failing
generation to `Blocked`, and a deleting object is escalated by age.

## 6. External-effect admission

Every call that leaves the application's state domain — a payment, an email,
a third-party API, a Kubernetes write — is exactly one
`externaleffect::Adapter<Args, ResolveArgs, Result>` (held as a `Call`) whose
`strategy` literal names, in code, what makes a retry of that call safe. The
sum is sealed; the four answers that exist, ordered by how much help the
receiver gives:

| Strategy | Pre-send requirement | Re-entry resolution |
|---|---|---|
| `Idempotent` | nothing — the receiver converges or dedupes | re-send is the resolution |
| `LookBeforeAct { lookup }` | `lookup` returned verified absence | look again; found ⇒ adopt, never re-send |
| `KeyedReplay { window, intent_age, provider_id, resolve }` | a durable key committed in the caller's own row before the send | same-key replay inside `window` (`dispatch` refuses an aged intent — hold, never re-POST); once the provider id is persisted, resolve by id forever |
| `Declared { slot_identity, Resolve(..) \| Hold }` | the slot written to typed status through `claim.write_status` before the send | the slot resolver on the next claim, or `Hold` forever when no receiver postcondition exists |

Plus the plain fields: `key` (deterministic identity from `Args`, never a
retry count), `classify` (`classify_transport` for reversible effects;
`classify_fail_closed` for anything `irreversible`, definitive ONLY on an
error the provider wrapped in `Definitive` as proof of non-execution),
`call_timeout`, `irreversible` (charges, paid orders, destructive writes —
restricts the strategy to `KeyedReplay`/`Declared` and makes the ack-loss
audit mandatory), `late_call` (`Convergent` / `KeyScoped` / `Compensated`).

In a reconciler, dispatch with the `Claim` as owner and match the result:
`DispatchError::OwnershipLost` ⇒ nothing was sent, plain `Retry`;
`Ambiguous` ⇒ it may have landed, `Retry` and let the next pass re-drive
(safe by the strategy's definition); `Definitive` is the receiver's answer. A
request-path handler holding no claim dispatches `Unfenced` — explicit and
greppable. A raw provider call that does not go through `Call::dispatch`
fails review. Reads (`lookup_*`, `resolve_*`, pre-flight checks) are not
adapters: they send nothing.

Every adapter has exactly one `effecttest` audit in the nanoservice's own
`tests/effects_audit.rs`, run against the nanoservice's simulator through the
real provider client. Needing `Declared` is a signal to first ask whether the
effect can be reshaped to pass a cheaper strategy.

## 7. Cross-nanoservice rules

- **The generation is the cross-nanoservice fence.** When a pass hands
  derived state to another nanoservice, send the claim-time generation with
  it; the receiver keeps the highest it has applied and drops anything
  strictly lower. An equal generation still passes, which is what lets a
  level-triggered loop re-drive the same intent every pass. Omitting the
  generation disables the fence.
- **When the receiver must observe writes in exact order, serialize on a
  counter row under `FOR UPDATE`** in the receiving nanoservice's own table —
  never a timestamp or a sequence (sequences are non-transactional: a lower
  value can commit after a higher one). Lock the counter, compare-and-accept
  the incoming sequence, do the guarded write, advance the counter, commit.
- **The receiver mints ids; the caller's seam is a name.** A nanoservice
  never mints another nanoservice's object id; it derives a stable name from
  its own local identity and keys ensure/get/delete on it.
- **Only derived state crosses.** Send the effective value, not the columns
  it was derived from, so the receiver never re-derives a rule it does not own.
- **An event is not a cascade.** If something downstream MUST happen, it is
  a direct messenger request from the reconcile pass carrying the
  generation; an event is for nanoservices that merely want to know.
- **No advisory locks in nanoservice code.** Unique indexes with
  `ON CONFLICT`, converging level state and key epochs instead.

## 8. Deliberately gone

Do not resurrect these:

- **Unfenced status observation.** No `observe_status`; unfenced input goes
  to a side table owned by its writer.
- **Cross-type cascades.** No registration DAG, no cross-type writes;
  coordination is the messenger plus strictly-increasing generations.
- **Generation-CAS spec writes.** `update_spec` is an unconditional
  read-modify-write under the envelope lock; the mutate closure is sync,
  sees the last committed status read-only, and does no I/O.
- **Permanent outcomes.** `Settled` is the successful parked outcome; a
  spec write or `nudge` re-arms it. `Blocked` is forbidden on a deleting
  object because failed teardown must not park silently.
- **`claimed_by` columns.** The claim token is the only authority, so a
  restarted process with the same name can never touch its predecessor's
  claim.
- **In-memory state that survives a restart or is trusted across replicas.**

## 9. What the runtime enforces, and what only you can

The crates make these mechanical: adapter completeness is a trait, so a
missing callback does not compile; `Outcome` has no zero value and scheduling
modifiers exist only on `Converged`; `Completion::Unknown` carries no outcome
to misread; `Claim::complete(self)` consumes the claim, so nothing can be
written or heartbeaten after completion; `update_spec`'s closure is sync and
sees `&T`, so there is no I/O under the envelope lock and no status write
through it; `Tx` implements only statement execution; `Strategy` is a closed
enum, `Declared` is `Resolve` XOR `Hold`, `LateCall` and `SlotIdentity` have
no unset variant, `DispatchError` forces the ambiguous/definitive match, and
an owner cannot be null; `WorkerConfig` fields are unsigned; one schema and
one role per stateful nanoservice, with `NanoPool<N: Stateful>` making a
cross-schema query a Postgres error and a pool for a nanoservice that owns
nothing a compile error; who may send what is a trait bound on the generated
sender (an undeclared send has no method), and another nanoservice's handler
is unreachable behind the router's private fields; every generated handler
future is `Send`, so a `std::sync::MutexGuard` held across a messenger send
does not compile (`clippy::await_holding_lock` and `disallowed_types` banning
`tokio::sync::Mutex` and `RefCell` in nanoservice crates back this up); the
app verifies at boot that the migration ledger holds every embedded version.

Only review catches these, so review for them: the scheduling policy identical
across replicas; a level-triggered pass that checks-then-acts and returns one
outcome; teardown that settles only on confirmed absence; the generation on
every cross-nanoservice push; the audit per adapter kept green against the
real provider; the docs per nanoservice (§10). In-memory state that does
exist (an abort map, a cache) goes behind `std::sync::Mutex`, atomics or
`DashMap`, and is never held across an `.await`.

## 10. The documentation obligation

Every nanoservice carries a `CLAUDE.md` (file responsibilities, what it
owns — types, tables, config, providers — persistence and authority, the
declared adapters with their strategies) and a `flows.md` (the state machine
per type, sequences, edge cases). A behaviour change without its doc update is
an incomplete change. The root `CLAUDE.md` points here first; `routing.yaml`
and `docs/architecture.md` are the map of who sends what to whom.
