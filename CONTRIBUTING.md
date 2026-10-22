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
Do not embed compiler-process addresses into object files. String literals copy
relocatable module data into budgeted, task-owned UTF-8 storage. Host text uses
explicit JokyText pointer/length descriptors; keep inputs alive through synchronous
calls or CPS completion/cancellation without copying their payloads. Owning text
roots use the versioned text ABI and copy results before task cleanup.
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

Choose validation by the actual impact of the change, following
[AGENTS.md](AGENTS.md#按改动范围验证). A commit or stage completion alone does not
require the full suite; mixed changes use the broader applicable scope.
During private development, use Debug by default. Use Release only for explicitly
requested release acceptance, performance measurements, or optimization-specific
issues, and explain why it is needed. Ordinary stage completion is not release acceptance.

- Documentation and ordinary comments: review content and relevant links, and run
  `git diff --check`. Executable documentation examples need code validation.
- Isolated tests, fixtures, or probes, including test-only module wiring: run the
  affected tests, plus `cargo fmt --check` and
  `cargo clippy --all-targets -- -D warnings` for Rust changes. Generated-code
  cases must cover JIT, interpreter, and AOT. Unrelated benchmarks and exhaustive
  suites are not required. Shared fixtures need every affected entry point checked.
- Local implementation changes: run focused behavioral tests, then
  `./scripts/check.sh`.
- Cross-cutting semantics, ABI, storage layout, ownership or task lifecycle
  changes; broadly affecting build, dependency or shared test infrastructure
  changes: run focused tests, then the default Debug check and affected Debug
  exhaustive tests.
- Explicit release acceptance: run both `--full --release` and `--stress --release`;
  run the default check separately when Debug coverage is also needed.
  During development, ownership, cancellation, and Map changes need the affected
  Debug exhaustive tests; unrelated stress suites are not required.

Run an affected Release or exhaustive test directly when that is the scope of the
change. Explain why broader validation is needed when expanding it. Reuse passing
results for unchanged code: documentation-only follow-ups do not require another
code check. Report the checks actually run and any unverified areas.

The check entry points are:

```sh
./scripts/check.sh          # Quick development checks
./scripts/check.sh --full   # Compatibility alias for quick; Debug only
./scripts/check.sh --stress # Independent Debug exhaustive tests and long benchmarks
./scripts/check.sh --release # Explicit Release tests and short benchmarks
```

The script stops at the first failure and locates the repository from its own
path, so it also works from another directory. Quick checks run formatting,
Clippy on all targets, default Debug tests and short while/Map benchmark
correctness inputs (while up to 1,000 iterations, Map up to 32 entries), preserving
boundary sizes, seeds and all backends. `--quick` is an explicit alias for the default.
Full is a compatibility alias for quick, so do not run both on unchanged code.
Stress checks run only Debug exhaustive tests and default long benchmark inputs;
they do not repeat formatting, Clippy or default tests. If these are slow, narrow
the run to affected cases rather than automatically switching to Release.
The explicit `--release` flag can be combined with any mode to select Release
tests and benchmark correctness without running both profiles. Formatting and
Clippy are unchanged. Individual Cargo commands remain useful for focused changes.

The default test profile uses optimization level 1 with Debug assertions and
overflow checks enabled. Stable dependencies use optimization level 2 in both
Dev and Test; Joky itself stays unoptimized in ordinary Dev builds. The first
build of these profiles takes longer, while subsequent source changes reuse
dependency artifacts and incremental compilation. For unoptimized test stepping,
use `CARGO_PROFILE_TEST_OPT_LEVEL=0 cargo test <test-name>`.

Every default test needs a distinct regression responsibility. Before adding one,
check whether the same fixture, diagnostic, or boundary already runs elsewhere.
Backend coverage is a property of the behavior across the suite, not a reason to
repeat its full matrix at the VM, AOT, and CLI layers. Separate shared type limits
from API behavior instead of multiplying their independent axes. Stop allocation
budget sweeps after the first complete success and its cleanup assertions.

Keep default tests useful for daily iteration: run short deterministic model cycles
and resource-boundary cancellation checks there. Name long model matrices,
every-instruction cancellation sweeps, and additional CLI type matrices with the
`exhaustive_` prefix, and mark them
`#[ignore = "exhaustive: ..."]`. The stress check runs these in Debug by default;
they remain available independently with:

```sh
cargo test exhaustive_ -- --ignored
```

Keep a default regression for the same behavior when adding an exhaustive test.
Compile shared fixture cases once per backend, then check results and reset host
effects per call. Keep registries with private invariant probes separate from
production registries. Batch AOT cases into one object and harness when their host
bindings are identical; reset runtime errors and the owner ledger before each call.

Static rejection matrices can use the shared front-end test helper when the error
occurs before backend selection. Keep focused entry-point parity regressions for
error codes, messages and source spans; host ABI validation and execution failures
still need their affected backends. CLI diagnostic matrices normally use `build`
once per source, retaining source locations and output-preservation assertions.
The CLI syntax, assignment, dependency and entry-point tests cover route parity.
When an identical fixture already runs in both VM modes with ownership
assertions, its CLI test should cover standalone AOT only. Batch related CLI
fixtures with `write_fixture_batch` when their entries return Unit and their
assertions do not depend on separate processes. Keep CLI argument, file, output,
import and diagnostic regressions independent of language-feature matrices. Keep CLI routing smoke
coverage separately; do not shrink message-size boundaries or remove real
backpressure assertions to speed up checks.
The full 1 MiB TCP transfers, interpreter backpressure completion, and additional
CLI network backend matrices live in ignored `exhaustive_` tests selected by
`--stress`, with their original sizes and assertions intact. Default Debug tests
retain short-message protocol boundaries, real backpressure, JIT resumption,
cancellation and owner cleanup in both VM modes, standalone AOT interoperability,
and CLI routing smoke coverage.
Reuse the cancellation harness to execute each selected prefix with one poll;
keep a budget-one completion trace to exercise repeated resumes.

Opt-in measurement and diagnostic tests stay separate from the
`exhaustive_` group and are not enabled by any correctness check mode.

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
