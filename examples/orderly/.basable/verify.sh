# The verify build the platform runs on every AI turn (bash .basable/verify.sh
# on the git host's runner, from a fresh checkout of the verify branch).
# It must leave the tree built and tested; whatever it changes — MODULE.bazel.lock,
# Cargo.Bazel.lock — is committed and merged.
set -euo pipefail
command -v bazel >/dev/null 2>&1 || npm install -g @bazel/bazelisk
bazel mod tidy
bazel test //...
# The images, built and not pushed: the deploy workflow pushes them.
bazel build --config=ci //:push
