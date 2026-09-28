#!/bin/sh
# One entry point for correctness checks; performance measurements stay opt-in.
set -eu

usage() {
    cat <<'EOF'
Usage: scripts/check.sh [--quick]

Default: formatting, Clippy, Debug/Release tests and benchmark correctness.
--quick: formatting, Clippy, Debug tests and benchmark correctness only.
--help:  show this help.

Requires the Rust toolchain and cc on macOS/Linux. No Lua or timed benchmarks.
EOF
}

mode=full
if [ "$#" -gt 1 ]; then
    usage >&2
    exit 2
fi
case "${1-}" in
    '') ;;
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
run cargo test --bench while
if [ "$mode" = full ]; then
    run cargo test --release
    run cargo test --release --bench while
fi
printf '\nChecks passed (%s). No performance measurements were run.\n' "$mode"
