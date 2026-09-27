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

Each phase appends a row from the CI run that landed it; Phase 10
(`examples/orderly`) is the baseline the "repo to live in ten minutes"
target is measured against.
