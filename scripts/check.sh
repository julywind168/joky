#!/bin/sh
# One entry point for correctness checks; performance measurements stay opt-in.
set -eu

usage() {
    cat <<'EOF'
Usage: scripts/check.sh [--full | --quick]

Default: formatting, Clippy, default Debug tests and short benchmark correctness.
--full:  also run Release tests, exhaustive Release tests and full benchmark correctness.
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

run cargo fmt -- --check
run cargo clippy --all-targets -- -D warnings
run cargo test
if [ "$mode" = quick ]; then
    # Preserve boundary sizes, seeds and all backends; leave longer runs to full checks.
    run cargo test --bench while -- --iterations 0,1000
    run cargo test --bench map -- --iterations 0,32
else
    run cargo test --bench while
    run cargo test --bench map
fi
if [ "$mode" = full ]; then
    run cargo test --release
    # Run expensive correctness sweeps without enabling opt-in measurement tests.
    run cargo test --release exhaustive_ -- --ignored
    run cargo test --release --bench while
    run cargo test --release --bench map
fi
printf '\nChecks passed (%s). No performance measurements were run.\n' "$mode"
