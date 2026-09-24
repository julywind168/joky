#!/usr/bin/env python3
"""Build and time Cown AOT workloads with an uninstrumented release runtime."""
import argparse
import json
import os
from pathlib import Path
import resource
import statistics
import subprocess
import time


def source(scenario, rounds):
    pair = scenario == "overlap"
    parameters = "a: Cown(Counter), b: Cown(Counter)" if pair else "a: Cown(Counter)"
    body = "when (a, b) |x, y| { x.bump(); y.bump() }" if pair else "when (a) |x| { x.bump() }"
    count = 1 if scenario == "hot" else 8
    allocations = "\n".join(f"let c{i} = Cown.new(Counter())" for i in range(count))
    calls = "\n".join(
        f"| work(c{i}, c{(i + 1) % 8})" if pair else f"| work(c{0 if scenario == 'hot' else i})"
        for i in range(8)
    )
    total = " + ".join(f"(when (c{i}) |x| {{ x.value }})" for i in range(count))
    return f"""
class Counter {{
    var value: Int32 = 0
    fn bump() {{ self.value = self.value + 1 }}
}}
fn work({parameters}) {{
    var i = 0
    while i < {rounds} {{ {body}; i = i + 1 }}
}}
fn main() {{
    {allocations}
    let _ = parallel {{
        {calls}
    }}
    let total = {total}
    if total != {rounds * 8 * (2 if pair else 1)} {{ panic("lost Cown update") }}
}}
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compiler", type=Path, default=Path("target/debug/joky"))
    parser.add_argument("--runtime", type=Path, help="release archive built without test-support")
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--rounds", type=int, default=100000)
    parser.add_argument("--repeats", type=int, default=5)
    parser.add_argument("--workers", type=int, nargs="+", default=[1, 2, 4, 8])
    parser.add_argument("--run-only", action="store_true", help="reuse previously built executables")
    args = parser.parse_args()
    if args.rounds < 1 or args.repeats < 1 or any(w < 1 or w > 64 for w in args.workers):
        parser.error("positive rounds/repeats and worker counts in 1..64 required")
    args.output_dir.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env.pop("JOKY_TEST_RESOURCE_REPORT", None)
    if args.runtime:
        env["JOKY_RUNTIME_ARCHIVE"] = str(args.runtime.resolve())
    results = []
    for scenario in ["independent", "hot", "overlap"]:
        executable = (args.output_dir / scenario).resolve()
        if not args.run_only:
            program = args.output_dir / f"{scenario}.jk"
            program.write_text(source(scenario, args.rounds))
            subprocess.run(
                [str(args.compiler.resolve()), "build", "--release", str(program.resolve()), "-o", str(executable)],
                env=env, check=True, timeout=120,
            )
        for workers in args.workers:
            env["JOKY_WORKER_COUNT"] = str(workers)
            wall, cpu = [], []
            for repeat in range(args.repeats + 1):
                before_cpu = resource.getrusage(resource.RUSAGE_CHILDREN)
                started = time.perf_counter()
                subprocess.run([str(executable)], env=env, check=True, timeout=60,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                elapsed = time.perf_counter() - started
                after_cpu = resource.getrusage(resource.RUSAGE_CHILDREN)
                if repeat:
                    wall.append(elapsed)
                    cpu.append(after_cpu.ru_utime + after_cpu.ru_stime - before_cpu.ru_utime - before_cpu.ru_stime)
            result = dict(scenario=scenario, workers=workers, samples_seconds=wall,
                          median_seconds=statistics.median(wall), median_cpu_seconds=statistics.median(cpu))
            results.append(result)
            print(json.dumps(result), flush=True)
    (args.output_dir / "results.json").write_text(json.dumps(results, indent=2) + "\n")


if __name__ == "__main__":
    main()
