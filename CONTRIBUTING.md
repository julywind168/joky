# Contributing to Joky

Use English for identifiers, source comments, diagnostics, test names, and
executable output. Chinese is welcome in project documentation and discussion.
Use `joky` for the package, CLI and Rust crate, `libjoky.a` for the static library,
and `joky/<module>` for built-in language modules. Public C functions use
`joky_<module>_`, C types use `Joky`, and header guards use `JOKY_`. Private CPS
helpers use `__joky_cps_`; generated CLI program symbols use `__joky_user_`.
Internal generic instances use `__joky_generic_`.
Native error helpers use `__joky_runtime_`; their public C queries use `joky_runtime_`.

Use Rust naming conventions: `snake_case` for functions and tests, `PascalCase`
for types, and `SCREAMING_SNAKE_CASE` for constants.

Format Rust with `cargo fmt`. Keep module documentation in `//!` comments and
public API documentation in `///` comments. Document every unsafe API's pointer,
layout, lifetime, and threading requirements. Do not suppress warnings to hide
unused implementation or ABI mistakes.

Generated AOT and JIT code must use the same CPS runtime. Never substitute a
handwritten frame layout for the opaque C frame. Record complete frame sizes
when registering generated entries, and retain every reachable code owner while
a continuation can still execute. Keep failure outcomes distinct from completion.

Keep backend-independent semantic checks in `core::ir`. Native code generation
belongs in `backends::native`, shared by JIT and AOT. Direct and CPS lowering,
and JIT and AOT backends, must agree on types, scope and evaluation order.
Do not embed compiler-process addresses into object files. String literals use
relocatable data objects owned by the compiled module. Native String views are
borrowed, length-prefixed UTF-8: never retain them beyond their code owner.
Standard module bindings must agree in the JIT registry, AOT symbols and C header.
Test installed-layout linking and standalone execution when changing the runtime
distribution; test closed handles and failed reloads when changing joky/vm.

Expose checked embedding operations through `vm::VmEngine`. Keep compiled
metadata immutable, publish modules only after successful compilation, and keep
compiler contexts out of continuations. Host registrations require an explicit
`FunctionSignature` and an unsafe ABI/lifetime contract. Check native errors before
consuming generated callback results, and share one runtime instance across linked
objects. Keep all Cranelift
dependencies on the same release series and commit Cargo.lock updates together.

Before submitting changes, run the complete check entry point from the repository:

```sh
./scripts/check.sh
```

The script stops at the first failure and runs formatting, Clippy on all targets,
Debug/Release tests, and the while and Map benchmarks' correctness checks in both profiles.
It locates the repository from its own path, so it also works from another directory.
For a shorter development loop, use `./scripts/check.sh --quick`; this skips the
Release runs and does not replace the complete pre-submission check.
The exact command list lives in the script; individual Cargo commands remain useful
when working on a focused test.

These checks do not time workloads, write benchmark reports, require Lua, or assert
performance thresholds. Passing them establishes correctness, not the absence of
a performance regression. Performance measurements are a separate, opt-in step;
follow [the benchmark methodology](benches/README.md), finish all builds/checks
before timing, and keep historical measurements separate from current results.

Compiler and runtime changes need behavioral tests that exercise generated code.
AOT interoperability tests require `cc`; the dynamic-library integration test runs
on Unix. Include multiple pending polls and a test that confirms side effects are
not replayed. Test failure and cancellation paths when changing task lifecycles.

Keep documentation focused: README for getting started, `docs/language.md` for
implemented syntax and built-ins, `docs/syntax.md` for the syntax and type-system
design, staged goals and implementation gaps, `docs/architecture.md` for boundaries
and ABI invariants, `docs/interpreter.md` for the current bytecode design, and `benches/README.md`
for measurement methodology. Consolidate historical interpreter measurements in
`docs/interpreter-performance.md`; do not present old timings as current results.
