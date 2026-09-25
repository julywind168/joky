# AOT Instantiation and Entry Registration

The initial AOT unit is one executable: source modules and concrete generic
instances are linked into one MIR program before one native object is emitted.
JIT and AOT share semantic analysis, HIR/MIR lowering, verification and function
code generation. AOT does not instantiate types or resolve trait methods at runtime.

## Generic Instances

An imported instance is identified by its defining `SymbolId` and canonical
concrete type arguments, including the defining identity of nominal types.
Artifact-local type indices, caller identity, inferred versus explicit argument
syntax and native addresses do not distinguish instances. The compiler expands
transitive requests until the instance set is closed and emits one instance
artifact per identity per link. Local instances are also collected transitively,
sorted and deduplicated before MIR lowering.

Templates are instantiated in their defining module's environment, retaining
private helpers and dependencies. Separate instances may contain copies of those
helpers; this contract does not require native code deduplication. Imported
instance exports always permit Pending, even when a particular body returns Ready
immediately. The existing module ABI and cache rules apply unchanged; see
[Module ABI](module-abi.md). JIT and AOT share the module MIR cache: `joky build`
reads and writes the same `.joky/cache/` as `joky run`. Backend choice, opt
level and debug info are not part of the MIR key. The linked native object is
cached separately (`.jo`) by source-graph fingerprints plus target, opt level
and debug info. A matching `joky build` reuses that object and skips Cranelift.
After linking, only functions reachable from `main`, compiler-protocol
methods (`Drop`, `Hash`, `PartialEq` and the other builtin implementations
codegen can invoke without a MIR call) and static callees (including function
values, tasks and handlers) are handed to Cranelift. Per-module `.jmir`
artifacts still keep unused exports. Cranelift still lowers those functions
serially into IR, then compiles the independent `Context`s in parallel
(`JOKY_CODEGEN_JOBS`, defaulting to the host's available parallelism) and
defines the machine code in enqueue order. JIT compilation still holds the
process-wide JIT memory lock around that work; AOT object emission does not.

## Static Trait Dispatch

Concrete type substitution resolves trait bounds and associated types before
code generation. Method calls resolve to concrete MIR function IDs or module
symbols that the MIR linker resolves. Receiver ownership, return layout, effects
and the Pending protocol follow the selected implementation. A concrete method
that suspends must work through the same continuation protocol as an ordinary
suspending call, including when selected by an imported generic template.

Dynamic trait objects, runtime method lookup and vtables are outside this phase.

## Build Profiles

The default build uses Cranelift `opt_level=none`. `--release` selects
`opt_level=speed` and the release runtime archive. `--strip` is passed to the
final C linker. These flags do not change MIR, generic identities, trait
resolution, or continuation entry keys.

## Target Selection

`joky build --target <TRIPLE>` accepts the compiler's own full target triple
(for example, `aarch64-apple-darwin` on an ARM64 macOS compiler). Omitting the
option selects the same target. Both forms use native CPU features; specifying
the triple does not select a portable CPU baseline.

A validated `AotTarget` in `AotBuildOptions` supplies the module compilation key,
pointer width, object backend selection and build-record target/CPU fields.
Its fields are private, so library callers cannot construct an unsupported
target. Runtime lookup, platform-specific foreign declarations and the system
C linker retain their native behavior in this first step.

Other triples, including malformed values, are rejected before reading source
or changing executables, records or debug bundles. The diagnostic names the
supported triple. Cross-target code generation and linking remain unimplemented;
they require target-aware layouts, foreign bindings, runtime archives and a
matching linker/toolchain together.

## Source Debug Information

`joky build -g` (or `--debug-info`) emits DWARF 4 function ranges and source
file/line tables. `--release -g` retains this information with optimized code;
optimization may eliminate instructions or associate several locations with one
source line. `-g --strip` is rejected before changing existing build outputs.
Without `-g`, no Joky source DWARF is emitted.

Source expression spans survive HIR/MIR lowering, optimization, module linking
and continuation splitting. Imported generic bodies refer to their defining
module. Synthetic wrappers without a real source span are omitted. Ordinary
functions and independently callable resume entries each have their own code
range and line table. Resume entries use the source function's name with a
`[resume N]` suffix in DWARF; debuggers may display the native linkage name.
MIR cache format `JKMIR016` preserves this metadata and rejects older artifacts.

On ELF targets, DWARF remains in the executable. On macOS, `dsymutil` must be
available on PATH: the build creates `<output>.dSYM` before deleting temporary
objects. Keep this bundle beside the executable when moving or debugging it.
The DWARF file inside the bundle is hashed as `debug_artifact` in the build record.
A relink invalidates an older bundle, including when rebuilding without `-g`.
Symbol-generation failures fail the build, remove partial symbols and leave no
successful build record. The executable may already exist after such a failure.
The `dsymutil` executable is not individually hashed by the current record schema.

LLDB can resolve `breakpoint set --file main.jk --line 5` and show the source line
after a suspension resumes on a worker. This initial support does not describe
local variables, provide debugger expression evaluation, or reconstruct async
logical call stacks. Source files must remain available for source display.
The macOS path is covered by native debugger validation; ELF emission uses native
object relocations and still needs equivalent Linux debugger validation.

## Build Records

Every successful `joky build` writes `<output>.build.json` beside the executable
and reports its path. The suffix is appended: `dist/app.bin` produces
`dist/app.bin.build.json`. Schema version 1 is JSON with these fields:

| Field | Contents |
| --- | --- |
| `compiler` | Package version, existing compiler build fingerprint, compiler executable path, SHA-256 and size |
| `options` | Target triple, pointer width, native CPU selection, Cranelift optimization level, runtime lookup profile, strip setting and source debug-info setting |
| `sources` | Sorted paths and SHA-256 hashes of every compiled source module, including transitive imports, used standard modules and embedded binary resources |
| `runtime_abi_version`, `runtime` | Expected runtime ABI version and the resolved archive path, SHA-256 and size |
| `linker` | Resolved C driver path, SHA-256, size, `--version` output, exact argument array, working directory and selected build environment variables |
| `object_sha256`, `launcher_sha256` | SHA-256 hashes of the generated object and launcher before their temporary files are removed |
| `artifact` | Final executable path, SHA-256 and size after linking, including `--strip` when requested |
| `debug_artifact` | Optional separate DWARF file path, SHA-256 and size inside the macOS dSYM bundle |

Source hashes come from the compilation's source units, not a reread after the
build. Generic instances derive from those source modules. Runtime and linker
hashes are checked again after linking; detected changes fail the build. The
compiler executable hash supplements `build_id`, which is the existing source
and dependency-lockfile fingerprint rather than a cryptographic tool identity.

`runtime_profile` describes archive lookup, while `opt_level` describes generated
code. `JOKY_RUNTIME_ARCHIVE` can override lookup; the `runtime` record always names
and hashes the actual archive. The record does not infer an overridden archive's
optimization profile. `debug_info` records whether `-g` was enabled for generated
Joky source information, not any debug information already present in the runtime archive.

The driver is resolved from PATH once and used for both version discovery and
linking. Arguments are serialized as an array, so spaces and quotes are preserved.
Recorded environment variables are limited to `SDKROOT`,
`MACOSX_DEPLOYMENT_TARGET`, `LIBRARY_PATH`, `CPATH`, `C_INCLUDE_PATH`,
`CPLUS_INCLUDE_PATH`, `COMPILER_PATH`, `DEVELOPER_DIR` and `SOURCE_DATE_EPOCH` when
set. Other environment values are not copied. Command arguments and recorded
environment values must be UTF-8; unsupported values produce a diagnostic.

Records have no generation timestamp and use stable source/key ordering. They are
published with a temporary file and rename only after successful linking and
artifact hashing. Before the linker can replace an executable, its old record is
removed; a linking or publication failure cannot leave old provenance attached
to a new binary. A failure earlier in compilation leaves any old executable and
its matching record intact. Record failures make `build` fail; a completed binary
may still exist if record publication fails after linking.

This is provenance, not a hermetic recipe or a guarantee of byte-identical
rebuilds. Absolute paths, native CPU feature selection, object ordering and native
toolchain timestamps may vary. SDKs, system libraries and tools invoked internally
by the C driver are not individually hashed; ambient toolchain configuration is
not fully captured. Temporary objects/launchers are not retained. A changed
artifact hash must be investigated rather than interpreted as a reproducibility
guarantee. Cross-compilation remains a separate task.

## Continuation Entries

After MIR linking, AOT assigns nonzero keys starting at one in sorted
`(MirFunctionId, MirContinuationId)` order. All continuation identities reserve a
key, but only independently callable machine entries enter the launcher manifest.
Their exported symbols are `joky_resume_entry_<function>_<continuation>`; the
manifest is sorted by key. Code and manifest consume the same key map. Neither
keys nor symbols depend on previous compilations in the compiler process or the
order in which functions are emitted.

These keys identify entries within one linked program, not across edits or
program versions. They are not cached native addresses. Full byte-for-byte
reproducibility of objects and executables is a separate build task. JIT continues
to allocate process-unique keys so independent backends can coexist.

The launcher references the versioned runtime ABI marker and registers the whole
manifest before calling main or starting its tasks. Each registration uses the
linker's relocated function address with the `void entry(void *continuation)` ABI.
The registry is scoped by runtime scope and key:

- Zero keys and null addresses are rejected.
- Registering the same key and address again succeeds without another ownership
  claim; entries are not reference counted.
- Registering a different address for an occupied key fails and preserves the
  original entry. The launcher exits with status 5 on registration failure.
- Distinct scopes may reuse keys. A JIT scope drains its work before removing
  registrations and releasing code. The standalone AOT launcher uses the
  default scope and statically linked code. After main and provider teardown,
  `jk_aot_shutdown` drains remaining work, retires machine entries and closed
  callback tokens, and releases any unclaimed root failure before returning to C.

Replacement requires explicit unregistration after users of the old entry have
drained. Loading multiple independent AOT objects into the same scope, dynamic
unloading and hot replacement are outside this contract.

## Regression Coverage

`tests/cli/parity.rs` runs the same packages through cold JIT and debug AOT by
default, checking their output and exit status. Cache-focused packages also run
cached JIT. Set `JOKY_TEST_AOT=full` to include release AOT as well. It covers transitive instance sharing by
multiple callers, explicit and inferred type arguments, associated types,
caller-defined Show implementations, Ready and repeatedly suspending consuming
methods, tagged managed results and parallel calls. A cancellation case uses a
Cown handshake after the first resume and checks that the owned receiver's Drop
runs exactly once. The AOT run happens after removing source and cache files.

`tests/cli/providers.rs` uses the same harness for file, socket, SQLite, recursive
timers, C heap cells, native file ownership, and suspended callbacks invoked from
foreign pthreads (64-bit Unix). It also checks synchronous and suspended Result
errors. `examples/concurrency/aot_runtime.jk` combines Cown synchronization, task races,
repeated suspension, parallel tasks and C stdio ownership. Cancellation must close
the native file exactly once with a successful `fclose`, before printing the race
winner. A second case returns an error after that cancellation. Each program run
has a 30-second deadline, shorter than the losing branch's 60-second timer.

For resource accounting, build both runtime archive profiles with the explicit
test feature before running the shared cases:

```sh
cargo build --release --features runtime-test-support
JOKY_TEST_AOT=full cargo test --features runtime-test-support --test cli jit_and_aot
```

The feature enables read-only startup and post-teardown snapshots when
`JOKY_TEST_RESOURCE_REPORT` is set. The harness requires all 18 counters to be zero
at both checkpoints in every selected mode, including the error paths. Counters cover
managed objects, the root scope's six resource categories, continuation handles,
task links, machine/provider registrations, suspend argument bytes, blocking
ready/waiting work, reactor requests, socket handles/waiters and callback tokens.
Managed objects and registry counts are process-wide so callback invocation scopes
are included. These are runtime ownership counts, not a census of libc allocations
or retained allocator capacity; the native file case separately checks `fclose`.
The reporting hook performs no cleanup: normal JIT teardown and AOT shutdown must
release resources themselves. Ordinary compiler builds emit no reporting calls.
Validation currently covers native macOS ARM64; Linux/Windows remain unverified.

Compiler tests check that equal imported callback signatures share their
structural identity across generic instantiation, while incompatible signatures
remain errors. Backend tests check that the entry manifest is independent of
intervening JIT allocation and MIR function emission order. Runtime tests cover
invalid registrations, idempotence, conflict preservation and scope isolation
and cleanup.
