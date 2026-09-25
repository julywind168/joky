#!/usr/bin/env python3
"""Differentially test pure Joky NFKC/SASLprep and PostgreSQL password bytes.

Python ucd_3_2_0/stringprep are independent runtime oracles. Optionally read the
official Unicode 3.2 NormalizationTest and link the installed PostgreSQL
pg_saslprep implementation for a second independent compatibility oracle.
No server, network, Rust Unicode dependency, or password-bearing logs required.
"""
import argparse
import ctypes
import hashlib
import os
from pathlib import Path
import random
import stringprep as prep
import struct
import subprocess
import sys
import tempfile
import unicodedata

ROOT = Path(__file__).resolve().parent.parent
UCD = unicodedata.ucd_3_2_0
NORMALIZATION_SHA256 = 'c4513869bb7098d19838be4a1fd5d760843c5804bfe03bd6bbb20623ceb6e57d'
PROHIBITED = [getattr(prep, 'in_table_' + t) for t in
              ('c12', 'c21', 'c22', 'c3', 'c4', 'c5', 'c6', 'c7', 'c8', 'c9')]


def strict(data):
    try:
        text = data.decode('utf-8')
    except UnicodeDecodeError:
        return 1, b''
    text = ''.join(' ' if prep.in_table_c12(c) else c
                   for c in text if not prep.in_table_b1(c))
    text = UCD.normalize('NFKC', text)
    for char in text:
        if any(test(char) for test in PROHIBITED):
            return 2, b''
        if prep.in_table_a1(char):
            return 3, b''
    if any(prep.in_table_d1(c) for c in text):
        if any(prep.in_table_d2(c) for c in text) or not (
                prep.in_table_d1(text[0]) and prep.in_table_d1(text[-1])):
            return 4, b''
    return 0, text.encode('utf-8')


def sample_inputs(full):
    points = {0, 1, 7, 0x7f, 0x80, 0xa0, 0xad, 0xaa, 0x2168, 0x200b,
              0x0340, 0x0345, 0x03f9, 0x1f600, 0x2f868, 0x2f874, 0x2f91f,
              0x2f95f, 0x2f9bf, 0x10ffff, 0xfe70}
    properties = [prep.in_table_a1, prep.in_table_b1, prep.in_table_c12,
                  *PROHIBITED, prep.in_table_d1, prep.in_table_d2]
    previous = None
    for cp in range(0x110000):
        char = chr(cp)
        state = tuple(test(char) for test in properties)
        if state != previous:
            points.update(c for c in (cp - 1, cp, cp + 1) if 0 <= c < 0x110000)
        previous = state
        if full and (UCD.decomposition(char) or UCD.combining(char)):
            points.add(cp)
    inputs = [b'', b'user', b'USER', b'I\xc2\xadX']
    for cp in sorted(points):
        if 0xd800 <= cp <= 0xdfff:
            continue
        value = chr(cp)
        for text in [value, 'a' + value + 'z', '\u0627' + value + '\u0627', value + '\u0627']:
            inputs.append(text.encode('utf-8'))
    inputs.extend(bytes([b]) for b in range(256))
    inputs.extend(bytes.fromhex(h) for h in ['c080', 'c1bf', 'e08080', 'eda080', 'edbfbf',
                  'f0808080', 'f4908080', 'f5808080', 'e282', 'f09f98', 'e228a1'])
    rng = random.Random(4013)
    alphabet = ['a', 'Z', '1', ' ', '\u00ad', '\u00a0', '\u200b', '\u0301', '\u0315',
                '\u0300', '\u0327', '\u0345', '\u0627', '\u05d0', '\ufe70', '\u1100',
                '\u1161', '\u11a8', '\u212b', '\ufb03', '\U0002f868']
    for _ in range(2000 if full else 120):
        inputs.append(''.join(rng.choices(alphabet, k=rng.randrange(1, 14))).encode())
    return list(dict.fromkeys(inputs))


def normalizations(inputs, official, full):
    result = {}
    for data in inputs:
        try:
            text = data.decode('utf-8')
        except UnicodeDecodeError:
            continue
        result[data] = UCD.normalize('NFKC', text).encode('utf-8')
    if full:
        for cp in range(0xac00, 0xd7a4):
            char = chr(cp)
            result[UCD.normalize('NFD', char).encode()] = char.encode()
            result[char.encode()] = char.encode()
    if official:
        contents = official.read_bytes()
        if hashlib.sha256(contents).hexdigest() != NORMALIZATION_SHA256:
            raise ValueError('expected official NormalizationTest-3.2.0.txt hash')
        for line in contents.decode('utf-8').splitlines():
            line = line.split('#', 1)[0].strip()
            if not line or line.startswith('@'):
                continue
            columns = [''.join(chr(int(cp, 16)) for cp in field.split()).encode()
                       for field in line.split(';')[:5]]
            for data in columns:
                # Official expected values, independently of Python normalization.
                result[data] = columns[3]
    return list(result.items())


def postgres_oracle(libdir, includedir, directory):
    source = directory / 'oracle.c'
    source.write_text('''#include <stdlib.h>
#include "common/saslprep.h"
int oracle(const char *input, char **output) { return pg_saslprep(input, output); }
void oracle_free(void *value) { free(value); }
''')
    library = directory / ('oracle.dylib' if sys.platform == 'darwin' else 'oracle.so')
    subprocess.run([os.environ.get('CC', 'cc'), '-dynamiclib' if sys.platform == 'darwin' else '-shared',
                    '-fPIC', '-I', str(includedir), str(source),
                    str(libdir / 'libpgcommon.a'), str(libdir / 'libpgport.a'), '-o', str(library)],
                   check=True, timeout=60)
    lib = ctypes.CDLL(str(library))
    lib.oracle.argtypes = [ctypes.c_char_p, ctypes.POINTER(ctypes.c_void_p)]
    lib.oracle_free.argtypes = [ctypes.c_void_p]

    def prepare(data):
        if b'\0' in data:
            return 1, b''
        output = ctypes.c_void_p()
        status = lib.oracle(data, ctypes.byref(output))
        if status == -1:
            raise MemoryError('PostgreSQL SASLprep oracle')
        try:
            return 0, ctypes.string_at(output) if status == 0 else data
        finally:
            if output.value:
                lib.oracle_free(output)
    return prepare


def write_vectors(target, norms, inputs, postgres):
    data = bytearray(b'USP1')
    def u32(value): data.extend(struct.pack('!I', value))
    def blob(value): u32(len(value)); data.extend(value)
    u32(len(norms))
    for source, expected in norms:
        blob(source); blob(expected)
    u32(len(inputs))
    for source in inputs:
        status, expected = strict(source)
        blob(source); data.append(status); blob(expected)
    pg_inputs = inputs if postgres else []
    u32(len(pg_inputs))
    for source in pg_inputs:
        status, expected = postgres(source)
        blob(source); data.append(status); blob(expected)
    target.write_bytes(data)
    return f'unicode conformance {len(norms)} {len(inputs)} {len(pg_inputs)}\n'


def main():
    if UCD.unidata_version != '3.2.0' or prep.unicodedata.unidata_version != '3.2.0':
        raise RuntimeError('Unicode 3.2.0 normalization and stringprep data required')
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--joky', type=Path, default=ROOT / 'target/debug/joky')
    parser.add_argument('--normalization-test', type=Path)
    parser.add_argument('--postgres-libdir', type=Path)
    parser.add_argument('--postgres-includedir', type=Path)
    parser.add_argument('--aot', choices=['off', 'debug', 'full'], default='debug')
    parser.add_argument('--full', action='store_true')
    parser.add_argument('--sample-count', type=int, help='cap inputs for a compact checked-in corpus')
    parser.add_argument('--write-fixture', type=Path, help='write a reusable differential corpus and exit')
    args = parser.parse_args()
    if bool(args.postgres_libdir) != bool(args.postgres_includedir):
        parser.error('supply both PostgreSQL library and server include directories')
    inputs = sample_inputs(args.full)
    if args.sample_count is not None:
        if args.sample_count < 128:
            parser.error('--sample-count must be at least 128')
        rng = random.Random(3454)
        inputs = inputs[:64] + rng.sample(inputs[64:], min(args.sample_count - 64, len(inputs) - 64))
    norms = normalizations(inputs, args.normalization_test, args.full)
    with tempfile.TemporaryDirectory(prefix='joky-saslprep-') as temp:
        directory = Path(temp)
        oracle = postgres_oracle(args.postgres_libdir, args.postgres_includedir, directory) if args.postgres_libdir else None
        vectors = args.write_fixture or directory / 'vectors.bin'
        expected = write_vectors(vectors, norms, inputs, oracle)
        print(expected.strip(), flush=True)
        if args.write_fixture:
            print(f'SHA-256 {hashlib.sha256(vectors.read_bytes()).hexdigest()}')
            return
        source = directory / 'main.jk'
        escaped = str(vectors.resolve()).replace('\\', '\\\\').replace('"', '\"')
        source.write_text((ROOT / 'tests/fixtures/unicode32_conformance.jk').read_text().replace('@VECTORS@', escaped))
        compiler = str(args.joky.resolve())
        environment = os.environ.copy()
        environment.update(JOKY_WORKER_COUNT='2', JOKY_CODEGEN_JOBS='2')

        def check(mode, command):
            result = subprocess.run(command, capture_output=True, text=True, timeout=300, cwd=ROOT, env=environment)
            if result.returncode or result.stdout != expected:
                raise AssertionError(f'{mode}: {result.stdout}\n{result.stderr}')
            if mode == 'cached JIT' and '[cache] compile' in result.stderr:
                raise AssertionError('cached JIT unexpectedly recompiled modules')
            print(f'PASS {mode}', flush=True)

        # The temporary package starts without a cache. AOT builds also populate
        # it, so run both JIT passes before building either native profile.
        for mode in ('cold JIT', 'cached JIT'):
            check(mode, [compiler, 'run', '--verbose', str(source)])
        if args.aot != 'off':
            for release in ([False, True] if args.aot == 'full' else [False]):
                executable = directory / ('release' if release else 'debug')
                command = [compiler, 'build', str(source), '-o', str(executable)]
                if release: command.append('--release')
                subprocess.run(command, check=True, timeout=120, cwd=ROOT, env=environment)
                check('release AOT' if release else 'debug AOT', [str(executable)])


if __name__ == '__main__':
    main()
