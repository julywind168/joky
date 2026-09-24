#!/usr/bin/env python3
"""Check local Markdown links, heading anchors, fences, and docs navigation.

Runs without third-party packages or network access. Covers root Markdown files,
docs/, and examples/, including untracked additions. Code is not executed.
"""

from collections import defaultdict
import html
from pathlib import Path
import re
import sys
import unicodedata
from urllib.parse import unquote, urlsplit


ROOT = Path(__file__).resolve().parents[1]
LINK_START = re.compile(r"!?\[(?:[^\[\]\n]|\[[^\]\n]*\])*\]\(")


def prose_lines(text):
    """Yield line numbers and text outside fenced code blocks."""
    fence = None
    fence_line = None
    for number, line in enumerate(text.splitlines(), 1):
        marker = re.match(r"^ {0,3}(`{3,}|~{3,})(.*)$", line)
        if fence is None and marker:
            fence = marker[1]
            fence_line = number
            continue
        if fence is not None:
            if (marker and marker[1][0] == fence[0]
                    and len(marker[1]) >= len(fence) and not marker[2].strip()):
                fence = None
            continue
        yield number, line
    if fence is not None:
        raise ValueError(f"line {fence_line}: unclosed code fence")


def slug(heading):
    heading = re.sub(r"\[([^]]+)\]\([^)]*\)", r"\1", heading)
    heading = html.unescape(re.sub(r"<[^>]*>", "", heading)).lower()
    heading = "".join(character for character in heading
                      if character in "-_ "
                      or unicodedata.category(character)[0] in "LNM")
    return heading.replace(" ", "-")


def anchors(lines):
    found = set()
    for _, line in lines:
        for anchor in re.findall(r'<[^>]+\b(?:id|name)=["\']([^"\']+)["\']', line):
            found.add(html.unescape(anchor))
        heading = re.match(r"^ {0,3}#{1,6}\s+(.+?)(?:\s+#+)?$", line)
        if heading:
            base = slug(heading[1])
            candidate = base
            suffix = 0
            while candidate in found:
                suffix += 1
                candidate = f"{base}-{suffix}"
            found.add(candidate)
    return found


def links(lines):
    """Read inline Markdown destinations, preserving nested parentheses."""
    definitions = {}
    for number, original_line in lines:
        # Inline code may contain Markdown-shaped examples or Rust attributes.
        line = re.sub(r"(`+)(.+?)\1", lambda match: " " * len(match[0]), original_line)
        definition = re.match(r"^ {0,3}\[([^]]+)\]:\s*<?([^\s>]+)>?", line)
        if definition:
            definitions[definition[1].casefold()] = definition[2]
            yield number, definition[2]
            continue
        for start in LINK_START.finditer(line):
            pos = start.end()
            while pos < len(line) and line[pos].isspace():
                pos += 1
            if pos >= len(line):
                continue
            if line[pos] == "<":
                end = line.find(">", pos + 1)
                if end >= 0:
                    yield number, line[pos + 1:end]
                continue
            begin = pos
            depth = 0
            while pos < len(line):
                char = line[pos]
                if char == "\\" and pos + 1 < len(line):
                    pos += 2
                    continue
                if char == "(":
                    depth += 1
                elif char == ")":
                    if depth == 0:
                        break
                    depth -= 1
                elif char.isspace() and depth == 0:
                    break
                pos += 1
            if pos > begin:
                yield number, re.sub(r"\\([()])", r"\1", line[begin:pos])
    # Explicit reference links should not silently point to missing definitions.
    for number, line in lines:
        line = re.sub(r"(`+)(.+?)\1", "", line)
        for label, reference in re.findall(r"\[([^]\n]+)\]\[([^]\n]*)\]", line):
            key = (reference or label).casefold()
            if key not in definitions:
                raise ValueError(f"line {number}: undefined link reference [{reference or label}]")


def markdown_files():
    return sorted(set(ROOT.glob("*.md")) | set((ROOT / "docs").rglob("*.md"))
                  | {path for path in example_files() if path.suffix == ".md"})


def example_files():
    return {path for path in (ROOT / "examples").rglob("*")
            if path.is_file() and path.suffix in {".md", ".jk"}
            and not {".joky", "target"}.intersection(path.relative_to(ROOT / "examples").parts)}


def reachable(graph, entry):
    reached = set()
    pending = [entry]
    while pending:
        path = pending.pop()
        if path not in reached:
            reached.add(path)
            pending.extend(graph[path] - reached)
    return reached


def exact_case(path):
    """Catch links that work on macOS but fail on case-sensitive hosts."""
    parent = ROOT
    for part in path.relative_to(ROOT).parts:
        if part not in {child.name for child in parent.iterdir()}:
            return False
        parent = parent / part
    return True


def main():
    files = markdown_files()
    errors = []
    parsed = {}
    graph = defaultdict(set)
    checked = 0
    for path in files:
        try:
            lines = list(prose_lines(path.read_text()))
            parsed[path.resolve()] = (anchors(lines), list(links(lines)))
        except ValueError as error:
            errors.append(f"{path.relative_to(ROOT)}: {error}")

    for path, (_, destinations) in parsed.items():
        for number, destination in destinations:
            url = urlsplit(html.unescape(destination))
            if url.scheme or url.netloc:
                continue
            checked += 1
            local = unquote(url.path)
            if local.startswith("/"):
                target = (ROOT / local.lstrip("/")).resolve()
            else:
                target = (path.parent / local).resolve() if local else path
            location = f"{path.relative_to(ROOT)}:{number}"
            if not target.is_relative_to(ROOT):
                errors.append(f"{location}: target leaves repository: {destination}")
                continue
            if not target.exists():
                errors.append(f"{location}: missing target: {destination}")
                continue
            if not exact_case(target):
                errors.append(f"{location}: target case does not match: {destination}")
                continue
            if target.is_dir():
                index = target / "README.md"
                if index.is_file():
                    target = index.resolve()
            graph[path].add(target)
            if target.suffix == ".md":
                if target not in parsed:
                    try:
                        parsed_lines = list(prose_lines(target.read_text()))
                        target_anchors = anchors(parsed_lines)
                    except ValueError as error:
                        errors.append(f"{location}: {error}")
                        continue
                else:
                    target_anchors = parsed[target][0]
                if url.fragment and unquote(url.fragment) not in target_anchors:
                    errors.append(f"{location}: missing heading: {destination}")

    entry = (ROOT / "docs/README.md").resolve()
    reached = reachable(graph, entry)
    documents = {path.resolve() for path in files if path.is_relative_to(ROOT / "docs")}
    for path in sorted(documents - reached):
        errors.append(f"{path.relative_to(ROOT)}: unreachable from docs/README.md")

    examples = {path.resolve() for path in example_files()}
    reached_examples = reachable(graph, (ROOT / "examples/README.md").resolve())
    for path in sorted(examples - reached_examples):
        errors.append(f"{path.relative_to(ROOT)}: unreachable from examples/README.md")

    if errors:
        print("\n".join(errors), file=sys.stderr)
        print(f"Documentation check failed: {len(errors)} problem(s).", file=sys.stderr)
        return 1
    print(f"Checked {len(files)} Markdown files, {checked} local links, and heading anchors.")
    print(f"All {len(documents)} docs pages are reachable from docs/README.md; code fences are closed.")
    sources = sum(path.suffix == ".jk" for path in examples)
    print(f"All {sources} example sources and their Markdown pages are reachable from examples/README.md.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
