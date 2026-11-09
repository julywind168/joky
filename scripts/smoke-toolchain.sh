#!/bin/sh
# Basic installed-toolchain behavior only; no full, stress or Release test suite.
set -eu

if [ "$#" -ne 1 ]; then
    printf 'Usage: scripts/smoke-toolchain.sh /absolute/path/to/joky\n' >&2
    exit 2
fi
task_cli=$1
case "$task_cli" in
    /*) ;;
    *) printf 'Expected an absolute path to the Joky executable.\n' >&2; exit 2 ;;
esac
if [ ! -x "$task_cli" ]; then
    printf 'Joky executable is not executable: %s\n' "$task_cli" >&2
    exit 2
fi
# Verify the toolchain's own runtime discovery and default execution mode.
unset JOKY_RUNTIME JOKY_TASK_WORKERS
task_work=$(mktemp -d "${TMPDIR:-/tmp}/joky-smoke.XXXXXX")
trap 'rm -rf "$task_work"' EXIT HUP INT TERM
cd "$task_work"
"$task_cli" --version
"$task_cli" init 'hello project' --name hello
"$task_cli" check 'hello project'
"$task_cli" fmt --check 'hello project'
"$task_cli" help run > help.txt
test -s help.txt
"$task_cli" run 'hello project' > jit.txt
test "$(cat jit.txt)" = 'Hello, Joky!'
"$task_cli" run --interpreter 'hello project' > interpreter.txt
test "$(cat interpreter.txt)" = 'Hello, Joky!'
"$task_cli" build 'hello project'
'./hello project/dist/hello' > native.txt
test "$(cat native.txt)" = 'Hello, Joky!'
if "$task_cli" check missing.jk > missing.out 2> missing.err; then
    printf 'Missing-source check unexpectedly succeeded.\n' >&2
    exit 1
fi
test ! -s missing.out
test -s missing.err
printf 'Passed: init, check, fmt --check, help, JIT, interpreter, AOT and missing-source diagnostics.\n'
