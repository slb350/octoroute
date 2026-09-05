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

    def scope(self, event, payload):
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
        outputs = dict(line.split("=", 1) for line in result.stdout.splitlines())
        self.assertEqual(set(outputs), {"run", "mode", "arguments"})
        mode = outputs["mode"]
        self.assertIn(mode, ["none", "files", "full"])
        self.assertEqual(outputs["run"], str(mode != "none").lower())
        arguments = json.loads(outputs["arguments"])
        self.assertIsInstance(arguments, list)
        return mode, arguments

    def push(self, before, after, **extra):
        return self.scope("push", {"before": before, "after": after, **extra})

    def test_added_items_distinguish_tests_from_comment_and_literal_examples(self):
        cases = [
            ("fn production() {}\n", "none"),
            ("#[test]\nfn example() {}\n", "files"),
            (
                '#[tokio::test(flavor = "current_thread")]\nasync fn example() {}\n',
                "files",
            ),
            (
                '#[\n tokio::test(\n flavor = "current_thread"\n )\n]\nasync fn example() {}\n',
                "files",
            ),
            ("// #[test]\nfn example() {}\n", "none"),
            ("/* outer /* #[test] */ comment */\nfn example() {}\n", "none"),
            ('const EXAMPLE: &str = "#[test]\\nfn example() {}";\n', "none"),
            ('const EXAMPLE: &str = r##"#[test]\nfn example() {}"##;\n', "none"),
            ('const EXAMPLE: &[u8] = br#"#[test]"#;\n', "none"),
            ('const EXAMPLE: &str = "\\"#[test]\\"";\n', "none"),
            ("/// ```no_run\n/// assert!(true);\n/// ```\nfn item() {}\n", "full"),
            ("/// ```text\n/// #[test]\n/// ```\nfn item() {}\n", "none"),
        ]
        for index, (source, mode) in enumerate(cases):
            with self.subTest(source=source):
                path = f"src/example_{index}.rs"
                before = self.git("rev-parse", "HEAD")
                after = self.commit(path, source)
                self.assertEqual(
                    self.push(before, after),
                    (mode, ["--file", path] if mode == "files" else []),
                )

    def test_changed_test_items_preserve_owning_file_and_original_literal_content(self):
        cases = [
            (
                "#[cfg(test)]\nmod tests {\n #[test]\n fn example() { assert_eq!(answer(), VALUE); }\n}\n",
                "files",
            ),
            (
                "#[\n cfg( test )\n]\nfn helper(\n input: u8,\n) -> u8 {\n VALUE\n}\n",
                "files",
            ),
            (
                '#[test]\nfn example(\n) { assert_eq!(answer(), "VALUE"); let braces = r##"/* } */"##; }\n',
                "files",
            ),
            (
                "proptest! { #[test] fn example(value in 0..VALUE) { assert!(value >= 0); } }\n",
                "files",
            ),
            (
                "pub fn answer() -> u8 { VALUE }\n#[cfg(test)]\nmod tests { #[test] fn example() { assert!(true); } }\n",
                "none",
            ),
            (
                "#[cfg(test)]\nmod tests { #[test] fn example() { assert!(true); } }\npub fn answer() -> u8 { VALUE }\n",
                "none",
            ),
            (
                "pub fn answer() -> u8 {\n #[cfg(test)]\n let delay = 1;\n consume(delay);\n VALUE\n}\n",
                "none",
            ),
            ('#[path = "VALUE.rs"]\n#[cfg(test)]\nmod service_tests;\n', "full"),
            (
                "/// ```no_run\n/// let expected = VALUE;\n/// ```\npub fn answer() {}\n",
                "full",
            ),
            (
                "/**\n```rust\nassert_eq!(answer(), VALUE);\n```\n*/\npub fn answer() {}\n",
                "full",
            ),
            ("/// Documentation VALUE.\npub fn answer() {}\n", "none"),
            ("/// ```text\n/// VALUE\n/// ```\npub fn answer() {}\n", "none"),
        ]
        for index, (template, mode) in enumerate(cases):
            with self.subTest(template=template):
                self.assertIn("VALUE", template)
                path = f"src/changed_{index}.rs"
                before = self.commit(path, template.replace("VALUE", "41"))
                after = self.commit(path, template.replace("VALUE", "42"))
                self.assertEqual(
                    self.push(before, after),
                    (mode, ["--file", path] if mode == "files" else []),
                )

    def test_ambiguous_test_files_and_fixtures_always_select_full_scope(self):
        for path in [
            "tests/api.rs",
            "src/body_map_tests.rs",
            "src/body_map_test.rs",
            "src/tests.rs",
            "src/test.rs",
            "src/service_tests/mod.rs",
            "src/service_tests/local.rs",
            "src/test_support/doubles.rs",
            "src/test_support.rs",
            "fixtures/request.json",
            "testdata/input.txt",
            "snapshots/answer.snap",
            ".cargo/mutants.toml",
        ]:
            with self.subTest(path=path):
                before = self.commit(path, "before\n")
                after = self.commit(path, "after\n")
                self.assertEqual(self.push(before, after), ("full", []))

    def test_removed_and_reordered_test_items_and_files_still_qualify(self):
        production = "pub fn answer() -> u8 { 42 }\n"
        source = production + "#[test]\nfn example() { assert!(true); }\n"
        before = self.commit("src/example.rs", source)
        after = self.commit("src/example.rs", production)
        self.assertEqual(
            self.push(before, after), ("files", ["--file", "src/example.rs"])
        )
        for path in ["src/example.rs", "tests/api.rs"]:
            with self.subTest(path=path):
                before = self.commit(path, source)
                (self.root / path).rename(self.root / "deleted-example.rs")
                self.stage(path)
                self.git("commit", "-qm", "delete test")
                self.assertEqual(
                    self.push(before, self.git("rev-parse", "HEAD")), ("full", [])
                )
        first = "#[cfg(test)]\nmod first;\n"
        second = "#[cfg(test)]\nmod second;\n"
        before = self.commit("src/modules.rs", first + second)
        after = self.commit("src/modules.rs", second + first)
        self.assertEqual(self.push(before, after), ("full", []))

    def test_moves_and_multiple_files_keep_complete_scope_and_literal_paths(self):
        source = "#[test]\nfn original() { assert_eq!(1 + 1, 2); }\n"
        current = "src/original.rs"
        self.commit(current, source)
        for destination, mode in [
            ("src/moved test.rs", "full"),
            ("src/archived.txt", "full"),
            ("src/restored.rs", "files"),
        ]:
            with self.subTest(destination=destination):
                before = self.git("rev-parse", "HEAD")
                (self.root / current).rename(self.root / destination)
                self.stage(current)
                after = self.commit(destination, source)
                self.assertEqual(
                    self.push(before, after),
                    (mode, ["--file", destination] if mode == "files" else []),
                )
                current = destination
        before = self.git("rev-parse", "HEAD")
        self.commit("src/one file.rs", source)
        after = self.commit("src/two.rs", source)
        self.assertEqual(
            self.push(before, after),
            ("files", ["--file", "src/one file.rs", "--file", "src/two.rs"]),
        )

    def test_complete_push_and_initial_push_inspect_more_than_the_tip_commit(self):
        self.commit("src/example.rs", "#[test]\nfn example() {}\n")
        head = self.commit("README.md", "tip only changes docs\n")
        expected = ("files", ["--file", "src/example.rs"])
        self.assertEqual(self.push(self.base, head), expected)
        self.assertEqual(self.push(ZERO, head), expected)
        self.assertEqual(self.push(head, ZERO, deleted=True), ("none", []))

    def test_pull_request_uses_merge_base_and_checks_the_whole_branch(self):
        self.git("checkout", "-qb", "feature")
        head = self.commit("src/feature.rs", "fn feature() {}\n")
        self.git("checkout", "-q", "main")
        base = self.commit("src/base.rs", "#[test]\nfn base_only() {}\n")
        payload = {"pull_request": {"base": {"sha": base}, "head": {"sha": head}}}
        self.assertEqual(self.scope("pull_request", payload), ("none", []))
        self.git("checkout", "-q", "feature")
        self.commit("src/feature.rs", "#[test]\nfn feature() {}\n")
        head = self.commit("README.md", "tip docs\n")
        payload["pull_request"]["head"]["sha"] = head
        self.assertEqual(
            self.scope("pull_request", payload), ("files", ["--file", "src/feature.rs"])
        )

    def test_explicit_sweeps_do_not_require_a_diff(self):
        for event in ["workflow_dispatch", "schedule"]:
            with self.subTest(event=event):
                self.assertEqual(self.scope(event, {}), ("full", []))


if __name__ == "__main__":
    unittest.main()
