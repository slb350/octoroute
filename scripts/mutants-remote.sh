#!/usr/bin/env bash
# Offload mutation testing; fall back locally when the SSH probe fails.
# Remote jobs retain the CPU ceiling, low priority, warm target cache, and lock.
# Overrides (legacy DREP names remain supported):
#   OCTOROUTE_MUTANTS_HOST, DREP_MUTANTS_DIR, DREP_MUTANTS_REMOTE=0
#   MUTANTS_JOBS, MUTANTS_LOCAL_JOBS, MUTANTS_EXTRA_FILES (repo-relative, no spaces)
#   MUTANTS_OUT_DIR (repository-relative directory)
#   OCTOROUTE_MUTANTS_CPUQUOTA, OCTOROUTE_MUTANTS_CPUWEIGHT, OCTOROUTE_MUTANTS_NICE

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
# shellcheck source=scripts/mutants-common.sh
. scripts/mutants-common.sh

HOST="${OCTOROUTE_MUTANTS_HOST:-homelab-1.local}"
REMOTE_DIR="${DREP_MUTANTS_DIR:-ci/$(basename "$PWD")}"
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

# Only an unreachable host selects local fallback; sync failures must still fail.
if ! ssh -o BatchMode=yes -o ConnectTimeout=5 "$HOST" true 2>/dev/null; then
  echo "warning: $HOST is unreachable - running the mutation sweep locally instead." >&2
  echo "         This will use this machine's CPU for the duration." >&2
  run_local "$@"
fi

echo "mutants: running on $HOST (-j $JOBS, CPUQuota=$CPUQUOTA), results mirrored back to $MUTANTS_OUT_DIR"

# The entrance lock admits one owner. The active lock also travels with rsync,
# keeping the checkout exclusive while a disconnected transfer winds down.
read -r -d '' MUTANTS_LOCK_SCRIPT <<'REMOTE_SCRIPT' || true
set -euo pipefail
export PATH=$HOME/.cargo/bin:$PATH
directory=$1 output=$2 jobs=$3 quota=$4 weight=$5 niceness=$6
shift 6
root=$(python3 -c '
import pathlib, sys, tomllib
home = pathlib.Path.home().resolve()
root = (home / sys.argv[1]).resolve()
if root == home or root in home.parents:
    sys.exit("refusing mutation sync into home or its ancestors")
if root.exists() and (not root.is_dir() or any(root.iterdir())):
    try:
        manifest = tomllib.loads((root / "Cargo.toml").read_text())
    except (OSError, ValueError):
        manifest = {}
    if manifest.get("package", {}).get("name") != "octoroute":
        sys.exit("refusing mutation sync into a nonempty non-Octoroute directory")
print(root)
' "$directory")
lease_root=$root.mutants-lease
mkdir -p -- "$root/$output" "$lease_root"
cd -- "$root"
exec 8< "$lease_root"
flock -w 1800 8
# Invalidate any killed owner's token before waiting out its active transfers.
: > "$lease_root/token"
exec 9>> "$lease_root/active"
flock -w 1800 9
token=$(python3 -c 'import secrets; print(secrets.token_hex(16))')
printf '%s\n' "$token" > "$lease_root/token"
# Conversion is protected by the entrance lock, so it need not be atomic.
flock -s 9
runner= watcher=
stop_runner() {
  if [ -n "$watcher" ]; then
    kill -TERM "$watcher" 2>/dev/null || true
    wait "$watcher" 2>/dev/null || true
  fi
  if [ -n "$runner" ]; then
    kill -TERM -- "-$runner" 2>/dev/null || true
    for _ in {1..50}; do
      kill -0 -- "-$runner" 2>/dev/null || break
      sleep 0.1
    done
    kill -KILL -- "-$runner" 2>/dev/null || true
    wait "$runner" 2>/dev/null || true
  fi
}
finish() {
  : > "$lease_root/token"
  stop_runner
}
trap finish EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
printf 'octoroute-lock-acquired:%s:%s\n' "$token" "$root"
IFS= read -r command && [ "$command" = run ] || exit 1

# CPUQuota caps use across cgroup slices; idle I/O priority needs no delegation.
if systemd-run --user --scope --quiet --collect /bin/true >/dev/null 2>&1; then
  limit=(systemd-run --user --scope --quiet --collect "--nice=$niceness"
    "--property=CPUWeight=$weight" "--property=CPUQuota=$quota" --)
else
  echo "mutants: no user systemd scope on $(hostname); limiting with nice only," >&2
  echo "         which cannot cap CPU across cgroup slices." >&2
  limit=(nice -n "$niceness")
fi
if command -v ionice >/dev/null 2>&1; then limit=(ionice -c3 "${limit[@]}"); fi
MUTANTS_JOBS=$jobs MUTANTS_OUT_DIR=$output setsid "${limit[@]}" ./scripts/mutants-run.sh "$@" 8<&- >&2 &
runner=$!
# A separate reader observes disconnects even while Bash waits on the runner.
(
  trap - EXIT HUP INT TERM
  IFS= read -r command || true
  stop_runner
) <&0 8<&- &
watcher=$!
status=0
wait "$runner" || status=$?
stop_runner
runner= watcher=
printf 'octoroute-sweep-status:%s\n' "$status"
IFS= read -r command && [ "$command" = release ] || exit 1
REMOTE_SCRIPT
export MUTANTS_LOCK_SCRIPT HOST REMOTE_DIR MUTANTS_OUT_DIR JOBS CPUQUOTA CPUWEIGHT NICE

# Coordinate rsync with the same SSH session on Bash 3.2 and newer. On failure,
# stop any local transfer first, then send EOF and await remote runner cleanup.
exec python3 - "$@" <<'PYTHON'
import os
import re
import shlex
import signal
import subprocess
import sys

environment = os.environ
remote_command = shlex.join([
    "bash", "-c", environment["MUTANTS_LOCK_SCRIPT"], "mutants-lock",
    *(environment[name] for name in ["REMOTE_DIR", "MUTANTS_OUT_DIR", "JOBS", "CPUQUOTA", "CPUWEIGHT", "NICE"]),
    *sys.argv[1:],
])
lease = None
transfer = None

def interrupt(number, _frame):
    raise SystemExit(128 + number)

def stop(process):
    if process is not None and process.poll() is None:
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            return
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()

def rsync(*arguments):
    global transfer
    transfer = subprocess.Popen(
        ["rsync", "--rsync-path=" + rsync_command, *arguments], start_new_session=True,
    )
    while transfer.poll() is None:
        if lease.poll() is not None:
            raise RuntimeError("remote mutation lock was lost during transfer")
        try:
            transfer.wait(timeout=0.1)
        except subprocess.TimeoutExpired:
            pass
    return transfer.returncode

for signum in [signal.SIGTERM, signal.SIGINT, signal.SIGHUP]:
    signal.signal(signum, interrupt)
try:
    output = environment["MUTANTS_OUT_DIR"]
    if not re.fullmatch(r"[\w.-]+(?:/[\w.-]+)*", output, re.ASCII) or any(
        part in {".", ".."} for part in output.split("/")
    ):
        raise ValueError("MUTANTS_OUT_DIR must be a simple repository-relative directory")
    extras = environment.get("MUTANTS_EXTRA_FILES", "").split()
    if any(path.startswith("/") or ".." in path.split("/") for path in extras):
        raise ValueError("MUTANTS_EXTRA_FILES must stay within the repository")
    lease = subprocess.Popen(
        ["ssh", "-o", "BatchMode=yes", "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3",
         environment["HOST"], remote_command],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, start_new_session=True,
    )
    ready = lease.stdout.readline().rstrip()
    if not ready.startswith("octoroute-lock-acquired:"):
        raise RuntimeError("remote mutation lock could not be acquired")
    _, token, root = ready.split(":", 2)
    destination = environment["HOST"] + ":" + root + "/"
    rsync_command = shlex.join([
        "bash", "-c", '''
set -euo pipefail
exec 9< "$1/active"
flock -s -w 1800 9
exec 8< "$1"
# A free entrance lock means the owner died, even if its token was not cleared.
if flock -s -n 8; then exit 1; fi
[ "$(cat "$1/token")" = "$2" ] || exit 1
shift 2
exec rsync "$@" 8<&-
''', "mutants-transfer", root + ".mutants-lease", token,
    ])
    status = rsync(
        "-a", "--delete", "--force", "--delete-excluded", "--filter=P /target", "--mkpath",
        "--filter=P /" + output, "--exclude", "/" + output,
        *(argument for name in ["target", "mutants.out*", ".git", "node_modules", "dist", "build", ".octoroute", ".env*"]
          for argument in ["--exclude", name]), "./", destination,
    )
    if status:
        raise SystemExit(status)
    if extras:
        status = rsync("-aR", "--mkpath", "--", *extras, destination)
        if status:
            raise SystemExit(status)
    lease.stdin.write("run\n")
    lease.stdin.flush()
    for line in lease.stdout:
        if line.startswith("octoroute-sweep-status:"):
            status = int(line.partition(":")[2])
            break
        print(line, end="", flush=True)
    else:
        raise RuntimeError("remote mutation session ended without a verdict")
    artifacts = rsync("-a", "--mkpath", destination + output + "/", "./" + output + "/")
    lease.stdin.write("release\n")
    lease.stdin.flush()
    exit_status = lease.wait(timeout=10)
    raise SystemExit(status or artifacts or exit_status)
finally:
    stop(transfer)
    if lease is not None:
        try:
            lease.stdin.close()
            lease.wait(timeout=10)
        except (BrokenPipeError, subprocess.TimeoutExpired):
            stop(lease)
PYTHON
