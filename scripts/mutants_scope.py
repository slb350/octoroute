"""Classify a complete GitHub diff as no mutation work, owning files, or full."""

import json
import re
import subprocess
import sys
from pathlib import Path

TEST_ITEM = re.compile(
    r"#\s*\[\s*(?:(?:\w+\s*::\s*)*(?:test|rstest|test_case|case|parameterized)\b"
    r"|cfg\s*\(\s*test\s*\))[^]]*\]|\bmod\s+tests\b|\b(?:proptest|rstest)\s*!"
)
NON_CODE = re.compile(
    r'//[^\n]*|/\*|(?:b|c)?r(?P<hashes>\#{0,255})"|"(?:\\.|[^"\\])*"'
    r"|'(?:\\(?:u\{[\da-fA-F_]+\}|x[\da-fA-F]{2}|.)|[^'\\\n])'",
    re.DOTALL,
)
COMMENT_DELIMITER = re.compile(r"/\*|\*/")
DOC_FENCE = re.compile(
    r"^[ \t]*(?P<fence>`{3,}|~{3,})([^\n]*)\n(.*?)^[ \t]*(?P=fence)[`~]*[ \t]*$",
    re.MULTILINE | re.DOTALL,
)


def git(*args, input=None):
    return subprocess.check_output(["git", *args], input=input, text=True)


def source_parts(source):
    """Mask non-code for item boundaries; retain documentation for doctests."""
    parts = []
    documentation = []
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
        comment = source[start:end]
        if comment.startswith(("///", "//!")) and not comment.startswith("////"):
            documentation.append(comment[3:])
        elif comment.startswith(("/**", "/*!")) and not comment.startswith("/***"):
            documentation.append(re.sub(r"(?m)^\s*\* ?", "", comment[3:-2]))
        parts.extend([source[position:start], re.sub(r"[^\n]", " ", source[start:end])])
        position = end
    return "".join([*parts, source[position:]]), "\n".join(documentation)


def item_end(code, start):
    brackets = []
    pairs = {"(": ")", "[": "]", "{": "}"}
    for index in range(start, len(code)):
        character = code[index]
        if character in pairs:
            brackets.append(pairs[character])
        elif character in ")]}":
            if not brackets or character != brackets.pop():
                return None
            if character == "}" and not brackets:
                return index + 1
        elif character == ";" and not brackets:
            return index + 1
    return None


def test_content(source):
    code, documentation = source_parts(source)
    items = []
    end = 0
    for marker in TEST_ITEM.finditer(code):
        if marker.start() < end:
            continue
        start = marker.start()
        attributes = re.search(r"(?:#\s*\[[^]]*\]\s*)+$", code[:start])
        if attributes:
            start = attributes.start()
        boundary = item_end(code, marker.end())
        end = boundary or len(source)
        external = re.search(r"\bmod\s+\w+\s*;\s*$", code[start:end]) is not None
        # Compare original slices: masking must not erase changed expected strings.
        items.append((source[start:end], external or boundary is None))
    doctests = []
    for fence in DOC_FENCE.finditer(documentation):
        flags = re.split(r"[\s,]+", fence[2].strip())
        if all(
            flag in {"", "rust", "no_run", "should_panic", "compile_fail", "ignore"}
            or re.fullmatch(r"edition\d{4}|ignore-.+", flag)
            for flag in flags
        ):
            doctests.append(fence[0])
    return items, doctests


def ambiguous_test_path(path):
    parts = Path(path).parts
    return (
        any(
            part
            in {"tests", "test", "test_support", "fixtures", "testdata", "snapshots"}
            or part.endswith(("_tests", "_test"))
            for part in parts[:-1]
        )
        or parts[-1] in {"tests.rs", "test.rs", "test_support.rs"}
        or path.endswith(("_test.rs", "_tests.rs", ".snap", ".snap.new"))
        or path == ".cargo/mutants.toml"
    )


def tree_paths(revision_id):
    return set(
        git("ls-tree", "-r", "--name-only", "-z", revision_id).split("\0")[:-1]
    )


def production_files_under(directory, files):
    prefix = f"{directory}/" if directory else ""
    return sorted(
        candidate
        for candidate in files
        if candidate.startswith(prefix)
        and candidate.endswith(".rs")
        and not ambiguous_test_path(candidate)
    )


def owned_source_files(path, files):
    """Map an in-tree unit-test module to its owning production file(s), if unambiguous."""
    if not path.startswith("src/") or not path.endswith(".rs"):
        return None
    parts = Path(path).parts
    if any(
        part in {"test_support", "fixtures", "testdata", "snapshots"}
        or part.endswith(("_tests", "_test"))
        for part in parts[:-1]
    ) or parts[-1] == "test_support.rs":
        return None

    # `src/.../<dir>/tests/<topic>.rs` owns the production files in `src/.../<dir>/`.
    if "tests" in parts[1:-1]:
        index = parts.index("tests", 1, len(parts) - 1)
        owner_dir = "/".join(parts[:index])
        owned = production_files_under(owner_dir, files)
        return owned or None

    parent = "/".join(parts[:-1])
    stem = Path(path).stem

    # `src/.../<dir>/tests.rs` or `src/.../<dir>/mutation_tests.rs` owns `<dir>/`.
    if stem in {"tests", "test", "mutation_tests", "mutation_test"}:
        if parent == "src":
            return None
        owned = production_files_under(parent, files)
        return owned or None

    # `src/.../<stem>_tests.rs` or `src/.../<stem>_test.rs`
    for suffix in ("_tests", "_test"):
        if not stem.endswith(suffix):
            continue
        base = stem[: -len(suffix)]
        if not base:
            return None
        # Direct sibling file (`auth_tests.rs` -> `auth.rs`, `process_tests.rs` -> `process.rs`).
        # Include the parent `mod.rs` when present because sibling test modules
        # often import `super::*` and exercise helpers declared in `mod.rs`.
        direct = f"{parent}/{base}.rs"
        if direct in files and not ambiguous_test_path(direct):
            owned = [direct]
            parent_mod = f"{parent}/mod.rs"
            if parent_mod in files:
                owned.append(parent_mod)
            return owned
        # Directory-backed module (`local_pool_tests.rs`, `local_pool_probe_tests.rs` -> `local_pool/`).
        prefix_parts = base.split("_")
        for count in range(len(prefix_parts), 0, -1):
            candidate_dir = f"{parent}/{'_'.join(prefix_parts[:count])}"
            if f"{candidate_dir}/mod.rs" in files:
                owned = production_files_under(candidate_dir, files)
                return owned or None
        return None

    return None


def changed_scope(base, head):
    changes = git("diff", "--name-status", "--no-renames", "-z", base, head).split(
        "\0"
    )[:-1]
    head_files = None
    selected = []
    for status, path in zip(changes[::2], changes[1::2], strict=True):
        if ambiguous_test_path(path):
            if status == "D":
                return "full", []
            if head_files is None:
                head_files = tree_paths(head)
            owned = owned_source_files(path, head_files)
            if not owned:
                return "full", []
            selected.extend(owned)
            continue
        if not path.endswith(".rs"):
            continue
        before = "" if status == "A" else git("show", f"{base}:{path}")
        after = "" if status == "D" else git("show", f"{head}:{path}")
        old_items, old_docs = test_content(before)
        new_items, new_docs = test_content(after)
        if old_docs != new_docs:
            return "full", []
        if old_items != new_items:
            if status == "D":
                return "full", []
            old_external = [text for text, ext in old_items if ext]
            new_external = [text for text, ext in new_items if ext]
            if old_external != new_external:
                return "full", []
            selected.append(path)
    if not selected:
        return "none", []
    arguments = []
    for path in sorted(set(selected)):
        arguments.extend(["--file", path])
    return "files", arguments


def revision(value):
    if not re.fullmatch(r"[0-9a-fA-F]{40}|[0-9a-fA-F]{64}", value):
        raise ValueError("event must contain complete Git object IDs")
    return value


def scope(event_name, event):
    if event_name in {"schedule", "workflow_dispatch"}:
        return "full", []
    if event_name == "push":
        if event.get("deleted"):
            return "none", []
        base, head = revision(event["before"]), revision(event["after"])
        if not base.strip("0"):
            base = git("hash-object", "-t", "tree", "--stdin", input="").strip()
    elif event_name == "pull_request":
        request = event["pull_request"]
        head = revision(request["head"]["sha"])
        base = git("merge-base", revision(request["base"]["sha"]), head).strip()
    else:
        raise ValueError(f"unsupported event: {event_name}")
    return changed_scope(base, head)


if __name__ == "__main__":
    mode, arguments = scope(sys.argv[1], json.loads(Path(sys.argv[2]).read_text()))
    print(f"run={str(mode != 'none').lower()}")
    print(f"mode={mode}")
    print(f"arguments={json.dumps(arguments)}")
