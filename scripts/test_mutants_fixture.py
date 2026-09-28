"""Every mutation test reaches git through mutants_fixture, which keeps the Git environment of a hook the tests may run under away from a fixture."""

import re
import unittest
from pathlib import Path

# A git spawn written as a subprocess argument list.
BARE_GIT = re.compile(r'\[\s*"git"')


class FixtureIsolationTests(unittest.TestCase):
    def test_tests_spawn_git_only_through_the_fixture_module(self):
        offenders = [
            f"{path.name}:{number}"
            for path in sorted(Path(__file__).parent.glob("test_*.py"))
            for number, line in enumerate(path.read_text().splitlines(), 1)
            if BARE_GIT.search(line.split("#", 1)[0])
        ]
        self.assertEqual(
            offenders,
            [],
            "spawn git through mutants_fixture.git",
        )


if __name__ == "__main__":
    unittest.main()
