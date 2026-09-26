#!/bin/sh
# Install the compiler and its matching standard runtime as one distribution.
set -eu

task_prefix=${1:-"$HOME/.local"}
case "$task_prefix" in
    /*) ;;
    *) task_prefix="$PWD/$task_prefix" ;;
esac
project_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$project_root"
task_target=${CARGO_TARGET_DIR:-target}
cargo build --release --lib --bin joky --target-dir "$task_target"
install -d "$task_prefix/bin" "$task_prefix/lib/joky" "$task_prefix/include/joky"
install -m 755 "$task_target/release/joky" "$task_prefix/bin/joky"
install -m 644 "$task_target/release/libjoky.a" "$task_prefix/lib/joky/libjoky.a"
install -m 644 include/joky_vm.h include/joky_cps.h "$task_prefix/include/joky/"
printf 'Installed Joky in %s\nAdd %s/bin to PATH.\n' "$task_prefix" "$task_prefix"
