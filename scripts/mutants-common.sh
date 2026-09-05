#!/usr/bin/env bash
# Shared output location for verdicts, staged diffs, and mirrored results.

MUTANTS_OUT_DIR="${MUTANTS_OUT_DIR:-target/mutants}"

# Tests can consume fixtures outside src/, so qualify the complete tracked tree.
require_matching_index() {
  if ! git diff --quiet -- || [ -n "$(git ls-files --others --exclude-standard)" ]; then
    echo 'mutation gate requires the working tree to match the index, without untracked inputs' >&2
    echo 'stage intended changes or isolate the commit; no files have been altered' >&2
    return 1
  fi
}
