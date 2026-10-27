#!/bin/sh
# Resolve the application before switching directories; runtime files use this root.
set -eu
app_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
if [ ! -f "$app_dir/main.jk" ] && [ -x "$app_dir/chat" ]; then
    if [ "$#" -ne 0 ]; then
        echo 'The native bundle takes no options; select task workers when building.' >&2
        exit 2
    fi
    cd "$app_dir"
    exec ./chat
fi
if [ -n "${JOKY:-}" ]; then
    tool=$JOKY
elif command -v joky >/dev/null 2>&1; then
    tool=joky
else
    tool=$app_dir/../../target/debug/joky
fi
tool=$(command -v "$tool") || {
    echo 'Cannot find Joky. Build it with cargo build --bin joky --lib, or set JOKY.' >&2
    exit 1
}
case $tool in
    /*) ;;
    *) tool=$(CDPATH= cd -- "$(dirname -- "$tool")" && pwd)/$(basename -- "$tool") ;;
esac
cd "$app_dir"
exec "$tool" run "$@" main.jk
