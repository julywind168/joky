# Test Scope and Timings

Module compilation allocation and cold/warm cache scaling measurements are in
[module-compilation.md](module-compilation.md), including the isolated benchmark command.

Keep exhaustive language semantics in compiler tests, and raw parser and storage
cases in runtime tests. CLI parity cases should exercise the boundaries between
modules, cached artifacts and native code with a smaller representative program.

The CLI parity harness runs cold JIT and, by default, debug AOT
(`opt_level=none`). Cache-focused cases opt into a second cached JIT run instead
of making every semantic fixture repeat code generation. AOT reuses the MIR
artifacts written by JIT, and a later build of the same graph reuses the linked
native object. Release AOT uses Cranelift
`opt_level=speed` and is expensive: many parallel `joky build --release`
processes pin every core. Set `JOKY_TEST_AOT=full` to restore debug and release
AOT, or `JOKY_TEST_AOT=off` to skip AOT. Concurrent AOT builds are limited
(`JOKY_TEST_AOT_JOBS`, default one job in `full` mode and about one quarter of
the host cores in `debug` mode). All JIT and AOT compiler children also share a
limit (`JOKY_TEST_COMPILE_JOBS`, default about half the host cores), and each
child gets `JOKY_CODEGEN_JOBS=2`. This prevents parallel Rust tests from
oversubscribing the machine. The harness still checks generic instance
counts when requested, runs native programs after removing their sources and
cache, and checks resource snapshots when `runtime-test-support` is enabled.
Reducing the fixture matrix does not disable any of these checks for the
selected AOT coverage.

## Coverage Allocation

| Area | Full semantic coverage | CLI parity coverage |
| --- | --- | --- |
| FromString | `tests/fixtures/from_string_builtins.jk`, used by `src/compiler/tests/from_string.rs` | One package covers all 11 builtin types and custom implementations through shared generic parsers: signed minima, unsigned maxima, overflow, Float32 rounding, both Bool values, owned error text and explicit dispatch |
| Equality | `tests/fixtures/partial_eq.jk`, `tests/fixtures/partial_eq_containers.jk` and `src/compiler/tests/equality_containers.rs` | Scalar and container checks share imported models and generic helpers in one package; nested shared/owned values, active variants, short-circuiting, NaN and repeated borrowing remain covered |
| Hash and collection keys | `tests/fixtures/hash.jk`, compiler rejection cases and runtime hash-cache tests | One compact package covers imported and caller-defined keys, a nested tuple, collision keys, growth/reinsertion, persistent sharing and resource cleanup. Exhaustive builtin-key combinations stay in the compiler fixture |
| Container ordering and sorting | `tests/fixtures/ordering_containers.jk` and `src/compiler/tests/ordering.rs` | One package uses `tests/fixtures/ordering_containers_parity.jk` for representative layouts, NaN, short-circuiting, operand evaluation, sorting, imported and caller-defined implementations |
| Sorting | `tests/fixtures/sorting.jk` and `src/compiler/tests/sorting.rs` | Standard module wrappers, generic dispatch, stability, preserved inputs, mutable sorting, managed payloads and comparator panic |

Keep width-specific ABI checks and ownership cases in CLI tests even when their
basic semantics also appear in compiler tests. Avoid multiplying every invalid
input or relational operator by every type and every backend. No exhaustive
compiler fixtures are skipped or removed by the compact CLI fixtures.

MutMap/MutSet index invariants, growth, deletion churn and hash reuse are covered
in runtime tests. The existing Hash parity package covers growth and reinsertion
at backend boundaries. The ignored [storage benchmark](../runtime/mutable-map-index.md)
reports scaling separately from the default test suite.

Persistent Map/Set path sharing, collision nodes, snapshots, depth and destruction
are checked in runtime HAMT tests. The same Hash parity package covers persistent
versions and parallel updates. The ignored [HAMT benchmark](../runtime/persistent-map.md)
measures runtime storage without duplicating the CLI fixture matrix.

Keep independent checks in small functions. Container comparisons lower to
control flow, so combining many checks into one large function can increase
compilation time even when the source program is shorter.

Group related successful scenarios into one package when they can share models
and generic helpers. Keep assertions in named functions so failures remain
locatable without repeating the entire JIT/cache/AOT cycle for each function.
Comparator panic remains a separate test because it terminates the process. It
uses the same execution harness, including cache checks and source removal, but
checks the trap instead of a successful exit and drained resource snapshot.

## CLI Phase Timings

The dev profile (also inherited by `cargo test`) optimizes the `sha2`
dependency. AOT build records hash the compiler executable and runtime archive,
and recheck the runtime after linking; unoptimized SHA-256 can cost more than
machine-code generation. Joky's own debug checks and optimization level remain
unchanged.

Set `JOKY_TEST_TIMINGS=1` and pass `--nocapture` to print timing lines from the
parity harness. Without this setting, successful test output stays unchanged.

```sh
cargo build --manifest-path crates/joky-runtime/Cargo.toml --release --features test-support
JOKY_TEST_AOT=full \
JOKY_TEST_TIMINGS=1 \
JOKY_RUNTIME_ARCHIVE="$PWD/crates/joky-runtime/target/release/libjoky_runtime.a" \
  cargo test --features runtime-test-support --test cli \
  -- --skip build_release_and_strip_control_aot_artifact --nocapture
```

The artifact-size test is excluded from this timing command because the explicit
archive gives debug and release builds the same runtime. Run that test separately
without `JOKY_RUNTIME_ARCHIVE`, using freshly built debug and release archives.
This exclusion is specific to measuring with one explicit archive, not a change
to the default test suite.

```sh
cargo test --test cli build_release_and_strip_control_aot_artifact -- --exact
```

This command uses the default compiler features. If you enable
`runtime-test-support` here too, both selected runtime archives must have been
built with `test-support` to provide the resource-reporting hooks.

Each package reports cold JIT, the selected AOT build and native execution
phases, and total wall time. Cache-focused packages additionally report cached
JIT. With `JOKY_TEST_AOT=full` that includes debug and release AOT. JIT timings
include compilation/loading and execution;
AOT build timings include compilation and linking. These are process-level
measurements, not a compiler pass profiler. The 10ms process poll interval also
limits the precision of very short execution timings.

Compare the same command, features, runtime archive and test concurrency before
and after a change. Cargo's final test duration excludes Rust compilation; the
sum of package durations is not suite wall time because tests run concurrently.
Record the first run after rebuilding separately from subsequent runs: native
executable startup on macOS can make the first run noticeably slower. Each
parity package still creates a fresh module cache on every test invocation.
Use `--test-threads=1` for isolated package comparisons, and keep the normal
parallel invocation for measuring everyday suite time. Timing output does not
change timeouts, assertion behavior or which execution modes run.

The standalone runtime has its own manifest, so root `cargo test` does not run
its unit tests. See [runtime development](../runtime/development.md) for runtime
and isolated stress checks.
