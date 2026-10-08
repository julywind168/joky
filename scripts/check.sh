#!/bin/sh
# One entry point for correctness checks; performance measurements stay opt-in.
set -eu

usage() {
    cat <<'EOF'
Usage: scripts/check.sh [--full | --stress | --quick] [--release]

Default: formatting, Clippy, default Debug tests and short benchmark correctness.
--full:  compatibility alias for the default Debug checks; no extra profile build.
--stress: run only exhaustive Debug tests and long Debug benchmark correctness.
--release: explicitly use Release for tests and benchmark correctness in either mode.
Release acceptance (when requested): run --full --release and --stress --release.
--quick: explicitly select the default quick mode.
--help:  show this help.

Requires the Rust toolchain and cc on macOS/Linux. No Lua or timed benchmarks.
EOF
}

mode=quick
mode_selected=false
profile=Debug
for arg in "$@"; do
    case "$arg" in
        --full|--stress|--quick)
            if [ "$mode_selected" = true ]; then
                usage >&2
                exit 2
            fi
            mode=${arg#--}
            mode_selected=true
            ;;
        --release)
            if [ "$profile" = Release ]; then
                usage >&2
                exit 2
            fi
            profile=Release
            ;;
        --help|-h) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
done

set --
if [ "$profile" = Release ]; then
    set -- --release
fi

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
    run cargo test "$@" exhaustive_ -- --ignored
    run cargo test "$@" --bench while
    run cargo test "$@" --bench map
else
    run cargo fmt -- --check
    run cargo clippy --all-targets -- -D warnings
    run cargo test "$@"
    # Preserve boundary sizes, seeds and all backends in the selected profile.
    run cargo test "$@" --bench while -- --iterations 0,1000
    run cargo test "$@" --bench map -- --iterations 0,32
fi
printf '\nChecks passed (%s, %s). No performance measurements were run.\n' "$mode" "$profile"
