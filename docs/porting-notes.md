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

## basable-processingobject (worker)

27. **The reconciler and the callback are traits with `impl Future`
    methods.** Go's `Reconciler` interface and `AfterComplete` func value
    become `Reconciler<S, T, A>` and `AfterComplete<S, T>`; the callback is
    implemented for any `Fn(&Ctx, Object, Completion) -> impl Future`
    closure and for `NoAfterComplete`, which replaces Go's nil. There is no
    boxing on the hot path.
28. **A panicking pass completes as a loud `Retry`, without a stack.** Each
    attempt is a task in a `JoinSet` (a panic there cannot take the loop
    down), and inside it the pass and the callback are polled under
    `catch_unwind`, so the claim outlives the panic and is completed with
    `reconciler panic: <message>` as the cause. Go attached
    `debug.Stack()`; the port records the payload message only — a
    backtrace is the panic hook's to print. This requires the default
    `unwind` panic strategy; under `panic = "abort"` the process ends
    first.
29. **The attempt deadline and cancellation are enforced by dropping the
    pass's future.** Go cancelled a context the reconciler was expected to
    honour; here the worker races the pass against the deadline and the
    context's cancellation and drops it when either fires, so a reconciler
    that never polls its context is still bounded. The resulting causes are
    `attempt timed out` and `attempt cancelled`, where Go recorded
    `context.DeadlineExceeded` / `context.Canceled` text.
30. **The heartbeat pump is a task on the `LeaseHandle`, aborted before
    completion.** Go cancelled the pump's context and joined it; the port
    aborts the task (dropping an in-flight heartbeat statement) and awaits
    the abort, for the same reason — a heartbeat stalled on a dead
    connection must not hold the completion.
31. **The wake listener was sqlx's `PgListener`; since 2026-10-08 there is
    none (note 81).** Through 0.1.0 it reconnected by itself, a `None` from
    `try_recv` marked a gap whose notifications were lost (the poll covered
    them, as it did Go's listener drop-out), the retry pacing was kept for
    connect and `LISTEN` failures, and the listener returned its connection
    with `UNLISTEN *` on drop, so `release_listen_conn` was never needed.
    The wake is in process now and holds no connection.
32. **The completion is bounded by a timeout, not a detached context.**
    `Claim::complete` takes no context; the worker wraps it in a 30 s
    `tokio::time::timeout` and, past it, drops the future — the pool rolls
    the transaction back — and leaves the claim to lease expiry, as Go did
    when its detached context expired.
33. **`run` returns `()` and `Replica::stop` is `async`.** Go's `Run`
    always returned nil after the drain and `Stop` returned that nil;
    neither carried information.
34. **`update_spec`'s status-copy proof is a type, not a test.** Go's
    `TestProcessingObjectStatusSightedCAS` scribbled on the status copy
    inside the closure to prove the write was discarded; here the closure
    sees `&Status` (note 16), so the port's `status_sighted_cas` asserts
    the rejection and the acceptance only.

## basable-externaleffect and basable-effecttest

35. **The strategy sum is an enum, and every "required unless" is a
    variant.** Go's sealed interface with four struct types and nil-checked
    function fields becomes `Strategy::{Idempotent, LookBeforeAct {
    lookup }, KeyedReplay { window, intent_age, provider_id, resolve },
    Declared { slot_identity, resolution: Resolve(f) | Hold }}`. A missing
    send, strategy, late-call policy, lookup, intent age, provider id,
    resolver or slot identity, and a resolver beside `Hold`, cannot be
    written; `Call::new` still checks the operation name, the timeout, the
    key rules, the window, `classify: None` iff `Hold`, and the
    irreversible restriction. The validation matrix test shrank by exactly
    those rows.
36. **Callbacks are boxed `Fn`s built by `*_fn` helpers.** `send`,
    `lookup`, `resolve`, `resolve_keyed` take the context and the args by
    value and return a boxed sendable future; `key`, `intent_age` and
    `provider_id` are boxed sync closures. The helpers (`send_fn(|ctx,
    args| async move { … })`) exist so closure signatures infer; the
    literal still reads like Go's struct.
37. **`Owner::ownership_deadline` returns `Option<Deadline>`, and a
    reference cannot be nil.** Go checked for a nil `Owner` and returned a
    loud wiring error; here `None` is "fenced" (a claim past its proof
    answers it), `Unfenced` is a unit struct passed as `&Unfenced`, and
    `check_ownership` returns the live `Deadline` the bound is computed
    from.
38. **The dispatch verdict is an enum.** `DispatchError::{OwnershipLost,
    ReplayWindowElapsed { age, window }, Ambiguous(e), Definitive(e)}`
    forces the match Go left to `IsAmbiguous` discipline; `Display` of the
    last two is the provider's text verbatim and `source()` keeps the
    provider error, as Go's `Error()`/`Unwrap` did. Observations
    (`lookup`, `resolve`, `resolve_keyed`) fail with `ResolveError::{
    OwnershipLost, Failed(e)}`.
39. **The transport floor is a marker, not a trait the transport
    implements.** Go's `ClassifyTransport` recognised `net.Error` and
    context errors; Rust has neither, so a provider client wraps its
    transport-shaped failures with `TransportError::wrap` at its boundary
    (the widget client: reqwest errors without a status), and
    `classify_transport` holds those, the `TimedOut` and `Cancelled`
    markers the bounded step produces, and any `io::Error` in the chain.
    The classifier signature loses the context argument: the bound
    produces the markers, so a cancelled dispatch is judged by the error
    it returns, not by a side channel.
40. **The step is bounded by dropping its future.** Go handed the send a
    context with a deadline and trusted the provider to honour it; here
    `dispatch` races the send against `tokio::time::timeout(min(
    call_timeout, remaining))` and the context's cancellation, drops it
    when either fires, and reports `Ambiguous(TimedOut | Cancelled)`. The
    context the send receives still carries the bound as its deadline.
41. **`declare` takes a `DateTime<Utc>`, which has no zero.** Go panicked
    on a zero clock; the type has no such value. The obligation that it be
    the DATABASE clock remains the caller's.
42. **`Resolution<R>` is an enum.** Go's struct carried a `Result` field
    meaningful only under `AttemptSucceeded`; here `Succeeded(R)` is the
    only variant holding one, and the other three carry their detail.
    `AttemptState` stays as the stored vocabulary (with `FromStr` for the
    status column).
43. **Metrics are tracing events.** Go's three labelled counters on the
    controller's `/metrics` become `tracing::debug!` events on the target
    `basable_externaleffect::metrics` with `operation` and the verdict as
    fields, one per dispatch, resolution and lookup; a metrics subscriber
    counts them. The libs have no metrics registry to increment.
44. **The audit is one test per adapter with a report, not one subtest
    per probe.** Go's `effecttest.Run` used `t.Run` per probe; Rust tests
    have no subtests, so `run` executes every applicable probe against a
    fresh harness, collects pass / loud skip / failure text per probe, and
    panics once with the whole report. `try_run` returns the report, which
    is how `tests/reference.rs` pins that a misclassified irreversible
    adapter fails exactly the classifier floor. The harness's shape checks
    (a missing `advance_intent`, `past_window`, `inject_ambiguity` for an
    irreversible adapter, `resolve_args` when `A != RA`) still panic
    immediately: a fixture bug is not a finding.
45. **Ack loss is injected on the wire.** Go wrapped the provider client's
    `http.RoundTripper`; reqwest has no such seam, so `AckLossProxy` is an
    HTTP/1.1 reverse proxy on a loopback port that forwards the next
    matching request with `Connection: close`, lets the upstream execute
    it, discards the answer, closes the client's connection, and refuses
    the next N requests unsent. The client sees a request error (a
    transport failure once the provider wraps it), never a timeout, for
    the same reason as Go's non-timeout `net.Error`: a timeout would be
    retried by clients that retry timeouts.
46. **The harness is built, not filled.** Go's `Harness` was a struct of
    optional func fields validated by `requireHarness`; here
    `Harness::new` takes the four required inputs and the optional ones
    are setters, and `landed_count`, `inject_ambiguity`,
    `inject_definitive` and `compensation_probe` are async, since a
    fixture that reads a database must await. `resolve_args` defaults to
    the identity when `A` and `RA` are one type (a `TypeId` comparison,
    as Go's `reflect.TypeFor` equality).
47. **`Claim` and `LeaseHandle` implement `Owner` explicitly.** Go's
    `*Claim` satisfied the `Owner` interface structurally with no import
    in either direction; Rust needs the impl written somewhere, so
    `basable-processingobject` depends on `basable-externaleffect` (still
    downward: the effect crate depends on core only) and implements the
    trait for both. A claim past its proof, or completed, answers `None`.

## basable-config

48. **The tenant's config schema is the platform's, verbatim.** The
    framework migration the scaffolder renders carries the monorepo's
    `create_config_framework` and `create_namespace_configuration` SQL in
    the `basable_config` schema: the temporal base table, the
    `versioning()` trigger (`SECURITY DEFINER`, its `search_path` pinned to
    `basable_config`), the type registry, and the namespace subtype. The
    `config_trigger_history_update` role and the controller `REVOKE`
    blocks are gone — the `app` login owns every config table, and the
    nanoservice roles get `SELECT` through their init migration's default
    privileges. A config type is one more subtype table pair in the shape
    of the `add-config-type` skill, rendered by the scaffolder.
49. **One binder per type, a trait instead of a registered value.** Go's
    `Binder` interface (`Type`, `New`, `Upsert`, `Delete`) registered in
    `init()` becomes `TypedBinder { type Msg; type_info; upsert; delete;
    read }` registered on `ConfigTypesBuilder`, whose `build()` refuses a
    duplicate id, name or prefix (two `init`s registering one name silently
    overwrote). `read` is the one addition: Go's per-type readers are
    hand-written projections; here the repository's `get` and `list` go
    through the binder, and the base identity travels beside the message
    as `Object<M>`. The namespace binder is built in.
50. **The message is the proto message, through serde.** buffa's `json`
    feature gives the generated types protobuf-JSON `Serialize` /
    `Deserialize`, so `ConfigMessage` is a blanket marker over `Serialize +
    DeserializeOwned`, and the header is read by the binder
    (`TypedBinder::header`, the two lines the scaffolder renders) — a
    generated message lives in the tenant's proto crate, where a
    nanoservice cannot implement a trait for it (the orphan rule; found by
    the Phase 10 example build). `ConfigHeader` here is the crate's own struct
    (a copy of the proto's four fields), so the crate carries no proto
    toolchain; `NamespaceConfiguration` is hand-written to the same wire
    shape. `protojson.Unmarshal` on the resolved bytes is
    `serde_json::from_value` on the resolved body.
51. **The seed format, the loader and the prune are Go's.** `@type`'s last
    segment names the type, `header.namespace` must be a
    `#{NamespaceConfiguration:<name>}` reference (a bare name is rejected),
    a run is self-contained (`validate_deps`), items apply in dependency
    order, every loader-managed object the run did not apply is pruned —
    namespaces last, a binder's `StillReferenced` refusal retried until a
    pass frees nothing — and a runtime object is adopted the first time the
    files declare it. `LoadResult` counts created, updated (every
    re-applied object, as Go's `Updated`) and deleted.
52. **Concurrent seed loads serialise on a table lock, not an advisory
    lock.** Go held `pg_advisory_lock` on a dedicated session around the
    load, with the unlock's own failure modes (a session lock survives the
    connection's return to the pool). The load is one transaction, so it
    takes `LOCK TABLE … IN SHARE ROW EXCLUSIVE MODE` inside it: released
    with the transaction, conflicts only with other loaders and writers,
    never with readers.
53. **References are found and replaced by walking the JSON values.** Go
    ran a regex over the raw bytes; here `#{…}` is matched inside every
    string value of the decoded item (keys included, as Go's regex would
    have matched them in text), and distinct references come out in the
    object's key order, which `serde_json` keeps sorted.
54. **What Go's package carried beyond the framework stays out.** The
    composed repository of eighteen platform types, the organisation slug
    allocation, the contact flows, the pricing cache and the
    database-free `SeedView` are the platform's, not a tenant's; the port
    is the framework: registry, loader, repository over any registered
    type.

## basable-messenger, basable-messenger-codegen and basable-messenger-gen

55. **Two Go generators, one Rust generator, two output crates.** Go's
    `interface-gen-v2` and `messenger-gen-v2` read one `routing.yaml`
    with the same templates duplicated; here `basable-messenger-codegen`
    parses once and emits `interfaces` (per nanoservice a router-generic
    `XHandler<R: XRoutes>` trait, `XSender<'a, R>` and the `XRoutes`
    bound) and `messenger` (the concrete router). The routing semantics
    are Go's untouched: typed response = strict 1:1, `response: error` =
    sequential fail-fast fan-out in declaration order, no response = void
    fan-out to 0..N handlers.
56. **Traits and static dispatch instead of interfaces and a struct of
    interfaces.** Go's messenger holds `geninterfaces.XInterface` fields
    and hands each sender a `Sendable` interface at construction
    (`SetSendable`). Here the router is one concrete type passed by shared
    reference into every handler (`s: XSender<'_, R>`), the handler is
    generic over `R: XRoutes`, and `XRoutes` is a bound made of one
    `Route<M, Resp, source::X, Ctx>` per declared send. A nanoservice
    sending an undeclared message has no `send_` method; who may send
    what is a trait bound, not a convention, and every call is a direct
    monomorphised call.
57. **Cycles are boxed, mechanically.** With static dispatch a route's
    future is part of its caller's future type, so a cycle in
    `routing.yaml` is an infinitely sized future — E0733, which Go's
    interface indirection never met. The generator computes a feedback
    vertex set of the route graph (depth-first in declaration order,
    boxing the route a back-edge points at unless the cycle already
    passes through a boxed route) and emits `boxed(async move { .. })` on
    exactly those routes, reported as `W_ROUTE_CYCLE` with the cycle
    named. To make the boxed hidden type nameable, `Route::route` ties the
    router and context borrows to one lifetime (`fn route<'a>(&'a self,
    ctx: &'a Ctx, msg: M) -> impl Future + Send + 'a`); an unboxed route
    is a plain `async fn`.
58. **The `Send` bound is the anti-deadlock rule.** Every generated handler
    method returns `impl Future + Send`, so a `std::sync::MutexGuard` held
    across a `send_*` is a compile error (E0277), not the runtime hang the
    design discussion feared; `tests/messenger/cyclic` pins it as a
    `compile_fail` doctest. Handlers take `&self`; the router is `Sync`,
    and the messenger crate asserts every component `Send + Sync` by name
    so the error lands on the component, not inside a route's future.
59. **Every nanoservice is a router field, sends-only ones included.** Go
    included only components that handle something (the api handler was
    wired through `SetSendable`). Here `AppMessenger::new` takes every
    nanoservice in `routing.yaml` order and exposes one accessor per
    component for the composition root (`main` leaks the router and hands
    `router.api()` to the Connect server; through 0.2.0 workers got their
    component the same way, and since note 83 the router hands the app
    every component's loops itself). Nanoservice crates depend on
    `interfaces` only, so they cannot name the router or reach another
    component; the accessors are the app's.
60. **Diagnostics carry a code and the YAML line; structure is a JSON
    Schema.** Go's `Validate` returned one `fmt.Errorf` at a time with an
    index (`component 2, sends[1]`). Here the file is read through
    yaml-rust2's event API into a line-aware tree, the structure is
    checked by a draft 2020-12 schema (`additionalProperties: false`,
    `version: 1`, the name pattern; `basable-messenger-gen schema` prints
    it for editors), and the semantic rules have stable codes
    (`E_DUP_COMPONENT`, `E_RESPONSE_MISMATCH`, …), every one at its line,
    all of them reported in one run. Two rules Go left to the compiler
    are explicit: `E_DUP_MESSAGE_IN_LIST` and `E_NOT_A_TYPE` (syn parses
    every type path; a message must be a plain path because its last
    segment names the methods). `E_RESPONSE_MISMATCH` subsumes Go's
    error-fan-out check: any two declarations of one message must agree.
    The corpus under `spec/routing/fixtures` is the contract with the
    monorepo's Go validator (still to be vendored there).
61. **Method names are the scaffolder's, not the Go generator's.** Go
    stripped the `Proto` suffix (`Send{{baseName}}`); the scaffold writes
    `handle_<snake message>` with its own `snake`, so `names.rs` is that
    function byte for byte: `GetProductRequest` →
    `handle_get_product_request`, `send_get_product_request`. The plan's
    examples (`handle_get_product`) were shorthand.
62. **A fan-out clones the message.** Go passed pointers to every handler;
    a Rust fan-out moves the message into its last handler and clones it
    for the others, so a fanned-out message must be `Clone` (the messages
    template derives it). A void fan-out with no handler consumes the
    message and returns.

## basable-pubsub and basable-app

63. **The bus is the Go bus, with `String` payloads and a listening
    signal.** `lib/pubsub`'s Bus, instance-id origin prefix, own-message
    dedup, `IncludeSelf` through the nil origin, the `MaxDataBytes` bound
    (8000 − 36 − 64) and the fixed reconnect delay are ported one to one.
    A `NOTIFY` payload is text, so `publish` takes `&str` and a `Message`
    carries a `String`. The nil-pool "stub mode" is gone: a bus needs a
    pool, and a test that wants no database does not build one. Added:
    `is_listening` / `listening()`, because a publish before the `LISTEN`
    is in place is silently lost and Go's tests worked around that with
    sleeps; and `subscribe` after `run` is a `SubscribeError`, not a
    panic. Reconnection is sqlx's `PgListener` (which re-issues the
    `LISTEN`s and returns its connection with `UNLISTEN *`), so the Go
    `db.ReleaseListenConn` dance has no counterpart; the `try_recv() ==
    None` signal is what fires the reconnect hooks.
64. **One wake listener per process (B8 deviation 5), superseded on
    2026-10-08 by none at all (note 81).** Go pinned one `LISTEN`
    connection per worker. Through 0.1.0 the library `Worker` did that on
    its own, and a worker registered with the app took a
    `WakeSubscription` from the app's `WakeBus` instead
    (`Worker::with_wake`): one listen connection per process on
    `processing_object_wake`, fanning each payload to the workers of the
    type it named. `WakeBus`, `WakeSubscription` and `with_wake` are gone;
    a worker registered with the app is woken by its store like any other.
65. **Boot is a bounded wait, then a verdict.** Go's `main` panics on the
    first failed connect and never checks the ledger. Here
    `App::new(cfg).expect_migrations(v).connect()` retries every second
    within `DATABASE_BOOT_WAIT_SECS` (the database may be starting, the
    dbmate Job may still be running), each attempt bounded by the
    remaining wait (sqlx would otherwise retry a refused connection for
    its whole acquire timeout), and past the wait the last error is the
    boot error: `Error::Connect` or `Error::Migrations(Missing(..))`
    naming the versions, the template's exit 3.
66. **The connection budget is checked at boot, per pool.** Every
    `App::pool::<N>()` adds `DATABASE_POOL_MAX_CONNECTIONS` to a running
    total that starts at the framework pool's three (four until the wake
    listener went, note 81); the pool that would pass
    `DATABASE_CONNECTION_BUDGET` (default 100, the template's
    `max_connections`) is `Error::ConnectionBudget` before it opens. Go
    sized pools by convention.
67. **Workers and tickers are joined by name.** Every loop is registered
    under `<component>/<worker type or ticker>`: since 2026-10-08 through
    `Serve::components`, the component name coming from `routing.yaml`
    (note 83; through 0.2.0 `Serve::worker` and `Serve::ticker` took the
    prefix from `main.rs`), and a name registered twice is
    `Error::DuplicateLoop` at `start`. Shutdown flips readiness off,
    cancels the root `Ctx`, joins every loop within
    `SHUTDOWN_GRACE_SECS`, and returns `Error::Stuck(names)` for what did
    not drain (abandoned to lease expiry), which `tests/app.rs` pins with a
    tick that ignores its context. An in-flight attempt completes as a
    retry with `last_error = "attempt cancelled"`, which `tests/app.rs`
    pins over the conformance type. A ticker is the
    `gitoperator/worker.go` shape (immediate first tick, then per
    interval, a failing or panicking tick logged and retried) and its
    tick borrows a child context cancelled with the app.
68. **Readiness is three checks, not a flag.** `/readyz` answers 200 once
    wiring is done, the framework pool answers `SELECT 1` within two
    seconds, and every registered loop is still running; the body says
    which check failed. `/healthz` answers while the process lives. The
    template's Deployment probes both.

## basable-connect, basable-auth and basable-protoc-gen-buffa

69. **The Connect boundary is connectrpc's; the glue is thin.** Go's
    `lib/server` framework (gRPC + grpc-gateway, middleware chain, public-id
    decode) has no port: a tenant's API is the generated `connectrpc`
    service traits, mounted through `ConnectRouter` (a mutating
    `add_service` over `connectrpc::Router`, then `into_axum` as the app
    router's fallback service). What `basable-connect` adds is the
    `AppError` ↔ `ConnectError` mapping (the sixteen codes one to one,
    `connect_code` / `app_code`; a code this crate does not know maps to
    `Unknown`, as the Connect protocol says of an unlisted one), the
    request-id layer (`x-request-id` in and out, a fresh UUID otherwise),
    and `request_ctx`: the `Ctx` a handler derives from its
    `RequestContext` — request id, the Connect deadline as the context's
    timeout, the validated identity.
70. **Auth is a layer on the axum router, answering in Connect's error
    shape.** Go's `AuthMiddleware` had three shapes (gRPC unary and stream
    interceptors and an HTTP middleware) reading the cookie from gRPC
    metadata or the header. Here there is one path: `Validator::apply`
    wraps the router; a request off the public paths and prefixes must
    carry a `Cookie` Kratos accepts at `/sessions/whoami`, the identity is
    put in the request extensions (from where `request_ctx` puts it on
    `Ctx`, read through `AuthCtx::user_id`), and the hook fires. A refusal
    is `401` with `{"code":"unauthenticated","message":"invalid session"}`
    (no detail of Kratos's answer leaks); Kratos unreachable, timing out or
    answering an unexpected status is `503 unavailable`, never a silent
    allow. Added: a bounded negative cache (`NEGATIVE_WINDOW`, five
    seconds) so a rejected cookie is not re-asked of Kratos per request.
    The `UserConfigurationID` resolution of Go's `context.go` is the
    platform's, not a tenant's.
71. **The test bypass is a feature, not a runtime flag.** Go's
    `WithTestBypass()` was a method production wiring simply never called
    (and `UserIDFromContext` fell back to `"test-user"` by sniffing the call
    stack). `Validator::bypassed_for_tests(identity)` exists only under the
    `test-bypass` cargo feature, which the testkit enables; a binary that
    does not carry the feature cannot name it. Under crate_universe the
    feature set of a crate is resolved for the whole workspace, so a tenant
    whose testkit is a dev-dependency still compiles the constructor in;
    what remains mechanical is that it is a distinct, grep-able name the
    scaffold's `main.rs` never writes.
72. **The protobuf codegen is a build step over two plugins, one of them
    ours.** F.2 as decided: one genrule per proto package
    (`tools/proto.bzl`) runs the protobuf module's prebuilt protoc with
    `protoc-gen-connect-rust` (from `connectrpc-codegen`, which has a
    library target) and `basable-protoc-gen-buffa`: `buffa-codegen` behind
    the plugin protocol, because upstream's `protoc-gen-buffa` is a
    binary-only crate cargo cannot list as a dependency and crate_universe
    therefore cannot build as a `gen_binaries` tool. Both run in
    `file_per_package` mode, so a package is exactly one
    `<dotted.package>.rs` per plugin (no packaging plugin, no per-file
    stitchers to declare as genrule outputs); the crate mounts them with
    `include!` from a hand-written module tree, and the generated files
    sit in `srcs` so rules_rust symlinks the hand-written sources beside
    them. Options travel through `--<plugin>_opt` (an option value with
    `::` would split the combined `<opts>:<dir>` form). The crate holding
    generated messages depends on `serde` itself: buffa's JSON impls
    derive from it.
73. **Handlers return the owned message.** The generated trait's return
    type is `ServiceResult<impl Encodable<Out> + Send + use<'a, Self>>`;
    a handler answering `ServiceResult<Out>` with `Response::new(msg)` is
    the refinement `refining_impl_trait` names, allowed on the impl. A
    server stream is `Response::stream_ok(futures::stream::iter(..))`.

## examples/orderly (Phase 10)

The example is not a port: it is the monorepo scaffolder's output for the
reference manifest, committed so that the crates as built and the templates
as written are checked against each other by a build. What the first build
of it changed, in the crates and in the templates:

74. **The example is rendered, not maintained.** The plan's Phase 10 had
    the example as the template SOURCE the scaffolder would vendor; it is
    the other way round: the templates live in `lib/scaffold` and the
    example is `tools/render-orderly.sh`'s output (a fresh render, rsync
    with delete so a file the templates stop producing goes, then the
    local-crates patch and a repin). A clean `git status` after the render
    is the contract; the CI's `orderly` job builds the committed tree
    exactly as a tenant's CI would (`--config=ci`, no repin, its own
    caches). Templates are never fixed in the rendered tree.
75. **The crates come in through a copy under `tools/`, with an explicit
    workspace root.** The example's `[patch.crates-io]` names the crates by
    path (and keeps doing so after the release, see the end of this note). Three things about
    crate_universe and cargo decide the shape: the splicer copies the root
    manifest into a temp dir and links the module root's entries beside
    it, so the path must lie inside the module; the splicer skips a
    top-level name `.bazelignore` lists, so the copy is nested
    (`tools/basable-crates`, ignored by that nested path, which Bazel
    honours and the splicer does not see); and crate_universe writes its
    generated `BUILD.bazel` into every path dependency's directory, which
    through a link would clobber the crates' hand-written BUILD files —
    hence a git-ignored copy (`tools/sync-orderly-crates.sh`), not a link.
    Cargo then resolves a path dependency under an already-loaded
    workspace root to THAT workspace (the example's, once the tree
    resolver adds its fake proc-macro root package even `exclude` does not
    stop it), so every crate's `[package]` names its root explicitly:
    `workspace = "../.."`. Harmless in this repository, decisive in the
    copy. One more consequence: crate_universe records a patched path
    dependency by ABSOLUTE path in `Cargo.Bazel.lock`, so the example's
    Bazel lockfile is not portable and is not committed — the CI's
    `orderly` job repins (`CARGO_BAZEL_REPIN=1`), the one way it differs
    from a tenant's CI. The crates are published since 0.1.0 and the patch
    stays anyway: the example's job is to build the working tree against
    the templates, which the published release cannot do.
76. **A bin-only crate cannot be a dependency.** cargo drops a dependency
    on a crate without a library target ("ignoring invalid dependency …
    missing a lib target"), so the lock never carried
    `basable-messenger-gen` and crate_universe could not build it as a
    `gen_binaries` tool for a tenant. The crate now has a library target
    (a re-export of the codegen crate) beside the binary, and a tenant's
    `crates/messenger` names it as a dev-dependency — the same shape
    `basable-protoc-gen-buffa` and `connectrpc-codegen` already had
    through `crates/proto`.
77. **The header is the binder's, not the message's.** `ConfigMessage`
    asked the message for its header; a buffa-generated message lives in
    the tenant's `proto` crate, where a nanoservice cannot implement a
    foreign trait for a foreign type (E0117, the orphan rule). The trait
    is now a blanket marker over `Serialize + DeserializeOwned`, and
    `TypedBinder::header(&self, msg)` reads the header (the repository
    reaches it through the erased binder's `decode_header`). buffa's
    embedded message field is `MessageField<T, Inline<T>>`, read with
    `as_option()`.
78. **An RPC's messages carry buf's standard names.** The tenant's
    `//proto:lint` gate is buf's STANDARD set, whose
    `RPC_RESPONSE_STANDARD_NAME` refused the reference manifest's
    `UpsertProduct → Product`. The manifest validator now requires
    `<Method>Request` / `<Method>Response` (or the service-prefixed form),
    and the api crate's messenger send takes its response from the
    nanoservice that `handles` the request message rather than from the
    proto response, which is the wire shape only. `buf.yaml` moved into
    `proto/` (a v2 module config; the root form with `modules: [proto]`
    made the lint plugin look for a module at `.`).
79. **Small template facts the build taught.** `sqlx`'s `FromRow` derive
    needs the `macros` feature (the template's baseline lists it; the
    crates never derive it); a message may share its name with the
    nanoservice that handles it (`order` answering `Order`), so a handler
    impl names messages `messages::X` instead of glob-importing them;
    `.bind(..)` takes a `Copy` column by value (clippy's
    `needless_borrows_for_generic_args`), which the templates decide by
    field type; a variable nothing reads is not declared (the Kratos admin
    URL left the config until an identity lookup needs it).

## The release (Phase 11)

80. **`cargo package --workspace` is the publish gate, because Bazel hides
    feature gaps.** crate_universe resolves every crate's features once for
    the whole workspace, so a crate using `Uuid::new_v4()` built under Bazel
    without declaring uuid's `v4` feature (another crate had it) and would
    have failed for every cargo consumer. The release dry run (every crate
    packaged and built from its packaged form, in dependency order, with
    the Bazel-pinned cargo 1.98) found it in `basable-processingobject`;
    the feature is declared now. The same run flagged a yanked transitive
    crate (`yoke-derive 0.8.3`), moved to 0.8.4 in both lockfiles. Run the
    dry run before every tag; the workflow's `cargo check` is the cheaper
    echo of it. 0.1.0 went up on 2026-10-05 from a logged-in machine with
    `cargo publish --workspace --locked`; crates.io's new-crate rate limit
    (five in a burst, then one every ten minutes) spread seventeen first
    publishes over two hours, so a first publish is a loop that waits out
    the `try again after` it is told, and the workflow holds no token.

## After 0.1.0

81. **The wake is in process, and the database wake is gone
    (2026-10-08).** Through 0.1.0 every write that made an object due ran
    `pg_notify` on `processing_object_wake` inside its transaction, and a
    worker heard it over a `LISTEN`: its own `PgListener` (note 31) or the
    app's `WakeBus` (note 64). Go and the port dropped it together. Go's
    per-worker `LISTEN` connections came to 12 per replica out of a
    30-connection pool, and they bought little: every replica runs every
    worker and every write to a type happens in a process running that
    type's worker, so a wake that crosses processes only helps when the
    local worker is full, and Go raises parallelism for that instead. Now
    `TypedStore`'s shared inner holds the wake (a `Notify`: a stored
    permit, so wakes coalesce) of every worker running on it, registered
    for the whole of `Worker::run` by an RAII guard. `create` (when it
    inserted, not when it adopted), `update_spec`, `mark_deleted` (when it
    stamped, not the idempotent repeat) and `nudge` signal them after the
    commit, and `Claim::complete` does the same for a completion it left
    due now (`requeue_now`, superseded or woken, the finalizer-failed retry
    under a stale fence): after the commit and before it returns, so before
    the worker's `after_complete` and identically for a manual drive. A
    completion due again after `d` with `0 < d <= poll_interval` arms a
    one-shot timer that signals after `d`, a detached task holding no
    parallelism slot and harmless once the worker stopped: before it,
    `after(5s)` under a 30 s poll ran up to 30 s late. A longer delay and a
    parked, deleted or unknown completion arm nothing. A store bound
    separately for the same type shares no wakes and models another
    process; the poll stays the correctness path for it and for every
    other replica. The resolved schedule travels from the settle statement
    to the wake on the completion itself, as
    `Completion::Committed { next_pass: Option<Duration>, .. }` — public,
    because an enum variant's fields cannot be private; Go keeps the same
    two values as unexported fields of its `Completion` struct. The
    decision is `Wake::for_next_pass`. Removed, a break against the
    published 0.1.0:
    `basable_processingobject::{WAKE_CHANNEL, WakeSubscription}`,
    `Worker::with_wake`, `basable_app::WakeBus`, `App::wake_bus`,
    `Running::wake_bus`; and `APP_POOL_CONNECTIONS` is 3, not 4, because
    one of the four was the wake listener. Added: the `next_pass` field on
    `Completion::Committed`, which a hand-built `Committed` must now name.

## 0.2.0

82. **0.2.0 is note 81 (2026-10-08).** The minor bump is the break note 81
    lists; nothing else changed for a consumer. The release ran the same
    way as 0.1.0: `cargo package --workspace --locked` with the
    Bazel-pinned cargo 1.98.1 (all seventeen crates built from their
    tarballs), then `cargo publish --workspace --locked` from a logged-in
    machine, then the `v0.2.0` tag. The monorepo's `lib/scaffold/pins.go`
    moved `FrameworkVersion`, `TemplateTag` and `DirectiveVersion` to it in
    the same piece of work (the Directive's invariant 2 changed wording),
    and `examples/orderly` is rendered from that.

## After 0.2.0

83. **Loops belong to the component, and the messenger lists the
    components (2026-10-08).** Go's `main.go` hands one canonical
    component list, the one wired into the messenger, to
    `server.CollectWorkers`, which keeps the components implementing
    `Worker` (a `Run` loop); each builds and joins its own workers and
    tickers, and `main` never names a component's types. Through 0.2.0 a
    tenant's `main.rs` registered every loop itself
    (`.worker("order_order", order::types::order::worker(router,
    router.order())).ticker("order", order::worker::tickers(..))`), so it
    knew every component's processing-object types, and a new loop was an
    edit in two crates. Now `basable_app::Component<R>` is the `Run`:
    `fn loops(&'static self, router: &'static R) -> Loops`, with `Loops`
    collecting named, type-erased workers (named by their type) and
    tickers (by their name). Rust cannot ask at run time whether a value
    implements a trait, so there is no subset to collect: every component
    implements `Component`, a plain executor takes the default (no loops),
    and one that does not is a build error in the messenger crate naming
    it (E0277, pinned in `tests/messenger/cyclic`). The list is the
    generated messenger, which already held every nanoservice as a field
    (note 59): `basable-messenger-codegen` emits `impl
    basable_app::Components for AppMessenger`, one entry per component in
    `routing.yaml` order, sends-only ones included (one may own tickers),
    each named as there. A loop's name `<component>/<loop>` therefore
    comes from `routing.yaml`, not from a string in `main.rs` (the
    reference project's `order/order` and `order/shipment` were
    `order_order/order` and `order_shipment/shipment` under 0.2.0), and
    `main.rs` calls `.components(router)` once. The generator still emits
    only text; the generated crate depends on basable-app.
    `Serve::start` refuses a name registered twice (the components
    registered twice, or two loops of one name in one component) with
    `Error::DuplicateLoop(name)` before it binds or spawns anything;
    readiness and the shutdown drain are note 67's, unchanged. Removed, a
    break against the published 0.2.0: `Serve::worker` and
    `Serve::ticker`, so a loop has one way in. Added: `Component`,
    `Components`, `Loops`, `Serve::components`, and the
    `Error::DuplicateLoop` variant, which an exhaustive match on
    `basable_app::Error` must now name.

## 0.3.0

84. **0.3.0 is note 83 (2026-10-08).** The minor bump is the break note 83
    lists: `Serve::worker` and `Serve::ticker` are gone, a component lists
    its loops in `Component::loops`, and the generated messenger hands
    them all to `Serve::components`. Released as 0.2.0 was (note 82): the
    package dry run with the Bazel-pinned cargo 1.98.1, `cargo publish
    --workspace --locked`, then the `v0.3.0` tag. The monorepo's scaffold
    renders each nanoservice's `impl Component` in its `lib.rs` (every
    type's worker and every schedule's ticker, or the empty default), an
    empty one for the API, and `.components(router)` in `main.rs`; its
    drift check names a declared type or schedule missing from `loops()`,
    which compiles and would never run. `FrameworkVersion` and
    `TemplateTag` moved to 0.3.0; the Directive did not change, so
    `DirectiveVersion` stays 0.2.0.
