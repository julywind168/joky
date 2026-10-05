# Joky Interpreter

Joky has two independent execution paths. The JIT lowers typed IR to native
machine code for desktop development, while the interpreter lowers the same IR
to an in-memory register bytecode and executes it without allocating executable
memory. AOT is a separate build-time output path and is not part of the
interpreter.

## Execution pipeline

```text
source
  -> parser
  -> explicit built-in source imports
  -> nominal/tuple layouts and transparent alias normalization
  -> built-in resolution
  -> inherent member resolution and scalar constant evaluation
  -> rigid generic template checks and bounded specialization
  -> concrete typed slot IR, control flow and frame plan
       ├── JIT lowering -> native code
       ├── AOT lowering -> object file
       └── bytecode lowering -> Interpreter
```

The interpreter consumes `core::ir::ModulePlan` and its `FunctionPlan` bodies.
JIT and AOT use the same plans through `backends::native`.
It produces a `BytecodeModule` in memory; no bytecode file format is committed
yet. This keeps the semantic source of truth shared with JIT and AOT while the
execution model is tested.

Generic functions are checked with rigid Copy parameters in shared IR, even when
unused. Calls infer or explicitly supply concrete types; a module-local cache and
worklist produce bounded monomorphic functions before bytecode lowering. The
interpreter retains those functions in its prebound callee list but excludes them,
and their templates, from public exports and signature queries. Generic recursion,
aggregate snapshots and suspension use the existing activation and ownership paths.
No generic bytecode instruction or runtime type-argument representation is added;
continuations retain concrete code, not the compiler context or template AST.

Generic record and enum applications resolve into finite nominal layouts before bytecode
lowering. Identity includes the declaration and every type argument, including
phantom arguments. Shared IR checks constructors and exhaustive matches, substitutes
field and payload types and flattens concrete aggregates into existing scalar slots.
`AggregateCall` and the usual activation ownership retain those values across
Pending, cancellation and module reload. No runtime aggregate type arguments, new value
tag or host aggregate ABI is introduced. Compile-time caches and layout limits
also bound nested and phantom type applications.

Generic aliases expand in the defining module before shared IR planning. Imports
retain definition-only templates; application checks every Copy argument, including
unused ones, and substitutes into the normalized target. Aliases add no nominal
identity, constructor namespace, bytecode instruction or runtime representation.
Bounded expansion and dependency-cycle checks apply before aliases are erased.

Inherent methods are statically resolved to internal ordinary calls, with the
receiver evaluated once as the first argument. Associated scalar constants are
checked and evaluated before lowering. Generic record and enum impl methods inherit
the impl parameters, append their own method parameters, and use the same bounded
specialization as free functions. Instance receivers bind the impl prefix; explicit
method arguments or ordinary inference supply the remaining parameters. Cache keys
include both lists. Template names stay in their definition scope while substituted
nominal arguments are checked against the consuming module's identities.
Member lookup uses nominal declaration identity; receiver arguments or associated
call inference determine the concrete signature. Self constructors retain those
arguments, and chained calls use substituted return types. Generic impl constants
retain fixed integer or Bool types and are evaluated once per member.
The explicit `joky/integer` source library
implements checked `to_*()` conversions in Joky using comparisons, branches,
Option constructors and existing wrapping conversion intrinsics. Option and Result
predicates, defaults, combinations, and payload conversions are ordinary source
methods. Defaults and alternatives are evaluated eagerly, even when unused. None of these
features introduces a bytecode instruction or changes frame ownership.

Synchronous and suspending language function values use a module-local callee index, distinct from
native vm.Function registration references. Const can produce a typed Function value;
IndirectCall reads a saved callee slot and supplies ordinary arguments and scalar or
aggregate result slots. It is a copy-propagation boundary. Register compaction visits
its callee, arguments and every result. Invalid function targets report a runtime error.
The signature and conservative allocation/retention effects are checked in shared IR.
Function values cannot cross host ABIs. Existing module ownership keeps their target
table alive across pending calls, reload, cancellation and VM destruction. Suspending
indirect calls use the same activation tree, poll budgets, aggregate completion and
cancellation paths as named CPS calls. Scope/race branches can select a saved callee
slot; register compaction visits that slot as well as all arguments. Callee selection
and argument side effects are never replayed on resume. Captured closures are not implemented.

## Register bytecode

Bytecode instructions initially address slots from the typed plan; a final
interpreter-only pass compacts unused slots while preserving parameter positions. Type checking has
already happened before bytecode generation, but the current implementation
still stores tagged `Value` enums, checks operand tags, and uses generic value
clone/drop paths at runtime. Further typed-storage specialization remains an optimization opportunity. A function contains its
signature, slot count, instructions, result slot, and suspension metadata.
Constants are owned by the bytecode module so String values remain alive for
the lifetime of calls and continuations.

The instruction set mirrors the shared IR:

| IR operation | Interpreter instruction |
| --- | --- |
| Literal | `Const` |
| Copy | `Copy` for assignment and value snapshots |
| Binary | `Binary`, or `BinaryImm` with a proven Int32 right operand |
| Unary | `Unary` for signed wrapping negation, float negation, integer bitwise complement or Bool not |
| Convert | `Convert` for wrapping integer conversion or numeric-to-float conversion with a fixed target type |
| Float | `Float` for backend-shared IEEE primitives: sqrt, floor, ceil, trunc, ties-to-even round, and width-matched to_bits/from_bits |
| Label | Compile-time marker; no emitted instruction |
| Jump / Branch | `Jump`, `Branch`, or fused `BranchCompare` / `BranchCompareImm` |
| synchronous or suspending Call | `Call`, or `AggregateCall` for a record, enum or tuple result, with a prebound callee index |
| `parallel` / `race` | `Scope` with a Join / Race completion policy |
| Alloc / Local | `Memory::Alloc`, selecting task-owned or activation-owned storage |
| Project | `Memory::Project`, adding a validated inline field byte offset to a local pointer |
| ReadMemory / WriteMemory | Checked-layout `Memory` reads and writes |
| Reference / Reserve | Reference copies after shared borrow validation |
| EndLocal | Lexical lifetime marker, erased after shared validation |
| function result | Completion at the end of the instruction stream |

Integer conversions evaluate the operand once before `Convert`. The source Value tag
selects sign or zero extension, and the target type selects truncation and interpretation.
All four integer tags are supported; non-numeric operands are rejected in shared IR.
Float targets use the same instruction: every numeric source tag converts to the target
float width with a single Rust `as` cast (round to nearest-even, finite overflow to
infinity), while float sources keep exact widening and signed saturation on narrowing.
Conversion results participate in copy propagation, scalar destination redirection and
slot compaction. The evaluator reads the source before writing the destination, so
in-place updates preserve values. Calls and suspension remain optimization boundaries;
no runtime helper or extra continuation state is needed for the conversion itself.

Records, enums and tuples are flattened in the shared planner. Construction, projection,
assignment and snapshots use existing scalar slots and instructions; no aggregate
variant is added to `Value`. Enums use an Int32 tag and a separate typed region for
each variant's payload, with all inactive lanes initialized to zero/false. The
layout sums payload widths rather than overlaying them; union packing is deferred.
Matching becomes tag comparisons, branches and copies, with one evaluation of the
scrutinee, exhaustive coverage and consistent normal result types checked before
lowering. No new match instruction or per-value runtime allocation is required.

Tuple elements are evaluated left to right before copying their flattened lanes.
Destructuring is checked in the shared planner and becomes slot views for immutable
bindings or independent copies for mutable leaves. Discards create no binding;
they never suppress initializer calls, errors or suspension. Nested shapes and
structural types require no interpreter-specific pattern instructions.

`AggregateCall` keeps arguments and result slots in boxed instruction metadata,
so scalar instruction size stays unchanged. Completion copies flattened results
into the waiting caller before releasing the child activation. Empty records and zero-storage tuples use
the same path with an empty result list. Pending, errors and cancellation retain
the existing iterative frame lifecycle. Optimization treats every aggregate result
as live, remaps every result slot, and preserves flattened parameter positions.

`if` and short-circuit `&&` / `||` are lowered in the shared typed IR using
branches, labels and copies, without new interpreter instructions. Both arms
write a common result slot before joining; Unit needs no slot. A logical
expression saves its left Bool and replaces it only when the right operand runs.
Skipped paths are still statically checked. Resume PCs enter the pending call,
so earlier conditions and side effects do not replay. Branch-local declarations
and conditional RHS declarations cannot escape; assignments to outer bindings
still reach their existing slots.

Initial lowering maps each non-label IR operation to one instruction, then a
bytecode-local pass propagates copies within basic blocks. Source writes
invalidate copy facts; jumps, calls and scopes bound propagation. Copies with no
remaining reads are removed, including chains of unused snapshots. Scalar
arithmetic and constants can write directly into an assignment destination when
the scalar instruction is the temporary's sole producer and an adjacent copy is
its sole use. Other reads, implicit returns and uses in other blocks keep the
temporary alive. This pass
does not remove arithmetic, including divisions and shifts whose discarded results can
still fail, or change the shared typed plan.

Next, local Int32 constants specialize right operands into `BinaryImm`.
Writes and block/call/parallel boundaries invalidate constant facts. An integer
load is removed only if no remaining instruction or return reads its slot.
The immediate instruction shares arithmetic semantics with `Binary`, including
signed wrapping, bitwise operations, division and shift errors; operands are never reordered.

An adjacent comparison and branch can become `BranchCompare` or
`BranchCompareImm` when the comparison is the Bool slot's sole writer, the
branch is its sole reader, and the branch has no independent control-flow entry.
The fused instruction compares matching Int32, Int64, UInt32, UInt64, Float32 or
Float64 values, or two Bools for equality
and inequality, and selects its next PC without writing a temporary Bool.
Comparisons used elsewhere remain ordinary value-producing operations.
`Binary` and `BranchCompare` require matching operand types; Bool ordering and
arithmetic are rejected. Each numeric type has a distinct value tag. Arithmetic uses shared macro-generated
handlers with concrete Rust widths and signedness; unsigned comparison never casts
through i64. Float handlers share `core::float` with constant folding, so NaN,
signed zero and subnormals behave identically in folded and executed code.
`BranchCompare` preserves the operand type, including UInt64's high bit.
Bitwise operations use the same concrete-width handlers. Shifts require matching
integer types and check the complete count before narrowing it to u32: negative
counts or counts at least as large as the width fail with a source span. Signed
right shifts extend the sign bit; unsigned right shifts insert zeros. Left shifts
discard high bits. Invalid immediate shifts fail too, even if their result is discarded.

Immediate instructions remain Int32-only. Operand tags
are still checked at runtime.

A final slot pass removes holes left by eliminated temporaries. Parameters,
including Unit positions, remain fixed; reads, writes, implicit results and parallel
completion slots are all remapped. This is not liveness-based register reuse:
values that are still mentioned by any instruction retain separate storage.
The current three loop kernels use 5/7/7 slots instead of 14/18/16. On a 64-bit
host the instruction enum is 48 bytes instead of 72 after replacing call names
with indices; Value remains 24 bytes. Source spans live in a separate array.
These are storage measurements, not a claim about throughput improvement.

The lowering pass resolves labels to instruction positions; optimization remaps
all jump targets after removing instructions. Consecutive labels share a program
counter, and terminal labels point to the end of the instruction stream, which
completes the call. Dispatch borrows immutable
instructions from the frame's function instead of cloning them. String values
are still retained when copied into slots, and pending calls own their arguments.
Mutable bindings have dedicated slots; reads copy into temporary slots so later
operand or argument evaluation cannot change an already evaluated value.
A loop reuses its slots on each iteration,
re-evaluates its Bool condition, and returns Unit. Body-local declarations do
not escape, but assignments to outer mutable bindings persist.

Semicolon sequences are flattened by the shared planner into ordinary IR
operations in evaluation order. They return the last expression's value, or
Unit with a trailing semicolon, and introduce no scope or bytecode instruction.
An abrupt exit is distinct from normal Unit completion: a trailing semicolon
does not absorb return, break or continue. The shared planner rejects
structurally unreachable expressions and merges only normally completing paths.
Suspension inside a sequence uses the same program-counter resume behavior as
any other call, so completed expressions are not replayed.

Explicit returns copy into a shared result slot and jump to a terminal label;
Unit returns need only the jump. The normal tail joins that same exit when
explicit returns exist. Functions without them retain the ordinary tail result.
Break and continue use the nearest function-local loop's exit and condition
labels, including inside its condition. These reuse Copy/Jump/Branch operations
and the existing return convention, without new bytecode or runtime frame fields.
A terminal divergent loop has no native epilogue; interpreter budgets still apply.

## Frames and suspension

`execute.rs` keeps user frames and native CPS calls in a flat arena. A frame
stores its immutable function, program counter, values and pending child indices.
An iterative cursor enters children and propagates completion/Pending back to
parents. Synchronous calls, suspending calls and nested scopes do not recurse on
the Rust stack. Slot arguments are copied directly into the child frame, and
returned values move into their parent result slots.

Separately from the activation store, `Execution` owns the implicit task arena
used by checked Joky allocations. Synchronous helpers, suspending helpers and
all parallel branches share it and its allocation budget. References, including
sealed cyclic graphs, remain valid across polls and after a child returns.
Completion and failure release activations before the
object arena, even while the terminal continuation is retained. Dropping a pending
task also releases both. Instruction-budget yields preserve allocations and PCs.
Completing a branch or an entire parallel does not reclaim its objects or reset the
budget. Shared IR checking keeps all branch arguments borrowed through the join,
including evaluation and suspension of later arguments. Shared readers may
overlap; exclusive branch loans must be disjoint. Allocation-capable branches
retain all reference input loans until task termination under the same storage
contract as ordinary calls. Scalar host registrations reject references; explicit
text registrations accept synchronous &String through JokyText descriptors.
Text descriptors borrow the original payload, live in the synchronous call or
CPS frame, and are released with that call/frame. String results are validated
and copied before input cleanup. Ordinary text borrows cannot cross language
suspension, including parallel/race. Explicit C borrowing is described below.

Synchronous local borrowing uses separate storage owned by each `Frame`. Shared
IR materializes borrowed Copy bindings with `Local` and checks their lexical
lifetimes with `EndLocal`; neither operation gives a function task arena
allocation capability. The local store reuses the aligned allocator but is not
the task arena and does not consume its allocation budget or its test fault-injection limit. Each declaration's
destination slot maps to a stable pointer. Loop re-entry initializes the same
place again; recursive activations have independent places. Reads and writes use
the shared byte layout instead of taking addresses into a movable `Vec<Value>`.
Functions containing these references retain the conservative boundary that
skips value-only optimizations and slot compaction.

Inline record field borrowing uses `Project` after validating the root local's
Copy layout. Bytecode computes the field pointer with `memory_field_path`; it
does not copy the field or allocate another object. The same owner keeps nested
and zero-size field pointers valid. A zero-size field may share an address or
sit at the end of the allocation; borrow identity comes from its logical path.
Shared IR tracks root/path origins through aliases, aggregates, control flow and
signature-based call results. Distinct local fields may be borrowed independently;
whole-object and parent/child accesses still conflict when exclusivity requires it.
Direct field reads and writes use the same checks, including field-specific write
reservations. This does not open field borrowing through reference values or
change the whole-object contract of arena references.

Explicit C imports use a separate binding kind and monomorphic pointer-call adapters.
They accept zero through four Ptr values or references to fixed-width integers or #repr(C) records,
and return an integer, Ptr or Unit. Reference addresses refer to the same stable arena/local storage
used by memory instructions, never to Value or packed u64 slot arrays. Shared IR
checks lexical unsafe permission, layouts, reference permissions and lifetimes.
The registration owner keeps native code alive across reload and continuation
retention; C calls finish synchronously and cannot retain reference arguments, including in Ptr results. Runtime errors
are checked before consuming results. Scalar direct/CPS registrations keep their
existing contract; text registrations use the explicit descriptor ABI.
Ptr has a distinct nullable value tag and pointer-sized memory layout. It can be
copied into ordinary aggregates or retained in activation slots across Pending,
but does not keep foreign objects alive or release them on completion, failure
or cancellation. Ptr.null() and Ptr.is_null(value) use a null constant and shared
IR comparison; both ordinary and fused comparison instructions preserve the tag.
External object validity and release remain the unsafe caller's responsibility.

Local references cannot escape their binding's scope or owning function, enter
task arena storage, or be passed to an allocation-capable callee. The current
source subset rejects local borrowing inside `@suspends` functions. An internal
instruction-budget slice can still pause a synchronous helper: its activation
continues owning the stable storage across polls and module reload. Completion,
failure, cancellation and instruction-limit failure release it with the frame.
Completing the helper releases its local storage before a later host suspension
in the caller. Shared IR limits the conservative total borrowed-local layout of
each concrete function to 64 KiB, including zero-size placeholders and alignment
slack; this is not a bound on the total activation stack or process memory.

Scope branches use the same activation representation as ordinary calls, so
both direct native CPS imports and Joky functions are valid branches. Each parallel
round polls incomplete branches in source order and skips completed branches.
Repeated Pending does not rebuild arguments or replay earlier side effects.
Failure immediately releases the entire arena, including siblings and locals,
even if the failed continuation remains in a scheduler. Cancellation and deep
nested-frame destruction are iterative as well. Native callback internals still
obey the native ABI and may have their own stack/allocation behavior.

Race uses the same instruction and pending branch representation with a distinct
completion policy. The first observed completion cancels all remaining branch
subtrees before parent instructions resume; the first observed failure fails the
whole task. Source-order polling breaks ties and can leave later branches unpolled.
All arguments are still prepared before any branch starts. Cancellation walks
only the losing subtrees iteratively and returns their frame capacity immediately,
without disturbing ancestors or unrelated siblings. The shared task arena and
its allocation budget remain live until the root finishes; prior effects are not
rolled back. Existing host frames are dropped, with no new external cancellation
callback or permission to retain Joky references across Pending.

Loop conditions and bodies can both suspend. Re-entering a call, parallel or race on the
next iteration creates fresh children. By default loops do not implicitly yield;
embedders can enable cooperative instruction slices as described below.

This is the same observable protocol as generated CPS code:

- `Pending` keeps the frame alive;
- `Completed` stores the result and advances the parent program counter;
- `Failed` stops the parent and releases children.

`Continuation` owns the interpreter task and its `BytecodeModule`, just as a JIT
continuation owns its compiled module. Reloading a VM therefore cannot invalidate
an older interpreter continuation.

## Persistent plain instances

The VM instance registry owns each P1 record outside `Execution`. A synchronous call borrows
that stable allocation through a compiler-checked `@noescape` receiver; it gets a fresh execution
arena and the configured per-call instruction limit. No receiver can survive a Pending boundary.
The private constructor entry captures flattened, plain aggregate result slots before root-frame
teardown. Owning or borrowed results are ineligible, so captured lanes never depend on the
invocation arena. Bytecode instructions and the ordinary scalar call ABI are unchanged.

Instances pin their bytecode module and nominal type across reloads. Reentry, deferred close,
failure and persistent budgets are shared VM policy, not interpreter-specific instructions.
See [the Rust API](language.md#rust-嵌入持久实例) and
[storage invariants](architecture.md#持久纯值根).

## Host functions

Joky functions are represented by bytecode function ids. Native host functions
remain explicit imports and are invoked through the existing checked signature
registry. The interpreter never treats a Joky function as a native function
pointer. This distinction lets the interpreter run on platforms where creating
new executable memory is restricted.

Static calls are linked to module-local user/host indices during loading. Native
adapters are selected and validated then, before the new module is published;
execution does not hash function names or match signatures on every call.
Unsupported used imports and native function references fail with an ABI error
and source span. Failed loads preserve the previously loaded module and version.
Unused imports do not require adapters, including injected `joky/vm` bindings.

The direct adapter subset covers zero-to-two homogeneous Int32, Int64, UInt32, UInt64,
Float32 or Float64 arguments with the same type or Unit results, three Float64 arguments,
the mixed (Float64, Int32) and (Int32, Float64) combinations, Int32-to-Float64,
(Int32, Float64, Float64)-to-Int32, String-to-Int32, zero/one String argument with String result,
Int32-to-String and (Int32, String)-to-Int32 (including synchronous borrowed text), and the
opaque-handle combinations used by the VM built-ins (including two-argument
`vm.call_int32` and the `vm.call_float64_*` entries). C reference adapters return
integers or Float64 through the exact same signature-checked dispatch. Unit parameters are erased for direct calls, and retain their
original positions in CPS frames. CPS imports use the shared frame ABI and keep
owned String argument buffers across polls. Host code owners are held by the
immutable module and survive registration replacement and VM reload/drop.

Passing an interpreter function to `vm.register_int32` is intentionally not
supported and is rejected during loading. The call-target indices are internal
module links, not first-class function values or native pointers.

## Resource limits

`VmEngine::set_interpreter_limits` configures subsequent calls. It rejects JIT
mode instead of silently ignoring limits. For example:

```rust
use joky::vm::{InterpreterLimits, VmEngine};

fn main() -> Result<(), joky::Error> {
    let mut vm = VmEngine::new_interpreter();
    vm.set_interpreter_limits(InterpreterLimits {
        max_frames: 1024,
        instruction_budget: Some(1_000_000),
        poll_budget: Some(10_000),
    })?;
    Ok(())
}
```

- `max_frames` bounds simultaneously live user and native CPS activations,
  including all parallel branches. Default: 1024. Zero is rejected.
- `instruction_budget` bounds total bytecode instructions plus native CPS polls
  across the whole call and every resume. Exhaustion returns `ResourceLimit` and
  releases execution state. Default: unlimited. Zero fails before any work.
- `poll_budget` optionally makes async execution return Pending after a slice,
  preserving the exact cursor and parallel round. It also works inside synchronous
  callees of an async entry. Synchronous entry calls ignore this per-poll limit
  and use the total budget. Default: disabled. Zero is rejected.

Limits are copied at invocation, shared by its children, and survive VM changes.
A budget yield does not itself imply external I/O readiness; when slices are
enabled the embedding scheduler or runtime::driver::block_on must resume runnable tasks.
The blocking driver distinguishes instruction-slice yields from external host Pending,
merges live host-frame timer/yield wake sources, and sleeps until the earliest deadline.
It reports unsupported external waits instead of polling them indefinitely. The CLI
uses this driver for suspending main, including joky/task imports. Scope/race order remains
deterministic, but slices do not promise fair CPU time among branches. A slice yield
does not advance to the next branch or count as a winning completion.
A caller can cancel an infinite interpreted loop between slices. Native callbacks
must return promptly: instruction accounting does not preempt callbacks, limit
their memory, or automatically constrain new VMs created by host code. These
controls are not an isolation boundary for untrusted native code.

## Runtime diagnostics

The shared planner records a source span per IR operation. Bytecode lowering and
instruction compaction preserve that mapping. Failures retain their original
phase/code/message, with the innermost source span and `Error::runtime_trace()`
containing function names, final bytecode PCs and call-site spans, innermost first.
The CLI renders the source location and trace. Embedders should retain the source
associated with the continuation's module version when interpreting spans after
reload; spans do not embed a file identifier or a copy of the source text.

## Implementation layout

- `mod.rs`: immutable bytecode, static linking, lowering and owned values.
- `execute.rs`: iterative frames/scopes, budgeting and task lifecycle.
- `host.rs`: prevalidated native adapters and owned CPS argument buffers.
- `optimize.rs` and `optimize/fuse.rs`: local transformations and slot compaction.

## Modes and limits

`VmEngine::new()` keeps JIT as the default. Interpreter mode shares the public
call, reload and continuation APIs, and runs the same behavior cases where the
backends support them. The crate still depends on Cranelift at build time;
interpreter mode alone is not a separate portable distribution. The bytecode
has no serialization format or version contract. See the
[architecture boundaries](architecture.md#已知边界) for diagnostic and resource-limit differences.
All three backends fail on integer division by zero, wrap arithmetic at the selected
width and wrap signed MIN / -1 to MIN, for every integer lane from Int8 to UInt64.
Unsigned division and comparison use unsigned operands, and shifts check the lane
width (7 for 8-bit, 15 for 16-bit). Immediate fusion remains Int32-only.
Float32/Float64 arithmetic and comparisons follow IEEE semantics in all backends:
NaN never compares equal, signed zero survives, and division by zero yields
infinity instead of failing.
Native adapters check the shared error channel before decoding returned values,
including String pointers; native failures retain the original module's span.

## Performance investigation

The [performance history](interpreter-performance.md) keeps measured stage
comparisons, local artifact locations and profiling commands. The
[benchmark guide](../benches/README.md) defines the current runner and its
measurement limits. Historical timings are not a guarantee for later revisions.
