# catalog

Owns the product catalogue.

Design: `docs/01-shop/02-catalog.md`. Code:
`nanoservices/catalog/` (read its `AGENTS.md` first).

## Owns

- Table `product` (`id`, `sku`, `price_cents`).
- Config type `pricing_rule`.

## Messages

- Handles: `UpsertProductRequest`, `GetProductRequest`, `OrderEvent`.
- Sends: nothing.

## Directive sections that bind

§2 always; §7 for each message sent; §10 for the docs.

## TODO, in order

- [ ] `handle_upsert_product_request` in `src/handlers.rs`.
- [ ] `handle_get_product_request` in `src/handlers.rs`.
- [ ] `handle_order_event` in `src/handlers.rs`.
- [ ] Table `product`: the queries in `repository.rs`.
- [ ] `CatalogService` field mapping in `api/src/services/catalog.rs` and the messages in `proto/catalog/v1/catalog.proto`.
- [ ] The message structs in `crates/messages/src/lib.rs`.
- [ ] `flows.md` and this file kept true.
