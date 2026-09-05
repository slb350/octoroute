"""Admit full mutation sweeps when a complete GitHub diff adds a Rust test."""

import json
import re
import subprocess
import sys
from pathlib import Path

TEST_ATTRIBUTE = re.compile(r"#\s*\[\s*(?:\w+\s*::\s*)*test\s*(?:\([^]]*\))?\s*\]")
NON_CODE = re.compile(
    r'//[^\n]*|/\*|(?:b|c)?r(?P<hashes>\#{0,255})"|"(?:\\.|[^"\\])*"'
    r"|'(?:\\(?:u\{[\da-fA-F_]+\}|x[\da-fA-F]{2}|.)|[^'\\\n])'",
    re.DOTALL,
)
COMMENT_DELIMITER = re.compile(r"/\*|\*/")
HUNK = re.compile(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@", re.MULTILINE)


def git(*args, input=None):
    return subprocess.check_output(["git", *args], input=input, text=True)


def code_only(source):
    """Mask Rust comments and literals, retaining offsets and line numbers."""
    parts = []
    position = 0
    while match := NON_CODE.search(source, position):
        start, end = match.span()
        if match[0] == "/*":
            depth = 1
            while depth and (delimiter := COMMENT_DELIMITER.search(source, end)):
                depth += 1 if delimiter[0] == "/*" else -1
                end = delimiter.end()
            if depth:
                end = len(source)
        elif match["hashes"] is not None:
            terminator = '"' + match["hashes"]
            closing = source.find(terminator, end)
            end = len(source) if closing < 0 else closing + len(terminator)
        parts.extend([source[position:start], re.sub(r"[^\n]", " ", source[start:end])])
        position = end
    return "".join([*parts, source[position:]])


def adds_test(base, head):
    changes = iter(
        git(
            "diff",
            "--name-status",
            "-z",
            "--find-renames",
            "--diff-filter=AMRT",
            base,
            head,
        )
        .rstrip("\0")
        .split("\0")
    )
    for status in filter(None, changes):
        path = next(changes)
        paths = [path]
        if status.startswith("R"):
            path = next(changes)
            # Retain both Rust paths so Git preserves the rename pairing. A
            # non-Rust source moved into Rust introduces all its declarations.
            paths = [*paths, path] if paths[0].endswith(".rs") else [path]
        if not path.endswith(".rs"):
            continue
        diff = git(
            "diff",
            "--no-ext-diff",
            "--no-color",
            "--unified=0",
            "--find-renames",
            base,
            head,
            "--",
            *paths,
        )
        added_lines = set()
        for hunk in HUNK.finditer(diff):
            start = int(hunk[1])
            added_lines.update(range(start, start + int(hunk[2] or 1)))
        source = code_only(git("show", f"{head}:{path}"))
        for attribute in TEST_ATTRIBUTE.finditer(source):
            first = source.count("\n", 0, attribute.start()) + 1
            last = source.count("\n", 0, attribute.end()) + 1
            if added_lines.intersection(range(first, last + 1)):
                return True
    return False


def revision(value):
    if not re.fullmatch(r"[0-9a-fA-F]{40}|[0-9a-fA-F]{64}", value):
        raise ValueError("event must contain complete Git object IDs")
    return value


def should_run(event_name, event):
    if event_name in {"schedule", "workflow_dispatch"}:
        return True
    if event_name == "push":
        if event.get("deleted"):
            return False
        base, head = revision(event["before"]), revision(event["after"])
        if not base.strip("0"):
            base = git("hash-object", "-t", "tree", "--stdin", input="").strip()
    elif event_name == "pull_request":
        request = event["pull_request"]
        head = revision(request["head"]["sha"])
        base = git("merge-base", revision(request["base"]["sha"]), head).strip()
    else:
        raise ValueError(f"unsupported event: {event_name}")
    return adds_test(base, head)


if __name__ == "__main__":
    admitted = should_run(sys.argv[1], json.loads(Path(sys.argv[2]).read_text()))
    print(f"run_mutants={str(admitted).lower()}")
