# order

Drives an order to paid and fulfilled.

Design: `docs/01-shop/02-order.md`. Code:
`nanoservices/order/` (read its `AGENTS.md` first).

## Owns

- Processing-object type `order` (key 1, `ord_…`): spec `customer_id`, `lines`; status `phase`, `payment_id`.
- Processing-object type `shipment` (key 2, `shp_…`): spec TODO; status TODO.
- Table `order_audit` (columns TODO).
- External call `capture_payment` on payments: `keyed_replay`, irreversible.
- Schedule `sweep_abandoned` every 15m.

## Messages

- Handles: `EnsureOrderRequest`.
- Sends: `GetProductRequest`, `OrderEvent`.

## Directive sections that bind

§2 always; §3, §4, §5 for each processing-object type; §6 for each adapter; §7 for each message sent; §10 for the docs.

## TODO, in order

- [ ] `handle_ensure_order_request` in `src/handlers.rs`.
- [ ] `order`: the reconcile pass in `types/order/reconciler.rs`; the worker policy.
- [ ] `shipment`: the TODO columns in the migration and in `types/shipment/{type,adapter}.rs`; the reconcile pass in `types/shipment/reconciler.rs`; the worker policy.
- [ ] Table `order_audit`: the columns in the migration and `model.rs`; the queries in `repository.rs`.
- [ ] `send` for `capture_payment` in `effects.rs`, the real client in `provider.rs`; the audit stays green.
- [ ] `tick_sweep_abandoned` in `worker.rs`.
- [ ] `OrderService` field mapping in `api/src/services/order.rs` and the messages in `proto/order/v1/order.proto`.
- [ ] The message structs in `crates/messages/src/lib.rs`.
- [ ] `flows.md` and this file kept true.
