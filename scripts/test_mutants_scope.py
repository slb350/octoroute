"""Exercise mutation admission against real, isolated Git histories."""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("mutants_scope.py")
ZERO = "0" * 40


class MutationScopeTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "Scope test")
        self.git("config", "user.email", "scope@example.invalid")
        self.git("config", "commit.gpgsign", "false")
        self.base = self.commit("README.md", "initial\n")

    def git(self, *args):
        return subprocess.check_output(
            ["git", *args], cwd=self.root, text=True, stderr=subprocess.PIPE
        ).strip()

    def commit(self, path, contents):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(contents)
        self.stage(path)
        self.git("commit", "-qm", "fixture")
        return self.git("rev-parse", "HEAD")

    def stage(self, path):
        ignored = subprocess.run(
            ["git", "check-ignore", "--", path],
            cwd=self.root,
            capture_output=True,
            check=False,
        )
        self.assertEqual(ignored.returncode, 1, f"fixture path is ignored: {path}")
        self.git("add", "--", path)

    def admits(self, event, payload):
        event_file = self.root / "event.json"
        event_file.write_text(json.dumps(payload))
        result = subprocess.run(
            [sys.executable, str(SCRIPT), event, str(event_file)],
            cwd=self.root,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(result.stdout.strip(), ["run_mutants=true", "run_mutants=false"])
        return result.stdout.strip() == "run_mutants=true"

    def push(self, before, after, **extra):
        return self.admits("push", {"before": before, "after": after, **extra})

    def test_only_added_test_attributes_admit_an_ordinary_change(self):
        cases = [
            ("fn production() {}\n", False),
            ("#[test]\nfn example() {}\n", True),
            (
                '#[tokio::test(flavor = "current_thread")]\nasync fn example() {}\n',
                True,
            ),
            (
                '#[\n tokio::test(\n flavor = "current_thread"\n )\n]\nasync fn example() {}\n',
                True,
            ),
            ("// #[test]\nfn example() {}\n", False),
            ("/* outer /* #[test] */ comment */\nfn example() {}\n", False),
            ('const EXAMPLE: &str = "#[test]\\nfn example() {}";\n', False),
            ('const EXAMPLE: &str = r##"#[test]\nfn example() {}"##;\n', False),
            ('const EXAMPLE: &[u8] = br#"#[test]"#;\n', False),
            ('const EXAMPLE: &str = "\\"#[test]\\"";\n', False),
            (
                "/// ```\n/// #[test]\n/// fn example() {}\n/// ```\nfn item() {}\n",
                False,
            ),
        ]
        for index, (source, expected) in enumerate(cases):
            with self.subTest(source=source):
                before = self.git("rev-parse", "HEAD")
                after = self.commit(f"src/example_{index}.rs", source)
                self.assertEqual(self.push(before, after), expected)

    def test_existing_and_deleted_tests_do_not_admit_a_sweep(self):
        before = self.commit(
            "src/example.rs", "#[test]\nfn example() { assert!(true); }\n"
        )
        changed = self.commit(
            "src/example.rs", "#[test]\nfn example() { assert_eq!(1, 1); }\n"
        )
        self.assertFalse(self.push(before, changed))
        docs = self.commit("README.md", "document existing tests\n")
        self.assertFalse(self.push(changed, docs))
        (self.root / "src/example.rs").rename(self.root / "deleted-example.rs")
        self.stage("src/example.rs")
        self.git("commit", "-qm", "delete test")
        self.assertFalse(self.push(docs, self.git("rev-parse", "HEAD")))

    def test_complete_push_and_initial_push_inspect_more_than_the_tip_commit(self):
        self.commit("src/example.rs", "#[test]\nfn example() {}\n")
        head = self.commit("README.md", "tip only changes docs\n")
        self.assertTrue(self.push(self.base, head))
        self.assertTrue(self.push(ZERO, head))
        self.assertFalse(self.push(head, ZERO, deleted=True))

    def test_moves_only_admit_new_rust_test_declarations(self):
        source = "#[test]\nfn original() {\n    assert_eq!(1 + 1, 2);\n    assert_eq!(2 + 2, 4);\n}\n"
        current = "src/original.rs"
        self.commit(current, source)
        for destination, addition, expected in [
            ("src/moved test.rs", "", False),
            ("src/expanded.rs", "\n#[test]\nfn added() {}\n", True),
            ("src/archived.txt", "", False),
            ("src/restored.rs", "", True),
        ]:
            with self.subTest(destination=destination):
                before = self.git("rev-parse", "HEAD")
                (self.root / current).rename(self.root / destination)
                self.stage(current)
                source += addition
                after = self.commit(destination, source)
                current = destination
                self.assertTrue(
                    self.git(
                        "diff", "--name-status", "--find-renames", before, after
                    ).startswith("R")
                )
                self.assertEqual(self.push(before, after), expected)

    def test_pull_request_uses_merge_base_and_checks_the_whole_branch(self):
        self.git("checkout", "-qb", "feature")
        head = self.commit("src/feature.rs", "fn feature() {}\n")
        self.git("checkout", "-q", "main")
        base = self.commit("src/base.rs", "#[test]\nfn base_only() {}\n")
        payload = {"pull_request": {"base": {"sha": base}, "head": {"sha": head}}}
        self.assertFalse(self.admits("pull_request", payload))
        self.git("checkout", "-q", "feature")
        self.commit("src/feature.rs", "#[test]\nfn feature() {}\n")
        head = self.commit("README.md", "tip docs\n")
        payload["pull_request"]["head"]["sha"] = head
        self.assertTrue(self.admits("pull_request", payload))

    def test_explicit_sweeps_do_not_require_a_diff(self):
        for event in ["workflow_dispatch", "schedule"]:
            with self.subTest(event=event):
                self.assertTrue(self.admits(event, {}))


if __name__ == "__main__":
    unittest.main()
