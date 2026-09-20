# F.2 — protobuf codegen is a Bazel build step, never `buf generate`

**Status: decided (user decision); implementation lands in Phase 9.**

One `genrule` per proto package, wrapped in `tools/proto.bzl`, runs the
PREBUILT protoc from `toolchains_protoc` with three plugin binaries built by
crate_universe `gen_binaries`:

- `protoc-gen-buffa` — message types and zero-copy views
  (`views=true,json=true`);
- `protoc-gen-buffa-packaging` — the `mod.rs` module tree
  (`filter=services` for the connect tree);
- `protoc-gen-connect-rust` — the service stubs (`buffa_module=crate::proto`).

Outputs land in `generated/buffa/*.rs` and `generated/connect/*.rs`, listed
as `rust_library` srcs and mounted through `#[path]`, exactly as
`connect-rust/examples/bazel/BUILD.bazel` does. `rules_rust_prost` is not
used (prost and buffa do not mix).

**Why not committed generated code:** the tenant's agent cannot run a
generator, so committed output goes stale on every `.proto` edit and a drift
check would block deploys. `buf` is used only for `buf lint` (a `bazel test`
target via `rules_buf`) and `buf breaking`; there is no `buf.gen.yaml`.

**Open measurement:** the cold cost of building the three plugin binaries
under crate_universe (expected 1–2 minutes), recorded here when Phase 9
lands, together with the F.1 residuals.
