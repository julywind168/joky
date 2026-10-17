"""Real-engine acceptance; every invocation has a timeout and output checks."""
import os
import subprocess
import sys

engine, project = sys.argv[1:]


def run(label, *args, marker=None, expected_errors=(), editor_test=False, visual=False, max_frames=600):
    result = subprocess.run(
        [engine, *([] if visual else ["--headless"]), "--path", project, "--quit-after", str(max_frames), *args],
        env={**os.environ, "JOKY_G0_EDITOR_TEST": "1" if editor_test else "0", "JOKY_G1_VISUAL_TEST": "1" if visual else "0"},
        text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=120,
    )
    print(result.stdout, end="", flush=True)
    if result.returncode or (marker and marker not in result.stdout):
        raise SystemExit(f"{label} failed ({result.returncode})")
    forbidden = ["panicked", "leaked", "G0 CHECK FAILED", "G1 CHECK FAILED", "G2 CHECK FAILED", "NET3 CHECK FAILED", "NET4 CHECK FAILED", "NET5 CHECK FAILED", "NET6 CHECK FAILED", "JSON3 CHECK FAILED"]
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

run("G1 gameplay", "--script", "res://tests/gameplay.gd", marker="G1 GAMEPLAY PASS")
run("G1 scene reopen", "--script", "res://tests/gameplay_reopen.gd", marker="G1 REOPEN PASS")
run("G1 diagnostics", "--script", "res://tests/gameplay_diagnostics.gd", marker="G1 DIAGNOSTICS PASS", expected_errors=("godot/node.jk:", "godot/node.jk:", "godot/node.jk:", "user://g1_collision.jk:"))
run("G1 signal reentry", "--script", "res://tests/gameplay_reentry.gd", marker="G1 REENTRY PASS", expected_errors=("res://mover.jk:1:1: Instance is busy (reentrant call)",))
run("G1 shutdown with live roots", "--script", "res://tests/gameplay_shutdown.gd", marker="G1 SHUTDOWN PASS")
run("G1 rendered gameplay", "--script", "res://tests/gameplay.gd", marker="G1 GAMEPLAY PASS", visual=True)

run("G2 async tasks", "--script", "res://tests/async.gd", marker="G2 ASYNC PASS")
run("G2 task errors", "--script", "res://tests/async_errors.gd", marker="G2 ERRORS PASS", expected_errors=("res://async_drop.jk:", "godot/task.jk:", "godot/task.jk:"))
run("G2 shutdown", "--script", "res://tests/async_shutdown.gd", marker="G2 SHUTDOWN PASS")
run("G2 asynchronous motion demo", "--script", "res://tests/async_motion.gd", marker="G2 MOTION PASS")
run("G2 reentrant script detach", "--script", "res://tests/async_detach.gd", marker="G2 DETACH PASS")
run("borrowed dyn instance views", "--script", "res://tests/borrowed_dyn.gd", marker="DYN GODOT PASS")

run("NET3 network composition", "--script", "res://tests/network.gd", marker="NET3 GODOT PASS")
run("NET4 buffered message composition", "--script", "res://tests/network_messages.gd", marker="NET4 GODOT PASS")

run("NET5 duplex message sessions", "--script", "res://tests/network_duplex.gd", marker="NET5 GODOT PASS", max_frames=1800)
run("NET6 system name lookup", "--script", "res://tests/network_dns.gd", marker="NET6 GODOT PASS", max_frames=2400)
run("NET6 shutdown with live lookup", "--script", "res://tests/network_dns_shutdown.gd", marker="NET6 SHUTDOWN PASS")
run("JSON3 framed JSON sessions", "--script", "res://tests/network_json.gd", marker="JSON3 GODOT PASS", max_frames=6000)
