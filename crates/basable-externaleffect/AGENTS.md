# basable-externaleffect — building blocks for external effect calls

The port of the monorepo's `golang/controller/lib/externaleffect` and its
`effecttest` subpackage (here the sibling crate `basable-effecttest`). It
makes the six-clause effect admission contract mechanical: every external
call a component makes is wrapped in exactly one `Adapter<A, RA, R>` value
that names, in code, what makes a retry of that call safe, and
`basable-effecttest` proves that evidence against the component's own
simulator. The Go package's `CLAUDE.md` is the specification; this file
says what is the same and what the type system changed
(`docs/porting-notes.md` 35–43 has the list).

Three deliberate boundaries, decided up front (do not re-litigate):

- **No runtime.** No tasks, no loops, no control-flow inversion.
  Reconcilers keep their explicit phase matches and resolve-vs-send
  branching; the crate supplies one validated handle (`Call`) per external
  call and small helpers invoked in sequence. The only tokio it uses is the
  timeout that bounds one step.
- **No registry.** There is no cross-component enumeration of adapters.
  The audit index is each component's own test file calling
  `basable_effecttest::run` (or `audit!`) once per adapter.
- **No state.** The crate owns no tables, executes no SQL, holds no pool,
  and depends on `basable-core` only. Claim authority arrives through the
  `Owner` trait (`ownership_deadline() -> Option<Deadline>`, which a
  processing-object `Claim` implements), and the declared-slot marker is a
  plain value type the component stores in its own status columns and
  writes through `Claim::write_status`.

## The strategy sum

`Strategy<A, RA, R>` is a closed enum — a new safety shape is a new
admission argument, made here where its validation rules and probes live:

| Variant | Pre-send requirement | Re-entry resolution |
|---|---|---|
| `Idempotent` | nothing — the receiver converges/dedupes | re-send is the resolution |
| `LookBeforeAct { lookup }` | `lookup` returned verified absence | look again; found ⇒ adopt, never re-send |
| `KeyedReplay { window, intent_age, provider_id, resolve }` | durable key committed (caller's row) | same-key replay within `window` — `dispatch` refuses an intent whose age is at or past it (`DispatchError::ReplayWindowElapsed`: hold, never re-POST); once the provider id is persisted, `resolve_keyed` forever |
| `Declared { slot_identity, resolution: Resolve(..) \| Hold }` | slot written via `Claim::write_status` first | the resolver, or `Hold` forever when no receiver postcondition exists |

`A` is the dispatch payload, `RA` the resolve identity; name one type twice
when they coincide. The remaining clauses are plain fields: `key`,
`late_call` (`Convergent` / `KeyScoped` / `Compensated`), `irreversible`
(restricts strategies to `KeyedReplay` and `Declared`, and makes the
ack-loss probes mandatory), `call_timeout` (tightened at dispatch to the
remaining ownership deadline).

## The helpers

`Call::new` validates fail-fast (`InvalidAdapter`, the first violation
named). On the handle:

- `dispatch(ctx, proof, args)` — `check_ownership` → the step bounded by
  min(`call_timeout`, remaining deadline) and the context's cancellation →
  send → classify. `DispatchError::{OwnershipLost, ReplayWindowElapsed,
  Ambiguous(e), Definitive(e)}`: the match Go left to `IsAmbiguous`
  discipline is forced by the type. `Display` of the last two is the
  provider's text verbatim; `source()` keeps the provider error.
- `declare(args, now)` — mints the slot (`Declared`-only; `now` MUST be the
  database clock). The reconciler places it on its WORKING status and
  writes it through the claim itself.
- `resolve(ctx, proof, args, slot)` — settles a slot found on claim, never
  sends; `Hold` answers `Unknown` with zero I/O.
- `lookup` / `resolve_keyed` / `provider_id` — the LookBeforeAct gate and
  the KeyedReplay resolve-by-persisted-id path. `ResolveError::{
  OwnershipLost, Failed(e)}`.
- `resolve_by_lookup` adapts an exact never-reused-identity lookup into a
  `Declared` resolver (found ⇒ `Succeeded`, verified absent ⇒
  `Superseded`, error ⇒ error).

**Classifiers.** Rust has no `net.Error`, so the transport floor is a
marker: a provider client wraps its transport-shaped failures with
`TransportError::wrap` at its boundary (reqwest's connect, timeout, request
and body errors), and `Classifier::transport()` holds those plus the
`TimedOut` / `Cancelled` markers the bounded step produces and any
`io::Error` in the chain; everything else is the receiver's answer. That
default is WRONG for an irreversible effect: those use
`Classifier::fail_closed_on_definitive()` — definitive ONLY on an error the
provider wrapped with `definitive()` as proof of non-execution (a parsed
4xx, a pre-flight failure), ambiguous for everything else — or
`Classifier::fail_closed(pred)` to refine the predicate. `Declared { Hold }`
adapters have no classifier at all (`classify: None`).

Strategy-mismatched helper calls panic: wrong wiring is a construction bug,
not data. Call sites that hold no claim (request-path handlers) pass
`&Unfenced` — explicit and greppable.

## basable-effecttest

`run(factory)` (or `audit!(name, factory)`) audits ONE adapter, rebuilding
the `Harness` per probe through the factory so state never leaks. The
harness is built with `Harness::new(call, args, landed_count,
inject_definitive)` plus the optional `advance_intent`, `past_window`,
`inject_ambiguity`, `compensation_probe`, `resolve_args` (defaulted to the
identity when `A == RA`). Universal probes: identity determinism, the
ownership fence, the classifier floor, definitive honesty. Per strategy:
send-twice-lands-once and the ack-loss retry; look-then-adopt; same-key
replay, resolve by id, the past-window refusal; slot shape, resolver never
sends, landed-never-judged-resend-safe (clean and ack-lost). Per late-call
policy: a `KeyScoped` advance changes the key; a `Convergent` stale
dispatch lands nothing new; a `Compensated` adapter's probe runs. Ack-loss
probes skip LOUDLY without `inject_ambiguity` and are REQUIRED for
irreversible adapters. The report lists every probe; `try_run` returns it
without panicking, for a test that pins the kit itself.

`AckLossProxy` injects real ack loss on the wire: an HTTP/1.1 proxy that
forwards the next matching request, lets the upstream execute it, drops
the answer and closes the client's connection, then refuses the next N
requests unsent (the provider's in-line re-read count). The port of Go's
`AckLossTransport`, which wrapped the client's round-tripper — reqwest has
no such seam.

`tests/reference.rs` audits three adapters over `WidgetSim` — the widget
upsert (`Idempotent` + `Convergent`), the order (`KeyedReplay`,
irreversible, with the replay window) and the declared order (`Declared {
Keyed }`, irreversible, resolved by the receipt under the key, with
`RA = String`) — and pins that a misclassified irreversible adapter fails
its audit and that a reversible adapter without injection skips loudly.
They are what a component's own audit file looks like.

## File map

| File | Responsibility |
|---|---|
| `src/verdict.rs` | `Verdict`, `Classifier` (`transport`, `fail_closed`, `fail_closed_on_definitive`, `new`), `TransportError`, `DefinitiveError`/`definitive`/`is_definitive`, `TimedOut`, `Cancelled`, `is_transport` |
| `src/owner.rs` | `Owner`, `Unfenced`, `OwnershipLost`, `check_ownership` |
| `src/slot.rs` | `EffectSlot`, `AttemptState` (the stored vocabulary), `Resolution<R>` (the typed verdict) |
| `src/adapter.rs` | `Strategy`, `DeclaredResolution`, `LateCall`, `SlotIdentity`, `Adapter`, the boxed fn aliases and their `*_fn` constructors, `Call::new` validation, `resolve_by_lookup` |
| `src/call.rs` | `Call`: `declare`/`dispatch`/`resolve`/`lookup`/`resolve_keyed`/`provider_id` + accessors, `DispatchError`, `ResolveError`, the verdict tracing events (`METRICS_TARGET`) |
| `../basable-effecttest/src/harness.rs` | `Harness`, `FixedOwner`, `live_owner`, `expired_owner` |
| `../basable-effecttest/src/probes.rs` | `run`, `try_run`, `ProbeReport`, the probes |
| `../basable-effecttest/src/proxy.rs` | `AckLossProxy`, `RequestHead` |
