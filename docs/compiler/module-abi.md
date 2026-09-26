# Module ABI v8

The language ABI is independent of native calling conventions and JIT addresses.
Module identity uses the package-relative source path. Nominal type identity uses
the defining module, declaration kind and name; layout changes affect the ABI
fingerprint, not the type identity. Trait identities retain their defining module
across dynamic dispatch and interface composition.

The `JKMIR021` artifact format stores `ModuleInterface` beside MIR's layout-only
`TypeTable`. Export signatures, generic templates/requests, import identities and
typed struct defaults belong to the interface. Source-node facts remain in the
non-serializable `CheckedTypes` used during lowering. Importers share immutable
`ModuleTypes` snapshots combining layouts and interfaces; old MIR formats rebuild
as cache misses. This changes compiler artifacts, not the runtime calling ABI.

Compiler ABI metadata version 46 adds resolved binary resources to AST templates,
typed exported constants and MIR. Module cache keys include SHA-256 hashes of
resource paths and bytes; the AOT object key inherits these fingerprints.
Discovery caches only resource paths and reloads contents for each new graph.
Each source unit carries the same resource snapshot used for hashing and lowering.
Older metadata and MIR artifacts rebuild as cache misses.

Runtime ABI version 28 exports the existing raw-data Bytes constructor as
`jk_bytes_from_data`. Embedded binary constants use ordinary managed Bytes
allocation and copying, with no UTF-8 conversion or native Unicode dependency.
Static zero-copy storage remains future work.

Compiler ABI metadata version 48 adds source-level effect aliases and enum rest
patterns to the serialized AST, while retaining the native `TlsStream` identity
and support for imported unit enum variants as typed constants/defaults. Previous
native indices are unchanged. Runtime ABI version 29 adds the `tls` operations
to the existing socket provider. Upgrade transfers TCP ownership; read/write
borrow TLS and close consumes it. Rebuild older static runtime archives.

## Values and Ownership

- Scalars use the existing native scalar representation; Unit has no words.
- String uses a managed pointer and length. Shared arguments and results transfer
  one retained reference. A caller that keeps using a shared value duplicates it.
- Tuples recursively flatten their elements in source order, including results.
- Classes and native resources use one opaque pointer. An owned parameter moves
  the handle to the callee; a borrowed parameter lends it only for the call. The
  callee cannot release or return that borrowed handle as an owned result.
- Class results transfer ownership to the caller. Class layouts and stable drop
  metadata are available to the linker for recursive field cleanup. Reading a
  shared field through a borrowed class retains the returned shared reference.
- A closure uses code and environment pointers. These addresses exist only in
  the linked executable and are never stable identities.

Import stubs preserve parameter labels, borrowing and declared effects. Linking
checks the resolved parameter types, ownership, return type and Pending contract
before native code generation. Artifact-local indices are relocated into the
linked program; each MIR type, handler and continuation reference participates.

The cross-module nominal import path preserves class, struct and enum identities
through public factory/consumer functions and `module.Type` annotations, including
types forwarded through another dependency. Methods use generated ABI wrappers
that retain the defining type's identity and receiver ownership. Static trait
implementations, including caller-defined Show methods, can participate in generic
instantiation. Ordinary class constructors are exposed through public factories;
type-function results support generated struct construction and constant defaults.
`Dyn(Trait)` and `Dyn(A + B)` values carry relocated method signatures and
associated-type bindings. Interface identity uses a sorted set of qualified trait
names, independent of source order. Method slots are sorted by method name;
compositions with duplicate method or associated-type names are rejected.
Module ABI metadata version 19 includes this dynamic interface and the owned
`DynamicUpcast` MIR instruction; the linker checks
method names, receiver modes, parameter borrows, and signatures before unifying it.
Owned upcasts compact selected entries in each object's private vtable in place.
Original payload offsets, call glue, allocation ownership and drop callbacks remain
unchanged. This requires unique ownership and must be revisited if vtables become
shared immutable data. Borrowed upcasts and implicit coercions are not supported.

Version 20 adds the builtin `Debug` trait and the `DebugString` / `Echo` runtime
intrinsics. Echo locations are embedded in the parsed module before generic
templates are cached, so instantiated code retains its defining source location.
Older artifacts are invalidated. The AOT runtime contract is version 3, including
the `jk_debug_string` and `jk_echo` exports.
Native shared-library export remains outside this contract.
Synchronous scalar C imports use explicit adapters as described below.

## C imports

`@extern(c, "library", "symbol") fn ...;` declares a synchronous
foreign function, optionally public to other Joky modules. The ordinary Joky
function signature remains the module-facing ABI. A declaration-only MIR function
retains the library and symbol; native codegen supplies an adapter to the host C
calling convention, including signed/unsigned integer extension. Libraries remain
loaded until all backend-owned scopes have drained and the JIT code is freed.

Fixed-width integers, floats and a Unit/void result are supported. Managed values,
pointer/layout types, varargs, callbacks and foreign Pending are rejected in v1.
The library/symbol binding is fingerprinted alongside the ordinary signature;
ABI v8 versions the new serialized AST/MIR fields. Cached MIR contains no resolved
address. Loading and symbol resolution repeat on every execution. Replacing a
native library does not recompile MIR and must preserve the declared ABI.
See [C FFI](../lang/c-ffi.md) for source-level and effect contracts.

## Effects and Pending

Imported effects retain the defining module's identity. Qualified names such as
`util.Timer` work in effect declarations, calls and handlers. Operation names are
relocated independently of declaration order. Built-in provider names (`time`,
`file`, `tcp`, `udp`, `unix`, `unix_dgram`) use a runtime identity and require matching
operation signatures across modules.

The artifact records each exported function's computed Pending property, including
internal task waits. Import stubs preserve it before continuation lowering. Ready,
Pending, resume and cancellation use the existing function Pending protocol;
transfers do not introduce another scheduler or allocation protocol. Class drop
glue accepts null cancellation placeholders. Changes to the exported Pending
property invalidate dependent artifacts.

Generic instance exports always use a Pending-capable ABI, including instances
whose bodies return Ready immediately. This covers task waits and concrete trait
methods discovered during instantiation without changing an already compiled
caller's protocol. Ready result buffers receive the same tagged cleanup glue.

## Artifact Cache

Builtin value and mutable collection method signatures are embedded from their
standard modules, including for importless calls. These sources participate in
the compiler build fingerprint, so signature changes invalidate cached code.
Local redeclarations must preserve the builtin receiver, parameter and result
types; method-level trait prerequisites remain part of the declaration ABI.

Metadata version 30 adds intrinsic method type parameters and their bounds,
static trait receivers, and the selected target type at static call sites.
Method ABIs explicitly record receiver mode: static implementations have no
runtime receiver parameter and are exported as `@static_method/<type>/<method>`.
`String.parse(T)` dispatches through `FromString`. Builtin implementations cover
all eight integer types, Float32, Float64 and Bool. AOT runtime ABI version 11
adds the corresponding typed parse exports; Bool writes a single 0/1 byte.
Parse outputs are initialized only on success, so callers must check the status
before loading them. Metadata remains version 30 because its layout is unchanged.

AOT runtime ABI version 12 adds `jk_bytes_compare`, which borrows two Bytes
handles and returns an i32 (-1, 0, 1) for unsigned byte lexicographic ordering.
Tuple, Option, Result and List ordering lowers to ordinary MIR branches,
projections and member trait calls. Lists use loop cursors; partial comparisons
short-circuit on both unequal and unordered results. No new metadata layout is
required. Option ordering explicitly reverses its Some/None storage tag order.

Metadata version 31 adds the owned builtin `Hasher` type, Hash runtime intrinsics,
and explicit Hash implementation metadata. AOT runtime ABI version 13 adds
`jk_hasher_new/word/string/finish` and the mutable map exports. Map key-kind slots
are now pointer-sized: generated code passes a descriptor with native hash and
equality function pointers. The runtime copies the descriptor during the call,
caches each entry's hash, and invokes equality only for matching hashes. Legacy
0/1 descriptors remain available for scalar/structural and String runtime tests.
Adapters retain shared key fields before invoking ordinary Joky methods, matching
the copy/shared parameter ABI; Hasher itself is borrowed. Hashes use a process
seed and must not be persisted in module caches or serialized values.

AOT runtime ABI version 14 adds `jk_hasher_bytes`, which hashes the visible Bytes
content with length framing. Key adapters also support Duration, Unit, payloadless
enums and recursively dispatched tuples. Tuple adapters retain shared members
only when calling custom methods, and equality stops at the first unequal member.
Zero-width keys use one zero storage word without changing the Unit function ABI.
The module metadata layout remains version 31.

AOT runtime ABI version 10 exports the existing Bytes slicing, lookup,
concatenation and UTF-8 conversion operations, and the String search, Unicode,
case conversion and numeric parsing operations used by the standard value
modules. Their declarations now consistently use `@intrinsic struct`.

Metadata version 29 records whether an intrinsic type is declared as `struct`
or `class`, and includes this distinction in its ABI fingerprint. `List` is
declared as an intrinsic struct while retaining its builtin value representation.
Older cached declarations are invalidated; the runtime ABI remains version 9.

Metadata version 28 adds intrinsic method `where` predicates to serialized
declarations and ABI fingerprints. Bound and predicate ordering does not affect
the fingerprint; changing a required trait or its type parameter does. Exported
predicates retain the defining trait's module identity. Embedded collection
declarations also participate in the compiler source fingerprint. The runtime
ABI remains version 9.

Public constants carry a typed, serializable expression tree. Importing a constant
remaps its types and inlines its expression without copying AST node identities.
Scalar, String, tuple, unary and binary expressions are supported, including
references to private local constants and public constants in dependencies.
The resolved exported content participates in the ABI fingerprint, so changing a
constant value invalidates transitive callers even if its type stays the same.

Generic exports carry checked parameter types, inferred constraints, effects and
their defining module's AST. A concrete call is instantiated in that module's
environment, preserving private helpers, constants and local type functions.
Instances use the defining symbol and canonical concrete type identities, never
local type indices or JIT addresses. An instance is compiled once per link and
stored as its own artifact for subsequent runs. Template implementation changes
(including private helpers) conservatively invalidate dependent artifacts.
Public type functions are evaluated in an isolated defining-module environment;
generated type identities include canonical argument types and do not depend on
local allocation order. Nested constant struct defaults retain their checked types.
Cold instantiation currently checks the defining module environment again and may
duplicate private helpers between instances; warm runs reuse the instance artifact.

`.jabi` contains readable metadata; `.jmir` contains versioned Bincode MIR, the
type table and symbol tables. Only relocatable artifact-local indices are stored.
JIT addresses, native function references and live runtime handles are absent.
Files have a magic/version header, checksum, bounded decoder and full cache key;
incomplete or incompatible payloads are cache misses. Publication uses unique
temporary files and atomic rename.

The key includes module identity, source contents, dependency ABI hashes, compiler
ABI version, compiler/runtime build fingerprint, target, pointer width and features.
The entry/library role is also part of the key so a module's own main cannot be
reused as the package entry when it is imported as a dependency.
The build fingerprint covers compiler sources and Cargo dependencies. A body-only
dependency edit recompiles that dependency; an ABI edit invalidates transitive
dependents. This transitive invalidation is conservative. A cache hit skips semantic
analysis, HIR/MIR lowering and a second per-module verifier pass; the linker
still verifies the merged program. Module graph discovery still reads/parses source.

The supported cache is local to matching compiler/runtime builds. Dynamic trait
objects use the existing Pending and managed allocation protocols.

## CLI and standard library

`joky run` and `joky build` use the modular compiler by default. They store
artifacts in the package root's `.joky/cache/` (beside a standalone entry file)
and share the same MIR key, so a run can feed a later build and vice versa.
`joky build` also stores a linked native object (`.jo`) keyed by the source
graph and AOT profile; a later build with the same sources, target, opt level
and debug-info flag reuses it. `--verbose` reports the directory, module/instance
hits, AOT object hits and missing or invalid cache entries to stderr.
`--no-cache` uses exactly the same compiler without disk reads or writes.
`--legacy` explicitly selects the original source expansion and whole-program API;
errors never trigger an automatic fallback. `Compiler::run_program` remains available
for embedding. JIT code generation runs each time; the cache contains relocatable MIR.

Standard modules retain their established short effect names (`time`, `file`,
`tcp`, `udp`, `unix`, `unix_dgram`), resource methods and FileMode/SeekFrom types.
Native resource classes bind their method interface with
`@intrinsic(effect = file)` (and the corresponding socket effect). Each method
must match the same-named operation's receiver, parameter types, ownership and
return type. The operation supplies its effect identity, control-flow mode and
suspension property; dispatch no longer depends on a compiler method-name list.
Imported bindings resolve in their defining module, including after cache reload.
The binding and declared method subset participate in the ABI fingerprint;
changing either invalidates dependent artifacts. ABI v7 also versions the new
serialized intrinsic declaration field.
These aliases are introduced by standard-library imports; ordinary user modules
continue to use qualified effect names. CLI integration tests exercise this default
modular path, including generic List/Map/Set wrappers and socket operations.
