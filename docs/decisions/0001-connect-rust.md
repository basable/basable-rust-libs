# F.1 — connect-rust for the API boundary

**Status: resolved 2026-09-05, adopt.** Recorded from the platform plan
(`docs/todo/plan-mode.md` in the monorepo, B7).

`connectrpc/connect-rust` v0.9.0 (Apache-2.0, MSRV 1.88, originated at
Anthropic and nominated the canonical Rust implementation) passes the full
Connect conformance suite (3,600 server / 6,872 client tests), serves
Connect + gRPC + gRPC-Web from one handler set, supports unary, server-,
client- and bidi-streaming (bidi needs HTTP/2), has JSON as a default
feature, mounts into axum (`ConnectRouter::into_axum_service()`), and ships
`protoc-gen-connect-rust` (crate `connectrpc-codegen`).

It requires buffa message types (`HasMessageView`); prost is not compatible
and must not be mixed into one binary. Pre-1.0: the minor is pinned
(`connectrpc = "0.9"`, `buffa = "0.9"`) in the scaffolder's pins and here;
upgrades ride the pins bump. No in-house Connect shim and no tonic fallback.

**Residual checks, owned by Phase 9 (`basable-connect`):**

- (a) HTTP/1.1 serving through the Cilium gateway's backend leg for unary
  and server streaming; only bidi needs h2 (`appProtocol: kubernetes.io/h2c`
  on the Service later).
- (b) the exact crate and `[[bin]]` names for crate_universe `gen_binaries`:
  `protoc-gen-buffa`, `protoc-gen-buffa-packaging`, `protoc-gen-connect-rust`.
- (c) prebuilt `toolchains_protoc` compatibility: buffa supports protoc
  v21.12+, editions 2024 needs v33; the platform pins protoc 36.2.
