# catalog

Owns the product catalogue.

Rendered by `scaffold_nanoservice` from `docs/01-shop/manifest.yaml`.
The Directive sections that bind this nanoservice: §2 (table ownership), §7
(cross-nanoservice rules for every message it sends), §10 (this file and
`flows.md` must stay true).

## What it owns

Plain tables (schema `nano_catalog`): `product`.

Config catalogs: `pricing_rule` (`config/base/pricing_rule.json`).

## Files

| File | Responsibility |
|---|---|
| `src/lib.rs` | The `Catalog` struct, its constructor (pools, providers, calls), the schema marker. |
| `src/handlers.rs` | `impl CatalogHandler<R>`: one `handle_<snake_message>` per handled message. Intent only — `create` / `update_spec` / `mark_deleted` / `nudge`; never a status write. |
| `src/repository.rs` / `src/model.rs` | Typed sqlx queries over this nanoservice's pool; the row structs. |
| `src/config.rs` | The config type declarations and their readers. |
| `tests/integration.rs` | Integration tests on the testkit (a database per test). |
| `flows.md` | State machines, sequences, edge cases. |

## Messages

Handles: `UpsertProductRequest` → `Product`, `GetProductRequest` → `Product`, `OrderEvent`.
Sends: nothing.

API: `CatalogService` in `api/src/services/catalog.rs` (UpsertProduct).

