#!/usr/bin/env python3
"""Collect locked host dependencies' license documents for a distribution."""

import argparse
import json
from pathlib import Path
import subprocess


ROOT = Path(__file__).resolve().parent.parent
LICENSE_NAMES = ("license", "licence", "copying", "notice", "copyright")


def license_documents(package):
    root = Path(package["manifest_path"]).parent
    files = set()
    declared = package.get("license_file")
    if declared and (root / declared).is_file():
        files.add(root / declared)
    for path in root.iterdir():
        if not any(word in path.name.lower() for word in LICENSE_NAMES):
            continue
        if path.is_file():
            files.add(path)
        elif path.is_dir():
            files.update(child for child in path.rglob("*") if child.is_file())
    return [(path.relative_to(root).as_posix(), path.read_text()) for path in sorted(files)]


def workspace_identity(package):
    vcs = Path(package["manifest_path"]).parent / ".cargo_vcs_info.json"
    commit = json.loads(vcs.read_text()).get("git", {}).get("sha1") if vcs.is_file() else None
    return (package.get("license"), commit) if commit else None


def collect():
    rustc = subprocess.check_output(["rustc", "-vV"], text=True)
    host = next(line.removeprefix("host: ") for line in rustc.splitlines() if line.startswith("host: "))
    metadata = json.loads(subprocess.check_output([
        "cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", host,
    ], cwd=ROOT, text=True))
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    selected = set()
    pending = [metadata["resolve"]["root"]]
    while pending:
        identity = pending.pop()
        if identity in selected:
            continue
        selected.add(identity)
        for dependency in nodes[identity]["deps"]:
            if any(kind["kind"] != "dev" for kind in dependency["dep_kinds"]):
                pending.append(dependency["pkg"])
    packages = sorted((package for package in metadata["packages"]
                       if package["id"] in selected and package["source"] is not None),
                      key=lambda package: (package["name"], package["version"]))
    documents = {package["id"]: license_documents(package) for package in packages}
    workspaces = {}
    for package in packages:
        key = workspace_identity(package)
        if key and documents[package["id"]]:
            workspaces.setdefault(key, package)
    output = [
        "Third-party license notices", "",
        f"Locked production and build dependencies for {host}.",
        "Build-time dependencies are included; this is not a list of runtime shared libraries.", "",
    ]
    for package in packages:
        provider = package
        files = documents[package["id"]]
        if not files:
            # Cranelift/Wasmtime crates share a workspace license. Only reuse a
            # document with the same upstream commit and license expression.
            provider = workspaces.get(workspace_identity(package))
            if provider:
                files = documents[provider["id"]]
        if not files or not package.get("license"):
            raise RuntimeError(f"Missing license documents: {package['name']} {package['version']}")
        output += ["=" * 72, f"{package['name']} {package['version']}",
                   f"Declared license: {package['license']}",
                   f"Source: https://crates.io/crates/{package['name']}/{package['version']}"]
        if package["authors"]:
            output.append("Declared authors: " + ", ".join(package["authors"]))
        if provider["id"] != package["id"]:
            output.append(f"Workspace license from {provider['name']} {provider['version']} "
                          f"at {workspace_identity(package)[1]}")
        for name, text in files:
            output += ["", f"--- {name} ---", text.rstrip(), ""]
    return "\n".join(output), len(packages)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    arguments = parser.parse_args()
    text, count = collect()
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    arguments.output.write_text(text, encoding="utf-8")
    print(f"Collected license documents for {count} dependencies")


if __name__ == "__main__":
    main()
