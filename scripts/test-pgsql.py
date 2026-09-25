#!/usr/bin/env python3
"""Run the pure Joky driver against an isolated, temporary PostgreSQL cluster."""
import argparse
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent


def run(command, **kwargs):
    return subprocess.run(command, check=True, timeout=120, **kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--joky", type=Path, default=ROOT / "target/debug/joky")
    parser.add_argument("--aot", choices=("off", "debug", "full"), default="debug")
    args = parser.parse_args()
    compiler = args.joky.resolve()
    for name in ("initdb", "pg_ctl", "psql"):
        if not shutil.which(name):
            parser.error(f"{name} must be installed and on PATH")
    if not compiler.is_file():
        parser.error(f"build Joky first: {compiler} does not exist")
    with tempfile.TemporaryDirectory(prefix="joky-pgsql-") as temp:
        directory = Path(temp)
        data = directory / "data"
        run(["initdb", "-D", str(data), "-A", "trust", "-U", "postgres",
             "--encoding=UTF8", "--no-locale"], stdout=subprocess.DEVNULL)
        hba = data / "pg_hba.conf"
        hba.write_text("host all scram_ascii,scram_unicode,scram_fallback 127.0.0.1/32 scram-sha-256\n"
                       + hba.read_text())
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        # TCP only: disable Unix sockets and their platform-specific path limits.
        options = f"-h 127.0.0.1 -p {port} -k '' -F"
        started = False
        try:
            # Cleanup also attempts a stop if pg_ctl times out after starting.
            started = True
            run(["pg_ctl", "-D", str(data), "-l", str(directory / "server.log"),
                 "-o", options, "-w", "start"], stdout=subprocess.DEVNULL)
            environment = os.environ.copy()
            environment.update(JOKY_PG_PORT=str(port), JOKY_PG_USER="postgres",
                               JOKY_PG_DATABASE="postgres")
            # Feed credentials through stdin, never command-line arguments.
            run(["psql", "-X", "-w", "-h", "127.0.0.1", "-p", str(port),
                 "-U", "postgres", "-d", "postgres", "-v", "ON_ERROR_STOP=1"],
                input="""
SET password_encryption = 'scram-sha-256';
CREATE ROLE scram_ascii LOGIN PASSWORD 'correct password';
CREATE ROLE scram_unicode LOGIN PASSWORD 'IX密码';
DO $$ BEGIN
  EXECUTE format('CREATE ROLE scram_fallback LOGIN PASSWORD %L', 'bad' || chr(7) || 'password');
END $$;
""", text=True, stdout=subprocess.DEVNULL, env=environment)
            fixtures = [("examples/networking/pgsql.jk", "1\n中文\ntrue\n\n2\n"),
                        ("tests/fixtures/pgsql_live.jk", "pgsql live ok\n"),
                        ("tests/fixtures/pgsql_scram_live.jk", "pgsql SCRAM live ok\n")]
            for index, (source, expected) in enumerate(fixtures):
                modes = [("JIT", [str(compiler), "run", "--no-cache", source])]
                if args.aot != "off":
                    executable = directory / f"debug-{index}"
                    run([str(compiler), "build", source, "-o", str(executable)], cwd=ROOT)
                    modes.append(("debug AOT", [str(executable)]))
                if args.aot == "full":
                    executable = directory / f"release-{index}"
                    run([str(compiler), "build", source, "--release", "-o", str(executable)], cwd=ROOT)
                    modes.append(("release AOT", [str(executable)]))
                for mode, command in modes:
                    result = run(command, cwd=ROOT, env=environment, capture_output=True, text=True)
                    if result.stdout != expected:
                        raise AssertionError(f"{source} ({mode}): {result.stdout!r}\n{result.stderr}")
                    print(f"PASS {source} ({mode})", flush=True)
        except Exception as error:
            if isinstance(error, subprocess.CalledProcessError):
                if error.stdout:
                    print(error.stdout, flush=True)
                if error.stderr:
                    print(error.stderr, flush=True)
            log = directory / "server.log"
            if log.exists():
                print(log.read_text(), flush=True)
            raise
        finally:
            if started:
                subprocess.run(["pg_ctl", "-D", str(data), "-m", "immediate", "-w", "stop"],
                               timeout=30, stdout=subprocess.DEVNULL, check=False)


if __name__ == "__main__":
    main()
