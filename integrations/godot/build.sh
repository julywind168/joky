#!/usr/bin/env bash
set -euo pipefail
adapter_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cargo build --locked --manifest-path "$adapter_dir/Cargo.toml"
case "$(uname -s)" in
    Darwin) library=libjoky_godot.dylib ;;
    Linux) library=libjoky_godot.so ;;
    *) echo 'Use Cargo and copy the platform library to demo/bin on this platform.' >&2; exit 1 ;;
esac
mkdir -p "$adapter_dir/demo/bin"
cp "$adapter_dir/target/debug/$library" "$adapter_dir/demo/bin/$library"
