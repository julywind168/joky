# Joky

English | [中文](README.zh.md)

**An experimental, statically typed language with structured concurrency, concurrent ownership, and algebraic effects.**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.98%2B-orange.svg)](Cargo.toml)
[![Tests](https://github.com/julywind168/joky/actions/workflows/test.yml/badge.svg)](https://github.com/julywind168/joky/actions/workflows/test.yml)

Joky brings task lifetimes, shared mutable state, and effect handling into the language. It is implemented in Rust and compiles to native code through Cranelift, with both JIT execution and AOT builds.

- **Structured concurrency:** run work on multiple worker threads with `parallel`, and collect results in source order.
- **Concurrent ownership:** share mutable state through `Cown(T)`; `when` grants temporary exclusive access to its contents.
- **Algebraic effects:** declare operations separately from their handlers. Suspending operations use compiler-generated continuations without user-written `async`/`await`.

Joky is under active development. Syntax, APIs, and runtime behavior can change; the project is intended for experimentation and language development, and is not yet production-ready.

## Quick start

### Requirements

- Rust **1.98+** and Cargo, installed through [rustup](https://rustup.rs/).
- Git to clone the repository.
- A C compiler/linker available as `cc` for native builds and tests: Xcode Command Line Tools on macOS, or a C build toolchain on Linux.
- The SQLite 3 shared library for SQLite examples and tests. macOS provides it; the Linux CI installs `libsqlite3-dev`.

**Platform status:** macOS on Apple Silicon is the primary development and verified platform. Linux x86_64 is included in [CI](.github/workflows/test.yml), with failures currently allowed for that job. Windows is untested. AOT supports the compiler's host target only; cross-compilation is not implemented.

### Build and run

```bash
git clone https://github.com/julywind168/joky.git
cd joky

cargo build --release
cargo run --release -- run examples/basics/functions.jk
```

Expected output:

```text
Result: 30
```

Run the remaining commands from the repository root. Replace the example path with your own `.jk` file when ready.

### Check a program

Check the program without executing it:

```bash
cargo run --release -- check examples/basics/functions.jk
```

### Build a native executable

```bash
cargo run --release -- build examples/basics/functions.jk --release -o target/joky-examples/functions
./target/joky-examples/functions
```

This also prints `Result: 30`. Cargo's `--release` builds the compiler with optimizations; the `--release` after `build` enables optimization of the generated Joky code. The resulting executable does not need the Joky compiler to run; programs using external libraries still need those libraries at runtime.

## A look at the language

### Structured concurrency

The branches of `parallel` can run concurrently on worker threads. The block waits for all branches and returns a tuple in source order; their execution order is not fixed.

```joky
fn square(value: Int32) -> Int32 {
    value * value
}

fn main() {
    let results = parallel {
        | square(2)
        | square(3)
        | square(4)
        | square(5)
    }
    let total = results.0 + results.1 + results.2 + results.3
    println(total)  // 54
}
```

See [parallel computation](examples/concurrency/parallel_compute.jk) and [tasks](examples/concurrency/tasks.jk) for runnable examples.

### Concurrent ownership

A `Cown` holds mutable state that tasks can share. Access to the payload goes through `when`, which acquires exclusive access for the block and releases it afterward.

```joky
class Counter {
    var value: Int32 = 0

    fn increment() {
        self.value = self.value + 1
    }
}

fn main() {
    let counter = Cown.new(Counter(value: 0))
    let result = when (counter) |state| {
        state.increment()
        state.value
    }
    println(result)  // 1
}
```

A `when` block can acquire multiple Cowns together. See [state transfer](examples/concurrency/cown_transfer.jk) for an example.

### Algebraic effects

An effect declares an operation; a handler supplies its behavior. Here the local handler returns a fixed age, so the program needs no interactive input.

```joky
eff console {
    fn read(prompt: String) -> Int64
}

fn read_age() -> Int64 {
    do {
        console.read(prompt: "age")
    } with {
        console.read(prompt) => 42
    }
}

fn main() {
    println(read_age())  // 42
}
```

See the [effect and handler guide](docs/lang/effects.md) for the model and [timer example](examples/effects/clock_sleep.jk) for a suspending operation.

## More examples

Joky also implements generics, dynamic traits and trait composition, modules, cursors, and native I/O providers. The [examples guide](examples/README.md) groups runnable programs by topic and lists their dependencies and behavior:

| Example | What it demonstrates |
| --- | --- |
| [Functions](examples/basics/functions.jk) | Typed functions and calls |
| [Dynamic traits](examples/traits/dyn_traits.jk) | Dynamic dispatch and receiver ownership |
| [Trait composition](examples/traits/dyn_trait_composition.jk) | Combining interfaces in a dynamic value |
| [Cursors](examples/collections/cursor.jk) | Custom iteration and generic collection |
| [File copying](examples/io/file_copy.jk) | Chunked reads and writes; creates files under `target/` |
| [C FFI](examples/ffi/abs.jk) | Calling a native C function |

For example:

```bash
cargo run --release -- run examples/concurrency/cown_when.jk
```

## Tests and benchmarks

Build the release runtime before running the tests. AOT-specific CLI tests require it even when the parity matrix uses debug AOT or has AOT disabled.

```bash
cargo build --release --manifest-path crates/joky-runtime/Cargo.toml

# Compiler and integration tests; parity defaults to JIT + debug AOT
cargo test

# Full JIT / debug AOT / release AOT parity matrix
JOKY_TEST_AOT=full cargo test --test cli

# Skip AOT in parity tests; AOT-specific CLI tests still run
JOKY_TEST_AOT=off cargo test --test cli
```

The runtime has a separate Cargo manifest; root `cargo test` does not run its unit tests. See [runtime development](docs/runtime/development.md) and [test scope](docs/testing/suite.md) for details.

Measure the parser and small compile-and-run workloads:

```bash
cargo bench --bench compiler_pipeline

# Quick parser-only run
cargo bench --bench compiler_pipeline parser -- \
  --warm-up-time 0.1 --measurement-time 0.2 --sample-size 10 --noplot
```

These benchmarks measure selected compiler workloads, not application performance relative to other languages.

## Documentation

Start with the [documentation index](docs/README.md). Most design and contributor documentation is currently in Chinese.

- [Language guide](docs/lang/README.md)
- [Packages and modules](docs/lang/modules.md)
- [Effects and handlers](docs/lang/effects.md)
- [Standard library](docs/stdlib/README.md)
- [Compiler architecture](docs/compiler/architecture.md)
- [Runtime development](docs/runtime/development.md)
- [Tests and measurements](docs/testing/README.md)
- [Contributing guide](CONTRIBUTING.md)

### Repository layout

```text
src/                       Compiler: syntax → sema → HIR → MIR → Cranelift
crates/joky-runtime/       Runtime: tasks, ownership, effects, and I/O
crates/joky-runtime-abi/   Compiler–runtime ABI
crates/joky-runtime-core/  Shared runtime foundations
std/                       Standard library modules
examples/                  Runnable Joky programs
tests/                     Integration tests and fixtures
benches/                   Compiler benchmarks
docs/                      Language, architecture, and design notes
```

## Roadmap

The next planned area is [embedded execution and script hot reload](docs/plans/embedded-execution.md): reloading module code while preserving compatible state, a C API for embedding, and host-driven scheduling. A portable execution path using Pulley is under investigation and requires a feasibility prototype.

These capabilities are not implemented. Watch mode, portable bytecode, and game-engine integration are design goals, not available commands or supported platforms. See the [current plans](docs/plans/README.md) for progress and open work.

## Contributing

Bug reports, small programs that expose compiler or runtime issues, documentation improvements, and implementation work are welcome. Please read the [contributing guide](CONTRIBUTING.md); for substantial language or runtime changes, open an issue to discuss the design first.

When reporting a problem, include your OS and architecture, Rust version, Joky commit, exact command, a minimal `.jk` program, and the resulting output. The [issue tracker](https://github.com/julywind168/joky/issues) is the place for bugs and proposals. English and Chinese contributions are welcome.

## License

Joky is licensed under the [MIT License](LICENSE).

Built with [Cranelift](https://cranelift.dev/) and the Rust ecosystem.
