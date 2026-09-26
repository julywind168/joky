# Contributing

Use English for identifiers, source comments, diagnostics, test names, and
executable output. Chinese is welcome in project documentation and discussion.
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

Keep semantic checks in the shared typed IR planner. Direct and CPS lowering,
and JIT and AOT backends, must agree on types, scope and evaluation order.
Do not embed compiler-process addresses into object files. String lowering stays
unsupported until its ownership and ABI are defined.

Expose checked embedding operations through `vm::VmEngine`. Keep compiled
metadata immutable, publish modules only after successful compilation, and keep
compiler contexts out of continuations. Host registrations require an explicit
`FunctionSignature` and an unsafe ABI/lifetime contract. Keep all Cranelift
dependencies on the same release series and commit Cargo.lock updates together.

Before submitting changes, run:

```sh
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --release
```

Compiler and runtime changes need behavioral tests that exercise generated code.
AOT interoperability tests require `cc`; the dynamic-library integration test runs
on Unix. Include multiple pending polls and a test that confirms side effects are
not replayed. Test failure and cancellation paths when changing task lifecycles.
