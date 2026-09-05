#!/usr/bin/env bash
# Run cargo-mutants; missed.txt governs the verdict even after a timeout.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=scripts/mutants-common.sh
. "$SCRIPT_DIR/mutants-common.sh"
OUT_DIR="$MUTANTS_OUT_DIR"
mkdir -p "$OUT_DIR"

# Keep per-job tree copies beside the checkout, off system tmpfs and outside target/.
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
export TMPDIR="${DREP_MUTANTS_TMPDIR:-${ROOT}.mutants-tmp}"
mkdir -p "$TMPDIR"

# Recoverably remove only immediate scratch directories; never follow descendants.
cleanup_mutation_scratch() {
  local trash_command=()
  if command -v trash >/dev/null 2>&1; then
    trash_command=(trash)
  elif command -v gio >/dev/null 2>&1; then
    trash_command=(gio trash)
  else
    echo 'trash or gio is required for recoverable mutation scratch cleanup' >&2
    return 1
  fi

  while IFS= read -r -d '' scratch; do
    [[ -e "$scratch" ]] || continue
    "${trash_command[@]}" "$scratch"
  done < <(
    find "$TMPDIR" -mindepth 1 -maxdepth 1 -type d \
      \( -name 'cargo-mutants-*.tmp' -o -name 'octoroute-diff-test-*' \) -print0
  )
}

self_and_ancestors() {
  local pid=$$
  while [[ "$pid" -gt 1 ]]; do
    printf '%s ' "$pid"
    pid="$(ps -o ppid= -p "$pid" 2>/dev/null | tr -d ' ')"
    [[ -n "$pid" ]] || break
  done
}

# Timed-out kill-path mutants can orphan busy fixtures. Exclude our ancestors.
reap_mutation_orphans() {
  command -v pgrep >/dev/null 2>&1 || return 0
  local protected pid
  protected=" $(self_and_ancestors) "
  for pid in $(pgrep -f "$TMPDIR" 2>/dev/null || true); do
    [[ "$protected" == *" $pid "* ]] && continue
    kill -9 "$pid" 2>/dev/null || true
  done
}

# Sweep interrupted runs on entry and preserve the verdict across EXIT cleanup.
reap_mutation_orphans
cleanup_mutation_scratch
# shellcheck disable=SC2329 # invoked by the EXIT trap below
finish_mutation_run() {
  local mutants_exit_status=$?
  local cleanup_status=0
  reap_mutation_orphans
  cleanup_mutation_scratch || cleanup_status=$?
  if [[ "$mutants_exit_status" -ne 0 ]]; then
    exit "$mutants_exit_status"
  fi
  exit "$cleanup_status"
}
trap finish_mutation_run EXIT

# Concurrent suites need timeout headroom; jobs are measured rather than CPU-count derived.
# The runner retains the remote lock; cargo and arbitrary test descendants must not.
cargo mutants -j "${MUTANTS_JOBS:-4}" --no-shuffle --minimum-test-timeout 60 \
  --output "$OUT_DIR" "$@" 9<&- && status=0 || status=$?

MISSED="$OUT_DIR/mutants.out/missed.txt"

# Exit 3 takes precedence over surviving mutants in cargo-mutants. Check survivors first.
if [ -s "$MISSED" ]; then
  echo "mutants survived - a surviving mutant is a test that cannot tell" >&2
  echo "correct behaviour from incorrect. Fix the test, never the mutant list." >&2
  cat "$MISSED" >&2
  exit 2
fi

if [ "$status" -eq 3 ]; then
  echo "note: mutants timed out with none missed; a hang is detection, not a failure"
  exit 0
fi

exit "$status"
