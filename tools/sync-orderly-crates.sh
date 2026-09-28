#!/bin/bash
# Copies the workspace crates into examples/orderly/tools/basable-crates so
# the example's [patch.crates-io] (see tools/render-orderly.sh) resolves
# them until the release publishes them. A COPY, not a link: crate_universe
# writes its generated BUILD.bazel into every path dependency's directory,
# which through a link would clobber the crates' hand-written BUILD files.
# The copy is git-ignored and refreshed by every render and by CI.
#
#   tools/sync-orderly-crates.sh
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
dest="$here/examples/orderly/tools/basable-crates"
mkdir -p "$dest"
rsync -a --delete \
  --exclude .git --exclude examples --exclude 'bazel-*' --exclude target \
  --exclude .github --exclude 'crates/*/BUILD.bazel' \
  "$here/" "$dest/"
echo "synced the workspace crates into $dest"
