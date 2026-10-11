#!/usr/bin/env python3
"""Compare JSON CLI process startup using two already-built Joky binaries."""
import argparse
import csv
import hashlib
import json
import pathlib
import platform
import statistics
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", type=pathlib.Path, required=True)
    parser.add_argument("--after", type=pathlib.Path, required=True)
    parser.add_argument("--before-revision", required=True)
    parser.add_argument("--after-revision", required=True)
    parser.add_argument("--profile", required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--samples", type=int, default=5)
    args = parser.parse_args()
    if args.samples < 1:
        parser.error("--samples must be positive")
    binaries = {name: getattr(args, name).resolve() for name in ("before", "after")}
    root = pathlib.Path(__file__).resolve().parent.parent
    cases = {
        "derive-jit": ["run", "examples/json_derive.jk"],
        "manual-jit": ["run", "examples/json_struct.jk"],
        "derive-interpreter": ["run", "--interpreter", "examples/json_derive.jk"],
    }
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    rows = []
    for sample in range(args.samples):
        for case, command in cases.items():
            order = ("before", "after") if sample % 2 == 0 else ("after", "before")
            for version in order:
                start = time.perf_counter_ns()
                result = subprocess.run(
                    [str(binaries[version]), *command], cwd=root, capture_output=True
                )
                elapsed = time.perf_counter_ns() - start
                if result.returncode or result.stdout != b"Ada\n37\n/age\n" or result.stderr:
                    raise RuntimeError(f"{version} {case}: {result}")
                rows.append(dict(case=case, version=version, sample=sample, elapsed_ns=elapsed))
        print(f"Completed sample {sample + 1}/{args.samples}", flush=True)
    with (output / "samples.csv").open("w", newline="") as file:
        writer = csv.DictWriter(file, fieldnames=rows[0])
        writer.writeheader()
        writer.writerows(rows)
    summary = []
    for case in cases:
        group = dict(case=case)
        for version in binaries:
            values = [row["elapsed_ns"] / 1e9 for row in rows
                      if row["case"] == case and row["version"] == version]
            group[version] = dict(median_s=statistics.median(values), min_s=min(values), max_s=max(values))
        group["reduction_pct"] = (1 - group["after"]["median_s"] / group["before"]["median_s"]) * 100
        summary.append(group)
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    metadata = dict(
        profile=args.profile, before_revision=args.before_revision,
        after_revision=args.after_revision, samples=args.samples,
        platform=platform.platform(), commands=cases,
        method="Alternating standalone processes; wall time includes startup, compilation, execution and teardown. OS file cache is not flushed. No timing thresholds.",
        binaries={name: dict(path=str(path), sha256=hashlib.sha256(path.read_bytes()).hexdigest())
                  for name, path in binaries.items()},
        sources={path: hashlib.sha256((root / path).read_bytes()).hexdigest()
                 for path in ("examples/json_derive.jk", "examples/json_struct.jk")},
    )
    (output / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    (output / "runner.py").write_bytes(pathlib.Path(__file__).read_bytes())
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
