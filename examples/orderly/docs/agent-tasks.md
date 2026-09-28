# Agent tasks

The ordered list of what is left after the scaffold. Each
`scaffold_nanoservice` call adds that nanoservice's items to
`docs/nanoservices/<name>.md`; this file is the whole-project order. Tick
items off as they land.

1. Read `AGENTS.md`, `docs/DIRECTIVE.md`, `docs/architecture.md`.
2. `scaffold_nanoservice` for `catalog`, then work through `docs/nanoservices/catalog.md`.
3. `scaffold_nanoservice` for `order`, then work through `docs/nanoservices/order.md`.
4. `scaffold_nanoservice` for `notifier`, then work through `docs/nanoservices/notifier.md`.
5. Declare every message struct in `crates/messages/src/lib.rs` (the build names the missing ones).
6. Fill the proto messages under `proto/` and the field mapping in `api/src/services/`.
7. Un-ignore the `#[ignore = "implement first"]` tests as each piece lands; keep `tests/effects_audit.rs` green.
8. `regex_search unimplemented_step` — only the definitions left (one per crate) means the skeleton is filled.
