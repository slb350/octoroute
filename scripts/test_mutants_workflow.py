"""Check the mutation scripts without contacting a remote host.

The scripts are drep's (`~/dev/drep/scripts/`), copied with this repository's ai-1 role and Octoroute's index gate.
"""

import itertools
import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ROLE = "octoroute-mutants"
FAKE_CARGO = """#!/usr/bin/env bash
set -euo pipefail
printf '%s\\n' "$@" > "$FAKE_ARGS"
mkdir -p "$TMPDIR/cargo-mutants-trap.tmp/nested" "$TMPDIR/.tmp-test-debris"
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


# perl that exits 0 when the lock file named by its argument could be taken now and 1 while another process holds it: the same kernel flock the mutation scripts take through perl, since macOS has no flock(1).
LOCK_PROBE = (
    "open(my $f, '>>', $ARGV[0]) or exit 2; exit(flock($f, LOCK_EX | LOCK_NB) ? 0 : 1)"
)


def lock_is_free(path):
    """Whether another process could take the lock at path right now."""
    return (
        subprocess.run(
            ["perl", "-MFcntl=:flock", "-e", LOCK_PROBE, str(path)], check=False
        ).returncode
        == 0
    )


def hold_lock(test, path, seconds="30"):
    """A process holding the lock at path until the test ends or seconds pass."""
    holder = subprocess.Popen(
        [
            "perl",
            "-MFcntl=:flock",
            "-MTime::HiRes=sleep",
            "-e",
            'open(my $f, ">>", $ARGV[0]) or die; flock($f, LOCK_EX) or die; $| = 1; print "locked\\n"; sleep $ARGV[1]',
            str(path),
            seconds,
        ],
        stdout=subprocess.PIPE,
        text=True,
    )
    test.addCleanup(holder.stdout.close)
    test.addCleanup(holder.wait)
    test.addCleanup(holder.kill)
    test.assertEqual(holder.stdout.readline(), "locked\n")
    return holder


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
            f"AI1_CI_ROLE={ROLE}",
            'REMOTE_DIR="$(remote_checkout_dir "$AI1_CI_ROLE")"',
        ]:
            self.assertIn(expected, script)
        for retired in ["strix", "homelab-1.", "homelab-2", "legion"]:
            self.assertNotIn(retired, script.lower())

    def test_remote_full_sweep_passes_no_phantom_argument(self):
        script = without_comments("scripts/mutants-remote.sh")
        self.assertIn("for remote_arg in", script)
        self.assertIn("shift 5", script)
        self.assertIn('./scripts/mutants-run.sh "$@"', script)
        self.assertNotIn("$(printf '%q ' \"$@\")", script)

    def test_remote_session_owns_sync_run_and_fresh_result_mirroring(self):
        script = without_comments("scripts/mutants-remote.sh")
        # The offloaded run takes the role's host lock, the one hosted sweeps take, and hands it to the run on descriptor 9.
        self.assertIn('exec 9>>"${DREP_MUTANTS_HOST_LOCK:?', script)
        self.assertIn('flock -E 75 -w "$wait_seconds" 9', script)
        self.assertNotIn("unset DREP_MUTANTS_HOST_LOCK", script)
        for bound in [
            '"$MUTANTS_HOST_LOCK_WAIT_SECONDS"',
            "DREP_MUTANTS_RSYNC_TIMEOUT_SECONDS",
            '--timeout="$RSYNC_IO_TIMEOUT_SECONDS"',
        ]:
            self.assertIn(bound, script)
        for proof in [
            'mkfifo "$CONTROL_IN" "$CONTROL_OUT"',
            "mutants-lock-ready:$RUN_TOKEN",
            "mutants-run-finished:$RUN_TOKEN",
            "DREP_MUTANTS_RESULT_TOKEN",
            ".run-token",
            "printf 'mirrored\\n'",
        ]:
            self.assertIn(proof, script)
        for cleanup in [
            'kill "$REMOTE_SESSION_PID"',
            'wait "$REMOTE_SESSION_PID"',
            "trap 'exit 74' PIPE",
        ]:
            self.assertIn(cleanup, script)
        self.assertLess(
            script.index("REMOTE_SESSION_PID=$!"), script.index("rsync -a --delete")
        )

    def test_remote_builds_the_source_it_is_given(self):
        # The staged run hands the wrapper a snapshot of the index: the sync ships that tree and a local fallback builds it.
        script = without_comments("scripts/mutants-remote.sh")
        self.assertIn('SOURCE="${MUTANTS_SOURCE_DIR:-.}"', script)
        self.assertIn('"$SOURCE/" "$REMOTE/"', script)
        self.assertIn('exec ./scripts/mutants-run.sh --dir "$SOURCE" "$@"', script)

    def test_remote_takes_the_checkout_lock_before_probing_the_host(self):
        script = without_comments("scripts/mutants-remote.sh")
        self.assertLess(
            script.index("acquire_checkout_lock mutants-remote"),
            script.index("ssh -o BatchMode=yes -o ConnectTimeout=5"),
        )

    def test_checkouts_with_one_name_get_their_own_remote_directories(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        other_machine = Path(directory.name) / "bin"
        other_machine.mkdir()
        executable(other_machine / "hostname", "#!/bin/sh\necho other-machine\n")

        def remote_dir(parent, name, environment=os.environ):
            scripts = Path(directory.name) / parent / name / "scripts"
            scripts.mkdir(parents=True, exist_ok=True)
            shutil.copy2(
                ROOT / "scripts/mutants-common.sh", scripts / "mutants-common.sh"
            )
            result = run(
                [
                    "bash",
                    "-c",
                    f'. "$1/mutants-common.sh" && remote_checkout_dir {ROLE}',
                    "remote-dir-test",
                    str(scripts),
                ],
                environment,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            return result.stdout

        first = remote_dir("one", "octoroute")
        second = remote_dir("two", "octoroute")
        odd = remote_dir("three", "my repo;x")
        cache = f".cache/{ROLE}/"
        for path in [first, second, odd]:
            self.assertTrue(path.startswith(cache), path)
            self.assertRegex(path[len(cache) :], r"^[A-Za-z0-9._-]+$")
            self.assertNotIn(path[len(cache) :], [".", ".."])
        self.assertTrue(first.startswith(cache + "octoroute-"))
        self.assertTrue(odd.startswith(cache + "myrepox-"))
        self.assertNotEqual(first, second)
        self.assertEqual(first, remote_dir("one", "octoroute"))
        # The same path on another machine must not share a directory.
        self.assertNotEqual(
            first,
            remote_dir(
                "one", "octoroute", with_fake_bin(Path(directory.name), os.environ)
            ),
        )

    def test_runner_holds_the_host_lock_and_keeps_it_from_cargo(self):
        script = without_comments("scripts/mutants-run.sh")
        self.assertIn('HOST_LOCK="${DREP_MUTANTS_HOST_LOCK:-}"', script)
        self.assertIn('hold_lock 9 "$HOST_LOCK" mutants-run', script)
        for token in [
            "DREP_MUTANTS_RESULT_TOKEN",
            "$OUT_DIR/mutants.out",
            "$OUT_DIR/.run-token",
        ]:
            self.assertIn(token, script)
        self.assertIn('"$@" 6<&- 9<&- && status=0', script)

    def test_host_lock_wait_policy_has_one_definition(self):
        common = without_comments("scripts/mutants-common.sh")
        self.assertIn(
            'MUTANTS_HOST_LOCK_WAIT_SECONDS="${DREP_MUTANTS_HOST_LOCK_WAIT_SECONDS:-1800}"',
            common,
        )
        self.assertIn("validate_mutants_host_lock_wait_seconds()", common)
        self.assertIn(
            'validate_mutants_host_lock_wait_seconds "$caller" || return', common
        )
        for name in ["mutants-remote", "mutants-run"]:
            self.assertNotIn(
                'HOST_LOCK_WAIT_SECONDS="${DREP_MUTANTS_HOST_LOCK_WAIT_SECONDS:-1800}"',
                without_comments(f"scripts/{name}.sh"),
            )

    def test_ai1_transport_fails_closed_without_bypassing_the_sandbox(self):
        result = run(["bash", str(ROOT / "scripts/test_ai1_transport.sh")], os.environ)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_scratch_copies_stay_off_the_tmpfs(self):
        script = without_comments("scripts/mutants-run.sh")
        self.assertIn('RUN_SCRATCH="$MUTANTS_SCRATCH_ROOT/run"', script)
        self.assertIn(
            'MUTANTS_SCRATCH_ROOT="${DREP_MUTANTS_TMPDIR:-${MUTANTS_ROOT}.mutants-tmp}"',
            without_comments("scripts/mutants-common.sh"),
        )
        self.assertIn('export TMPDIR="$RUN_SCRATCH"', script)
        self.assertFalse(
            any("TMPDIR=" in line and "/tmp" in line for line in script.splitlines())
        )
        self.assertNotIn("rm ", script)
        self.assertNotIn("rmdir ", script)
        self.assertIn("trap 'remove_tree \"$RUN_SCRATCH\"' EXIT", script)


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
    def test_lock_holder_cleanup_closes_its_output_pipe(self):
        probe = unittest.TestCase()
        with tempfile.TemporaryDirectory() as directory:
            try:
                holder = hold_lock(probe, Path(directory) / "held.lock")
            finally:
                probe.doCleanups()
            try:
                self.assertIsNotNone(holder.poll())
                self.assertTrue(holder.stdout.closed)
            finally:
                holder.stdout.close()

    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="octoroute mutants ")
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.scratch = self.root / "scratch root"
        self.output = self.root / "mutation output"
        self.lock = self.output.with_name(self.output.name + ".lock")
        (self.root / "bin").mkdir()
        executable(self.root / "bin/cargo", FAKE_CARGO)
        self.environment = with_fake_bin(
            self.root,
            {
                key: value
                for key, value in os.environ.items()
                if not key.startswith("DREP_MUTANTS_")
            },
        )
        self.environment.update(
            DREP_MUTANTS_TMPDIR=str(self.scratch),
            MUTANTS_OUT_DIR=str(self.output),
            FAKE_ARGS=str(self.root / "cargo-args"),
        )

    def run_script(self, prelude="", **environment):
        """Runs mutants-run.sh from a shell that runs prelude first, so the prelude can hand the script an open descriptor."""
        return run(
            [
                "bash",
                "-c",
                prelude + 'exec bash "$1"',
                "mutants-run",
                str(ROOT / "scripts/mutants-run.sh"),
            ],
            {**self.environment, **environment},
        )

    def test_cleanup_is_destructive_only_inside_the_run_directory(self):
        (self.scratch / "run/cargo-mutants-killed.tmp/nested").mkdir(parents=True)
        adjacent = self.scratch / "adjacent"
        adjacent.mkdir()
        (adjacent / "keep").write_text("keep")

        result = self.run_script()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.scratch / "run").exists())
        self.assertTrue((adjacent / "keep").exists())
        self.assertTrue(lock_is_free(self.lock))
        arguments = (self.root / "cargo-args").read_text().splitlines()
        timeouts = [
            value
            for flag, value in itertools.pairwise(arguments)
            if flag == "--minimum-test-timeout"
        ]
        self.assertEqual(timeouts, ["120"])

    def test_a_symlinked_run_directory_is_not_followed(self):
        outside = self.root / "outside"
        outside.mkdir()
        (outside / "keep").write_text("keep")
        self.scratch.mkdir()
        (self.scratch / "run").symlink_to(outside)

        result = self.run_script()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((outside / "keep").exists())
        self.assertFalse((self.scratch / "run").exists())

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

    def test_second_run_in_one_checkout_waits_for_the_first(self):
        hold_lock(self, self.lock)
        results = self.output / "mutants.out"
        results.mkdir(parents=True)
        (results / "missed.txt").write_text("first run's survivor\n")

        result = self.run_script(DREP_MUTANTS_HOST_LOCK_WAIT_SECONDS="0")

        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertFalse((self.root / "cargo-args").exists())
        self.assertEqual((results / "missed.txt").read_text(), "first run's survivor\n")

    def test_a_waiting_run_starts_once_the_lock_is_released(self):
        hold_lock(self, self.lock, "0.2")

        result = self.run_script(DREP_MUTANTS_HOST_LOCK_WAIT_SECONDS="30")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.root / "cargo-args").exists())

    def test_leftover_lock_file_holds_nothing(self):
        self.lock.write_text("")

        result = self.run_script()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.root / "cargo-args").exists())
        self.assertTrue(lock_is_free(self.lock))

    def test_a_run_started_by_the_host_lock_holder_reuses_its_lock(self):
        result = self.run_script(
            'exec 9>>"$DREP_MUTANTS_HOST_LOCK" && perl -MFcntl=:flock -e \'open(my $l, ">&=", 9) or exit 2; flock($l, LOCK_EX) or exit 1\' && ',
            DREP_MUTANTS_HOST_LOCK=str(self.root / "host.lock"),
            DREP_MUTANTS_HOST_LOCK_WAIT_SECONDS="0",
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.root / "cargo-args").exists())

    def test_a_run_waits_for_a_host_lock_another_sweep_holds(self):
        hold_lock(self, self.root / "host.lock")

        result = self.run_script(
            DREP_MUTANTS_HOST_LOCK=str(self.root / "host.lock"),
            DREP_MUTANTS_HOST_LOCK_WAIT_SECONDS="0",
        )

        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertFalse((self.root / "cargo-args").exists())


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
        # Records whether another process is refused the checkout lock, whether this process, started by the lock's holder, gets it at once, and the source it was handed. With FAKE_RESTAGE set it stages a further change, as an editor could while the run waits.
        executable(
            self.root / "scripts/mutants-remote.sh",
            "#!/usr/bin/env bash\n"
            'perl -MFcntl=:flock -e "$LOCK_PROBE" target/mutants.lock; refused=$?\n'
            ". scripts/mutants-common.sh\n"
            "MUTANTS_HOST_LOCK_WAIT_SECONDS=0\n"
            "acquire_checkout_lock fake-remote; inherited=$?\n"
            'source=$(cat "$MUTANTS_SOURCE_DIR/source.rs"); untracked=$(ls "$MUTANTS_SOURCE_DIR/untracked.rs" 2>/dev/null)\n'
            'printf "remote:%s extra:%s inherited:%s refused:%s source:%s untracked:%s\\n" "$*" "$MUTANTS_EXTRA_FILES" "$inherited" "$refused" "$source" "$untracked" >> events\n'
            "if [ -n \"${FAKE_RESTAGE:-}\" ]; then echo 'fn three() {}' > source.rs && git add source.rs; fi\n",
        )
        executable(
            self.root / "bin/cargo",
            '#!/bin/sh\nprintf "cargo:%s\\n" "$*" >> events\nexit 1\n',
        )
        self.environment = with_fake_bin(
            self.root,
            {
                key: value
                for key, value in os.environ.items()
                if not key.startswith(("DREP_MUTANTS_", "GIT_"))
            },
        )
        self.environment.update(
            LOCK_PROBE=LOCK_PROBE, DREP_MUTANTS_HOST_LOCK_WAIT_SECONDS="0"
        )
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

    def run_script(self, path, **environment):
        return run(["bash", path], {**self.environment, **environment}, cwd=self.root)

    def events(self):
        path = self.root / "events"
        return path.read_text().splitlines() if path.exists() else []

    def test_a_commit_without_rust_changes_does_not_wait_for_a_running_sweep(self):
        (self.root / "notes.md").write_text("notes\n")
        self.git("add", "notes.md")
        (self.root / "target").mkdir()
        hold_lock(self, self.root / "target/mutants.lock")

        result = self.run_script("scripts/mutants-staged.sh")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("no staged Rust changes", result.stdout)
        self.assertEqual(self.events(), [])

    def test_hook_refuses_unstaged_or_untracked_inputs_before_any_check(self):
        # fmt and clippy read the working tree, so the hook refuses one that differs from the index.
        source = self.root / "source.rs"
        source.write_text("fn staged() {}\n")
        self.git("add", "--", "source.rs")
        for kind in ["unstaged", "untracked"]:
            with self.subTest(kind=kind):
                path = source if kind == "unstaged" else self.root / "new_test.rs"
                path.write_text("#[test]\nfn unstaged_test() {}\n")
                result = self.run_script(".githooks/pre-commit")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("index", result.stderr)
                self.assertEqual(path.read_text(), "#[test]\nfn unstaged_test() {}\n")
                self.assertEqual(self.git("show", ":source.rs"), "fn staged() {}\n")
                self.assertEqual(self.events(), [])
                if kind == "unstaged":
                    source.write_text("fn staged() {}\n")
                else:
                    path.unlink()

    def test_staged_run_tests_the_index_under_the_checkout_lock(self):
        source = self.root / "source.rs"
        source.write_text("fn staged() {}\n")
        self.git("add", "--", "source.rs")
        source.write_text("fn unstaged() {}\n")
        (self.root / "untracked.rs").write_text("fn untracked() {}\n")

        result = self.run_script("scripts/mutants-staged.sh")

        self.assertEqual(result.returncode, 0, result.stderr)
        diff = "target/mutants/staged.diff"
        self.assertIn("+fn staged() {}", (self.root / diff).read_text())
        self.assertEqual(
            self.events(),
            [
                f"remote:--in-diff {diff} extra:{diff} inherited:0 refused:1 source:fn staged() {{}} untracked:"
            ],
        )
        self.assertTrue(lock_is_free(self.root / "target/mutants.lock"))
        self.assertFalse(Path(str(self.root) + ".mutants-tmp", "index").exists())

    def test_a_change_staged_during_the_run_is_refused(self):
        # git commit reads the index again after the hook, so a change staged while the run worked would be committed untested.
        (self.root / "source.rs").write_text("fn staged() {}\n")
        self.git("add", "--", "source.rs")

        result = self.run_script("scripts/mutants-staged.sh", FAKE_RESTAGE="1")

        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("the index changed during the run", result.stderr)

    def test_hook_refuses_formatting_failure_without_rewriting_or_staging(self):
        before = self.git("write-tree")
        result = self.run_script(".githooks/pre-commit")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.events(), ["cargo:fmt --all -- --check"])
        self.assertEqual(self.git("write-tree"), before)


if __name__ == "__main__":
    unittest.main()
