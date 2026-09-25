# Joky examples

Runnable programs grouped by topic. Start with [hello](basics/hello.jk),
[functions](basics/functions.jk), and [effect handlers](effects/handlers.jk), then
try [parallel computation](concurrency/parallel_compute.jk) and
[shared state](concurrency/cown_when.jk).

For setup and platform support, see the [project README](../README.md#requirements).
For language and API explanations, see the [documentation index](../docs/README.md).

## Run an example

Run these commands from the **repository root**:

```bash
cargo run --release -- run examples/basics/functions.jk
# Result: 30

# Check without executing the program.
cargo run --release -- check examples/basics/functions.jk

# Compile and run a native executable.
cargo run --release -- build examples/basics/functions.jk --release -o target/joky-examples/functions
./target/joky-examples/functions
```

Replace the source path with an entry below. Single-file examples keep their
working directory at the repository root; moving into a topic directory changes
the meaning of relative file paths.

After `cargo build --release`, you can also use `./target/release/joky` in place
of `cargo run --release --`.

## Find an example

| Directory | Topics |
| --- | --- |
| [basics](#basics) | Functions, strings, parsing, and debugging |
| [collections](#collections) | Ranges, cursors, and sorting |
| [traits](#traits) | Traits, associated types, comparison, and dynamic dispatch |
| [effects](#effects) | Local handlers, resumption, and suspension |
| [concurrency](#concurrency) | Parallel work, races, Cowns, and regions |
| [io](#io) | Environment, files, processes, and SQLite |
| [networking](#networking) | TCP round trips and long-running servers |
| [ffi](#ffi) | C calls, strings, resource ownership, and callbacks |
| [packages](#packages) | A minimal project with `joky.toml` |

## Basics

| Example | What to look for |
| --- | --- |
| [hello.jk](basics/hello.jk) | Prints `hello world` |
| [functions.jk](basics/functions.jk) | Typed parameters and return values; prints `Result: 30` |
| [string.jk](basics/string.jk) | Interpolation, raw strings, multiline strings, and byte strings |
| [debug_values.jk](basics/debug_values.jk) | Custom `Debug`, containers, and `echo`; debug output also uses stderr |
| [crypto.jk](basics/crypto.jk) | Pure Joky Base64, incremental SHA-256, HMAC and PBKDF2; see the [SHA-256 guide](../docs/stdlib/sha256.md) and [HMAC/PBKDF2 guide](../docs/stdlib/hmac-pbkdf2.md) |
| [from_string.jk](basics/from_string.jk) | `FromString`, generic parsing, and validation errors |

## Collections

| Example | What to look for |
| --- | --- |
| [range.jk](collections/range.jk) | Exclusive/inclusive ranges, descending steps, and collection |
| [cursor.jk](collections/cursor.jk) | A custom countdown cursor, generic collection, and early exit |
| [sorting.jk](collections/sorting.jk) | Immutable and mutable sorting with a custom ordering |

## Traits

| Example | What to look for |
| --- | --- |
| [traits.jk](traits/traits.jk) | Trait methods, generic constraints, and qualified calls |
| [trait_associated_types.jk](traits/trait_associated_types.jk) | Associated types in generic code |
| [partial_eq.jk](traits/partial_eq.jk) | Custom equality and generic comparison |
| [partial_eq_containers.jk](traits/partial_eq_containers.jk) | Equality for containers and nested values |
| [ordering.jk](traits/ordering.jk) | Partial and total ordering |
| [ordering_containers.jk](traits/ordering_containers.jk) | Ordering for containers |
| [hash_keys.jk](traits/hash_keys.jk) | Custom and composite map keys |
| [dyn_traits.jk](traits/dyn_traits.jk) | Dynamic dispatch with borrowed and owned receivers |
| [dyn_trait_composition.jk](traits/dyn_trait_composition.jk) | Combining interfaces and associated types |
| [dyn_upcast.jk](traits/dyn_upcast.jk) | Converting a dynamic value to a smaller set of traits |

## Effects

| Example | What to look for |
| --- | --- |
| [handlers.jk](effects/handlers.jk) | A local `console.read` handler; prints `42` without user input |
| [resumable.jk](effects/resumable.jk) | An `@resumable` operation and `resume`; prints `hello, Joky` |
| [clock_sleep.jk](effects/clock_sleep.jk) | A suspending timer operation; prints `awake` |

## Concurrency

These programs finish on their own. Results of `parallel` are returned in source
order, while the winning branch of a `race` depends on execution timing.

| Example | What to look for |
| --- | --- |
| [parallel_compute.jk](concurrency/parallel_compute.jk) | Parallel branches and tuple results; prints `54` |
| [tasks.jk](concurrency/tasks.jk) | A race between two computations; prints `41` or `43` |
| [bounded_for.jk](concurrency/bounded_for.jk) | `break`/`continue` and `@parallel(limit: 2)` |
| [cown_when.jk](concurrency/cown_when.jk) | Exclusive access to shared mutable state; prints `1` |
| [cown_transfer.jk](concurrency/cown_transfer.jk) | Acquiring two Cowns together; prints `5` |
| [race_timeout.jk](concurrency/race_timeout.jk) | Cancellation of a branch suspended in a timer |
| [regions.jk](concurrency/regions.jk) | Region lifetimes, cyclic Cowns, cancellation, and child failure |
| [suspending_recursion.jk](concurrency/suspending_recursion.jk) | Recursive suspension and continuation frames; prints `3` |
| [aot_runtime.jk](concurrency/aot_runtime.jk) | JIT/AOT resource cleanup across cancellation; uses C `tmpfile` and prints `closed`, `winner`, `42` |

## I/O

| Example | Behavior and requirements |
| --- | --- |
| [env.jk](io/env.jk) | Prints the working/temp directories, program arguments, and `PATH`; output depends on your environment |
| [path.jk](io/path.jk) | Pure path operations; does not access the filesystem |
| [file_batch.jk](io/file_batch.jk) | Reads `Cargo.toml` and `README.md` with bounded concurrency; prints their byte lengths |
| [file_copy.jk](io/file_copy.jk) | Creates or replaces `target/file-copy-input.bin` and `target/file-copy-output.bin` |
| [file_timeout.jk](io/file_timeout.jk) | Races a write to `target/file-timeout.txt` against a timer; cancellation can leave a partial file |
| [process.jk](io/process.jk) | Runs `git --version`; requires Git on `PATH` |
| [pipeline.jk](io/pipeline.jk) | Counts tracked `.jk` files using `git`, `grep`, and `wc`; run in a Git checkout with those commands on `PATH` |
| [sqlite.jk](io/sqlite.jk) | Uses `joky/sqlite` with an in-memory database; requires SQLite 3 and prints `Alice: 42` and `Bob: unknown` |

The Cargo build creates `target/`; keep running file examples from the repository
root. SQLite library names and supported platforms are described in the
[SQLite guide](../docs/stdlib/sqlite.md).

Pass program arguments after a second `--`:

```bash
cargo run --release -- run examples/io/env.jk -- first second
```

## Networking

All network examples use the loopback interface. Run them one at a time when
they share a port.

| Example | Behavior and requirements |
| --- | --- |
| [pgsql.jk](networking/pgsql.jk) | Queries a local PostgreSQL server using trust authentication; see the [driver guide](../docs/stdlib/pgsql.md) for configuration and isolated testing |
| [tcp_roundtrip.jk](networking/tcp_roundtrip.jk) | Starts its own server and client on `127.0.0.1:7878`, exchanges one message, and exits |
| [echo_server.jk](networking/echo_server.jk) | Listens on `127.0.0.1:7878` and echoes client bytes; runs until stopped |
| [push_server.jk](networking/push_server.jk) | Listens on `127.0.0.1:7879`, echoes client bytes, and pushes `tick` once per second; runs until stopped |

Try the self-contained round trip first:

```bash
cargo run --release -- run examples/networking/tcp_roundtrip.jk
```

For the echo server, start the server in one terminal:

```bash
cargo run --release -- run examples/networking/echo_server.jk
```

Connect from a second terminal using `nc` (netcat), type a line, and press Enter:

```bash
nc 127.0.0.1 7878
```

For the push server, run `examples/networking/push_server.jk` instead and connect
with `nc 127.0.0.1 7879`. You should receive `tick` even without sending a line.
Use **Ctrl-C** to stop the client and server. A busy port prevents the server
from starting; the round-trip example reports `listen error` in that case.

## FFI

These examples load C libraries for the current platform. Platform-specific
declarations in a source file do not imply that every platform has been tested;
see the [project platform status](../README.md#requirements).

| Example | Behavior and requirements |
| --- | --- |
| [abs.jk](ffi/abs.jk) | Calls C `abs`; prints `42` |
| [strings.jk](ffi/strings.jk) | Borrows a C string and copies a C error message into Joky; text varies by platform |
| [tmpfile.jk](ffi/tmpfile.jk) | Creates a temporary C stream and closes it explicitly or through `Drop` |
| [sqlite.jk](ffi/sqlite.jk) | Calls SQLite directly through C FFI, manages output pointers, and releases resources; requires SQLite 3 |
| [async_callback.jk](ffi/async_callback.jk) | Resumes a Joky callback on a C thread; requires 64-bit macOS/Linux with pthreads and prints `42` |

The [I/O SQLite example](io/sqlite.jk) demonstrates the public standard library
API; the [FFI SQLite example](ffi/sqlite.jk) demonstrates manual C interoperation.

## Packages

[hello](packages/hello/) is a minimal package containing
[`joky.toml`](packages/hello/joky.toml) and [`src/main.jk`](packages/hello/src/main.jk).
It prints `hello, package`.

```bash
cargo run --release -- check examples/packages/hello
cargo run --release -- run examples/packages/hello/src/main.jk
cargo run --release -- build examples/packages/hello --release -o target/joky-examples/hello-package
./target/joky-examples/hello-package
```

`check` and `build` accept a package directory. `run` accepts a source file;
when invoked without a path from inside a package, it uses `src/main.jk`.

See [packages and modules](../docs/lang/modules.md) for imports and package layout.

## Maintaining examples

- Put each program in the closest topic directory. Keep source names in
  `snake_case`, and keep multi-file projects under `packages/`.
- Add a link here with the behavior, dependencies, filesystem writes, and any
  interactive or long-running behavior readers need to know about.
- Keep runnable examples separate from compiler test fixtures in `tests/fixtures/`.
  Tests may reuse examples, so update `include_str!` and CLI paths when moving them.
- Run `check` and then exercise the example. For servers, verify a client exchange
  and stop the server afterward; a successful static check does not test networking.
- Run `python3 scripts/check-docs.py` to check local links, anchors, and navigation.
  It also checks that every example source is reachable from this index; it does
  not execute the examples.
