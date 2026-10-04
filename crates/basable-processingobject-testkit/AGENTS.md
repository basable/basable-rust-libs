# basable-processingobject-testkit — the conformance substrate

The port of the monorepo's `golang/controller/lib/processingobject/testkit`
and, under `tests/`, of the framework conformance suites in
`golang/test/processingobject` (`processingobject_*_test.go`), phase by
phase. It is what `basable-processingobject` is tested THROUGH: a reserved
test-only type, a provider simulator, a harness that builds scenarios, and
the running side of a worker. The Go testkit's section of the framework's
`CLAUDE.md` is the specification; `docs/porting-notes.md` 11, 12, 19, 20
and 33 say what the port changed. The Directive (`docs/DIRECTIVE.md` in every tenant repository, `golang/controller/lib/scaffold/directive.md` in the monorepo) is the contract this crate serves.
Every suite below pins one of its eight invariants against a real database.

Nothing here is a `#[cfg(test)]` module: a tenant's own integration tests
may use the harness shapes (a per-test database, a simulator behind a
real client, a manual one-shot drive) as the pattern for testing their
types.

## The conformance type

`conformance.rs`: a nanoservice like any other (note 19) — `nano_conformance`
role and schema, its two migrations under `migrations/` (the init and the
type), the type `conformance` with key `32000` and prefix `ek`, which no
production type uses, `Spec` / `Status` and the `ConformanceAdapter` (a dumb
column mapper; status carries no activity marker — an unfenced writer
would get its own table). `apply_schema(&migrator)` applies the migrations
on top of the tenant fixture's framework migration, as the migrator;
`conformance_type()` is the declaration; `identity_name(id)` mints the
deterministic envelope name a create replay must present again. A `CHECK
(provisioned_widgets >= 0)` on the status table is the seam that drives the
status-constraint → loud-`Retry` path.

## The simulator

`WidgetSim` (`widgetsim.rs`) is an axum server on a loopback port behind a
reqwest `WidgetClient` (note 20), so the effect-admission suites can put a
proxy between the caller and it. Two resources: idempotent widgets keyed by
`canonical_key(type, id)`, counting only the FIRST physical creation per
key; and orders, an irreversible resource minted under an idempotency key
the receiver replays and re-reads (`place_order`, `get_order`,
`find_order_by_key`). `fail_next`, `reject_next`, `drop_ack_next` and
`set_latency` inject the provider conditions the retry, rejection and
ack-loss scenarios need; `count`, `delete_count`, `exists`, `keys`,
`order_count` are the assertions' readers. `WidgetClient` maps errors the
way a production provider boundary does: a 4xx is `definitive`, anything
without a status is a `TransportError`.

## The harness

`Harness` (`harness.rs`) over a `TestDb` from `basable-testkit` (note 11:
`TEST_DATABASE_URL`, a database per test, `from_env()` returns `None`
without it) with `fast_config()` as the default policy. Scenario helpers:

- `create(spec)` / `create_with_id(id, spec)` — the envelope identity
  minted deterministically (`identity(id)`), so a replay presents the
  identity the object was born with;
- `drive_once(..)` / `drive_once_with(..)` — the manual one-shot drive: one
  claim, one pass given as a `DriveFn`, one completion; `DriveError::
  NothingDue` when nothing is claimable;
- the crash seam: `claim_batch()` plus `force_expire_claim(&ref)`, which
  drops a claim without completing it and collapses its lease so a
  successor adopts at once. There is no kill: a worker stops only by
  cancellation, so crash and expired-lease adoption are simulated at the
  claim level;
- the running worker: `start_worker(rec, after)` /
  `start_worker_config(..)` return a `Replica` (`runtime.rs`) whose
  `stop()` cancels and drains (note 33), `claimed_count()`;
- the envelope readers: `meta`, `envelope` (an `EnvelopeSnapshot`),
  `lease_expires_at`, `gen_changed_at`, `is_archived`, `wait_for`;
- `second_store()` — a second pool and store, for the two-replica cases;
- `finish()` — drop every listener before the database goes (a
  `PgListener` holds a pool connection).

`ExampleReconciler` (`runtime.rs`) is the default pass over the simulator
with two injection points, `before(hook)` and `after_call(hook)` (`hook`
builds one from a closure), and `Gate` holds a pass open (`wait_entered`,
`release`) so a test can act while an attempt is in flight.

## What the suites pin

| File | Tests | Ported from | Covers |
|---|---|---|---|
| `tests/store.rs` | 10 | `processingobject_store_test.go` | envelope identity round trip, name conflicts and adoption, release by deletion intent, the create carve-outs, the intent writes, the read model, identity across an ambiguous create commit in both commit-fault modes |
| `tests/fencing.rs` | 10 | `processingobject_fencing_test.go` | the token fence, lease adoption, the local proof, superseded and woken completions — through manual `claim_batch`, `complete` and `heartbeat` plus raw SQL |
| `tests/writestatus.rs` | 10 | `processingobject_writestatus_test.go` | the mid-attempt write: durable across a dropped claim, fenced after a lease steal, envelope untouched, lands under a superseded generation, a rejected row writes nothing, the no-status completion forms preserve it, a deleting object takes the marker |
| `tests/deletion.rs` | 5 | `processingobject_deletion_test.go` | one-way deletion, intent guards, hard delete versus a settled tombstone, the savepoint rollback of a failed finalizer |
| `tests/worker.rs` | 8 | `processingobject_worker_test.go` | multi-replica claim exclusivity, crash adoption, panic containment, parallelism-bounded claiming, wake versus poll scheduling, `after_complete` containment — the real worker loop |
| `tests/carveouts.rs` | 6 | `processingobject_carveouts_test.go` | label routing across logical workers, the unroutable unlabelled row, the adoption flag, the status-sighted `update_spec` against a committed status and against an attempt in flight |
| `tests/commitfault.rs` | 3 | `processingobject_commitfault_test.go` | a completion whose COMMIT ack is lost: applied (adopted, `Unknown`) and rolled back (re-run cleanly); the provider-side twin, an effect whose ack is lost lands once |
| `tests/schema.rs` | 2 | `processingobject_schema_test.go` | the claim scan rides the partial `due_at` index; a status constraint violation becomes a loud `Retry` |

Commit faults come from `basable_testkit::CommitFaultProxy`, a loopback
TCP proxy that drops exactly one COMMIT acknowledgement, applied or rolled
back (note 12: pgx's dial hook has no sqlx equivalent).

## Gotchas

- Drop every `PgListener` (through `Harness::finish` or `Replica::stop`)
  before `TestDb` closes, or the close deadlocks on the held connection.
- The suites need `TEST_DATABASE_URL` and return early without it; `bazel
  test //crates/basable-processingobject-testkit/...` alone is not the gate.
- `fast_config()` shortens the lease and the attempt timeout; a scenario
  that reads the clock must use the harness's readers, not its own
  arithmetic.
