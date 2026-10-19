# CI

One workflow, `.github/workflows/build.yaml`, on every push to `dev` and
`main` except docs-only ones, every step in a Debian 13 job container on
the hosted Ubuntu VM: install the toolchain and tools → lint `k8s/` →
restore the two caches → Bazel test (unit, clippy aspect, integration
suites against the `services:` Postgres) → push the images by commit SHA
with rules_oci → read each image's digest from its `index.json` → render
`k8s/` with those digests → upload `rendered-manifests` → prune, save and
clean the caches.

## The job container (why the job is not simply on the VM)

The app image stands on `gcr.io/distroless/cc-debian13`, and the binary is
linked against the glibc of whatever builds it. glibc runs an older binary
on a newer libc, never the reverse, and the hosted VM's Ubuntu (26.04 since
2026-10-19, glibc 2.43) is ahead of every Debian release (13: 2.41), so a
binary linked on the VM can need a symbol version the base does not have
and fail at start, in the cluster, where the build never looked. Linking
inside a container of the base's own release (`debian:13`, pinned by
digest like every other image) removes the question. There is no Debian VM
to pick with `runs-on`; the `container:` key is how a job builds on one.

What follows from it:

- **The container is bare.** The first step installs the C toolchain
  (rules_rust links with it, the crates' build scripts compile with it;
  cmake is aws-lc-sys's fallback), git (without it `actions/checkout`
  downloads an archive with no history), zstd (the cache entries' format),
  curl and jq, kustomize and helm, and on GitHub gh. The VM's preinstalled
  tools are not visible from inside.
- **The Postgres service is `postgres`, not `localhost`.** A job container
  reaches a service by its name on the job's network; no port mapping.
- **The registry login writes `~/.docker/config.json` itself.** There is
  no Docker in the container; crane (what `//:push` runs) reads that file,
  which is all `docker login` would have written.
- **No disk cleanup step.** The VM's preinstalled toolchains are neither
  in the way nor reachable from the container, and a cold build of the
  reference project needs about 2 GB of output base and 1 GB of disk cache
  (measured 2026-10-19), well inside the VM's free disk.

## The build step and the platform's template

The platform's own workflow template builds a Dockerfile; this project
keeps everything in that template except the build-and-push step, which is
`bazel run --config=ci //:push -- --tag <sha>` plus a step that reads each
built image's digest into `steps.build.outputs.digest_*`. Never reintroduce
a Dockerfile step: the images are `oci_image` targets.

## Cache hygiene (why the workflow looks like this)

- **Two caches, keyed by how often they change.** The repository cache and
  bazelisk under a key hashed from the lockfiles (it changes only with a
  dependency); the disk cache per SHA with a prefix restore (it changes
  every commit). `actions/cache/restore` and `save` are separate steps so
  pruning runs between them.
- **Prune before save.** Bazel refreshes an entry's mtime when a build hits
  it, so `find ~/.cache/bazel-disk -type f ! -newer <marker> -delete` keeps
  exactly one build's worth, however the dependencies moved.
- **Delete the superseded entries after save** (`gh cache delete`, GitHub
  only). A per-SHA key is required — a cache entry is immutable — but
  without cleanup a ~GB entry per commit piles up until the quota is hit.
  Only this branch's entries (`--ref`): a push to `main` can read `main`'s
  entries alone, so a dev run leaves `main`'s last one standing and the
  live deploy starts warm from it. Only entries created before this run's
  own, so a concurrent run on a newer commit keeps what it saved. Deleted
  by id, never by key: after a fast-forward one commit sits on both
  branches with an entry on each. The repository cache gets the same
  treatment: every entry but the most recently accessed one is superseded,
  since a dependency bump saves a fresh entry under a new lockfile hash and
  leaves the old one standing.
- **Only on success**, so a failed build keeps the previous good cache.
- **setup-bazel's own caches are off.** Its disk cache keys on the BUILD and
  MODULE files alone and skips the save on an exact hit, so it would stop
  updating while the sources keep changing.
- **A later step never recomputes what an earlier one pushed**: the Render
  step uses the digests the build step reported, never a rebuild.
- **The platform's verified builds keep caches of their own.** The
  platform's `basable-builder.yaml` runs `.basable/verify.sh` from the
  default branch and saves its Bazel caches there under `basable-builder-`
  keys, so every verified build starts from the last one's. This
  workflow's cleanup deletes only `disk-cache-` and `repo-cache-` entries
  and leaves those alone. `.basable/verify.sh` builds in one configuration
  (`bazel test //...` builds the images too); a second `--config=ci` build
  there would compile the whole graph twice.
- **The two share the repository cache only.** The builder runs
  `.basable/verify.sh` with `--config=ci` (from the runner's `~/.bazelrc`;
  on a laptop the script builds in the default configuration) on the VM,
  not in a job container. A download is the same bytes whatever builds, so
  this workflow restores the builder's `basable-builder-repo-` entries as
  a fallback. Its disk-cache entries never hit here: the toolchain is an
  input of every action (rules_rust links the process wrapper every rustc
  runs through), so an action linked on Ubuntu has another key than the
  same action in the Debian container. Each branch therefore keeps its own
  last `disk-cache-` entry (the cleanup above), and only the first build
  on a branch is cold.

## Cold and warm

A cold Bazel + rules_rust build is 20–40 minutes on a hosted 2 vCPU runner;
warm, 2–5 minutes. The platform's remote cache (`.bazelrc`, commented until
provisioned) is what makes the first push on a fresh runner fast.
