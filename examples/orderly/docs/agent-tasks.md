# Agent tasks

The ordered list of what is left after the scaffold. Each nanoservice's
items are in `docs/nanoservices/<name>.md`; this file is the whole-project
order. Tick items off as they land.

1. Read `AGENTS.md`, `docs/DIRECTIVE.md`, `docs/architecture.md`.
2. Work through `docs/nanoservices/catalog.md` for `catalog`.
3. Work through `docs/nanoservices/order.md` for `order`.
4. Work through `docs/nanoservices/notifier.md` for `notifier`.
5. Work through `docs/nanoservices/accounting.md` for `accounting`.
6. Declare every message struct in `crates/messages/src/lib.rs` (the build names the missing ones).
7. Fill the proto messages under `proto/` and the field mapping in `api/src/services/`.
8. Un-ignore the `#[ignore = "implement first"]` tests as each piece lands; keep `tests/effects_audit.rs` green.
9. `regex_search unimplemented_step` — only the definitions left (one per crate) means the skeleton is filled.
