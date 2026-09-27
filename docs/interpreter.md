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
  -> built-in resolution
  -> typed slot IR, control flow and frame plan
       ├── JIT lowering -> native code
       ├── AOT lowering -> object file
       └── bytecode lowering -> Interpreter
```

The interpreter consumes `core::ir::ModulePlan` and its `FunctionPlan` bodies.
JIT and AOT use the same plans through `backends::native`.
It produces a `BytecodeModule` in memory; no bytecode file format is committed
yet. This keeps the semantic source of truth shared with JIT and AOT while the
execution model is tested.

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
| Label | Compile-time marker; no emitted instruction |
| Jump / Branch | `Jump`, `Branch`, or fused `BranchCompare` / `BranchCompareImm` |
| synchronous or suspending Call | `Call` with a prebound callee index |
| `scope` | `Scope` |
| function result | Completion at the end of the instruction stream |

Initial lowering maps each non-label IR operation to one instruction, then a
bytecode-local pass propagates copies within basic blocks. Source writes
invalidate copy facts; jumps, calls and scopes bound propagation. Copies with no
remaining reads are removed, including chains of unused snapshots. Scalar
arithmetic and constants can write directly into an assignment destination when
the scalar instruction is the temporary's sole producer and an adjacent copy is
its sole use. Other reads, implicit returns and uses in other blocks keep the
temporary alive. This pass
does not remove arithmetic, including divisions whose discarded results can
still fail, or change the shared typed plan.

Next, local integer constants specialize right operands into `BinaryImm`.
Writes and block/call/scope boundaries invalidate constant facts. An integer
load is removed only if no remaining instruction or return reads its slot.
The immediate instruction shares arithmetic semantics with `Binary`, including
signed wrapping and division errors; operands are never reordered.

An adjacent comparison and branch can become `BranchCompare` or
`BranchCompareImm` when the comparison is the Bool slot's sole writer, the
branch is its sole reader, and the branch has no independent control-flow entry.
The fused instruction compares signed Int32 values and selects its next PC
without writing a temporary Bool. Comparisons used elsewhere remain ordinary
value-producing operations. Operand tags are still checked at runtime.

A final slot pass removes holes left by eliminated temporaries. Parameters,
including Unit positions, remain fixed; reads, writes, implicit results and scope
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
Suspension inside a sequence uses the same program-counter resume behavior as
any other call, so completed expressions are not replayed.

## Frames and suspension

`execute.rs` keeps user frames and native CPS calls in a flat arena. A frame
stores its immutable function, program counter, values and pending child indices.
An iterative cursor enters children and propagates completion/Pending back to
parents. Synchronous calls, suspending calls and nested scopes do not recurse on
the Rust stack. Slot arguments are copied directly into the child frame, and
returned values move into their parent result slots.

Scope branches use the same activation representation as ordinary calls, so
both direct native CPS imports and Joky functions are valid branches. Each scope
round polls incomplete branches in source order and skips completed branches.
Repeated Pending does not rebuild arguments or replay earlier side effects.
Failure immediately releases the entire arena, including siblings and locals,
even if the failed continuation remains in a scheduler. Cancellation and deep
nested-frame destruction are iterative as well. Native callback internals still
obey the native ABI and may have their own stack/allocation behavior.

Loop conditions and bodies can both suspend. Re-entering a call or scope on the
next iteration creates fresh children. By default loops do not implicitly yield;
embedders can enable cooperative instruction slices as described below.

This is the same observable protocol as generated CPS code:

- `Pending` keeps the frame alive;
- `Completed` stores the result and advances the parent program counter;
- `Failed` stops the parent and releases children.

`Continuation` owns the interpreter task and its `BytecodeModule`, just as a JIT
continuation owns its compiled module. Reloading a VM therefore cannot invalidate
an older interpreter continuation.

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

The direct adapter subset covers zero-to-two Int32 arguments with Int32/Unit
results, String-to-Int32, zero/one String argument with String result, and the
opaque-handle combinations used by the VM built-ins (including two-argument
`vm.call_int32`). Unit parameters are erased for direct calls, and retain their
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
  including all scope branches. Default: 1024. Zero is rejected.
- `instruction_budget` bounds total bytecode instructions plus native CPS polls
  across the whole call and every resume. Exhaustion returns `ResourceLimit` and
  releases execution state. Default: unlimited. Zero fails before any work.
- `poll_budget` optionally makes async execution return Pending after a slice,
  preserving the exact cursor and scope round. It also works inside synchronous
  callees of an async entry. Synchronous entry calls ignore this per-poll limit
  and use the total budget. Default: disabled. Zero is rejected.

Limits are copied at invocation, shared by its children, and survive VM changes.
A budget yield does not itself imply external I/O readiness; when slices are
enabled the embedding scheduler must resume runnable tasks. Scope order remains
deterministic, but slices do not promise fair CPU time among scope branches.
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
[architecture boundaries](architecture.md#已知边界) for native trap and resource-limit differences.

## Performance investigation

The [performance history](interpreter-performance.md) keeps measured stage
comparisons, local artifact locations and profiling commands. The
[benchmark guide](../benches/README.md) defines the current runner and its
measurement limits. Historical timings are not a guarantee for later revisions.
