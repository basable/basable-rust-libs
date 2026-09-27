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
  depends on core, publicid and db; a testkit depends on what it tests,
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
- **Every crate has a `CLAUDE.md`** once it exists, ported from the Go
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
| `crates/basable-externaleffect` | The effect admission contract as types: the closed `Strategy` enum (`Idempotent`, `LookBeforeAct`, `KeyedReplay`, `Declared { Resolve \| Hold }`), `Adapter` validated into `Call` (`dispatch`, `declare`, `resolve`, `lookup`, `resolve_keyed`), `DispatchError`/`ResolveError`, classifiers with the `TransportError`/`definitive` markers, `Owner`/`Unfenced`, `EffectSlot`/`AttemptState`/`Resolution`. No runtime, no SQL; core only. Has its own `CLAUDE.md` |
| `crates/basable-effecttest` | The per-adapter audit: `Harness` + `run`/`try_run`/`audit!` over the universal, per-strategy and late-call probes, `AckLossProxy` (an HTTP proxy that lands a request and drops its answer); `tests/reference.rs` audits three adapters over `WidgetSim` |
| `crates/basable-config` | The declarative configuration framework, the monorepo's `lib/config` over the tenant's `basable_config` schema: temporal class-table inheritance (base `configuration_object` + one subtype table pair per type, history by the `versioning()` trigger), `TypeInfo` + `ConfigTypesBuilder::register(binder)` (a `TypedBinder` per type: upsert / delete / read; refused on a duplicate id, name or prefix), the platform's seed format (`{configSetName, items[{"@type", header{namespace: "#{NamespaceConfiguration:x}", name, labels}, …}]}`, `<name>.<env>.json` scoping, `#{Type:ns:name}` references), `Loader::load` (validate deps → topo sort → ONE transaction under a table lock → prune what the files no longer declare), `load_seed`, `Repository` (`get`/`list`/`lookup_id`/`namespace_id`, `upsert`/`delete` stamped `managed-by=runtime`); `tests/` over the fixture's `PricingRuleConfiguration` and the seed directories under `tests/seed/` |
| `docs/decisions/` | The spikes, one file each, with what was measured |
| `docs/porting-notes.md` | Every deviation from the Go originals |
| `.github/workflows/ci.yaml` | `bazel test //...` with the two-cache hygiene |
| `.github/workflows/release.yaml` | `cargo publish` in dependency order on a `v*` tag |

## Versions

One workspace version (`[workspace.package] version`); MSRV
`rust-version = "1.88"` (connect-rust's); the toolchain `MODULE.bazel` pins
is newer and mirrors the scaffolder's `RustVersion`. A release bumps the
version, tags `vX.Y.Z`, and opens a PR against the monorepo bumping
`pins.go` and re-vendoring the template tree.
