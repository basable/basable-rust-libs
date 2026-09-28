<!-- basable-scaffold plan=01-shop language=rust crates=0.1.0 template=v0.1.0 -->
# Shop

A basable nanoservice application: ONE Rust binary (`app/`) running as
several identical replicas over ONE Postgres database, made of the
nanoservices under `nanoservices/`, talking only through the generated
messenger. Scaffolded from the plan under `docs/01-shop/`; the
plan is the design record, this tree is reality.

## Read first, in this order

1. `docs/DIRECTIVE.md` — the constitution. Where code and Directive disagree,
   the code is wrong.
2. `routing.yaml` and `docs/architecture.md` — the map: who sends what to
   whom, and what each nanoservice owns.
3. `docs/nanoservices/<name>.md` and `nanoservices/<name>/AGENTS.md` for the
   nanoservice you are about to touch.
4. `docs/agent-tasks.md` — what is left to implement, in order.

## The six rules

1. **A nanoservice never touches another nanoservice's tables**, reads
   included. Cross-nanoservice data is a messenger request answered by the
   owner. The database enforces it: one schema and one role per stateful
   nanoservice.
2. **Every cross-nanoservice call goes through the messenger**, declared in
   `routing.yaml`. Editing that file is how messages and nanoservices are
   added; the build then names the `handle_<snake_message>` /
   `send_<snake_message>` methods that are missing. Never call another
   nanoservice's functions.
3. **Every external call is an `externaleffect` adapter** with a strategy
   literal (`effects.rs`), audited in `tests/effects_audit.rs`. A raw
   provider call in a handler or reconciler fails review.
4. **A nanoservice's character is whatever it owns.** Adding a lifecycle
   means adding a processing-object type (spec table, status table, adapter,
   reconciler, worker), not changing a label. Adding plain data means a
   table and a migration.
5. **Never hand-write a skeleton.** A new nanoservice, type, table, adapter
   or migration header comes from `scaffold_nanoservice` after the plan's
   `manifest.yaml` declares it; you fill the bodies. BUILD files use
   `all_crate_deps()` and globs, so a new file needs no BUILD edit.
6. **The skeleton compiles and deploys as rendered.** `unimplemented_step(..)`
   returns `Unimplemented` instead of panicking; `regex_search
   unimplemented_step` lists what is left; `#[ignore = "implement first"]`
   tests are templates to un-ignore.
7. **Errors are typed enums or `basable_core::BoxError`, hand-written.** An
   error a caller decides on is an enum with its `Display` and `Error` impls
   spelled out (`app/src/config.rs` is the model); one nobody inspects is
   `BoxError`, which `?` converts anything into. A repository or adapter
   returns `sqlx::Error` as it comes; a handler turns a failure into the
   caller's `AppError`. No `anyhow`, no `thiserror`.

## Build and test

Bazel only (`.bazelversion` pins it). `.basable/verify.sh` is what the platform's verify build runs on every AI turn (gazelle, `bazel mod tidy`, `bazel test //...`, the images without push); keep it the gate. `bazel test //...` runs unit tests,
the clippy aspect and, with `TEST_DATABASE_URL` set, the integration suites
on a per-test database. Protobuf and the messenger are generated at build
from `proto/` and `routing.yaml`; nothing generated is committed. A new
crate dependency is one line in the workspace `Cargo.toml`
(`docs/adding-a-dependency.md`); the CI repins.

## Deploy

The platform deploys the `rendered-manifests` artifact the one workflow
under `.github/workflows/` uploads, in apply waves: the CNPG operator and
cluster and the Kratos secret Job (-2; the gate waits for the Cluster's `Ready` condition), the
kratos Database (-1), the migration Job and the
Kratos migration Job (0), then the app, the frontend, Kratos and the
HTTPRoute (1). `k8s/app/deployment.yaml` keeps the literal image path
`registry.test.local/abandon-above-tilt/shop`; the workflow pins the digest. `docs/platform-contract.md`
has the rules that must hold.

## Nanoservices

| Nanoservice | Owns | Description |
|---|---|---|
| `catalog` | 0 processing-object type(s), 1 table(s), 1 config type(s), 0 external system(s) | Owns the product catalogue. |
| `order` | 2 processing-object type(s), 1 table(s), 0 config type(s), 1 external system(s) | Drives an order to paid and fulfilled. |
| `notifier` | 0 processing-object type(s), 0 table(s), 0 config type(s), 1 external system(s) | Sends one templated email per event. |
