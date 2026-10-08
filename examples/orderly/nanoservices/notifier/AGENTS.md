# notifier

Sends one templated email per event.

Rendered by `scaffold_nanoservice` from `docs/01-shop/manifest.yaml`.
The Directive sections that bind this nanoservice: §2 (table ownership), §6 (external-effect admission — it owns adapters), §7
(cross-nanoservice rules for every message it sends), §10 (this file and
`flows.md` must stay true).

## What it owns

| External call | System | Strategy | Irreversible |
|---|---|---|---|
| `send_email` | email | `keyed_replay` | yes |

This nanoservice owns no data: it is a pure executor (no schema, no role, no
pool). Its handlers dispatch adapters `Unfenced`; the caller's re-drive is the
recovery.

## Files

| File | Responsibility |
|---|---|
| `src/lib.rs` | The `Notifier` struct, its constructor (pools, providers, calls), the schema marker, and its `basable_app::Component` impl: `loops()` lists every worker and ticker it runs (none for a plain executor). |
| `src/handlers.rs` | `impl NotifierHandler<R>`: one `handle_<snake_message>` per handled message. Intent only — `create` / `update_spec` / `mark_deleted` / `nudge`; never a status write. |
| `src/effects.rs` | One `Call` per external call with its strategy literal; `send` is the only dispatch path. |
| `src/provider.rs` / `src/simulator.rs` | The provider trait, the HTTP client (a parsed 4xx is `SendError::Refused`), the noop; the in-memory simulator the audit and the tests drive. |
| `tests/effects_audit.rs` | One `audit!` per adapter — the audit index. Green on day one; the first thing a real provider must keep green. |
| `tests/integration.rs` | Integration tests on the testkit (a database per test). |
| `flows.md` | State machines, sequences, edge cases. |

## Messages

Handles: `OrderEvent`.
Sends: nothing.

