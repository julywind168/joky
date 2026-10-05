"""Real-engine acceptance; every invocation has a timeout and output checks."""
import os
import subprocess
import sys

engine, project = sys.argv[1:]


def run(label, *args, marker=None, expected_errors=(), editor_test=False):
    result = subprocess.run(
        [engine, "--headless", "--path", project, "--quit-after", "600", *args],
        env={**os.environ, "JOKY_G0_EDITOR_TEST": "1" if editor_test else "0"},
        text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=120,
    )
    print(result.stdout, end="", flush=True)
    if result.returncode or (marker and marker not in result.stdout):
        raise SystemExit(f"{label} failed ({result.returncode})")
    forbidden = ["panicked", "leaked", "G0 CHECK FAILED"]
    errors = [line for line in result.stdout.splitlines() if "ERROR:" in line]
    if len(errors) != len(expected_errors) or any(
        not actual.startswith("ERROR: " + expected)
        for actual, expected in zip(errors, expected_errors)
    ):
        raise SystemExit(f"{label} emitted unexpected diagnostics: {errors}")
    if any(word in result.stdout for word in forbidden):
        raise SystemExit(f"{label} emitted unexpected errors")


run("editor import", "--editor", "--import")
run("editor placeholders", "--editor", marker="G0 EDITOR PASS", editor_test=True)
run("runtime", "--script", "res://tests/headless.gd", marker="G0 HEADLESS PASS")
run("reopen", "--script", "res://tests/reopen.gd", marker="G0 REOPEN PASS")
run("diagnostics", "--script", "res://tests/diagnostics.gd", marker="G0 DIAGNOSTICS PASS", expected_errors=("user://bad_type.jk:6:", "user://bad_runtime.jk:6:", "user://bad_constructor.jk:"))
