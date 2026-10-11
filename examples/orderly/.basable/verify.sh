# The verify build the platform runs on every AI turn (bash .basable/verify.sh
# on the git host's runner, from a fresh checkout of the verify branch).
# It must leave the tree built and tested; whatever it changes — MODULE.bazel.lock,
# Cargo.lock, Cargo.Bazel.lock — is committed and merged.
set -euo pipefail
command -v bazel >/dev/null 2>&1 || npm install -g @bazel/bazelisk
# crate_universe repins Cargo.lock and Cargo.Bazel.lock from the Cargo.toml
# files, as the CI does: the scaffold renders no Cargo.Bazel.lock (without
# this the first build cannot read it), and a crate added to a Cargo.toml
# must reach both locks.
export CARGO_BAZEL_REPIN=workspace
bazel mod tidy
bazel test //...
# The images, built and not pushed: the deploy workflow pushes them.
bazel build --config=ci //:push
