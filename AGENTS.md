# basable-rust-libs

The basable nanoservice frameworks for Rust: the `basable-*` crates a
tenant project depends on, released in lockstep and pinned exactly by the
basable platform's scaffolder (`golang/controller/lib/scaffold/pins.go` in
the monorepo). The Go originals in the monorepo are the specification;
semantics stay identical except where Rust makes an invariant mechanical,
and every such deviation is listed in `docs/porting-notes.md`.

## Rules

- **Bazel is the only build.** `bazel test //...` is the gate. The
  `Cargo.toml` files are package metadata: crate_universe reads them for
  dependency resolution and the release job needs them for `cargo publish`.
  Nobody runs cargo otherwise, and a `target/` directory is never committed.
- **A dependency change is one edit plus one repin.** Add the crate to
  `[workspace.dependencies]` in the root `Cargo.toml` (the crate takes it
  with `workspace = true`), then `CARGO_BAZEL_REPIN=1 bazel build //...`
  and commit `Cargo.lock` and `Cargo.Bazel.lock` together. A new crate is a
  directory under `crates/`, a `members` line, and its `Cargo.toml` listed in
  `MODULE.bazel`'s `manifests`.
- **BUILD files never name a dependency twice.** `all_crate_deps()` from
  `@crates//:defs.bzl` carries the external crates; workspace crates are
  listed by label. `glob(["src/**/*.rs"])`, so a new file needs no BUILD edit.
- **Never openssl, never a crate that needs a system library.** rustls
  everywhere; the tenant image is distroless.
- **clippy and rustfmt are build errors**, through the aspects in
  `.bazelrc`: `-Dwarnings`, `await_holding_lock`, and the `disallowed_types`
  in `clippy.toml` (an async mutex). `bazel run @rules_rust//:rustfmt` fixes
  formatting.
- **Dependency direction is strictly downward**: core → publicid → db →
  {processingobject, config} → app; externaleffect depends on core only
  (and processingobject on it, for the `Owner` impl on `Claim`); config
  depends on core, publicid and db; messenger has no dependencies and the
  generator crates depend on nothing in the workspace (they run at build
  time); pubsub depends on core and db; auth on core; connect on core and
  auth; app on everything below it; a testkit depends on what it tests,
  never the reverse.
- **Errors are typed enums, or `BoxError`; both hand-written.** An error a
  consumer decides on is an enum with its `Display` and `Error` impls spelled
  out; one nobody inspects is `basable_core::BoxError`. No `anyhow`, no
  `thiserror` (porting note 2).
- **Database-backed tests read `TEST_DATABASE_URL`** (a login that can
  create roles and databases; the CI's Postgres service, locally the
  monorepo's `test_postgres` container on port 29000) and return early
  without it. `bazel test //...` alone is therefore not the whole gate: run
  it with the variable set before a change to `basable-db` or the testkit.
- **Every crate has an `AGENTS.md`** once it exists, ported from the Go
  original's, and links the Directive.

## Layout

| Path | What |
|---|---|
| `crates/basable-core` | Leaf: naming rules, labels, the two-clock `Deadline`, `BoxError`, `AppError` with the Connect codes, `Ctx` |
| `crates/basable-publicid` | `encode`/`decode`, the boot-time `Registry` (a port of `golang/lib/publicid`) |
| `crates/basable-db` | `Nanoservice`/`Stateful` markers, `NanoPool<N>` (the `app` login switched to `nano_<name>`, `search_path` pinned), `MigratorPool`, the dbmate-format runner and ledger `verify`, SQLSTATE helpers, `begin_snapshot`, `release_listen_conn` |
| `crates/basable-testkit` | `TestDb` (a database per test from `TEST_DATABASE_URL`, migrations applied as `app`), `nano_pool::<N>()`, `CommitFaultProxy` (drops one COMMIT ack, applied or rolled back) |
| `crates/basable-processingobject` | The declarative reconciliation framework: `ProcessingObjectType` + the `Adapter` trait, `Tx`, the read model (`Meta`, `Object`), `Outcome`, `WorkerConfig`/`Backoff` (bit-identical to Go), `TypedStore` bind/create/update_spec/mark_deleted/nudge/read/read_many and `claim_batch` over the type's PARTITION through a `NanoPool`; `Claim` with `heartbeat` (`LeaseHandle`), `write_status` and `complete` (`Completion`), both implementing the effect `Owner`; the `Worker` runtime (`Reconciler` and `AfterComplete` traits, a `JoinSet` of attempts, the heartbeat pump, `catch_unwind` around the pass and the callback, the `PgListener` wake) |
| `crates/basable-processingobject-testkit` | The conformance nanoservice (key 32000, prefix `ek`: role, schema, migrations, adapter), `WidgetSim` (an HTTP provider simulator: idempotent widgets and keyed orders) with `WidgetClient` (the provider boundary's error mapping), the `Harness` (create, `claim_batch`, `drive_once`, `force_expire_claim`, envelope readers, `start_worker`), `ExampleReconciler` with `hook` / `Gate` injection points, `Replica`; `tests/` are the ported conformance suites: store, fencing, writestatus, deletion, schema, commitfault, worker, carveouts |
| `crates/basable-externaleffect` | The effect admission contract as types: the closed `Strategy` enum (`Idempotent`, `LookBeforeAct`, `KeyedReplay`, `Declared { Resolve \| Hold }`), `Adapter` validated into `Call` (`dispatch`, `declare`, `resolve`, `lookup`, `resolve_keyed`), `DispatchError`/`ResolveError`, classifiers with the `TransportError`/`definitive` markers, `Owner`/`Unfenced`, `EffectSlot`/`AttemptState`/`Resolution`. No runtime, no SQL; core only. Has its own `AGENTS.md` |
| `crates/basable-effecttest` | The per-adapter audit: `Harness` + `run`/`try_run`/`audit!` over the universal, per-strategy and late-call probes, `AckLossProxy` (an HTTP proxy that lands a request and drops its answer); `tests/reference.rs` audits three adapters over `WidgetSim` |
| `crates/basable-config` | The declarative configuration framework, the monorepo's `lib/config` over the tenant's `basable_config` schema: temporal class-table inheritance (base `configuration_object` + one subtype table pair per type, history by the `versioning()` trigger), `TypeInfo` + `ConfigTypesBuilder::register(binder)` (a `TypedBinder` per type: header / upsert / delete / read — the binder reads the message's header because a buffa message lives in the tenant's proto crate, where the nanoservice cannot implement a trait for it; refused on a duplicate id, name or prefix), the platform's seed format (`{configSetName, items[{"@type", header{namespace: "#{NamespaceConfiguration:x}", name, labels}, …}]}`, `<name>.<env>.json` scoping, `#{Type:ns:name}` references), `Loader::load` (validate deps → topo sort → ONE transaction under a table lock → prune what the files no longer declare), `load_seed`, `Repository` (`get`/`list`/`lookup_id`/`namespace_id`, `upsert`/`delete` stamped `managed-by=runtime`); `tests/` over the fixture's `PricingRuleConfiguration` and the seed directories under `tests/seed/` |
| `crates/basable-messenger` | The messenger runtime: the `Route<M, Resp, Source, Ctx>` trait the generated router implements (one lifetime ties router and context so a boxed route is nameable) and `boxed`, the one place a route's future is boxed. No dependencies |
| `crates/basable-messenger-codegen` | The generator as a library: `routing.yaml` through a line-aware YAML tree, the JSON Schema (`schema.json`), the coded rules (`E_*`/`W_*` with lines), the message table and the cycle analysis (a feedback vertex set, `W_ROUTE_CYCLE`), the `quote` + `prettyplease` emitters for the `interfaces` and `messenger` crates, the Markdown topology. Has its own `AGENTS.md` |
| `crates/basable-messenger-gen` | The command line the `messenger_generated` genrule runs: `generate --crate interfaces\|messenger`, `validate`, `docs`, `schema`; diagnostics as `routing.yaml:LINE: CODE: …` on stderr, exit 1 on an `E_*`. Carries a library target too, so a tenant's `crates/messenger` can name it as a dev-dependency and the lock keeps it (cargo drops a dependency on a bin-only crate) |
| `crates/basable-pubsub` | The `lib/pubsub` port: `Bus` over one `LISTEN` connection per process (`subscribe` / `on_reconnect` before `run`, `publish` / `publish_tx` with `Delivery::{ExcludeSelf, IncludeSelf}`, own-message dedup by instance id, the 7900-byte payload bound, `listening()`); `tests/bus.rs` runs two buses over one database including a killed listen backend |
| `crates/basable-app` | The runtime a binary is assembled on: `Config` from the environment (`var::*`), `App::new(cfg).expect_migrations(v).connect()` (the framework pool with a bounded wait, the ledger gate), `App::pool::<N>()` under the connection budget, `App::bus()`, `App::serve()` → `Serve::{raw, worker, ticker}` → `start()`/`run()` (one axum server with `/healthz` and `/readyz`, the `WakeBus` feeding every registered worker, tickers, SIGTERM drain naming stuck loops), `basable_app::tracing::init_json()`; `tests/app.rs` is the Phase 8 verification |
| `crates/basable-auth` | Kratos session validation: `Validator::kratos(url)` with public paths and prefixes, a `session_hook`, `apply(router)` as the axum layer (401 `unauthenticated` / 503 `unavailable` in Connect's JSON shape, a bounded negative cache), `Identity` in the request extensions and on `Ctx` (`AuthCtx::user_id`), `bypassed_for_tests` behind the `test-bypass` feature |
| `crates/basable-connect` | The connectrpc glue: `ConnectRouter` (`add_service`, `into_axum`), `into_connect_error` / `IntoConnect` (the sixteen codes one to one), `with_request_ids`, `request_ctx` (request id, deadline, identity); re-exports `connectrpc` |
| `crates/basable-protoc-gen-buffa` | The `protoc-gen-buffa` binary `tools/proto.bzl` runs: `buffa-codegen` behind the plugin protocol with the platform's options (`views`, `json`, `file_per_package`), a workspace crate because upstream's plugin is binary-only |
| `tools/proto.bzl` | `buffa_connect_library(name, protos, package, ...)`: the genrule over the prebuilt protoc and the two plugins, one `<package>.rs` per plugin; `tools/protoc.bzl` resolves the protobuf module's prebuilt protoc as `//tools:protoc` |
| `tests/connect/` | The Phase 9 verification: an EchoService generated through the macro, mounted on the app behind the auth layer, driven by the generated client in both codecs over HTTP/1.1 and h2c, plain JSON, a server stream, and the auth cases against a Kratos simulator |
| `tools/messenger.bzl` | `messenger_generated(name, crate, spec, tool, out)`: the genrule a tenant's `crates/interfaces` and `crates/messenger` BUILD files use (the scaffolder renders a copy) |
| `spec/routing/` | The `routing.yaml` fixture corpus (`fixtures/valid`, `fixtures/invalid` with `# expect: CODE`), the orderly goldens, `VERSION` (the monorepo's Go validator vendors the corpus at that version) |
| `tests/messenger/` | The generated crates compiled against stub components: `orderly` (the reference topology, routes driven under tokio) and `cyclic` (the boxed back-edge, re-entrancy `a → b → a`, the `compile_fail` doctests for the unboxed cycle and a `MutexGuard` across a send) |
| `examples/orderly/` | The scaffolder's output for the reference manifest (`ExampleManifest`, the GitHub driver), committed verbatim plus the local-crates patch: its own Bazel module (root `.bazelignore` hides it from this workspace), built and tested by the CI's `orderly` job as a tenant's CI does (`bazel test --config=ci //...`), except that it repins: a patched path dependency lands in `Cargo.Bazel.lock` by absolute path, so that lockfile is git-ignored until the release publishes the crates. NOT a template source — the templates live in the monorepo's `lib/scaffold`; `tools/render-orderly.sh` re-renders it and a clean `git status` afterwards is the nightly contract. Its `tools/basable-crates/` is a git-ignored copy of this repository that the `[patch.crates-io]` block resolves the crates from until the release publishes them (`tools/sync-orderly-crates.sh`; a copy, because crate_universe writes its generated `BUILD.bazel` into every path dependency's directory) |
| `tools/render-orderly.sh`, `tools/sync-orderly-crates.sh` | The example's render (fresh render, rsync with delete, the crates copy, the workspace `exclude` + patch block, a repin so the committed `Cargo.lock` and `MODULE.bazel.lock` are current) and the crates copy the CI refreshes before building the example |
| `docs/decisions/` | The spikes, one file each, with what was measured |
| `docs/porting-notes.md` | Every deviation from the Go originals |
| `.github/workflows/ci.yaml` | `bazel test //...` with the two-cache hygiene, and the `orderly` job over the example with caches of its own |
| `.github/workflows/release.yaml` | `cargo publish` in dependency order on a `v*` tag |

## Versions

One workspace version (`[workspace.package] version`); MSRV
`rust-version = "1.88"` (connect-rust's); the toolchain `MODULE.bazel` pins
is newer and mirrors the scaffolder's `RustVersion`. A release bumps the
version, tags `vX.Y.Z`, and opens a PR against the monorepo bumping
`pins.go` and re-vendoring the template tree.
