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
  -> typed linear IR and frame plan
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

The initial instruction set mirrors the current IR:

| IR operation | Interpreter instruction |
| --- | --- |
| Literal | `Const` |
| Binary | `Add`, `Sub`, `Mul`, `Div` |
| synchronous Call | `Call` |
| suspending Call | `SuspendCall` |
| `scope` | `Scope` |
| function result | `Return` |

## Frames and suspension

An interpreter frame stores a function id, program counter, slot values and a
pending operation. A suspending call keeps its child frame in the parent frame;
resuming polls that child without re-evaluating its arguments. A scope keeps all
branch frames and polls each incomplete branch on every resume. A failed branch
releases the remaining branch frames and propagates the failure.

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
