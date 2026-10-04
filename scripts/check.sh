#!/bin/sh
# One entry point for correctness checks; performance measurements stay opt-in.
set -eu

usage() {
    cat <<'EOF'
Usage: scripts/check.sh [--full | --stress | --quick]

Default: formatting, Clippy, default Debug tests and short benchmark correctness.
--full:  also run default Release tests and short Release benchmark correctness.
--stress: run only exhaustive Release tests and long Release benchmark correctness.
Release acceptance: run --full and --stress.
--quick: explicitly select the default quick mode.
--help:  show this help.

Requires the Rust toolchain and cc on macOS/Linux. No Lua or timed benchmarks.
EOF
}

mode=quick
if [ "$#" -gt 1 ]; then
    usage >&2
    exit 2
fi
case "${1-}" in
    '') ;;
    --full) mode=full ;;
    --stress) mode=stress ;;
    --quick) mode=quick ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
esac

project_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$project_root"

run() {
    printf '\n+'
    printf ' %s' "$@"
    printf '\n'
    "$@"
}

if [ "$mode" = stress ]; then
    # Keep expensive sweeps independent of the normal development checks.
    run cargo test --release exhaustive_ -- --ignored
    run cargo test --release --bench while
    run cargo test --release --bench map
else
    run cargo fmt -- --check
    run cargo clippy --all-targets -- -D warnings
    run cargo test
    # Preserve boundary sizes, seeds and all backends in both profiles.
    run cargo test --bench while -- --iterations 0,1000
    run cargo test --bench map -- --iterations 0,32
    if [ "$mode" = full ]; then
        run cargo test --release
        run cargo test --release --bench while -- --iterations 0,1000
        run cargo test --release --bench map -- --iterations 0,32
    fi
fi
printf '\nChecks passed (%s). No performance measurements were run.\n' "$mode"
