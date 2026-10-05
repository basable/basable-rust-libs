# CI

One workflow, `.github/workflows/build.yaml`, on every push to `dev` and
`main` except docs-only ones: lint `k8s/` → restore the two caches → Bazel
test (unit, clippy aspect, integration suites against the `services:`
Postgres) → push the images by commit SHA with rules_oci → read each image's
digest from its `index.json` → render `k8s/` with those digests → upload
`rendered-manifests` → prune, save and clean the caches.

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
  Only entries created before this run's own, so a concurrent run on a
  newer commit keeps what it saved. The repository cache gets the same
  treatment: every entry but the most recently accessed one is superseded,
  since a dependency bump saves a fresh entry under a new lockfile hash and
  leaves the old one standing.
- **Only on success**, so a failed build keeps the previous good cache.
- **Free the disk first** (GitHub only). A hosted runner ships about 14 GB
  free, most of it preinstalled toolchains Bazel never uses; the build plus
  the restored caches do not fit beside them. The removal list leaves
  `/usr/local/lib/android` alone, because a transitive rule auto-detects a
  real SDK path and breaks when it is half-gone.
- **setup-bazel's own caches are off.** Its disk cache keys on the BUILD and
  MODULE files alone and skips the save on an exact hit, so it would stop
  updating while the sources keep changing.
- **A later step never recomputes what an earlier one pushed**: the Render
  step uses the digests the build step reported, never a rebuild.

## Cold and warm

A cold Bazel + rules_rust build is 20–40 minutes on a hosted 2 vCPU runner;
warm, 2–5 minutes. The platform's remote cache (`.bazelrc`, commented until
provisioned) is what makes the first push on a fresh runner fast.
