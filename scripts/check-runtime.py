#!/usr/bin/env python3
"""Run the lib suite and each process-isolated runtime test, locally or in CI."""

import argparse
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
ROOT_ISOLATED = [
    "compiler::tests::files::file_workflows_release_handles_buffers_and_cancelled_outputs",
    "compiler::tests::resources::bounded_for_task_storage_and_cancellation_cleanup",
    "compiler::tests::resources::bounded_runtime_metadata_across_batches_and_scopes",
    "compiler::tests::runtime::tcp_read_cancelled_while_queued_discards_the_late_payload",
    "compiler::tests::runtime::tcp_read_completing_racing_cancellation_keeps_one_winner",
    "compiler::tests::runtime::tcp_write_argument_share_is_released_after_the_suspend",
    "compiler::tests::continuations::stress_cancellation_racing_nested_suspend_completions",
]
RUNTIME_ISOLATED = [
    "runtime::blocking::tests::file_backpressure_cancels_queued_requests_while_workers_are_occupied",
]


def run(command, env, log, timeout):
    print(f"Running {' '.join(map(str, command))}\n  log: {log}", flush=True)
    with log.open("w") as output:
        process = subprocess.Popen(
            command, cwd=ROOT, env=env, stdout=output,
            stderr=subprocess.STDOUT, start_new_session=True,
        )
        try:
            code = process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
            raise RuntimeError(f"Timed out after {timeout}s: {log}") from None
    text = log.read_text()
    if code != 0:
        print("\n".join(text.splitlines()[-80:]), file=sys.stderr)
        raise RuntimeError(f"Exit {code}: {log}")
    return text


def build_test_binary(cargo, arguments, manifest, env, log):
    output = run(cargo + arguments + ["--no-run", "--message-format=json"],
                 env, log, 1200)
    binaries = []
    for line in output.splitlines():
        if not line.startswith("{"):
            continue
        record = json.loads(line)
        if (record.get("reason") == "compiler-artifact"
                and record.get("manifest_path") == str(manifest)
                and record.get("profile", {}).get("test")
                and record.get("executable")):
            binaries.append(record["executable"])
    if len(binaries) != 1:
        raise RuntimeError(f"Expected one lib test executable, got {binaries}")
    return binaries[0]


def run_test_suite(binary, isolated, env, logs, prefix):
    listed = run([binary, "--list", "--ignored", "--format=terse"],
                 env, logs / f"{prefix}-list.log", 30)
    for name in isolated:
        if f"{name}: test" not in listed.splitlines():
            raise RuntimeError(f"Missing ignored test: {name}")
    output = run([binary, "--test-threads=1"],
                 env, logs / f"{prefix}-lib.log", 600)
    if not re.search(r"test result: ok\. [1-9][0-9]* passed; 0 failed;", output):
        raise RuntimeError(f"{prefix} library suite did not execute successfully")
    print(next(line for line in output.splitlines() if line.startswith("test result:")))
    for name in isolated:
        output = run([binary, name, "--ignored", "--exact", "--nocapture", "--test-threads=1"],
                     env, logs / f"{prefix}-{name.rsplit('::', 1)[-1]}.log", 180)
        if "test result: ok. 1 passed; 0 failed; 0 ignored;" not in output:
            raise RuntimeError(f"Expected exactly one executed test: {name}")
        for line in output.splitlines():
            if line.startswith(("test result:", "stress iterations completed:", "stress iteration latency:")):
                print(line, flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tsan", action="store_true",
                        help="use nightly, instrument std, and apply the documented stress floor")
    args = parser.parse_args()
    env = os.environ.copy()
    # A lower instrumented floor must not leak into the normal regression run.
    env["JOKY_STRESS_MIN_ITERATIONS"] = "50" if args.tsan else "300"
    logs = ROOT / "target" / "runtime-checks" / ("tsan" if args.tsan else "normal")
    logs.mkdir(parents=True, exist_ok=True)
    cargo = ["cargo"]
    if args.tsan:
        cargo += ["+nightly"]
        version = subprocess.check_output(["rustc", "+nightly", "-vV"], text=True)
        host = next(line.removeprefix("host: ") for line in version.splitlines()
                    if line.startswith("host: "))
        # Rebuild std too: prebuilt std hides synchronization (e.g. OnceLock)
        # from TSan. Do not bypass sanitizer ABI checks or suppress reports.
        env["RUSTFLAGS"] = "-Zsanitizer=thread"
        env.pop("CARGO_ENCODED_RUSTFLAGS", None)
        env["CARGO_TARGET_DIR"] = str(ROOT / "target" / "tsan")
        env["TSAN_OPTIONS"] = "halt_on_error=1:exitcode=66"
        cargo += ["test", "-Zbuild-std", "--target", host]
    else:
        cargo += ["test"]
    root_binary = build_test_binary(
        cargo, ["--locked", "--lib"], ROOT / "Cargo.toml",
        env, logs / "root-build.log")
    runtime_binary = build_test_binary(
        cargo,
        ["--locked", "--manifest-path", ROOT / "crates/joky-runtime/Cargo.toml", "--lib"],
        ROOT / "crates/joky-runtime/Cargo.toml",
        env,
        logs / "runtime-build.log",
    )
    run_test_suite(root_binary, ROOT_ISOLATED, env, logs, "root")
    run_test_suite(runtime_binary, RUNTIME_ISOLATED, env, logs, "runtime")
    print(f"All runtime checks passed; logs: {logs}")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, subprocess.SubprocessError) as error:
        sys.exit(str(error))
