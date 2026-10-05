#!/usr/bin/env bash
set -euo pipefail
adapter_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
if [[ -n "${GODOT_BIN:-}" ]]; then
    godot_bin="$GODOT_BIN"
elif command -v godot >/dev/null 2>&1; then
    godot_bin="$(command -v godot)"
elif [[ -x /Applications/Godot.app/Contents/MacOS/Godot ]]; then
    godot_bin=/Applications/Godot.app/Contents/MacOS/Godot
else
    echo 'Set GODOT_BIN to a standard Godot 4.7 executable.' >&2
    exit 1
fi
version="$("$godot_bin" --version)"
case "$version" in
    4.7.*) ;;
    *) echo "Expected Godot 4.7.x; found $version" >&2; exit 1 ;;
esac
cargo fmt --manifest-path "$adapter_dir/Cargo.toml" --check
cargo clippy --locked --manifest-path "$adapter_dir/Cargo.toml" --all-targets -- -D warnings
cargo test --locked --manifest-path "$adapter_dir/Cargo.toml"
"$adapter_dir/build.sh"
python3 "$adapter_dir/tests/run.py" "$godot_bin" "$adapter_dir/demo"
