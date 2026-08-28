#!/usr/bin/env bash
#
# Run the mutation sweep on a bigger machine over SSH, then report its verdict
# here.
#
# Mutation testing is the most CPU-hungry gate in this repo: every mutant is a
# full build plus a full test run, and the hook fires on a laptop the developer
# is still using. Offloading it costs this machine nothing.
#
# homelab-1 is shared: it also runs self-hosted GitHub Actions runners, and those
# are system.slice services while an ssh session lands in user.slice. cpu.weight
# only arbitrates between siblings, so a weight on this run competes with other
# *user* work and does nothing at all against those runners. CPUQuota is the
# knob that crosses the slice boundary, because it is an absolute cpu.max
# ceiling rather than a share. Both are set: the quota is what stops the sweep
# pegging a box someone else's CI depends on, the weight and nice make it yield
# politely among peers. All three apply to a --user scope without root.
#
# The ceiling costs idle capacity by design. A sweep that finishes sooner but
# starves a CI runner is the worse trade on a machine that is not ours alone.
#
# More jobs is not better, and -j 4 on a 32-thread box is not a typo. Each job
# gets its own copy of the tree *including* target/, which is how its builds
# stay warm - so raising -j multiplies a multi-gigabyte copy before any mutant
# is tested. Measured on that same scope: 38s at -j 4, 54s at -j 8, 72s at
# -j 16, never above 2200% CPU of a possible 3200. The run is I/O-bound on the
# copy, not CPU-bound. Timings drift by a third between runs on a shared box,
# so treat MUTANTS_JOBS as a knob to measure, not a number to raise on
# principle.
#
# The verdict rule is NOT duplicated here. This script syncs, invokes
# scripts/mutants-run.sh on the remote, and propagates its exit code - so the
# hook, CI and the remote sweep cannot disagree about what counts as a failure.
#
# Falls back to a local run, loudly, when the host is unreachable. A commit gate
# that silently skips itself because the LAN blipped is worse than a slow one.
#
#   OCTOROUTE_MUTANTS_HOST    ssh target (default: homelab-1.local)
#   DREP_MUTANTS_DIR     remote path, $HOME-relative (default: ci/<repo name>)
#   DREP_MUTANTS_REMOTE  0 to force a local run
#   MUTANTS_JOBS         -j for the remote run (default: 4)
#   MUTANTS_LOCAL_JOBS   -j for a local or fallback run (default: 4)
#   MUTANTS_EXTRA_FILES  repo-relative paths this run needs that the sync
#                        would otherwise skip (space-separated, no spaces in
#                        the paths themselves)
#   OCTOROUTE_MUTANTS_CPUQUOTA   absolute CPU ceiling (default: 500%, i.e. five
#                        cores). The one limit that applies across cgroup slices.
#   OCTOROUTE_MUTANTS_CPUWEIGHT  share among user.slice peers (default: 20 of 100)
#   OCTOROUTE_MUTANTS_NICE       scheduler niceness (default: 19)
#
# I/O politeness is ionice, not IOWeight: systemd accepts IOWeight on a user
# scope but user@.service is delegated only cpu, memory and pids, so the io
# controller never sees it and the property is silently inert. `ionice -c3` is
# per-process and applies regardless of delegation. This matters because the run
# is I/O-bound on per-job tree copies, not CPU-bound.

set -euo pipefail

# `git rev-parse`, not `dirname "$0"/..`: the same answer install.sh already
# uses, and it does not care whether the script was reached through a symlink,
# a relative path or PATH.
cd "$(git rev-parse --show-toplevel)"
# shellcheck source=scripts/mutants-common.sh
. scripts/mutants-common.sh

HOST="${OCTOROUTE_MUTANTS_HOST:-homelab-1.local}"
REMOTE_DIR="${DREP_MUTANTS_DIR:-ci/$(basename "$PWD")}"
REMOTE="$HOST:$REMOTE_DIR"
JOBS="${MUTANTS_JOBS:-4}"
CPUQUOTA="${OCTOROUTE_MUTANTS_CPUQUOTA:-500%}"
CPUWEIGHT="${OCTOROUTE_MUTANTS_CPUWEIGHT:-20}"
NICE="${OCTOROUTE_MUTANTS_NICE:-19}"

run_local() {
  MUTANTS_JOBS="${MUTANTS_LOCAL_JOBS:-4}" exec ./scripts/mutants-run.sh "$@"
}

if [ "${DREP_MUTANTS_REMOTE:-1}" = "0" ]; then
  run_local "$@"
fi

# A bare probe rather than letting the first rsync fail and reading its exit
# code: "the host is down, here is what I am doing instead" is the message the
# developer needs, and inferring it from an rsync failure would also swallow a
# full disk or an unwritable directory as "unreachable". One handshake, ~145ms,
# against a run measured in minutes.
if ! ssh -o BatchMode=yes -o ConnectTimeout=5 "$HOST" true 2>/dev/null; then
  echo "warning: $HOST is unreachable - running the mutation sweep locally instead." >&2
  echo "         This will use this machine's CPU for the duration." >&2
  run_local "$@"
fi

echo "mutants: running on $HOST (-j $JOBS, CPUQuota=$CPUQUOTA), results mirrored back to $MUTANTS_OUT_DIR"

# --mkpath creates the destination directory as part of the transfer, which is
# an `ssh mkdir -p` round trip saved on every commit.
#
# --delete so a file deleted locally cannot linger and be mutated remotely.
# target/ is excluded in both directions: the remote keeps its own, which is
# what makes the second run incremental. The cache directories are excluded
# because they are 64MB of this checkout that no mutation run reads, re-diffed
# on every commit against a Rust payload of about 1MB. Credentials are excluded
# because nothing in the suite reads them and they have no business on another
# host.
# --delete alone leaves the remote tree stale, and the sweep then tests a tree
# the commit does not have. An excluded name *inside* a directory protects that
# directory from removal, so `docs/api/build/html` kept `docs/api` alive after
# the commit that deleted it, and a test asserting the directory is gone failed
# on the remote while passing here. --force is not enough - it deletes
# non-empty directories, not protected ones.
#
# So: --delete-excluded, which removes the excluded leftovers too, with an
# explicit `P` (protect) rule for `/target`. That directory is the build cache
# this whole offload exists to reuse - 1.7GB of it - and --delete-excluded
# would otherwise take it, turning every run into a cold build.
rsync -a --delete --force --delete-excluded --filter='P /target' --mkpath \
  --exclude target --exclude 'mutants.out*' \
  --exclude .git --exclude node_modules \
  --exclude dist --exclude build --exclude .octoroute \
  --exclude '.env*' \
  ./ "$REMOTE/"

# Files the run needs that the sync above skipped - in practice the staged diff,
# which mutants-staged.sh writes under the excluded target/. Named by the caller
# rather than recovered by scanning "$@" for cargo-mutants' own flags: what
# belongs at this layer is "move these bytes", not that layer's argument
# grammar. -R recreates each path under the remote root, directories included.
if [ -n "${MUTANTS_EXTRA_FILES:-}" ]; then
  for extra in ${MUTANTS_EXTRA_FILES}; do
    case "$extra" in
      /*) echo "mutants-remote: MUTANTS_EXTRA_FILES must be repo-relative, got $extra" >&2
          exit 64 ;;
    esac
  done
  # shellcheck disable=SC2086  # word splitting is the interface: it is a list
  rsync -aR --mkpath ${MUTANTS_EXTRA_FILES} "$REMOTE/"
fi

# flock serialises two commits racing for the same remote tree; they would
# otherwise share one target/ and one results directory. -w so a stuck run
# cannot block a commit forever.
#
# `bash -s` rather than a quoted one-liner: the arguments are quoted with
# printf %q, which is bash's dialect, so the remote end must be bash whatever
# login shell the account uses.
status=0
REMOTE_ARGS=
if [ "$#" -gt 0 ]; then
  printf -v REMOTE_ARGS ' %q' "$@"
fi
# shellcheck disable=SC2087  # local expansion is the point: the remote dir, the
# job count and the %q-quoted arguments are all known here. \$HOME is escaped so
# it resolves there.
ssh -o BatchMode=yes "$HOST" bash -s <<EOF || status=$?
set -euo pipefail
export PATH=\$HOME/.cargo/bin:\$PATH
cd ~/'$REMOTE_DIR'
mkdir -p '$MUTANTS_OUT_DIR'

# Prefer a transient systemd scope: it is the only way to set an absolute CPU
# ceiling, and the ceiling is the part that protects other tenants. nice and
# ionice are the fallback for a host without a user manager - they are better
# than nothing, but they arbitrate within a cgroup rather than across slices, so
# say so rather than implying the same protection.
LIMIT=
if systemd-run --user --scope --quiet --collect /bin/true >/dev/null 2>&1; then
  LIMIT="systemd-run --user --scope --quiet --collect --nice=$NICE"
  LIMIT="\$LIMIT --property=CPUWeight=$CPUWEIGHT --property=CPUQuota=$CPUQUOTA --"
else
  echo "mutants: no user systemd scope on \$(hostname); limiting with nice only," >&2
  echo "         which cannot cap CPU across cgroup slices." >&2
  LIMIT="nice -n $NICE"
fi
# Idle I/O class in both paths: the systemd scope cannot express it, and the run
# spends most of its wall clock copying trees.
if command -v ionice >/dev/null 2>&1; then LIMIT="ionice -c3 \$LIMIT"; fi

# Unquoted on purpose: \$LIMIT is a command prefix, and an empty one must
# vanish rather than become an argument.
# shellcheck disable=SC2086
MUTANTS_JOBS=$JOBS flock -w 1800 '$MUTANTS_OUT_DIR' \$LIMIT ./scripts/mutants-run.sh$REMOTE_ARGS
EOF

# Mirror the results back so `missed.txt`, the logs and the diffs of surviving
# mutants can be read here, where the fix gets written.
rsync -a --mkpath "$REMOTE/$MUTANTS_OUT_DIR/" "$MUTANTS_OUT_DIR/" 2>/dev/null || true

exit "$status"
