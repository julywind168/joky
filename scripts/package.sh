#!/bin/sh
# Package the host toolchain as a relocatable archive. Never publish from this script.
set -eu

task_profile=release
case "${1:-}" in
    --help|-h)
        printf 'Usage: scripts/package.sh [--debug] [output-directory]\nDefault output: target/dist; default profile: Release.\n'
        exit 0 ;;
    --debug) task_profile=debug; shift ;;
esac
if [ "$#" -gt 1 ]; then
    printf 'Expected one output directory. Use --help.\n' >&2; exit 2
fi
project_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
task_output=${1-"$project_root/target/dist"}
case "$task_output" in
    ''|-*) printf 'Invalid output directory. Use --help.\n' >&2; exit 2 ;;
esac
mkdir -p "$task_output"
task_output=$(CDPATH= cd -- "$task_output" && pwd)
cd "$project_root"
task_host=$(rustc -vV | sed -n 's/^host: //p')
case "$task_host" in
    *-apple-darwin|*-unknown-linux-gnu) ;;
    *) printf 'Unsupported distribution host: %s\n' "$task_host" >&2; exit 2 ;;
esac
task_version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)
task_name="joky-$task_version-$task_host"
if [ "$task_profile" = debug ]; then task_name="$task_name-debug"; fi
task_archive="$task_output/$task_name.tar.gz"
if [ -e "$task_archive" ] || [ -e "$task_archive.sha256" ]; then
    printf 'Refusing to overwrite an existing package: %s\n' "$task_archive" >&2; exit 2
fi
task_work=$(mktemp -d "$task_output/.joky-package.XXXXXX")
trap 'rm -rf "$task_work"' EXIT HUP INT TERM
set --
if [ "$task_profile" = debug ]; then set -- --debug; fi
./scripts/install.sh "$@" "$task_work/$task_name"
task_bundle="$task_work/$task_name"
python3 scripts/collect-licenses.py "$task_bundle/share/licenses/joky/THIRD_PARTY_NOTICES.txt"
mkdir -p "$task_bundle/share/doc/joky"
"$task_bundle/bin/joky" doc -o "$task_bundle/share/doc/joky/html"
cp -R examples "$task_bundle/share/doc/joky/examples"
cat > "$task_bundle/INSTALL.txt" <<'INSTALL'
Keep this entire directory together and add its bin directory to PATH.
Run `joky --version` to check the installation.
Run `joky run share/doc/joky/examples/hello.jk` from this directory (output: Hello, world!).
Native builds need the system C compiler (cc) and lib/joky/libjoky.a.
Open share/doc/joky/html/index.html for offline standard-library documentation.
Tutorial: https://github.com/julywind168/joky/blob/main/docs/getting-started.md
Installation: https://github.com/julywind168/joky/blob/main/docs/distribution.md
Licenses: share/licenses/joky/
INSTALL
printf '%s\n' "$task_host" > "$task_bundle/TARGET"
tar -czf "$task_work/$task_name.tar.gz" -C "$task_work" "$task_name"
(cd "$task_work" && shasum -a 256 "$task_name.tar.gz" > "$task_name.tar.gz.sha256")
mv "$task_work/$task_name.tar.gz" "$task_archive"
mv "$task_work/$task_name.tar.gz.sha256" "$task_archive.sha256"
printf 'Packaged %s\n' "$task_archive"
