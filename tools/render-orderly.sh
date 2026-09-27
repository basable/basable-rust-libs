#!/bin/bash
# Renders examples/orderly from the monorepo's scaffolder (the reference
# manifest under the GitHub driver) and applies the local-crates patch the
# example needs until the crates are published. The nightly contract check
# is this script followed by a clean `git status`: a template change the
# example does not carry, or a file the render no longer produces, shows up
# as a diff.
#
#   tools/render-orderly.sh [path-to-monorepo]
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
mono="${1:-$here/../basable}"
out="$here/examples/orderly"
fresh="$(mktemp -d)"
trap 'rm -rf "$fresh"' EXIT

(cd "$mono" && bazel run //golang/controller/lib/scaffold/cmd/render -- --out "$fresh" --driver github)

# crate_universe splices the root manifest into a temp dir and links the
# module root's entries beside it, and cargo walks LOGICAL parents to find a
# crate's workspace, so the patch paths must (1) stay inside the module and
# (2) pass through the libs root before any other Cargo.toml: a copy of the
# repository (tools/sync-orderly-crates.sh), nested one level down because
# the splicer honours .bazelignore for top-level names (the example's Bazel
# must not see the crates' packages, and a nested ignore entry is invisible
# to the splicer).
echo "tools/basable-crates" >> "$fresh/.bazelignore"
# Cargo resolves a path dependency under an already-loaded workspace's root
# to THAT workspace (the example's, which inherits nothing the crates need)
# unless the workspace excludes the path; then it reads the crates' own
# `package.workspace` root, where `workspace = true` resolves.
sed -i.bak 's#^resolver = "2"$#resolver = "2"\nexclude = ["tools/basable-crates"]#' "$fresh/Cargo.toml"
rm -f "$fresh/Cargo.toml.bak"
cat >> "$fresh/Cargo.toml" <<'EOF'

# basable:local-crates-begin — the workspace crates through the tools/basable-crates copy, until the release publishes them
[patch.crates-io]
basable-core = { path = "tools/basable-crates/crates/basable-core" }
basable-publicid = { path = "tools/basable-crates/crates/basable-publicid" }
basable-db = { path = "tools/basable-crates/crates/basable-db" }
basable-testkit = { path = "tools/basable-crates/crates/basable-testkit" }
basable-processingobject = { path = "tools/basable-crates/crates/basable-processingobject" }
basable-processingobject-testkit = { path = "tools/basable-crates/crates/basable-processingobject-testkit" }
basable-externaleffect = { path = "tools/basable-crates/crates/basable-externaleffect" }
basable-effecttest = { path = "tools/basable-crates/crates/basable-effecttest" }
basable-config = { path = "tools/basable-crates/crates/basable-config" }
basable-messenger = { path = "tools/basable-crates/crates/basable-messenger" }
basable-messenger-gen = { path = "tools/basable-crates/crates/basable-messenger-gen" }
basable-connect = { path = "tools/basable-crates/crates/basable-connect" }
basable-auth = { path = "tools/basable-crates/crates/basable-auth" }
basable-pubsub = { path = "tools/basable-crates/crates/basable-pubsub" }
basable-app = { path = "tools/basable-crates/crates/basable-app" }
basable-protoc-gen-buffa = { path = "tools/basable-crates/crates/basable-protoc-gen-buffa" }
# basable:local-crates-end
EOF

# The render replaces the tree, so a file the templates no longer produce
# goes; the lockfiles and the crates copy are the build's, not the render's.
mkdir -p "$out"
rsync -a --delete \
  --exclude Cargo.lock --exclude Cargo.Bazel.lock --exclude MODULE.bazel.lock \
  --exclude 'bazel-*' --exclude tools/basable-crates \
  "$fresh/" "$out/"
"$here/tools/sync-orderly-crates.sh"

# Repin: a render that changes a dependency (or a crates change the copy
# brings in) must land in the committed lockfiles, which the CI builds
# against exactly as a tenant's does, without repinning.
(cd "$out" && CARGO_BAZEL_REPIN=1 bazel fetch //...)
echo "rendered examples/orderly with the local-crates patch"
