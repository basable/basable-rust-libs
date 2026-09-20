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

Each phase appends a row from the CI run that landed it; Phase 10
(`examples/orderly`) is the baseline the "repo to live in ten minutes"
target is measured against.
