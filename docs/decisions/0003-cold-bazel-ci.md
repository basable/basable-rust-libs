# F.3 — cold Bazel + rules_rust in CI

**Status: open; mitigations decided, timings to record per phase.**

A cold `bazel test //...` with rules_rust and the sqlx/tokio stack is
expected at 10–20 minutes on a small Gitea act_runner and 20–40 minutes on a
GitHub-hosted 2 vCPU runner. It is the first impression of every new tenant
project, so the mitigations are not optional for launch. In order:

1. `--disk_cache` and `--repository_cache` restored through `actions/cache`
   with the two-key split (`.github/workflows/ci.yaml` here is the reference
   shape the scaffolder's template also renders): repository cache and
   bazelisk keyed by the lockfiles, disk cache per SHA with a prefix
   restore, pruned to the build before saving, superseded entries deleted.
2. Committed `Cargo.Bazel.lock`, so crate_universe never resolves in CI.
3. `--config=ci`: `--compilation_mode=opt` for tests and image in one graph,
   `-Cdebuginfo=0`, `--jobs=4`, `pipelined_compilation`.
4. Later: a platform `bazel-remote` with `--remote_cache` and a per-tenant
   instance name (the platform plan's Phase 6b).

## Timings

| Date | Where | What | Cold | Warm |
|---|---|---|---|---|
| 2026-09-20 | Apple M-series laptop, Bazel 9.2.0, rules_rust 0.74.0, Rust 1.98.1 | Phase 0: two leaf crates (396 actions), including the rules_rust and toolchain downloads and the crate_universe repin; clippy and rustfmt aspects on | 114 s | 1 s (nothing rebuilt, 3 tests cached) |
| 2026-09-20 | GitHub-hosted `ubuntu-latest` (2 vCPU), empty caches, CI run 35519185578 | Phase 0, same tree: analysis + downloads 70 s (`--nobuild`), then `bazel test //...` 61 s; whole job 2 min 26 s | 131 s | — (first run; the disk cache saved was 2 s to upload) |
| 2026-09-27 | GitHub-hosted `ubuntu-latest`, caches restored from the Phase 1 run, CI run 36325740386 | Phase 2: the full sqlx + tokio + axum + reqwest stack compiled for the first time on the runner (two new crates, a Postgres service container); analysis + downloads 58 s, `bazel test //...` 147 s; whole job 4 min 16 s | 205 s (stack cold, rules warm) | — |
| 2026-09-27 | GitHub-hosted `ubuntu-latest`, caches restored from the Phase 3 run, CI run 36332125579 | Phase 4: the worker runtime (tokio added to `basable-processingobject`, a repin) and three more conformance targets over the same stack; lockfile check 70 s, `bazel test //...` 100 s; whole job 3 min 52 s | 170 s (incremental over a warm stack) | — |
| 2026-09-27 | GitHub-hosted `ubuntu-latest`, caches restored from the Phase 4 run, CI run 36334200404 | Phase 5: two new crates (`basable-externaleffect`, `basable-effecttest`) and a new dependency edge into `basable-processingobject`, so the rebuild reached the conformance suites; lockfile check 46 s, `bazel test //...` 112 s; whole job 3 min 26 s | 158 s (incremental over a warm stack) | — |
| 2026-09-27 | GitHub-hosted `ubuntu-latest`, caches restored from the Phase 5 run, CI run 36340950535 | Phase 6 (the platform-parity rewrite of `basable-config`): one crate over the warm stack, the fixture framework migration rewritten so every database-backed suite re-ran; lockfile check 45 s, `bazel test //...` 99 s; whole job 3 min 12 s. Two earlier runs of this phase failed on a cluster-global `CREATE ROLE` race between test databases, fixed in the fixture migrations | 144 s (incremental over a warm stack) | — |
| 2026-09-27 | GitHub-hosted `ubuntu-latest`, caches restored from the Phase 6 run, CI run 36346162950 | Phase 7 (the messenger generator): three new crates and the first build-time dependency set on the runner (syn 3, quote, prettyplease, yaml-rust2, jsonschema with its regex and referencing stack), the generator run in genrules over two test topologies, plus a rustdoc compile-fail target; lockfile check 64 s, `bazel test //...` 365 s; whole job 7 min 59 s | 429 s (the codegen stack cold, everything else warm) | — |
| 2026-09-27 | GitHub-hosted `ubuntu-latest`, caches restored from the Phase 7 run, CI run 36350296425 | Phase 8 (`basable-app`, `basable-pubsub`): two crates over the warm stack, `tracing-subscriber` new, every database-backed suite re-ran; lockfile check 51 s, `bazel test //...` 270 s; whole job 6 min 11 s | 321 s (incremental over a warm stack) | — |
| 2026-09-27 | GitHub-hosted `ubuntu-latest`, caches restored from the Phase 8 run, CI run 36351729407 | Phase 9 (`basable-connect`, `basable-auth`, `basable-protoc-gen-buffa`, `tests/connect`): the protobuf stack cold on the runner (buffa, buffa-codegen, connectrpc with its client and axum features, connectrpc-codegen, the protobuf module's prebuilt protoc) and the first protoc genrules; lockfile check 32 s, `bazel test //...` 711 s; whole job 13 min 23 s | 743 s (the protobuf stack cold, everything else warm) | — |

| 2026-09-27 | GitHub-hosted `ubuntu-latest`, EMPTY caches (the `orderly` job's first run), CI run 36354842501 | Phase 10, the F.3 baseline: `examples/orderly` — a tenant-shaped project (three nanoservices, the proto and messenger genrules, nine test targets over a Postgres service) built from nothing with `bazel test --config=ci //...`, the crate_universe repin included (the patched crates, note 75); crates copy 0 s, `bazel test` 562 s; whole job 10 min 0 s. The libs' `test` job in the same run: 67 s warm | 562 s (everything cold: rules, toolchain, ~340 crates in opt) | — |

Each phase appends a row from the CI run that landed it; Phase 10
(`examples/orderly`) is the baseline the "repo to live in ten minutes"
target is measured against: 10 min 0 s for the whole job with empty
caches, so the target holds only with the caches warm (or a remote cache,
mitigation 4) — the second run of the job will say by how much.
