# order

Drives an order to paid and fulfilled.

Rendered by `scaffold_nanoservice` from `docs/01-shop/manifest.yaml`.
The Directive sections that bind this nanoservice: §2 (table ownership), §3–§5
(the invariants, the author obligations, the adapter and outcome contract — it
owns processing-object types), §6 (external-effect admission — it owns adapters), §7
(cross-nanoservice rules for every message it sends), §10 (this file and
`flows.md` must stay true).

## What it owns

| Processing-object type | Key | Prefix | Spec fields | Status fields |
|---|---|---|---|---|
| `order` | 1 | `ord` | `customer_id` (immutable), `lines` | `phase`, `payment_id` |
| `shipment` | 2 | `shp` | TODO | TODO |

Plain tables (schema `nano_order`): `order_audit`.

| External call | System | Strategy | Irreversible |
|---|---|---|---|
| `capture_payment` | payments | `keyed_replay` | yes |

## Files

| File | Responsibility |
|---|---|
| `src/lib.rs` | The `Order` struct, its constructor (pools, providers, calls), the schema marker. |
| `src/handlers.rs` | `impl OrderHandler<R>`: one `handle_<snake_message>` per handled message. Intent only — `create` / `update_spec` / `mark_deleted` / `nudge`; never a status write. |
| `src/types/<type>/type.rs` | The `ProcessingObjectType` declaration: spec/status structs, `TYPE_KEY`, `PUBLIC_ID_PREFIX`. |
| `src/types/<type>/adapter.rs` | The dumb column mapper: every column in BOTH `write_spec` and `read_rows`. |
| `src/types/<type>/reconciler.rs` | The level-triggered pass and the worker policy (identical on every replica). |
| `src/repository.rs` / `src/model.rs` | Typed sqlx queries over this nanoservice's pool; the row structs. |
| `src/effects.rs` | One `Call` per external call with its strategy literal; `send` is the only dispatch path. |
| `src/provider.rs` / `src/simulator.rs` | The provider trait, the HTTP client (a parsed 4xx is `SendError::Refused`), the noop; the in-memory simulator the audit and the tests drive. |
| `tests/effects_audit.rs` | One `audit!` per adapter — the audit index. Green on day one; the first thing a real provider must keep green. |
| `src/worker.rs` | The ticker workers (`sweep_abandoned` every 15m), registered in `app/src/main.rs`, joined on shutdown. |
| `tests/integration.rs` | Integration tests on the testkit (a database per test). |
| `flows.md` | State machines, sequences, edge cases. |

## Messages

Handles: `EnsureOrderRequest` → `Order`.
Sends: `GetProductRequest`, `OrderEvent`.

API: `OrderService` in `api/src/services/order.rs` (EnsureOrder).

