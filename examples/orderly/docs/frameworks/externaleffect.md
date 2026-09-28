# basable-externaleffect and basable-effecttest (crates 0.1.0)

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
    pub operation: &'static str,
    pub key: fn(&A) -> String,            // deterministic identity, never a retry count
    pub send: …,                          // the ONE dispatch path: Result<R, SendError>
    pub call_timeout: Duration,
    pub irreversible: bool,               // ⇒ KeyedReplay | Declared
    pub late_call: LateCall,              // Convergent | KeyScoped | Compensated
    pub strategy: Strategy<A, RA, R>,
}
pub enum SendError { Refused(BoxError), Failed(BoxError) }   // the provider's verdict on its own call
impl Call { dispatch(&self, proof: &dyn Owner, args) -> Result<R, DispatchError>; declare; resolve; lookup; resolve_keyed }
pub enum DispatchError { OwnershipLost, ReplayWindowElapsed { age, window }, Ambiguous(BoxError), Definitive(BoxError) }
```

`new(adapter)` validates every rule (irreversible requires durable intent;
`KeyedReplay` and `Declared{Keyed}` need `key`).
A reconciler dispatches with the `Claim` as owner; a request-path handler
dispatches `Unfenced`. Match `DispatchError`: `OwnershipLost` ⇒ nothing sent,
`Retry`; `Ambiguous` ⇒ may have landed, `Retry` and re-drive; `Definitive`
⇒ the receiver's answer.

The provider decides what its own failure means, in the type:
`SendError::Refused` is the receiver's structured answer (a parsed 4xx),
proof that nothing executed, and dispatch reports it as `Definitive`;
`SendError::Failed` is a transport error, a 5xx or a timeout, whose outcome
is unknowable, and dispatch reports it as `Ambiguous`. There is no
classifier to configure and no error chain to walk: an irreversible adapter
is fail-closed because a provider cannot express a definitive failure any
other way. A `Declared { resolution: Hold }` adapter never judges a send.

`effecttest::audit!(name, factory)` runs the audit for one adapter: identity
determinism, the ownership fence, the classifier floor, definitive honesty,
the per-strategy probe (send-twice-lands-once, look-then-adopt,
same-key-replays + past-window-never-sent, landed-never-judged-resend-safe)
and the ack-loss probes through `AckLossProxy` (mandatory for irreversible
adapters). The scaffolder renders one audit per declared call in
`tests/effects_audit.rs`, green on day one against the simulator.
