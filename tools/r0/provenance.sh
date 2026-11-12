#!/bin/sh
# Print the environment that ties an oracle run on a rsynced test host to the sending
# tree: `eval "$(tools/r0/provenance.sh)"` on the sending side, then pass the two
# variables through ssh. The diff hash covers tracked changes only (git diff HEAD).
cd "$(dirname "$0")/../.." || exit 1
c=$(git rev-parse --short HEAD)
if [ -n "$(git status --porcelain)" ]; then c="$c+dirty"; fi
d=$(git diff HEAD | sha256sum | cut -d' ' -f1)
echo "PRIM_R0_COMMIT=$c PRIM_R0_DIFF_SHA=$d"
