#!/usr/bin/env python3
"""Check the pinned JSONTestSuite corpus and encoded values on all three backends."""

import argparse
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
import hashlib
import itertools
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from urllib.parse import quote as quote_url
import urllib.request


ROOT = Path(__file__).resolve().parents[1]
LOCK = Path(__file__).with_name("json_conformance.lock.json")


class CheckError(Exception):
    pass


@dataclass(frozen=True)
class Number:
    # A separate type preserves number spelling without equating 1 with "1".
    spelling: str


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate key")
        result[key] = value
    return result


def reject_constant(value):
    raise ValueError(f"non-JSON constant: {value}")


def semantic_value(data):
    """Python's independent parser plus Joky's documented strict default policy."""
    if len(data) > 1048576:
        raise ValueError("input limit")
    value = json.loads(
        data.decode("utf-8"),
        parse_int=Number,
        parse_float=Number,
        parse_constant=reject_constant,
        object_pairs_hook=unique_object,
    )
    pending = [(value, 0)]
    nodes = 0
    while pending:
        item, depth = pending.pop()
        nodes += 1
        if nodes > 65536:
            raise ValueError("node limit")
        if isinstance(item, Number):
            if len(item.spelling) > 1024:
                raise ValueError("number limit")
        elif isinstance(item, str):
            if len(item.encode("utf-8")) > 262144:
                raise ValueError("string limit")
        elif isinstance(item, (list, dict)):
            if depth >= 128:
                raise ValueError("depth limit")
            if isinstance(item, dict):
                for key in item:
                    if len(key.encode("utf-8")) > 262144:
                        raise ValueError("key limit")
                children = item.values()
            else:
                children = item
            pending.extend((child, depth + 1) for child in children)
    return value


def expected_values(cases, policy_rejections):
    expected = []
    for name, data in cases:
        try:
            result = (True, semantic_value(data))
        except (ValueError, RecursionError):
            result = (False, None)
        # Do not silently let a permissive reference parser override y_/n_ labels.
        if name.startswith("y_") and result[0] != (name not in policy_rejections):
            raise CheckError(f"reference policy disagrees with required acceptance: {name}")
        if name.startswith("n_") and result[0]:
            raise CheckError(f"reference parser accepted a forbidden case: {name}")
        expected.append(result)
    return expected


def verified_bytes(path, checksum):
    data = path.read_bytes()
    if hashlib.sha256(data).hexdigest() != checksum:
        raise CheckError(f"corpus SHA-256 mismatch: {path}")
    return data


def corpus(lock, directory, download):
    files = lock["files"]
    if download:
        directory.mkdir(parents=True, exist_ok=True)

        def fetch(entry):
            name, checksum = entry
            path = directory / name
            if not path.exists():
                url = (
                    "https://raw.githubusercontent.com/nst/JSONTestSuite/"
                    f"{lock['revision']}/test_parsing/{quote_url(name, safe='')}"
                )
                with urllib.request.urlopen(url, timeout=30) as response:
                    data = response.read()
                if hashlib.sha256(data).hexdigest() != checksum:
                    raise CheckError(f"download SHA-256 mismatch: {name}")
                # Never publish an incomplete download into the reusable cache.
                with tempfile.NamedTemporaryFile(dir=directory, delete=False) as temporary:
                    temporary.write(data)
                    temporary_path = Path(temporary.name)
                temporary_path.replace(path)
            verified_bytes(path, checksum)

        with ThreadPoolExecutor(max_workers=8) as pool:
            list(pool.map(fetch, files.items()))
    actual = {path.name for path in directory.glob("*.json")}
    if actual != set(files):
        raise CheckError(
            f"corpus file set differs: missing={sorted(set(files) - actual)}, "
            f"extra={sorted(actual - set(files))}"
        )
    return [(name, verified_bytes(directory / name, digest)) for name, digest in sorted(files.items())]


def quote(text):
    parts = ['"']
    for char in text:
        if char in '\\"':
            parts.append("\\" + char)
        elif char in "{}":
            parts.append(char * 2)
        elif ord(char) < 32:
            parts.append("\\u{" + format(ord(char), "x") + "}")
        else:
            parts.append(char)
    return "".join(parts) + '"'


def input_source(data):
    try:
        text = data.decode("utf-8")
    except UnicodeError:
        text = None
    if text is not None and len(text) <= 5000:
        return [f"let text = {quote(text)}", "check(text.into_bytes())"]
    parts = ["var bytes = Array.new::<UInt8>()"]
    # Keep the two large repetitive adversarial inputs out of compiler AST limits.
    prefix = data.rstrip(b"\n")
    for width in range(1, 65):
        if len(prefix) > 5000 and prefix == prefix[:width] * (len(prefix) // width):
            parts.extend(["var repeat: UInt64 = 0", f"while repeat < {len(prefix) // width} {{"])
            parts.extend(f"bytes.push({byte})" for byte in prefix[:width])
            parts.extend(["repeat = repeat + 1", "}"])
            data = data[len(prefix):]
            break
    for byte, group in itertools.groupby(data):
        count = sum(1 for _ in group)
        if count < 4:
            parts.extend([f"bytes.push({byte})"] * count)
        else:
            parts.extend([
                "var i: UInt64 = 0",
                f"while i < {count} {{ bytes.push({byte}) i = i + 1 }}",
            ])
    return parts + ["check(bytes)"]


def fixture(cases):
    parts = ['''import joky/json as json
import joky/array.{type Array}
fn check(input: Array<UInt8>) {
    match json.decode_bytes(input) {
        | Ok(value) => {
            match json.encode(&value) {
                | Ok(output) => { println(output) }
                | Err(_) => { println("ENCODE_ERROR") }
            }
        }
        | Err(_) => { println("REJECT") }
    }
}''']
    for index, (_, data) in enumerate(cases):
        parts.extend([f"fn case_{index}() {{", *input_source(data), "}"])
    parts.extend(["fn main() {", *(f"case_{i}()" for i in range(len(cases))), "}"])
    return "\n".join(parts) + "\n"


def check_output(data, cases, expected):
    # U+0085/U+2028/U+2029 are valid raw JSON string content, not record separators.
    if not data.endswith(b"\n"):
        raise CheckError("missing final output newline")
    lines = data[:-1].split(b"\n")
    if len(lines) != len(cases):
        raise CheckError(f"expected {len(cases)} output records, got {len(lines)}")
    accepted = 0
    for (name, _), (valid, value), line in zip(cases, expected, lines):
        if line == b"REJECT":
            if valid:
                raise CheckError(f"{name}: unexpected rejection")
            continue
        if not valid:
            raise CheckError(f"{name}: unexpected acceptance or encoding failure")
        try:
            actual = semantic_value(line)
        except (ValueError, RecursionError) as error:
            raise CheckError(f"{name}: invalid encoded output: {error}") from error
        if actual != value:
            raise CheckError(f"{name}: changed value kind, content, or number spelling")
        accepted += 1
    return accepted


def command(args, work, label, timeout=None, env=None):
    try:
        result = subprocess.run(args, cwd=ROOT, env=env, capture_output=True, timeout=timeout)
    except subprocess.TimeoutExpired as error:
        (work / f"{label}.stdout").write_bytes(error.stdout or b"")
        (work / f"{label}.stderr").write_bytes(error.stderr or b"")
        raise CheckError(f"{label}: timed out after {timeout}s") from error
    (work / f"{label}.stdout").write_bytes(result.stdout)
    (work / f"{label}.stderr").write_bytes(result.stderr)
    if result.returncode:
        raise CheckError(f"{label}: exited with status {result.returncode}; see {work}")
    return result


def build(work):
    result = command(
        ["cargo", "build", "--locked", "--bin", "joky", "--lib", "--message-format=json"],
        work, "cargo-build",
    )
    executable = archive = None
    for line in result.stdout.splitlines():
        artifact = json.loads(line)
        if artifact.get("reason") != "compiler-artifact":
            continue
        if artifact["target"]["name"] == "joky":
            if artifact.get("executable"):
                executable = Path(artifact["executable"])
            for filename in artifact["filenames"]:
                if Path(filename).name == "libjoky.a":
                    archive = Path(filename)
    if executable is None or archive is None:
        raise CheckError("Cargo did not produce the Joky executable and static runtime")
    return executable, archive


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", type=Path, help="offline test_parsing directory; verify every checksum")
    parser.add_argument("--work-dir", type=Path, default=ROOT / "target/json-conformance",
                        help="corpus cache and retained execution artifacts")
    parser.add_argument("--timeout", type=int, default=240, help="seconds per backend command (default: 240)")
    args = parser.parse_args()
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    if sys.platform not in ("darwin", "linux"):
        parser.error("native AOT execution currently requires macOS or Linux")
    lock = json.loads(LOCK.read_text(encoding="utf-8"))
    base = args.work_dir.resolve()
    base.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix="run-", dir=base))
    summary = {"corpus_revision": lock["revision"], "backends": {}, "passed": False}
    print(f"Artifacts: {work}", flush=True)
    try:
        directory = args.corpus.resolve() if args.corpus else base / "corpus" / lock["revision"]
        cases = corpus(lock, directory, download=args.corpus is None)
        expected = expected_values(cases, lock["policy_rejections"])
        source = work / "corpus.jk"
        source.write_text(fixture(cases), encoding="utf-8")
        summary.update(cases=len(cases), policy_rejections=lock["policy_rejections"])
        print(f"Verified {len(cases)} cases at {lock['revision']}; building Debug tools", flush=True)
        executable, archive = build(work)
        env = {**os.environ, "JOKY_RUNTIME": str(archive)}
        for backend, arguments in [
            ("jit", ["run", str(source)]),
            ("interpreter", ["run", str(source), "--interpreter"]),
            ("aot", ["build", str(source)]),
        ]:
            label = "aot-build" if backend == "aot" else backend
            result = command([str(executable), *arguments], work, label, args.timeout, env)
            expected_stderr = f"Built {source.with_suffix('')}\n".encode() if backend == "aot" else b""
            if result.stderr != expected_stderr:
                raise CheckError(f"{label}: unexpected stderr; see {work}")
            if backend == "aot":
                result = command([str(source.with_suffix(""))], work, backend, args.timeout, env)
                if result.stderr:
                    raise CheckError(f"{backend}: unexpected stderr; see {work}")
            accepted = check_output(result.stdout, cases, expected)
            summary["backends"][backend] = {"accepted": accepted, "rejected": len(cases) - accepted}
            print(f"{backend}: {len(cases)} passed ({accepted} accepted, {len(cases) - accepted} rejected)", flush=True)
        summary["passed"] = True
    except (CheckError, OSError, ValueError) as error:
        summary["error"] = str(error)
        print(f"JSON conformance failed: {error}", file=sys.stderr)
        return 1
    finally:
        (work / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    print("JSON conformance passed on all three backends. No performance measurements were run.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
