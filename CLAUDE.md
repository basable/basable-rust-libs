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
  {processingobject, config} → app; externaleffect depends on core only;
  a testkit depends on what it tests, never the reverse.
- **Errors are typed enums, or `BoxError`; both hand-written.** An error a
  consumer decides on is an enum with its `Display` and `Error` impls spelled
  out; one nobody inspects is `basable_core::BoxError`. No `anyhow`, no
  `thiserror` (porting note 2).
- **Every crate has a `CLAUDE.md`** once it exists, ported from the Go
  original's, and links the Directive.

## Layout

| Path | What |
|---|---|
| `crates/basable-core` | Leaf: naming rules, labels, the two-clock `Deadline`, `BoxError`, `AppError` with the Connect codes, `Ctx` |
| `crates/basable-publicid` | `encode`/`decode`, the boot-time `Registry` (a port of `golang/lib/publicid`) |
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
