#!/usr/bin/env bash
# Mutate staged Rust lines only when the working tree matches the index.

set -euo pipefail

# shellcheck source=scripts/mutants-common.sh
. "$(dirname "$0")/mutants-common.sh"
require_matching_index
DIFF="$MUTANTS_OUT_DIR/staged.diff"
mkdir -p "$MUTANTS_OUT_DIR"

git diff --cached -- '*.rs' > "$DIFF"

if [ ! -s "$DIFF" ]; then
  echo "no staged Rust changes; nothing to mutate"
  exit 0
fi

# target/ is excluded from the remote sync; explicitly transfer the staged diff.
MUTANTS_EXTRA_FILES="$DIFF" exec ./scripts/mutants-remote.sh --in-diff "$DIFF"
