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

The first interpreter implementation uses the existing slot plan as its input.
It produces a `BytecodeModule` in memory; no bytecode file format is committed
yet. This keeps the semantic source of truth shared with JIT and AOT while the
execution model is tested.

## Register bytecode

Bytecode instructions address typed slots directly. Joky does not need Lua's
dynamic tagged value representation because type checking has already happened
before bytecode generation. A function contains its signature, slot count,
instructions, result slot, and suspension metadata. Constants are owned by the
bytecode module so String values remain alive for the lifetime of calls and
continuations.

The instruction set mirrors the shared IR:

| IR operation | Interpreter instruction |
| --- | --- |
| Literal | `Const` |
| Copy | `Copy` for assignment and value snapshots |
| Binary | `Binary` with arithmetic or signed Int32 comparison |
| Label / Jump / Branch | `Label`, `Jump`, `Branch` with resolved program counters |
| synchronous or suspending Call | `Call` with suspension metadata |
| `scope` | `Scope` |
| function result | Completion at the end of the instruction stream |

Each IR operation maps to one instruction. The lowering pass resolves labels
before emitting jumps and branches. Mutable bindings have dedicated slots;
reads copy into temporary slots so later operand or argument evaluation cannot
change an already evaluated value. A loop reuses its slots on each iteration,
re-evaluates its Bool condition, and returns Unit. Body-local declarations do
not escape, but assignments to outer mutable bindings persist.

Semicolon sequences are flattened by the shared planner into ordinary IR
operations in evaluation order. They return the last expression's value, or
Unit with a trailing semicolon, and introduce no scope or bytecode instruction.
Suspension inside a sequence uses the same program-counter resume behavior as
any other call, so completed expressions are not replayed.

## Frames and suspension

An interpreter frame stores a function id, program counter, slot values and a
pending operation. A suspending call keeps its child frame in the parent frame;
resuming polls that child without re-evaluating its arguments. A scope keeps all
branch frames and polls each incomplete branch on every resume. A failed branch
releases the remaining branch frames and propagates the failure.
Loop conditions and bodies can both suspend. Resume polls the current child,
then continues at the next instruction without replaying prior mutations.
Re-entering a call or scope on the next iteration creates fresh child frames.
Loops do not implicitly yield; cooperative suspension still requires a call
that returns Pending.

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

The first release supports direct Int32/Unit calls and borrowed String input
for native imports, plus CPS imports using the shared frame ABI. Passing an
interpreter function to `vm.register_int32` is intentionally not supported:
that API registers a native function pointer for a JIT VM. A future VM-to-VM
registration API will use an interpreter function id instead.

## Modes and future work

`VmEngine::new()` keeps JIT as the desktop default. An interpreter constructor
selects the portable execution path. The public call, reload, CPS and `scope`
APIs are shared by both modes, and behavior tests run against both backends.

The next steps are bytecode serialization and a stable bytecode version. A
Cranelift Pulley backend can be added later as a separate portable execution
backend; it is not required to validate Joky's own interpreter semantics.
