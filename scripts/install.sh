#!/bin/sh
# Install the compiler and matching runtime together; Debug is opt-in for development.
set -eu

task_profile=release
case "${1:-}" in
    --help|-h)
        printf 'Usage: scripts/install.sh [--debug] [prefix]\nDefault prefix: ~/.local; default profile: Release.\n'
        exit 0 ;;
    --debug) task_profile=debug; shift ;;
esac
if [ "$#" -gt 1 ]; then
    printf 'Expected one installation prefix. Use --help.\n' >&2
    exit 2
fi
task_prefix=${1-"$HOME/.local"}
case "$task_prefix" in
    '') printf 'Installation prefix must not be empty.\n' >&2; exit 2 ;;
    -*) printf 'Unknown option: %s\n' "$task_prefix" >&2; exit 2 ;;
    /*) ;;
    *) task_prefix="$PWD/$task_prefix" ;;
esac
project_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$project_root"
task_target=${CARGO_TARGET_DIR:-target}
set --
if [ "$task_profile" = release ]; then set -- --release; fi
cargo build "$@" --locked --lib --bin joky --target-dir "$task_target"
install -d "$task_prefix/bin" "$task_prefix/lib/joky" "$task_prefix/include/joky" "$task_prefix/share/licenses/joky"
install -m 755 "$task_target/$task_profile/joky" "$task_prefix/bin/joky"
install -m 644 "$task_target/$task_profile/libjoky.a" "$task_prefix/lib/joky/libjoky.a"
install -m 644 include/joky_vm.h include/joky_cps.h include/joky_runtime.h include/joky_task.h include/joky_text_ffi.h include/joky_io.h include/joky_fs.h "$task_prefix/include/joky/"
install -m 644 LICENSE-MIT LICENSE-APACHE "$task_prefix/share/licenses/joky/"
printf 'Installed Joky in %s\nAdd %s/bin to PATH.\n' "$task_prefix" "$task_prefix"
