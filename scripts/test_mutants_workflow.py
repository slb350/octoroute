"""Check mutation workflow boundaries without contacting a remote host."""

import fcntl
import os
import shutil
import signal
import subprocess
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FAKE_REMOTE = r"""#!/usr/bin/env python3
import fcntl, os, shlex, signal, sys, time
from pathlib import Path
root = Path(os.environ["FIXTURE"])
def event(name):
    with (root / "events").open("a") as output:
        output.write(name + "\n")
name = Path(sys.argv[0]).name
if name == "ssh":
    if sys.argv[-1] == "true":
        sys.exit(0)
    (root / "lease_pid").write_text(str(os.getpid()))
    os.execvp("bash", shlex.split(sys.argv[-1]))
if name == "flock":
    mode = fcntl.LOCK_SH if "-s" in sys.argv else fcntl.LOCK_EX
    try:
        fcntl.flock(int(sys.argv[-1]), mode | (fcntl.LOCK_NB if "-n" in sys.argv else 0))
    except BlockingIOError:
        sys.exit(1)
    if sys.argv[-1] == "9" and "-s" not in sys.argv:
        event("locked")
    sys.exit(0)
if name == "setsid":
    os.setsid()
    os.execvp(sys.argv[1], sys.argv[1:])
if name == "systemd-run":
    sys.exit(1)
if name == "ionice":
    os.execvp(sys.argv[2], sys.argv[2:])
if name == "nice":
    os.execvp(sys.argv[3], sys.argv[3:])
operation = "sweep" if name == "mutants-run.sh" else (
    "upload" if ":" in sys.argv[-1] else "artifacts"
)
if "--fixture-operation" in sys.argv:
    operation = sys.argv[-1]
else:
    for argument in sys.argv:
        if argument.startswith("--rsync-path="):
            os.execvp("bash", ["bash", "-c", argument.partition("=")[2]
                + " --fixture-operation " + operation])
lock_path = root / "remote.mutants-lease"
lock = os.open(lock_path / "active" if lock_path.exists() else root / "remote/target/mutants", os.O_RDONLY)
try:
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        event(operation + ":unlocked")
    except BlockingIOError:
        event(operation + ":locked")
    if os.environ.get("HOLD_OPERATION") == operation:
        if os.environ.get("SLOW_STOP"):
            def slow_stop(*_):
                event("stopping")
                time.sleep(0.5)
                sys.exit(143)
            signal.signal(signal.SIGTERM, slow_stop)
        (root / "operation_pid").write_text(str(os.getpid()))
        event("waiting")
        time.sleep(60)
    if operation == "sweep" and os.environ.get("FORGED_STATUS"):
        print("octoroute-sweep-status:0", flush=True)
    sys.exit(int(os.environ.get("FAIL_" + operation.upper(), "0")))
finally:
    os.close(lock)
"""


class MutationWorkflowTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / "scripts").mkdir()
        (self.root / ".githooks").mkdir()
        (self.root / "bin").mkdir()
        (self.root / "remote/scripts").mkdir(parents=True)
        (self.root / "remote/target/mutants").mkdir(parents=True)
        (self.root / "remote/Cargo.toml").write_text('[package]\nname = "octoroute"\n')
        for name in ["mutants-common.sh", "mutants-remote.sh", "mutants-staged.sh"]:
            shutil.copy2(ROOT / "scripts" / name, self.root / "scripts" / name)
        shutil.copy2(ROOT / ".githooks/pre-commit", self.root / ".githooks/pre-commit")
        for name in [
            "ssh",
            "rsync",
            "flock",
            "setsid",
            "systemd-run",
            "ionice",
            "nice",
        ]:
            path = self.root / "bin" / name
            path.write_text(FAKE_REMOTE)
            path.chmod(0o755)
        (self.root / "remote/scripts/mutants-run.sh").write_text(FAKE_REMOTE)
        (self.root / "remote/scripts/mutants-run.sh").chmod(0o755)
        (self.root / "bin/cargo").write_text(
            '#!/bin/sh\nprintf "cargo:%s\\n" "$*" >> "$FIXTURE/events"\nexit 1\n'
        )
        (self.root / "bin/cargo").chmod(0o755)
        self.environment = {
            **os.environ,
            "PATH": str(self.root / "bin") + os.pathsep + os.environ["PATH"],
            "FIXTURE": str(self.root),
            "OCTOROUTE_MUTANTS_HOST": "fixture.invalid",
            "DREP_MUTANTS_REMOTE": "1",
            "DREP_MUTANTS_DIR": os.path.relpath(self.root / "remote", Path.home()),
        }
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "Workflow fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        self.git("config", "commit.gpgsign", "false")
        (self.root / "source.rs").write_text("fn source() {}\n")
        (self.root / ".gitignore").write_text(
            "target/\nremote/\nevents\noperation_pid\nlease_pid\n"
        )
        self.stage("source.rs", "scripts", ".githooks", "bin", ".gitignore")
        self.git("commit", "-qm", "fixture")

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root, text=True)

    def stage(self, *paths):
        for path in paths:
            result = subprocess.run(
                ["git", "check-ignore", "--", path],
                cwd=self.root,
                capture_output=True,
                check=False,
            )
            self.assertEqual(result.returncode, 1)
        self.git("add", "--", *paths)

    def run_script(self, path, **environment):
        return subprocess.run(
            ["bash", path],
            cwd=self.root,
            env={**self.environment, **environment},
            text=True,
            capture_output=True,
            timeout=10,
            check=False,
        )

    def events(self):
        path = self.root / "events"
        return path.read_text().splitlines() if path.exists() else []

    def assert_released(self):
        lock_path = self.root / "remote.mutants-lease"
        lock = os.open(
            lock_path / "active"
            if lock_path.exists()
            else self.root / "remote/target/mutants",
            os.O_RDONLY,
        )
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        finally:
            os.close(lock)

    def test_remote_lock_covers_upload_sweep_and_artifacts_even_on_failure(self):
        for failure, code, operations in [
            ({}, 0, ["upload", "sweep", "artifacts"]),
            ({"FAIL_UPLOAD": "19"}, 19, ["upload"]),
            ({"FAIL_SWEEP": "2"}, 2, ["upload", "sweep", "artifacts"]),
            (
                {"FAIL_SWEEP": "2", "FORGED_STATUS": "1"},
                2,
                ["upload", "sweep", "artifacts"],
            ),
            ({"FAIL_ARTIFACTS": "23"}, 23, ["upload", "sweep", "artifacts"]),
            (
                {"FAIL_SWEEP": "2", "FAIL_ARTIFACTS": "23"},
                2,
                ["upload", "sweep", "artifacts"],
            ),
        ]:
            with self.subTest(failure=failure):
                (self.root / "events").write_text("")
                result = self.run_script("scripts/mutants-remote.sh", **failure)
                self.assertEqual(result.returncode, code, result.stderr)
                self.assertEqual(
                    self.events(),
                    ["locked", *(operation + ":locked" for operation in operations)],
                )
                self.assert_released()

    def test_interrupt_releases_lock_and_stops_work(self):
        for operation in ["upload", "sweep"]:
            for target in ["supervisor", "lease"]:
                with self.subTest(operation=operation, target=target):
                    (self.root / "events").write_text("")
                    self.interrupt_operation(operation, target)

    def interrupt_operation(self, operation, target, slow_stop=False):
        process = subprocess.Popen(
            ["bash", "scripts/mutants-remote.sh"],
            cwd=self.root,
            env={
                **self.environment,
                "HOLD_OPERATION": operation,
                **({"SLOW_STOP": "1"} if slow_stop else {}),
            },
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        try:
            deadline = time.monotonic() + 5
            while "waiting" not in self.events() and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertIn("waiting", self.events())
            pid = (
                process.pid
                if target == "supervisor"
                else int((self.root / "lease_pid").read_text())
            )
            os.kill(pid, signal.SIGTERM)
            if slow_stop:
                deadline = time.monotonic() + 5
                while "stopping" not in self.events() and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertIn("stopping", self.events())
                with self.assertRaises(BlockingIOError):
                    self.assert_released()
            self.assertEqual(
                process.wait(timeout=10), 143 if target == "supervisor" else 1
            )
            self.assert_released()
            pid = int((self.root / "operation_pid").read_text())
            with self.assertRaises(ProcessLookupError):
                os.kill(pid, 0)
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()

    def test_transfer_keeps_checkout_locked_during_lease_loss_cleanup(self):
        self.interrupt_operation("upload", "lease", slow_stop=True)

    def test_remote_refuses_home_ancestors_and_unrelated_nonempty_directories(self):
        for directory in [".", "..", "/", os.path.relpath(self.root, Path.home())]:
            with self.subTest(directory=directory):
                result = self.run_script(
                    "scripts/mutants-remote.sh", DREP_MUTANTS_DIR=directory
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.events(), [])

    def test_staged_gate_refuses_unstaged_or_untracked_inputs_without_touching_them(
        self,
    ):
        source = self.root / "source.rs"
        source.write_text("fn staged() {}\n")
        self.stage("source.rs")
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

    def test_staged_gate_dispatches_a_matching_index(self):
        (self.root / "source.rs").write_text("fn staged() {}\n")
        self.stage("source.rs")
        result = self.run_script("scripts/mutants-staged.sh")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(
            "+fn staged() {}", (self.root / "target/mutants/staged.diff").read_text()
        )
        self.assertEqual(
            self.events(),
            [
                "locked",
                "upload:locked",
                "upload:locked",
                "sweep:locked",
                "artifacts:locked",
            ],
        )

    def test_hook_refuses_formatting_failure_without_rewriting_or_staging(self):
        before = self.git("write-tree")
        result = self.run_script(".githooks/pre-commit")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.events(), ["cargo:fmt --all -- --check"])
        self.assertEqual(self.git("write-tree"), before)


if __name__ == "__main__":
    unittest.main()
