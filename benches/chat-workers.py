#!/usr/bin/env python3
"""Prepare immutable chat artifacts, then measure Joky or Rust/Tokio workers."""
import argparse
import asyncio
import csv
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import shutil
import statistics
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parent.parent
BALANCE = 1_000_000_000_000


def command(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def save_json(path, value):
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n")


def environment():
    return dict(
        utc=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        revision=command("git", "rev-parse", "HEAD"),
        dirty=command("git", "status", "--porcelain"),
        platform=platform.platform(), machine=platform.machine(),
        logical_cpus=os.cpu_count(), python=sys.version,
        cpu=command("sysctl", "-n", "machdep.cpu.brand_string")
        if sys.platform == "darwin" else platform.processor(),
        argv=sys.argv,
    )


def replace_once(path, old, new):
    text = path.read_text()
    if text.count(old) != 1:
        raise RuntimeError(f"Benchmark patch no longer matches {path}: {old}")
    path.write_text(text.replace(old, new))


def prepare(args):
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    source = out / "source"
    shutil.copytree(ROOT / "examples/chat", source,
                    ignore=shutil.ignore_patterns("dist"))
    main = source / "main.jk"
    replace_once(main, '"127.0.0.1", 7777', '"127.0.0.1", 0')
    replace_once(main, '"127.0.0.1", 7778', '"127.0.0.1", 0')
    replace_once(source / "script/main.jk",
                 "const START_BALANCE_CENTS: UInt64 = 500",
                 f"const START_BALANCE_CENTS: UInt64 = {BALANCE}")
    joky, runtime = args.joky.resolve(), args.runtime.resolve()
    invocation = [str(joky), "build", str(source), "--parallel", "-o", str(out / "app")]
    with (out / "build.log").open("w") as log:
        subprocess.run(invocation, cwd=ROOT, check=True, stdout=log,
                       stderr=subprocess.STDOUT,
                       env={**os.environ, "JOKY_RUNTIME": str(runtime)})
    shutil.copy2(ROOT / "Cargo.lock", out / "Cargo.lock")
    shutil.copy2(__file__, out / "runner.py")
    metadata = environment()
    metadata.update(
        profile=args.profile, command=invocation, script_vm="JIT (vm.Vm.new)",
        rustc=command("rustc", "--version"), cc=command("cc", "--version"),
        joky_sha256=digest(joky), runtime_sha256=digest(runtime),
        start_balance_cents=BALANCE,
        files={str(p.relative_to(out)): digest(p) for folder in [source, out / "app"]
               for p in sorted(folder.rglob("*")) if p.is_file()},
    )
    save_json(out / "build.json", metadata)
    print(f"Prepared {out / 'app/chat'}", flush=True)


def prepare_tokio(args):
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    source = ROOT / "benches/chat-tokio"
    shutil.copytree(source, out / "source", ignore=shutil.ignore_patterns("target", ".gitignore"))
    (out / "app").mkdir()
    shutil.copy2(args.binary.resolve(), out / "app/chat")
    shutil.copy2(__file__, out / "runner.py")
    metadata = environment()
    metadata.update(
        implementation="Rust/Tokio", profile=args.profile,
        rustc=command("rustc", "--version"), script_vm="none (static Rust business logic)",
        binary_sha256=digest(args.binary.resolve()), start_balance_cents=BALANCE,
        run_args=["--chat-port", "0", "--admin-port", "0", "--balance-cents", str(BALANCE)],
        files={str(p.relative_to(out)): digest(p) for folder in [out / "source", out / "app"]
               for p in sorted(folder.rglob("*")) if p.is_file()},
    )
    save_json(out / "build.json", metadata)
    print(f"Prepared {out / 'app/chat'}", flush=True)


def cpu_seconds(value):
    days, sep, rest = value.partition("-")
    seconds = float(days) * 86400 if sep else 0
    for index, part in enumerate(reversed((rest if sep else value).split(":"))):
        seconds += float(part) * 60 ** index
    return seconds


async def usage(pid):
    process = await asyncio.create_subprocess_exec(
        "ps", "-p", str(pid), "-o", "time=,rss=", stdout=asyncio.subprocess.PIPE)
    stdout, _ = await process.communicate()
    fields = stdout.decode().split()
    if process.returncode or len(fields) != 2:
        raise RuntimeError(f"Cannot sample server {pid}: {stdout!r}")
    return cpu_seconds(fields[0]), int(fields[1]) * 1024


def percentile(values, quantile):
    ordered = sorted(values)
    return ordered[max(0, math.ceil(len(ordered) * quantile) - 1)]


class Load:
    def __init__(self, peers, rooms, payload_bytes, writer, case):
        self.peers, self.rooms = peers, rooms
        self.payload_bytes, self.writer, self.case = payload_bytes, writer, case
        self.pending = {}
        self.failed = asyncio.get_running_loop().create_future()
        self.sequence = [0] * len(peers)
        self.received = 0
        self.latencies = []
        self.balances = {}

    def payload(self, sender, seq):
        prefix = f"{sender:04d}:{seq:08d}:".encode()
        return prefix + b"x" * (self.payload_bytes - len(prefix))

    async def read(self, receiver):
        reader = self.peers[receiver][0]
        expected = {sender: 0 for sender in range(len(self.peers))
                    if sender % self.rooms == receiver % self.rooms}
        try:
            while True:
                line = await reader.readline()
                if line.startswith(b"balance ") and receiver in self.balances:
                    expected_balance = BALANCE // 100 - self.sequence[receiver]
                    if line != f"balance {expected_balance}\n".encode():
                        raise RuntimeError(f"Incorrect final account balance: {line!r}")
                    self.balances.pop(receiver).set_result(None)
                    continue
                fields = line.rstrip(b"\n").split(b" ", 3)
                if len(fields) != 4 or fields[0] != b"message":
                    raise RuntimeError(f"Unexpected receiver {receiver} output: {line!r}")
                sender = int(fields[2]) - 1
                if sender not in expected or fields[1] != f"r{receiver % self.rooms}".encode():
                    raise RuntimeError(f"Wrong room/sender: {line!r}")
                seq = expected[sender]
                if fields[3] != self.payload(sender, seq):
                    raise RuntimeError(f"Lost, duplicate, reordered or corrupt message: {line!r}")
                expected[sender] += 1
                item = self.pending[(sender, seq)]
                item[1] -= 1
                self.received += 1
                if item[1] == 0:
                    item[0].set_result(time.perf_counter_ns())
        except asyncio.CancelledError:
            raise
        except Exception as error:
            if not self.failed.done():
                self.failed.set_result(error)

    async def send(self, sender, count, measured):
        stream = self.peers[sender][1]
        for _ in range(count):
            seq = self.sequence[sender]
            self.sequence[sender] += 1
            future = asyncio.get_running_loop().create_future()
            self.pending[(sender, seq)] = [future, len(self.peers) // self.rooms]
            start = time.perf_counter_ns()
            stream.write(b"say " + self.payload(sender, seq) + b"\n")
            await stream.drain()
            end = await future
            del self.pending[(sender, seq)]
            if measured:
                self.latencies.append(end - start)
                self.writer.writerow([*self.case, sender, seq, start, end, end - start])

    async def phase(self, count, measured):
        deliveries = self.received
        sends = asyncio.gather(*(self.send(i, count, measured) for i in range(len(self.peers))))
        try:
            done, _ = await asyncio.wait([sends, self.failed], return_when=asyncio.FIRST_COMPLETED)
            if self.failed in done:
                raise self.failed.result()
            await sends
            expected = len(self.peers) * count * (len(self.peers) // self.rooms)
            if self.received - deliveries != expected or self.pending:
                raise RuntimeError("Incomplete broadcast delivery")
        finally:
            if not sends.done():
                sends.cancel()
            await asyncio.gather(sends, return_exceptions=True)

    async def fence(self):
        futures = []
        for receiver, (_, writer) in enumerate(self.peers):
            future = asyncio.get_running_loop().create_future()
            self.balances[receiver] = future
            futures.append(future)
            writer.write(b"balance\n")
            await writer.drain()
        replies = asyncio.gather(*futures)
        try:
            done, _ = await asyncio.wait([replies, self.failed], return_when=asyncio.FIRST_COMPLETED)
            if self.failed in done:
                raise self.failed.result()
            await replies
        finally:
            if not replies.done():
                replies.cancel()
            await asyncio.gather(replies, return_exceptions=True)


async def sample(args, out, binary, workers, rooms, repeat, latency_writer, run_args):
    label = f"r{rooms}-w{workers}-s{repeat}"
    peers, readers = [], []
    monitor = None
    stderr = (out / f"{label}.stderr").open("wb")
    server = await asyncio.create_subprocess_exec(
        str(binary), *run_args, "--task-workers", str(workers), cwd=binary.parent,
        stdout=asyncio.subprocess.PIPE, stderr=stderr,
        env={key: val for key, val in os.environ.items() if key != "JOKY_TASK_WORKERS"})
    try:
        async def port(prefix):
            line = await asyncio.wait_for(server.stdout.readline(), 90)
            if not line.startswith(prefix):
                raise RuntimeError(f"Startup failed: {line!r}; see {label}.stderr")
            return int(line[len(prefix):])
        chat_port, admin_port = await port(b"chat "), await port(b"admin ")
        for client in range(args.clients):
            reader, writer = await asyncio.open_connection("127.0.0.1", chat_port)
            peers.append((reader, writer))
            welcome = await reader.readline()
            if welcome != f"welcome {client + 1}\n".encode():
                raise RuntimeError(f"Unexpected welcome: {welcome!r}")
            writer.write(f"join r{client % rooms}\n".encode())
            await writer.drain()
            for index in range(client + 1):
                if index % rooms == client % rooms:
                    line = await peers[index][0].readline()
                    if line != f"joined r{client % rooms} {client + 1}\n".encode():
                        raise RuntimeError(f"Unexpected join: {line!r}")
        load = Load(peers, rooms, args.payload_bytes, latency_writer, (rooms, workers, repeat))
        readers = [asyncio.create_task(load.read(i)) for i in range(args.clients)]
        await load.phase(args.warmup, False)
        initial_cpu, initial_rss = await usage(server.pid)
        rss = [initial_rss]

        async def observe():
            while True:
                await asyncio.sleep(0.25)
                _, resident = await usage(server.pid)
                rss.append(resident)
        monitor = asyncio.create_task(observe())
        client_cpu = time.process_time()
        start = time.perf_counter()
        batches = 0
        while True:
            await load.phase(args.messages, True)
            batches += 1
            if time.perf_counter() - start >= args.min_seconds:
                break
        elapsed = time.perf_counter() - start
        client_cpu = time.process_time() - client_cpu
        final_cpu, final_rss = await usage(server.pid)
        rss.append(final_rss)
        monitor.cancel()
        monitored = await asyncio.gather(monitor, return_exceptions=True)
        if isinstance(monitored[0], Exception):
            raise monitored[0]
        monitor = None
        # An ordered reply on every connection checks the final debit and catches
        # extra broadcasts after the final expected message, outside measurement.
        await load.fence()
        admin_reader, admin_writer = await asyncio.open_connection("127.0.0.1", admin_port)
        admin_writer.write(b"shutdown\n")
        await admin_writer.drain()
        if await admin_reader.readline() != b"shutting down\n":
            raise RuntimeError("Admin shutdown failed")
        admin_writer.close()
        await admin_writer.wait_closed()
        await asyncio.wait_for(server.wait(), 30)
        remaining = await server.stdout.read()
        (out / f"{label}.stdout").write_bytes(remaining)
        stderr.close()
        if server.returncode or remaining or (out / f"{label}.stderr").stat().st_size:
            raise RuntimeError(f"Server failed: {label}, exit={server.returncode}")
        broadcasts = args.clients * args.messages * batches
        return dict(rooms=rooms, workers=workers, sample=repeat, clients=args.clients,
                    payload_bytes=args.payload_bytes, broadcasts=broadcasts,
                    deliveries=broadcasts * (args.clients // rooms), elapsed_s=elapsed,
                    broadcasts_s=broadcasts / elapsed,
                    deliveries_s=broadcasts * (args.clients // rooms) / elapsed,
                    p50_ms=percentile(load.latencies, .5) / 1e6,
                    p95_ms=percentile(load.latencies, .95) / 1e6,
                    p99_ms=percentile(load.latencies, .99) / 1e6,
                    server_cpu_s=final_cpu - initial_cpu,
                    server_cpu_cores=(final_cpu - initial_cpu) / elapsed,
                    client_cpu_cores=client_cpu / elapsed,
                    rss_start_bytes=initial_rss, rss_end_bytes=final_rss,
                    rss_sampled_max_bytes=max(rss))
    finally:
        for task in [*readers, *([monitor] if monitor else [])]:
            task.cancel()
        await asyncio.gather(*readers, *([monitor] if monitor else []), return_exceptions=True)
        if server.returncode is None:
            server.kill()
            await server.wait()
        for _, writer in peers:
            writer.close()
        await asyncio.gather(*(writer.wait_closed() for _, writer in peers), return_exceptions=True)
        stderr.close()


def csv_file(path, rows):
    with path.open("w", newline="") as file:
        writer = csv.DictWriter(file, fieldnames=rows[0])
        writer.writeheader()
        writer.writerows(rows)


async def run(args):
    build = args.build.resolve()
    metadata = json.loads((build / "build.json").read_text())
    for path, expected in metadata["files"].items():
        if digest(build / path) != expected:
            raise RuntimeError(f"Build artifact changed: {path}")
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    shutil.copy2(__file__, out / "runner.py")
    shutil.copy2(build / "build.json", out / "build.json")
    save_json(out / "environment.json", environment())
    rows = []
    with (out / "latencies.csv").open("w", newline="") as file:
        writer = csv.writer(file)
        writer.writerow(["rooms", "workers", "sample", "sender", "sequence",
                         "start_monotonic_ns", "end_monotonic_ns", "latency_ns"])
        for repeat in range(args.samples):
            # Rotate worker order and reverse scene order to reduce temporal bias.
            workers = args.workers[repeat % len(args.workers):] + args.workers[:repeat % len(args.workers)]
            for rooms in args.rooms if repeat % 2 == 0 else list(reversed(args.rooms)):
                for worker in workers:
                    row = await asyncio.wait_for(
                        sample(args, out, build / "app/chat", worker, rooms, repeat, writer,
                               metadata.get("run_args", [])),
                        args.timeout)
                    rows.append(row)
                    file.flush()
                    csv_file(out / "samples.csv", rows)
                    print(f"{rooms} rooms / {worker} workers / {repeat + 1}: "
                          f"{row['broadcasts_s']:.1f} broadcasts/s, "
                          f"p99 {row['p99_ms']:.1f} ms, "
                          f"CPU {row['server_cpu_cores']:.2f} cores", flush=True)
    summaries = []
    for rooms in args.rooms:
        baseline = statistics.median(r["broadcasts_s"] for r in rows
                                     if r["rooms"] == rooms and r["workers"] == args.workers[0])
        for workers in args.workers:
            group = [r for r in rows if r["rooms"] == rooms and r["workers"] == workers]
            summary = dict(rooms=rooms, workers=workers)
            for key in ["broadcasts_s", "deliveries_s", "p50_ms", "p95_ms", "p99_ms",
                        "server_cpu_cores", "client_cpu_cores", "rss_sampled_max_bytes"]:
                summary[key] = statistics.median(r[key] for r in group)
            summary.update(min_broadcasts_s=min(r["broadcasts_s"] for r in group),
                           max_broadcasts_s=max(r["broadcasts_s"] for r in group),
                           relative_to_first_worker=summary["broadcasts_s"] / baseline)
            summaries.append(summary)
    csv_file(out / "summary.csv", summaries)
    lines = ["# Chat worker baseline", "", "Medians across independent server processes. "
             "Latency quantiles are per-sample all-recipient completion latencies; "
             "their medians are shown here. See README for measurement limits.", "",
             "| Rooms | Workers | Broadcasts/s | Deliveries/s | P95 ms | P99 ms | Server CPU cores | Client CPU cores | Sampled RSS MiB |",
             "| --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    for row in summaries:
        lines.append(f"| {row['rooms']} | {row['workers']} | {row['broadcasts_s']:.1f} | "
                     f"{row['deliveries_s']:.1f} | {row['p95_ms']:.2f} | {row['p99_ms']:.2f} | "
                     f"{row['server_cpu_cores']:.2f} | {row['client_cpu_cores']:.2f} | "
                     f"{row['rss_sampled_max_bytes'] / 2**20:.1f} |")
    (out / "report.md").write_text("\n".join(lines) + "\n")


def numbers(value):
    values = [int(item) for item in value.split(",")]
    if not values or min(values) < 1 or len(set(values)) != len(values):
        raise argparse.ArgumentTypeError("Expected distinct positive integers")
    return values


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    build = commands.add_parser("prepare", help="Copy and patch the example, then build one AOT app")
    build.add_argument("--joky", type=Path, required=True)
    build.add_argument("--runtime", type=Path, required=True)
    build.add_argument("--profile", required=True, help="Caller-declared Rust tool/runtime build profile")
    build.add_argument("--output", type=Path, required=True)
    tokio = commands.add_parser("prepare-tokio", help="Snapshot an already built Rust/Tokio server")
    tokio.add_argument("--binary", type=Path, required=True)
    tokio.add_argument("--profile", required=True, help="Caller-declared binary build profile")
    tokio.add_argument("--output", type=Path, required=True)
    measure = commands.add_parser("run", help="Measure a previously prepared app without rebuilding")
    measure.add_argument("--build", type=Path, required=True)
    measure.add_argument("--output", type=Path, required=True)
    measure.add_argument("--workers", type=numbers, default=[1, 2, 4])
    measure.add_argument("--rooms", type=numbers, default=[1, 4])
    measure.add_argument("--clients", type=int, default=16)
    measure.add_argument("--messages", type=int, default=100)
    measure.add_argument("--warmup", type=int, default=10)
    measure.add_argument("--payload-bytes", type=int, default=64)
    measure.add_argument("--samples", type=int, default=5)
    measure.add_argument("--min-seconds", type=float, default=0,
                         help="Repeat complete message batches until this duration (default: one batch)")
    measure.add_argument("--timeout", type=float, default=180)
    args = parser.parse_args()
    if args.command == "prepare":
        prepare(args)
    elif args.command == "prepare-tokio":
        prepare_tokio(args)
    else:
        if not (1 <= args.clients <= 32 and 16 <= args.payload_bytes <= 1024
                and 1 <= args.messages <= 100_000 and 1 <= args.warmup <= 100_000
                and args.samples >= 1 and 0 <= args.min_seconds < args.timeout
                and all(args.clients % room == 0 for room in args.rooms)):
            parser.error("Require 1..32 clients, dividing room counts, 16..1024 byte payloads, "
                         "1..100000 messages/warmup, positive samples and timeout")
        asyncio.run(run(args))


if __name__ == "__main__":
    main()
