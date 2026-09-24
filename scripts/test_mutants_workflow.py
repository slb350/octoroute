"""Check the mutation scripts without contacting a remote host.

The scripts are drep's (`~/dev/drep/scripts/`), copied with this repository's
ai-1 role, lock and workspace, plus Octoroute's orphan reaper and index gate.
"""

import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ROLE = "octoroute-mutants"
FAKE_CARGO = """#!/usr/bin/env bash
set -euo pipefail
printf '%s\\n' "$@" > "$FAKE_ARGS"
mkdir -p "$TMPDIR/cargo-mutants-trap.tmp/nested"
mkdir -p "$MUTANTS_OUT_DIR/mutants.out"
printf '%s' "${FAKE_MISSED:-}" > "$MUTANTS_OUT_DIR/mutants.out/missed.txt"
exit "${FAKE_EXIT:-0}"
"""


def without_comments(relative):
    lines = (ROOT / relative).read_text().splitlines()
    return "\n".join(line for line in lines if not line.lstrip().startswith("#"))


def executable(path, text):
    path.write_text(text)
    path.chmod(0o755)


def with_fake_bin(root, environment):
    """The environment with root/bin ahead of the real PATH."""
    return {**environment, "PATH": str(root / "bin") + os.pathsep + os.environ["PATH"]}


def run(command, environment, cwd=None):
    return subprocess.run(
        command,
        cwd=cwd,
        env=environment,
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )


class ScriptContractTests(unittest.TestCase):
    def test_remote_defaults_to_this_repositorys_ai1_role(self):
        script = without_comments("scripts/mutants-remote.sh")
        for expected in [
            'HOST="${DREP_MUTANTS_HOST:-steve@192.168.68.88}"',
            f'REMOTE_DIR="${{DREP_MUTANTS_DIR:-.cache/{ROLE}/$(basename "$PWD")}}"',
            f"DREP_MUTANTS_REMOTE_HOST_LOCK:-/srv/ci/fleet/{ROLE}/home/host.lock",
            f"AI1_CI_ROLE={ROLE}",
        ]:
            self.assertIn(expected, script)
        transport = without_comments("scripts/mutants-ai1-transport.sh")
        self.assertIn(f"  {ROLE}) return 0 ;;", transport)
        for retired in ["strix", "homelab-1.", "homelab-2", "legion"]:
            self.assertNotIn(retired, script.lower(), "every mutation workload runs on ai-1")

    def test_remote_full_sweep_passes_no_phantom_argument(self):
        script = without_comments("scripts/mutants-remote.sh")
        self.assertIn("for remote_arg in", script)
        self.assertIn("shift 6", script)
        self.assertIn('./scripts/mutants-run.sh "$@"', script)
        self.assertNotIn("$(printf '%q ' \"$@\")", script)

    def test_remote_session_owns_sync_run_and_fresh_result_mirroring(self):
        script = without_comments("scripts/mutants-remote.sh")
        for expected in [
            'exec 9>"$host_lock"',
            'flock -E 75 -w "$wait_seconds" 9',
            "DREP_MUTANTS_HOST_LOCK_WAIT_SECONDS",
            "DREP_MUTANTS_RSYNC_TIMEOUT_SECONDS",
            '--timeout="$RSYNC_IO_TIMEOUT_SECONDS"',
            'mkfifo "$CONTROL_IN" "$CONTROL_OUT"',
            "mutants-lock-ready:$RUN_TOKEN",
            "mutants-run-finished:$RUN_TOKEN",
            "DREP_MUTANTS_RESULT_TOKEN",
            ".run-token",
            "printf 'mirrored\\n'",
            'kill "$REMOTE_SESSION_PID"',
            'wait "$REMOTE_SESSION_PID"',
        ]:
            self.assertIn(expected, script)
        self.assertLess(
            script.index("REMOTE_SESSION_PID=$!"),
            script.index("rsync -a --delete"),
            "the host lock must be held before source synchronization begins",
        )

    def test_runner_holds_the_host_lock_and_keeps_it_from_cargo(self):
        script = without_comments("scripts/mutants-run.sh")
        for expected in [
            "DREP_MUTANTS_HOST_LOCK",
            "validate_mutants_host_lock_wait_seconds mutants-run",
            "flock -w",
            'exec 9>"$HOST_LOCK"',
            "DREP_MUTANTS_RESULT_TOKEN",
            "$OUT_DIR/mutants.out",
            "$OUT_DIR/.run-token",
        ]:
            self.assertIn(expected, script)
        cargo = next(
            line for line in script.splitlines() if "--output \"$OUT_DIR\"" in line
        )
        self.assertIn(
            "9<&-", cargo, "a fixture that outlives its mutant must not hold the host lock"
        )

    def test_host_lock_wait_policy_has_one_definition(self):
        common = without_comments("scripts/mutants-common.sh")
        self.assertIn(
            'MUTANTS_HOST_LOCK_WAIT_SECONDS="${DREP_MUTANTS_HOST_LOCK_WAIT_SECONDS:-1800}"',
            common,
        )
        self.assertIn("validate_mutants_host_lock_wait_seconds()", common)
        for name in ["mutants-remote", "mutants-run"]:
            script = without_comments(f"scripts/{name}.sh")
            self.assertIn(f"validate_mutants_host_lock_wait_seconds {name}", script)
            self.assertNotIn(
                'HOST_LOCK_WAIT_SECONDS="${DREP_MUTANTS_HOST_LOCK_WAIT_SECONDS:-1800}"',
                script,
            )

    def test_ai1_transport_fails_closed_without_bypassing_the_sandbox(self):
        result = subprocess.run(
            ["bash", "scripts/test_ai1_transport.sh"],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_scratch_copies_stay_off_the_tmpfs(self):
        script = without_comments("scripts/mutants-run.sh")
        self.assertIn('export TMPDIR="${DREP_MUTANTS_TMPDIR:-${ROOT}.mutants-tmp}"', script)
        self.assertFalse(
            any("TMPDIR=" in line and "/tmp" in line for line in script.splitlines())
        )
        self.assertNotIn("rm ", script)
        self.assertNotIn("rmdir ", script)


class WorkflowTests(unittest.TestCase):
    @staticmethod
    def jobs():
        body = (ROOT / ".github/workflows/ci.yml").read_text().split("\njobs:\n", 1)[1]
        jobs, name = {}, None
        for line in body.splitlines():
            header = re.match(r"^  ([\w-]+):$", line)
            if header:
                name = header.group(1)
                jobs[name] = []
            elif name:
                jobs[name].append(line)
        return {name: "\n".join(lines) for name, lines in jobs.items()}

    def test_only_mutation_runs_self_hosted_and_never_for_fork_pull_requests(self):
        jobs = self.jobs()
        mutants = jobs.pop("mutants")
        for expected in [
            "    runs-on: [self-hosted, linux, x64, homelab-ai-1, octoroute-mutants]",
            "    if: needs.mutation-policy.outputs.run == 'true' && (github.event_name != 'pull_request' || github.event.pull_request.head.repo.full_name == github.repository)",
            "          persist-credentials: false",
            "          name: mutation-repair\n",
        ]:
            self.assertIn(expected, mutants)
        # cargo-mutants never reads target/, so a cache is dead weight; on a persistent runner
        # it would also prune host-installed cargo binaries.
        for removed in ["--shard", "matrix", "actions/cache@", "Swatinem/rust-cache@"]:
            self.assertNotIn(removed, mutants)
        for name, job in jobs.items():
            self.assertIn("    runs-on: ubuntu-latest", job, name)


class RunScriptTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="octoroute mutants ")
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.scratch = self.root / "scratch root"
        (self.root / "bin").mkdir()
        executable(self.root / "bin/cargo", FAKE_CARGO)
        self.environment = with_fake_bin(
            self.root,
            {key: value for key, value in os.environ.items() if not key.startswith("DREP_MUTANTS_")},
        )
        self.environment.update(
            DREP_MUTANTS_TMPDIR=str(self.scratch),
            MUTANTS_OUT_DIR=str(self.root / "mutation output"),
            FAKE_ARGS=str(self.root / "cargo-args"),
        )

    def run_script(self, command=None, **environment):
        return run(
            command or ["bash", str(ROOT / "scripts/mutants-run.sh")],
            {**self.environment, **environment},
        )

    def test_cleanup_is_destructive_only_inside_its_prefix(self):
        stale = self.scratch / "cargo-mutants-stale.tmp/nested"
        adjacent = self.scratch / "cargo-mutants-stale.tmp.keep"
        outside = self.root / "outside"
        for directory in [stale, adjacent, outside]:
            directory.mkdir(parents=True)
        (adjacent / "keep").write_text("keep")
        (outside / "keep").write_text("keep")
        (self.scratch / "cargo-mutants-link.tmp").symlink_to(outside)

        result = self.run_script()

        self.assertEqual(result.returncode, 0, result.stderr)
        for removed in ["cargo-mutants-stale.tmp", "cargo-mutants-trap.tmp", "cargo-mutants-link.tmp"]:
            self.assertFalse((self.scratch / removed).exists(), removed)
        self.assertTrue((adjacent / "keep").exists())
        self.assertTrue((outside / "keep").exists())
        arguments = (self.root / "cargo-args").read_text().splitlines()
        floors = [
            value
            for flag, value in zip(arguments, arguments[1:])
            if flag == "--minimum-test-timeout"
        ]
        self.assertEqual(floors, ["120"])

    def test_verdict_prioritizes_survivors_over_timeouts(self):
        for status, missed, expected in [
            (0, "", 0),
            (3, "", 0),
            (3, "survivor", 2),
            (0, "survivor", 2),
            (7, "", 7),
        ]:
            with self.subTest(status=status, missed=missed):
                result = self.run_script(FAKE_EXIT=str(status), FAKE_MISSED=missed)
                self.assertEqual(result.returncode, expected, result.stderr)

    def test_reaps_fixture_orphans_but_spares_its_own_callers(self):
        self.scratch.mkdir()
        orphan = subprocess.Popen(
            [sys.executable, "-c", "import time; time.sleep(300)", str(self.scratch)]
        )
        self.addCleanup(orphan.kill)
        # The caller's own command line names the scratch root; it must survive.
        caller = [
            "bash",
            "-c",
            'bash "$1"; echo "caller survived: $?"',
            "caller",
            str(ROOT / "scripts/mutants-run.sh"),
            str(self.scratch),
        ]

        result = self.run_script(caller)

        self.assertIn("caller survived: 0", result.stdout, result.stderr)
        self.assertEqual(orphan.wait(timeout=10), -signal.SIGKILL, "the orphaned fixture must be reaped")


class StagedGateTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        for relative in ["scripts", ".githooks", "bin"]:
            (self.root / relative).mkdir()
        for name in ["mutants-common.sh", "mutants-staged.sh"]:
            shutil.copy2(ROOT / "scripts" / name, self.root / "scripts" / name)
        shutil.copy2(ROOT / ".githooks/pre-commit", self.root / ".githooks/pre-commit")
        executable(
            self.root / "scripts/mutants-remote.sh",
            '#!/usr/bin/env bash\nprintf "remote:%s extra:%s\\n" "$*" "$MUTANTS_EXTRA_FILES" >> events\n',
        )
        executable(
            self.root / "bin/cargo",
            '#!/bin/sh\nprintf "cargo:%s\\n" "$*" >> events\nexit 1\n',
        )
        self.environment = with_fake_bin(self.root, os.environ)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "Workflow fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        self.git("config", "commit.gpgsign", "false")
        (self.root / "source.rs").write_text("fn source() {}\n")
        (self.root / ".gitignore").write_text("target/\nevents\n")
        self.git("add", "--", "source.rs", "scripts", ".githooks", "bin", ".gitignore")
        self.git("commit", "-qm", "fixture")

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root, text=True)

    def run_script(self, path):
        return run(["bash", path], self.environment, cwd=self.root)

    def events(self):
        path = self.root / "events"
        return path.read_text().splitlines() if path.exists() else []

    def test_refuses_unstaged_or_untracked_inputs_without_touching_them(self):
        source = self.root / "source.rs"
        source.write_text("fn staged() {}\n")
        self.git("add", "--", "source.rs")
        for kind in ["unstaged", "untracked"]:
            with self.subTest(kind=kind):
                path = source if kind == "unstaged" else self.root / "new_test.rs"
                path.write_text("#[test]\nfn unstaged_test() {}\n")
                result = self.run_script("scripts/mutants-staged.sh")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("index", result.stderr)
                self.assertEqual(path.read_text(), "#[test]\nfn unstaged_test() {}\n")
                self.assertEqual(self.git("show", ":source.rs"), "fn staged() {}\n")
                self.assertEqual(self.events(), [])
                if kind == "unstaged":
                    source.write_text("fn staged() {}\n")
                else:
                    path.unlink()

    def test_dispatches_a_matching_index_to_the_remote_run(self):
        (self.root / "source.rs").write_text("fn staged() {}\n")
        self.git("add", "--", "source.rs")
        result = self.run_script("scripts/mutants-staged.sh")
        self.assertEqual(result.returncode, 0, result.stderr)
        diff = "target/mutants/staged.diff"
        self.assertIn("+fn staged() {}", (self.root / diff).read_text())
        self.assertEqual(self.events(), [f"remote:--in-diff {diff} extra:{diff}"])

    def test_hook_refuses_formatting_failure_without_rewriting_or_staging(self):
        before = self.git("write-tree")
        result = self.run_script(".githooks/pre-commit")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.events(), ["cargo:fmt --all -- --check"])
        self.assertEqual(self.git("write-tree"), before)


if __name__ == "__main__":
    unittest.main()
