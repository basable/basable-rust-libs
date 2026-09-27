# Porting notes

Every place the Rust crates deviate from the Go originals in the basable
monorepo, with the reason. The rule is: identical semantics unless the type
system makes an invariant mechanical, and then the deviation is written down
here.

## basable-core

1. **`AppError` carries the sixteen Connect codes, not Go's nine.** Go's
   `apperror.Code` is a local enum the server framework maps onto gRPC
   statuses; a tenant's boundary is connect-rust, so the port carries the
   wire vocabulary directly. The mapping: `InvalidInput` → `InvalidArgument`,
   `Unauthorized` → `Unauthenticated`, `NotImplemented` → `Unimplemented`,
   `Conflict` → `FailedPrecondition` (which is also what Go's server mapped
   it to), `PaymentRequired` → `FailedPrecondition` with the
   `PAYMENT_REQUIRED_MESSAGE` discriminator the frontend matches on, kept
   because the reason for the magic string (no structured detail in the
   status) still holds.
2. **Errors are typed enums, or a plain `Box<dyn Error + Send + Sync>`.**
   Go passes one `error` value everywhere and recovers its meaning with
   `errors.As`. The port lets the seam decide: where a consumer acts on the
   kind of failure the error is an enum (`InvalidName`, `InvalidLabel`,
   `DecodeError`, `RegistryError`; the effect framework's `SendError` and
   `DispatchError`), and where nobody inspects it (a reconciler's retry
   cause, an `AppError`'s source) it is `BoxError`, the standard library's
   own boxed error, which `?` converts any error, `String` or `&str` into.
   No `anyhow` and no `thiserror`: the `Display` and `Error` impls are
   written out. `AppError::code_of` still walks the `source()` chain for the
   first `AppError`, because a boundary error wrapped by another crate's
   error must keep its code.
3. **`Deadline` is a value with both clock readings.** Go's `requireProof`
   checks `time.Now()` against a `time.Time` twice (with and without the
   monotonic reading). The port stores both readings and `is_live` applies
   the same two-clock rule; a passed deadline cannot be extended, only
   replaced (`max`).
4. **`Ctx` is a value, not an interface.** Request id, deadline, typed values
   and cancellation in one cheap-to-clone struct; a child narrows its parent
   and cancels with it. Deadlines are not timers: the runtime that owns the
   request sleeps for `remaining()`.

## basable-publicid

5. **The registry is an explicit boot-time builder, not `init()`.** Go's
   `publicid` imports both type systems and builds its registry under a
   `sync.Once`, panicking on a collision at the first call. The port's
   `Registry::builder().register_all(..).build()` returns the collision as an
   error at boot (`RegistryError`); `encode`/`decode_typed` on an unregistered
   NAME still panic, as in Go, because that is a programmer error.
6. **The encoding is byte-identical.** `tests/go_fixture.rs` pins twenty ids
   the Go implementation produced, including the leading-zero-byte cases the
   format is lenient about on decode (`proj_abc` = `proj_0abc`).

## basable-db and basable-testkit

7. **One `app` login, `SET ROLE` per pool, not a login per nanoservice.**
   Plan B3 preferred per-role logins; the tenant template settled on the
   one login CNPG mints (`app`, `CREATEROLE`), a member of every
   `nano_<name>` role `WITH SET TRUE, INHERIT FALSE`. `NanoPool<N>` runs
   `SET ROLE` and `SET search_path` in `after_connect` and proves the switch
   on a direct connection first, so a missing role fails at boot with the
   database's error rather than as a pool timeout. `RESET ROLE` remains the
   one convention-enforced escape.
8. **The migrator owns the nanoservice schema; the role owns what is in it.**
   `CREATE SCHEMA nano_x AUTHORIZATION nano_x` cannot work: the migrator must
   create a type's envelope partition inside the schema (only the parent's
   owner may) and cannot `GRANT` on a schema it does not own without
   inheriting the role. So the migration creates the schema as `app`, grants
   `USAGE, CREATE` to the role, and the role creates and owns its tables;
   unswitched, `app` is refused on them like anyone else (pinned by
   `tests/ownership.rs`).
9. **Typed tables reference the partition, not the envelope parent.** Go's
   composite FK points at the parent, which needs `REFERENCES` on it. The
   port points it at `processing_object_<type>`: the same rows, and the role
   holds nothing on the parent at all, which is what makes every statement
   shape work on the partition alone (F.7, pinned).
10. **Role-switching migrations end with `RESET ROLE`.** dbmate records the
    version in the migration's own transaction, after the file's statements,
    so a file that leaves `SET LOCAL ROLE nano_x` in force makes dbmate write
    the ledger as a role that may not; the runner here does the same on
    purpose, so the tests fail where the Job would.
11. **The testkit uses `TEST_DATABASE_URL`, not testcontainers.** The tenant
    template's CI provides a Postgres service and its tests skip without the
    variable; the libs follow the same contract so one harness serves both.
    The harness creates the `app` login (CNPG's job in production) and a
    database per test, and applies the migrations as `app`.
12. **`CommitFaultProxy` is a TCP proxy, not a dial hook.** pgx let the Go
    testkit wrap the connection's dial function; sqlx has no such seam, so
    the proxy sits on a loopback port in front of Postgres. The two modes
    and the ReadyForQuery-bounded ack drop are the Go ones byte for byte.

## basable-processingobject (store half)

13. **Every framework statement targets the type's partition.** Go queries
    the envelope parent with `WHERE processing_object_type_key = $1`; the
    port queries `processing_object_<type>` in the nanoservice's schema
    (unqualified, through the pool whose `search_path` is pinned there), so
    the nanoservice role needs nothing on the parent and a cross-nanoservice
    write is a permission error. The registry read stays on
    `basable.processing_object_type`, which every role may `SELECT`.
14. **The adapter is a trait, the transaction a newtype.** Go's five
    function fields checked for nil at bind become trait methods
    (`-> impl Future + Send`, no boxing); `finalize_delete` keeps its
    default. `Tx` wraps the connection and implements only sqlx's
    `Executor`, so commit, rollback and savepoint are unreachable by type.
    Adapter methods return `sqlx::Error`, so completion can still classify
    a class-23 status violation.
15. **`Outcome` is an enum; the scheduling modifiers live on `Converged`.**
    Go's zero `Outcome{}` and a modifier chained onto the wrong decision
    were runtime contract violations completion turned into a loud retry.
    Here there is no zero value and `Schedule` is a field of `Converged`
    only, so both are unrepresentable; `retry` and `blocked` require a
    cause, so `last_error` is never empty.
16. **`update_spec`'s closure is synchronous and sees `&T`.** Go passed the
    status by value so writes to the copy were discarded; a shared
    reference says the same thing at the type level, and a synchronous
    `FnOnce` cannot do I/O under the envelope lock. Its error is
    `Error::Mutate`, nothing written.
17. **One `Error` enum, no sentinels.** `NotFound`, `Deleting`, `NameTaken`,
    `Invariant`, `Fenced`, `InvalidConfig`, `Mutate`, `Sql { op }` and
    `CommitUnknown { op }` replace `errors.Is` on wrapped sentinels; the
    commit-ambiguity Go reported as message text ("commit outcome
    unknown") is a variant a caller can match.
18. **`WorkerConfig` fields are unsigned and a zero means "default".** Go
    admitted negatives and rejected them in `validated`; here they do not
    exist. `Backoff::delay` reproduces Go's `int64` shift (`wrapping_shl`)
    and float arithmetic; 405 Go-generated tuples pin it.
19. **The conformance type is a nanoservice with migrations, not ad-hoc
    DDL.** Go's `ApplySchema` ran `CREATE TABLE IF NOT EXISTS` as the test
    login; under the ownership model the type needs its role, its schema
    and the partition hand-over, so the testkit ships two dbmate-format
    migrations applied through the ledger by the migrator.
20. **`WidgetSim` is an axum server behind a reqwest client.** The Go
    simulator was an `httptest.Server`; the HTTP shape is kept (rather than
    an in-process fake) so the effect-admission suites can put the ack-loss
    proxy between the caller and it.

## basable-processingobject (claims and completion)

21. **`complete(self, …)` consumes the claim.** Go closed the claim with a
    sticky flag and let a later `WriteStatus` or `Complete` return
    `ErrFenced`; here the claim is moved into `complete`, so a write after
    completion does not compile. The heartbeat half is a cloneable
    `LeaseHandle` sharing the token, proof and closed flag, which is what a
    worker keeps while the reconciler holds the claim — and what still
    fences (`Error::Fenced`) after completion, as Go's late call did.
22. **The verdict is `Result<Outcome<T>, BoxError>`, not an outcome and an
    error side by side.** Go's `Complete(out, attemptErr)` kept `out`'s
    status as the observation when `attemptErr` was set; an `Err` here
    carries no outcome, so it resolves to `Retry` with no status. The
    contract violations Go resolved at completion (a zero outcome, a
    modifier on the wrong decision) cannot be built (note 15); `Delete` on
    a non-deleting claim and `Blocked` on a deleting one still resolve to a
    loud `Retry`.
23. **`Completion` is an enum: `Committed { outcome, superseded, woken }`
    or `Unknown`.** Go's struct carried a zero `Outcome` next to an
    `OutcomeUnknown` flag; the ambiguity adoption has no outcome to
    misread.
24. **The ownership proof is a `Deadline`.** `require_proof` is
    `Deadline::is_live` (note 3): measured before the claim is sent,
    extended from the pre-send reading by heartbeat and `write_status`,
    never past the database lease.
25. **Savepoints are the framework's own statements.** `SAVEPOINT` /
    `RELEASE` / `ROLLBACK TO` run as simple queries on the transaction, as
    in Go, rather than sqlx's nested `begin()`; the adapter's `Tx` still
    cannot issue them (note 14).
26. **The completion retry shares its cause.** A retried completion reuses
    the reconciler's verdict, whose cause is an `Arc<dyn Error>` internally
    and a `BoxError` again on the way out; `T: Clone` is required of a
    status type for the same reason (Go copied values freely).
