# basable-externaleffect and basable-effecttest (crates 0.8.0)

Every call that leaves the application's state domain is one
`Adapter<Args, ResolveArgs, Result>` held as a `Call`, whose `strategy`
literal names what makes a retry safe (the Directive §6):

```rust
pub enum Strategy<A, RA, R> {
    Idempotent,
    LookBeforeAct { lookup },
    KeyedReplay { window, intent_age, provider_id, resolve },
    Declared { slot_identity, resolution: DeclaredResolution::{Resolve(..) | Hold} },
}
pub struct Adapter<A, RA, R> {
    pub operation: String,
    pub key: Option<KeyFn<A>>,            // deterministic identity, never a retry count
    pub send: SendFn<A, R>,               // the ONE dispatch path: Result<R, BoxError>
    pub classify: Option<Classifier>,     // judges a send error; None only under Declared { Hold }
    pub call_timeout: Duration,
    pub irreversible: bool,               // ⇒ KeyedReplay | Declared
    pub late_call: LateCall,              // Convergent | KeyScoped | Compensated
    pub strategy: Strategy<A, RA, R>,
}
impl Call { new(adapter) -> Result<Call, InvalidAdapter>; dispatch(&self, ctx, proof: &dyn Owner, args) -> Result<R, DispatchError>; declare; resolve; lookup; resolve_keyed }
pub enum DispatchError { OwnershipLost, ReplayWindowElapsed { operation, age, window }, Ambiguous(BoxError), Definitive(BoxError) }
```

`Call::new(adapter)` validates every rule (irreversible requires durable
intent; `KeyedReplay` and `Declared{Keyed}` need `key`).
A reconciler dispatches with the `Claim` as owner; a request-path handler
dispatches `Unfenced`. Match `DispatchError`: `OwnershipLost` ⇒ nothing sent,
`Retry`; `Ambiguous` ⇒ may have landed, `Retry` and re-drive; `Definitive`
⇒ the receiver's answer.

The provider marks what its own failure means at its boundary, and the
adapter's classifier reads the mark:

| The provider returns | for | `Classifier::transport()` (reversible) | `Classifier::fail_closed_on_definitive()` (irreversible) |
|---|---|---|---|
| `definitive(err)` | a parsed 4xx: the receiver's answer, proof that nothing executed | `Definitive` | `Definitive` |
| `TransportError::wrap(err)` | a transport error, a 5xx, a timeout: the outcome is unknowable | `Ambiguous` | `Ambiguous` |
| a plain error | a failure left unmarked | `Definitive` | `Ambiguous` |

Mark every failure as one of the first two. A plain error is the one shape
whose verdict depends on the adapter: the reversible baseline takes it as the
receiver's answer and consumes it, which is wrong for an ack loss. The
dispatch's own `TimedOut` and `Cancelled`, and an `std::io::Error` anywhere in
the chain, are ambiguous under both. A `Declared { resolution: Hold }`
adapter never judges a send.

`effecttest::audit!(name, factory)` runs the audit for one adapter: identity
determinism, the ownership fence, the classifier floor, definitive honesty,
the per-strategy probe (send-twice-lands-once, look-then-adopt,
same-key-replays + past-window-never-sent, landed-never-judged-resend-safe)
and the ack-loss probes through `AckLossProxy` (mandatory for irreversible
adapters). The scaffolder renders one audit per declared call in
`tests/effects_audit.rs`, green on day one against the simulator.
