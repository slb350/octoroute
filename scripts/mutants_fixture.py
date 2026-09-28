"""Git for the mutation tests' fixture repositories, kept away from the Git environment of a hook the tests may run under.

A hook exports the committing repository's GIT_DIR and GIT_INDEX_FILE, any -c configuration and the commit's identity; a fixture's git that inherited them would act on that repository instead of its own.
"""

import functools
import os
import subprocess

# What a hook exports beyond the variables git itself calls repository-local.
IDENTITY = (
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_COMMITTER_DATE",
    "GIT_QUARANTINE_PATH",
)


@functools.cache
def outer_git_variables():
    """Every variable git calls repository-local (the repository, its index and object store, and -c configuration), the commit identity, and a receiving push's quarantine."""
    listed = subprocess.run(
        ["git", "rev-parse", "--local-env-vars"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.split()
    return frozenset(listed) | frozenset(IDENTITY)


def fixture_env(base=None):
    """base, os.environ by default, without what a fixture's git must not inherit."""
    source = os.environ if base is None else base
    return {
        name: value
        for name, value in source.items()
        if name not in outer_git_variables()
    }


def git(root, *args):
    """Run git in the fixture repository at root and return its output."""
    return subprocess.check_output(
        ["git", *args],
        cwd=root,
        env=fixture_env(),
        text=True,
        stderr=subprocess.PIPE,
    )


def run_git(root, *args):
    """Run git in the fixture repository at root and return the completed process, whatever its exit status."""
    return subprocess.run(
        ["git", *args],
        cwd=root,
        env=fixture_env(),
        capture_output=True,
        text=True,
        check=False,
    )
