#!/bin/sh
# Build a self-contained application directory from one source snapshot.
set -eu
app_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
output=${1:-$app_dir/dist}
if [ "$#" -gt 0 ]; then shift; fi
if [ -e "$output" ] || [ -L "$output" ]; then
    echo "Output already exists: $output. Choose a new directory." >&2
    exit 1
fi
mkdir -p -- "$(dirname -- "$output")"
output=$(CDPATH= cd -- "$(dirname -- "$output")" && pwd)/$(basename -- "$output")
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
# Resolve a relative runtime override before entering the source snapshot.
if [ -n "${JOKY_RUNTIME:-}" ]; then
    case $JOKY_RUNTIME in /*) ;; *) JOKY_RUNTIME=$PWD/$JOKY_RUNTIME; export JOKY_RUNTIME ;; esac
fi
stage=$(mktemp -d "${output}.tmp.XXXXXX")
trap 'rm -rf -- "$stage"' EXIT
trap 'exit 1' HUP INT TERM
cp "$app_dir/main.jk" "$app_dir/contract.jk" "$app_dir/run.sh" "$stage/"
cp -R "$app_dir/host" "$app_dir/script" "$stage/"
(cd "$stage" && "$tool" build "$@" main.jk -o chat)
rm "$stage/main.jk"
rm -r "$stage/host"
cat > "$stage/README.md" <<'TEXT'
# Chat application

Run `./run.sh` from any working directory. No Joky compiler installation is needed.
The executable targets the platform on which it was built.

Chat listens on 127.0.0.1:7777; administration listens on 127.0.0.1:7778.
Edit `script/main.jk`, then send `reload` to the administration port.
A rejected reload keeps the previous version running. Send `shutdown` to stop.

`contract.jk` is the shared contract compiled into this host. Keep it paired with
this executable; contract changes require rebuilding and restarting the host.
Move or copy this entire directory, and always launch it through `run.sh`.
TEXT
# Never replace an existing application or its edited scripts.
if [ -e "$output" ] || [ -L "$output" ]; then
    echo "Output appeared during the build: $output" >&2
    exit 1
fi
mv "$stage" "$output"
trap - EXIT
printf 'Built chat application: %s\n' "$output"
