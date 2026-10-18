# accounting

Books orders and payments on the ledger.

Rendered by the scaffold from `docs/01-shop/manifest.yaml`.
The Directive sections that bind this nanoservice: §2 (table ownership), §7
(cross-nanoservice rules for every message it sends), §10 (this file and
`flows.md` must stay true), §11 (money: it owns the ledger).

## What it owns

The project's ledger (`journal` and `process_ledger_object_version` in `nano_accounting`), booking:

| Object type | Snapshot | States | Booked first as | Changes (a state listing itself allows amount changes; none listed: frozen) | Producer |
|---|---|---|---|---|---|
| `order` | `messages::OrderSnapshot` | `placed`, `paid`, `cancelled` | `placed` | `placed` → `placed`, `paid`, `cancelled`; `paid` → nothing; `cancelled` → nothing | `order` |
| `payment` | `messages::PaymentSnapshot` | `pending`, `settled`, `reversed` | `pending`, `settled` | `pending` → `settled`, `reversed`; `settled` → `reversed`; `reversed` → nothing | `order` |

## Files

| File | Responsibility |
|---|---|
| `src/lib.rs` | The `Accounting` struct, its constructor (pools, providers, calls), the schema marker, and its `basable_app::Component` impl: `loops()` lists every worker and ticker it runs (none for a plain executor). |
| `src/handlers.rs` | `impl AccountingHandler<R>`: one `handle_<snake_message>` per handled message. Intent only — `create` / `update_spec` / `mark_deleted` / `nudge`; never a status write. |
| `src/ledger/mod.rs` | The ledger over this nanoservice's pool with every object type registered; `build` restates at boot. Generated-final: a new object type is added by `scaffold_nanoservice`. |
| `src/ledger/accounts.rs` | The `Account` enum: what names a batch (key, ledger, type). Yours. |
| `src/ledger/<type>.rs` | The `Accounting` of one object type: allowed changes (from the manifest), `desired_state` per state, `validate_balances`. Yours. |
| `../../crates/messages/src/ledger.rs` | The snapshot structs and state enums every producer builds and this ledger books; `Book`, `Booked`. The fields are yours. |
| `tests/ledger.rs` | The tables exist; one property test per object type on the in-memory store. |
| `tests/integration.rs` | Integration tests on the testkit (a database per test). |
| `flows.md` | State machines, sequences, edge cases. |

## Messages

Handles: `Book` → `Booked`.
Sends: nothing.

