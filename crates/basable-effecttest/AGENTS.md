# basable-effecttest — the per-adapter audit

The port of the monorepo's `golang/controller/lib/externaleffect/effecttest`:
each component's own test file builds a `Harness` over its simulator or
fake for ONE `basable_externaleffect::Call` and runs `run` (or the `audit!`
macro), and the suite mechanically proves the admission evidence the
adapter declares. The Go package's `CLAUDE.md` (its `effecttest` section)
is the specification; `docs/porting-notes.md` 44–46 say what the port
changed, and the sibling `basable-externaleffect/AGENTS.md` has the
strategy sum the probes are written against. The Directive (`docs/DIRECTIVE.md` in every tenant repository, `golang/controller/lib/scaffold/directive.md` in the monorepo) is the contract this crate serves.
Its section 6, external-effect admission, is what an audit proves for one
call.

There is deliberately no registry to enumerate: the component's test file
calling `run` once per adapter IS the audit index. The scaffolder renders
that file (`effects_audit.rs`) for every nanoservice with external calls,
one `audit!` per declared call over the nanoservice's own simulator.

## The harness

`Harness::new(call, args, landed_count, inject_definitive)` is built, not
filled (note 46): the `Call` under audit, a fresh argument value per probe,
a closure that counts what has landed at the provider, and a closure that
makes the provider answer definitively. Builders refine it:

- `advance_intent(f)` — a later version of the same intent (a `KeyScoped`
  late call must change the key);
- `past_window(f)` — the same intent aged past the replay window
  (`KeyedReplay` only);
- `inject_ambiguity(f)` — drops the next answer on the wire; REQUIRED for
  an irreversible adapter, the ack-loss probes skip loudly without it;
- `compensation_probe(f)` — the `Compensated` late-call policy's own check;
- `resolve_args(f)` — `A -> RA` when they differ (defaulted to the identity
  otherwise).

`FixedOwner(Deadline)` with `live_owner()` / `expired_owner()` are the
owner doubles the probes dispatch under: the fence probe uses the expired
one and expects `DispatchError::OwnershipLost` with nothing sent.

## The probes

`run(factory)` rebuilds the harness through the factory for EVERY probe, so
state never leaks between them; `try_run` returns the `ProbeReport`
(`failures()`, `skipped()`, `is_ok()`) without panicking, for a test that
pins the kit itself. One test per adapter with a report, not one subtest
per probe (note 44). Universal probes: identity determinism (equal args,
equal key), the ownership fence, the classifier floor (a transport failure
is ambiguous, never definitive), definitive honesty (a definitive answer
landed nothing). Per strategy:

| Strategy | Probes |
|---|---|
| `Idempotent` | send twice lands once; the ack-loss retry lands once |
| `LookBeforeAct` | look, then adopt what is found, never re-send |
| `KeyedReplay` | same-key replay lands once; resolve by the persisted id; the past-window dispatch is refused unsent |
| `Declared` | the slot shape; the resolver never sends; landed is never judged resend-safe, clean and ack-lost |

Per late-call policy: a `KeyScoped` advance changes the key; a
`Convergent` stale dispatch lands nothing new; a `Compensated` adapter's
probe runs. Each probe answers with a pass, a loud skip or the failure
text.

## Real ack loss

`AckLossProxy::start(upstream_url)` is an HTTP/1.1 proxy that forwards the
next matching request (`arm(..)` / `arm_next()`), lets the upstream
execute it, drops the answer and closes the client's connection, then
refuses the next N requests unsent — the provider's in-line re-read count.
Ack loss is injected on the wire (note 45) because reqwest has no
round-tripper seam like the one Go's `AckLossTransport` wrapped. The
component's client points at `proxy.url()` instead of the simulator.

## The reference audits

`tests/reference.rs` runs three adapters over the conformance `WidgetSim`
(from `basable-processingobject-testkit`) through the real `WidgetClient`
and the proxy, one per strategy family: the widget upsert (`Idempotent` +
`Convergent`), the order (`KeyedReplay`, irreversible, with the replay
window) and the declared order (`Declared`, irreversible, resolved by the
receipt under the key, `RA = String`). Two more tests pin the kit: a
misclassified irreversible adapter fails its audit, and a reversible
adapter without injection skips the ack-loss probe loudly. They are what a
component's own audit file looks like, and they need no database: the
simulator and the proxy are loopback servers.

## File map

| File | Responsibility |
|---|---|
| `src/lib.rs` | The crate doc, `audit!`, re-exports |
| `src/harness.rs` | `Harness` and its builders, `FixedOwner`, `live_owner`, `expired_owner` |
| `src/probes.rs` | `run`, `try_run`, `ProbeReport`, `ProbeOutcome`, the probe suites |
| `src/proxy.rs` | `AckLossProxy`, `RequestHead` |
| `tests/reference.rs` | The three reference audits and the two kit pins |
