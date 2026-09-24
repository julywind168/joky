//! CFG-oriented MIR lowering for control flow.
//!
//! The first MIR pass makes branches, loops, and transfers explicit. Literal
//! values are represented by typed `Const` statements; more complex scalar
//! expressions are lowered into typed MIR statements.

use crate::diagnostic::Diagnostic;
use crate::hir::CoreProgram;
use crate::module::SymbolId;
use crate::sema::{Type, TypeTable};
use crate::syntax::{BinaryOp, FieldAccess, UnaryOp, Visibility};

#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct MirFunctionId(pub(crate) usize);

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct MirModuleId(pub(crate) usize);

#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum MirTypeId {
    Struct(usize),
    Class(usize),
    Enum(usize),
    Option(usize),
    Result(usize),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MirFieldAccess {
    Index(usize),
}

impl MirTypeId {
    pub(crate) fn from_type(ty: Type) -> Option<Self> {
        match ty {
            Type::Struct(id) => Some(Self::Struct(id)),
            Type::Class(id) => Some(Self::Class(id)),
            Type::Enum(id) => Some(Self::Enum(id)),
            Type::Option(id) => Some(Self::Option(id)),
            Type::Result(id) => Some(Self::Result(id)),
            _ => None,
        }
    }
}

mod continuation_capability;
mod dump;
pub(crate) mod local_ssa;
mod lower;
pub(crate) mod passes;
mod reachability;
pub(crate) mod regions;
mod runtime_spec;
pub(crate) mod suspending_analysis;
#[cfg(test)]
mod tests;
mod verifier;

pub(crate) use continuation_capability::machine_entry_blocker;
pub(crate) use runtime_spec::IntrinsicArgOwnership;

#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub(crate) struct MirProgram {
    pub(crate) functions: Vec<MirFunction>,
    pub(crate) types: TypeTable,
}

#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub(crate) struct MirFunction {
    pub(crate) source: MirFunctionSource,
    pub(crate) declared_effects: crate::sema::EffectGroupSet,
    pub(crate) foreign: Option<crate::syntax::ForeignFunction>,
    pub(crate) id: MirFunctionId,
    pub(crate) module: MirModuleId,
    pub(crate) name: String,
    /// Populated for import stubs that are resolved to concrete functions at link time.
    pub(crate) external_symbol: Option<SymbolId>,
    pub(crate) imported_region_contract: Option<regions::Summary>,
    pub(crate) visibility: Visibility,
    pub(crate) receiver: Option<Type>,
    pub(crate) parameters: Vec<MirParameter>,
    pub(crate) receiver_local: Option<MirLocalId>,
    pub(crate) locals: Vec<MirLocal>,
    pub(crate) return_type: Type,
    pub(crate) is_task: bool,
    /// Whether this function may suspend during execution.
    ///
    /// A function is suspending if:
    /// - It directly contains `Suspend` statements (calls to `@suspends` operations), OR
    /// - It transitively calls other suspending functions
    ///
    /// This flag is computed by `suspending_analysis::compute_suspending_functions()`
    /// using fixed-point propagation after MIR lowering.
    ///
    /// Drives call-site continuation materialization for the Pending ABI.
    pub(crate) is_suspending: bool,
    pub(crate) entry: MirBlockId,
    pub(crate) blocks: Vec<MirBlock>,
    pub(crate) value_types: Vec<Type>,
    pub(crate) value_ownership: Vec<MirOwnership>,
    /// Continuation points owned by this function. The table is function-local
    /// so a resume can never target another function's block.
    pub(crate) continuations: Vec<MirContinuation>,
}

/// Value identities survive statement insertion and continuation block splitting.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
pub(crate) struct MirFunctionSource {
    pub(crate) module: Option<crate::module::StableId>,
    pub(crate) body: Option<crate::Span>,
    pub(crate) values: Vec<Option<crate::Span>>,
    pub(crate) mutable_locals: std::collections::HashMap<MirLocalId, crate::Span>,
    pub(crate) mutable_capture: Option<crate::Span>,
}

impl MirFunction {
    pub(crate) fn statement_span(&self, statement: &MirStatement) -> Option<crate::Span> {
        lower::statement_destination(statement)
            .and_then(|value| self.source.values.get(value.0).copied().flatten())
    }
}

/// Metadata for one stackless suspension boundary. `spill_values` is empty
/// until typed spill lowering is enabled, but remains part of the MIR contract
/// for the ownership/cleanup pass.
#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub(crate) struct MirContinuation {
    pub(crate) id: MirContinuationId,
    pub(crate) kind: MirContinuationKind,
    /// Effect operation for effect continuations. Function-call
    /// continuations use `callee` instead.
    pub(crate) operation: Option<crate::sema::EffectOperationId>,
    /// Statically resolved callee for a function-call continuation.
    pub(crate) callee: Option<MirFunctionId>,
    pub(crate) suspend_block: MirBlockId,
    pub(crate) resume_block: MirBlockId,
    /// Result written by the operation's completion/resume protocol. This is
    /// optional while older suspend-only metadata is materialized, but every
    /// new resumable continuation must identify its request destination.
    pub(crate) resume_destination: Option<MirValueId>,
    pub(crate) generation: u64,
    /// Number of locals that existed when the suspension point was lowered.
    /// Locals introduced only in the resume tail are not frame inputs.
    pub(crate) locals_before_suspend: usize,
    pub(crate) spill_values: Vec<MirValueId>,
    pub(crate) spill_slots: Vec<MirSpillSlot>,
    pub(crate) frame_slots: Vec<MirFrameSlot>,
}

/// A symbolic slot in a continuation's typed spill layout. The slot index is
/// stable within the continuation; byte offsets are assigned by codegen.
#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MirSpillSlot {
    pub(crate) value: MirValueId,
    pub(crate) slot: usize,
    pub(crate) ownership: MirOwnership,
}

/// A local that must survive a suspension boundary. Byte offsets are assigned
/// by codegen from the stable slot order.
#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MirFrameSlot {
    pub(crate) local: MirLocalId,
    pub(crate) slot: usize,
    /// The SSA value that initializes this local before suspension, when the
    /// local has an explicit MIR binding. Function parameters and receiver
    /// locals may remain `None` until entry-value materialization is lowered.
    pub(crate) value: Option<MirValueId>,
    pub(crate) ty: Type,
    pub(crate) ownership: MirOwnership,
}

#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub(crate) struct MirParameter {
    pub(crate) local: MirLocalId,
    pub(crate) name: String,
    pub(crate) ty: Type,
    pub(crate) ownership: MirOwnership,
}

#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub(crate) struct MirLocal {
    pub(crate) id: MirLocalId,
    pub(crate) name: String,
    pub(crate) ty: Type,
    pub(crate) ownership: MirOwnership,
    pub(crate) scope_depth: usize,
}

#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct MirLocalId(pub(crate) usize);

#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct MirBlockId(pub(crate) usize);

#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct MirValueId(pub(crate) usize);

/// Stable identity for a suspension point within a function.
#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct MirContinuationId(pub(crate) usize);

/// The runtime protocol associated with a continuation point.
#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MirContinuationKind {
    /// A synchronous normal-effect request. The request is serviced by the
    /// active handler frame and then resumes at the next MIR statement.
    Normal,
    Suspending,
    /// Internal scheduler wait; never dispatched through an effect handler.
    TaskWait,
    /// Acquire an entire Cown set without retaining a worker stack while waiting.
    CownAcquire,
    Resumable,
}

impl MirContinuation {
    /// Direct and indirect function calls have no effect operation. Internal
    /// task waits use their own kind even though they also have no operation.
    pub(crate) fn is_function_call(&self) -> bool {
        self.kind == MirContinuationKind::Suspending && self.operation.is_none()
    }
}

impl MirContinuationKind {
    pub(crate) fn is_suspending(self) -> bool {
        matches!(self, Self::Suspending | Self::TaskWait | Self::CownAcquire)
    }

    pub(crate) fn is_normal(self) -> bool {
        matches!(self, Self::Normal)
    }

    pub(crate) fn uses_heap_storage(self) -> bool {
        !self.is_normal()
    }

    /// The single mapping from continuation kind to the effect operations that
    /// may be serviced under it. Both the request verifier and the continuation
    /// metadata verifier must agree through this method.
    pub(crate) fn matches_operation(self, mode: crate::sema::EffectMode, suspends: bool) -> bool {
        match self {
            Self::Normal => mode == crate::sema::EffectMode::Normal,
            Self::Suspending => suspends,
            Self::TaskWait | Self::CownAcquire => false,
            Self::Resumable => mode == crate::sema::EffectMode::Resumable,
        }
    }
}

/// The four control protocols understood by MIR and the backend.
/// Requests are classified separately from statement variants so verifier and
/// codegen passes do not have to infer control flow from operation flags.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum MirControlProtocol {
    Return,
    Abort,
    Suspend,
    Resume,
}

#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct MirScopeId(pub(crate) usize);

#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct MirTaskId(pub(crate) usize);

#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MirOwnership {
    Copy,
    Owned,
    Shared,
    Borrowed,
}

pub(crate) fn ownership_for_type(ty: Type, types: &TypeTable) -> MirOwnership {
    match ty {
        _ if types.is_owned(ty) => MirOwnership::Owned,
        _ if types.is_shared(ty) => MirOwnership::Shared,
        _ => MirOwnership::Copy,
    }
}

#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub(crate) struct MirBlock {
    pub(crate) id: MirBlockId,
    pub(crate) scoped: bool,
    pub(crate) scope_depth: usize,
    pub(crate) statements: Vec<MirStatement>,
    pub(crate) terminator: Option<MirTerminator>,
}

/// A statically lowered normal-effect handler arm.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub(crate) struct MirHandlerArm {
    pub(crate) operation: crate::sema::EffectOperationId,
    pub(crate) target: MirBlockId,
    /// Operation parameter indices that are bound to handler locals. Wildcards
    /// are intentionally omitted from this mapping.
    pub(crate) parameter_indices: Vec<usize>,
    pub(crate) parameter_locals: Vec<MirLocalId>,
    pub(crate) join: MirBlockId,
    pub(crate) result_type: Type,
    /// Payload for the first runtime resumable slice. `None` means this arm
    /// still uses the legacy inline lowering path.
    pub(crate) resumable_value: Option<MirConstant>,
    /// A restricted trampoline form that forwards one copyable operation
    /// parameter as the resume result. Complex handler bodies remain on the
    /// direct MIR path until synthetic handler functions are available.
    pub(crate) resumable_parameter: Option<usize>,
    /// A small scalar transformation handled by the runtime callback. This is
    /// the first dynamic handler-body slice; larger bodies will use synthetic
    /// MIR functions.
    pub(crate) resumable_transform: Option<MirResumableTransform>,
    /// A compiler-generated, capture-free handler body with the normal MIR
    /// function ABI. The runtime callback invokes this function for the first
    /// dynamic-body slice.
    pub(crate) resumable_function: Option<MirFunctionId>,
    /// Immutable scalar captures passed to a synthetic handler in the same
    /// order as the synthetic function's trailing parameters.
    pub(crate) resumable_captures: Vec<MirValueId>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy)]
pub(crate) struct MirResumableTransform {
    pub(crate) parameter: usize,
    pub(crate) operator: BinaryOp,
    pub(crate) immediate: i64,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NumericMethod {
    Abs,
    Min,
    Max,
    To,
}

#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub(crate) enum MirStatement {
    Unit {
        destination: MirValueId,
    },
    Const {
        destination: MirValueId,
        value: MirConstant,
    },
    Read {
        destination: MirValueId,
        local: MirLocalId,
    },
    BorrowLocal {
        destination: MirValueId,
        local: MirLocalId,
    },
    TakeLocal {
        destination: MirValueId,
        local: MirLocalId,
    },
    Unary {
        destination: MirValueId,
        op: UnaryOp,
        operand: MirValueId,
    },
    Binary {
        destination: MirValueId,
        op: BinaryOp,
        left: MirValueId,
        right: MirValueId,
    },
    Numeric {
        destination: MirValueId,
        method: NumericMethod,
        arguments: Vec<MirValueId>,
    },
    Call {
        destination: MirValueId,
        function: MirFunctionId,
        arguments: Vec<MirCallArgument>,
        /// Function-call boundary, populated by Pending call materialization.
        continuation: Option<MirContinuationId>,
    },
    FunctionValue {
        destination: MirValueId,
        function: MirFunctionId,
        captures: Vec<MirValueId>,
    },
    DynamicValue {
        destination: MirValueId,
        methods: Vec<MirFunctionId>,
        value: MirValueId,
    },
    DynamicUpcast {
        destination: MirValueId,
        value: MirValueId,
    },
    CallIndirect {
        destination: MirValueId,
        callee: MirValueId,
        arguments: Vec<MirCallArgument>,
        /// Static function-type effect information for continuation lowering.
        may_suspend: bool,
        /// Function-call boundary, populated by continuation materialization.
        continuation: Option<MirContinuationId>,
    },
    Project {
        destination: MirValueId,
        base: MirValueId,
        access: MirFieldAccess,
    },
    EnumTag {
        destination: MirValueId,
        value: MirValueId,
    },
    EnumProject {
        destination: MirValueId,
        value: MirValueId,
        variant: usize,
        field: usize,
    },
    Tuple {
        destination: MirValueId,
        elements: Vec<MirValueId>,
    },
    EnumConstruct {
        destination: MirValueId,
        enum_id: MirTypeId,
        variant: usize,
        arguments: Vec<MirCallArgument>,
    },
    Construct {
        destination: MirValueId,
        type_id: MirTypeId,
        fields: Vec<MirValueId>,
    },
    Store {
        destination: MirValueId,
        receiver: MirValueId,
        access: MirFieldAccess,
        value: MirValueId,
    },
    MethodCall {
        destination: MirValueId,
        receiver: MirValueId,
        receiver_type: Type,
        method: MirFunctionId,
        arguments: Vec<MirCallArgument>,
        /// Function-call boundary, populated by Pending call materialization.
        continuation: Option<MirContinuationId>,
    },
    RuntimeCall {
        destination: MirValueId,
        intrinsic: RuntimeIntrinsic,
        arguments: Vec<MirCallArgument>,
    },
    /// A resumable effect request that transfers control to a runtime handler
    /// and later returns through the associated continuation. This is kept
    /// distinct from abortive/dynamic operations so the verifier and
    /// backend cannot silently discard the continuation identity.
    ResumableRequest {
        destination: MirValueId,
        operation: crate::sema::EffectOperationId,
        continuation: MirContinuationId,
        arguments: Vec<MirCallArgument>,
    },
    /// A normal effect request dispatched through the active runtime handler
    /// frame. It carries a continuation identity and
    /// is expected to return a typed payload synchronously.
    HandlerRequest {
        destination: MirValueId,
        operation: crate::sema::EffectOperationId,
        continuation: MirContinuationId,
        arguments: Vec<MirCallArgument>,
    },
    /// A statically identified suspending operation and its function-local
    /// continuation identity. Typed spill lowering will populate the metadata
    /// table's live values in a later pass.
    Suspend {
        destination: MirValueId,
        operation: crate::sema::EffectOperationId,
        continuation: MirContinuationId,
        arguments: Vec<MirCallArgument>,
    },
    /// Marker emitted at the continuation's resume block. It carries no
    /// runtime value yet while the backend still uses a synchronous timer.
    Resume {
        continuation: MirContinuationId,
    },
    /// A cooperative cancellation checkpoint inside a generated task body.
    TaskPoll {
        destination: MirValueId,
    },
    /// A compiler-generated zero result used only by a cancelled task path.
    TaskCancelled {
        destination: MirValueId,
    },
    /// A non-resumable effect escaped a task body. The task runtime records
    /// the operation and cancels sibling tasks before this task returns.
    TaskAbort {
        destination: MirValueId,
        operation: crate::sema::EffectOperationId,
        arguments: Vec<MirValueId>,
    },
    /// Reads the first abort operation recorded by a task scope.
    TaskFailureOperation {
        destination: MirValueId,
        scope: MirScopeId,
    },
    /// Loads one typed parameter from a scope-owned task failure payload.
    TaskFailurePayload {
        destination: MirValueId,
        scope: MirScopeId,
        operation: crate::sema::EffectOperationId,
        parameter: usize,
    },
    /// Marks a scope failure payload as transferred into typed handler values.
    TaskFailureClaim {
        scope: MirScopeId,
    },
    /// Propagates a nested scope's failure into the current task's parent
    /// group and produces only a cancellation placeholder return value.
    TaskFailureRethrow {
        destination: MirValueId,
        scope: MirScopeId,
    },
    HandlerEnter {
        handlers: Vec<MirHandlerArm>,
    },
    HandlerExit,
    ScopeEnter {
        scope: MirScopeId,
        region: bool,
    },
    ScopeExit {
        scope: MirScopeId,
    },
    TaskCreate {
        scope: MirScopeId,
        task: MirTaskId,
        function: MirFunctionId,
        arguments: Vec<MirCallArgument>,
    },
    /// Acquire all handles before entering the resume block's lease body.
    CownAcquire {
        /// Atomically park a false guard, release its leases, then reacquire on change.
        wait_for_change: bool,
        destination: MirValueId,
        arguments: Vec<MirCallArgument>,
        continuation: MirContinuationId,
    },
    /// Wait for a structured group to drain (or select and drain a race).
    TaskWait {
        destination: MirValueId,
        scope: MirScopeId,
        race: bool,
        continuation: MirContinuationId,
    },
    TaskJoin {
        destination: MirValueId,
        scope: MirScopeId,
        task: MirTaskId,
    },
    /// Transfers a joined task result from scope storage to its MIR value.
    TaskClaimResult {
        scope: MirScopeId,
        task: MirTaskId,
    },
    TaskCancel {
        scope: MirScopeId,
        task: MirTaskId,
    },
    RaceStart {
        scope: MirScopeId,
        tasks: Vec<MirTaskId>,
    },
    RaceSelect {
        destination: MirValueId,
        scope: MirScopeId,
        tasks: Vec<MirTaskId>,
    },
    Dup {
        destination: MirValueId,
        value: MirValueId,
    },
    Move {
        destination: MirValueId,
        value: MirValueId,
    },
    Drop {
        destination: MirValueId,
        value: MirValueId,
    },
    Deinit {
        destination: MirValueId,
        value: MirValueId,
        variant: Option<usize>,
    },
    DropLocal {
        destination: MirValueId,
        local: MirLocalId,
    },
    Bind {
        local: MirLocalId,
        value: Option<MirValueId>,
        destination: MirValueId,
    },
    Phi {
        destination: MirValueId,
        incoming: Vec<(MirBlockId, MirValueId)>,
    },
}

#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub(crate) struct MirCallArgument {
    pub(crate) parameter: usize,
    pub(crate) value: MirValueId,
}

impl MirStatement {
    pub(crate) fn destination(&self) -> Option<MirValueId> {
        lower::statement_destination(self)
    }

    /// Return the control protocol introduced by this statement, if any.
    pub(crate) fn control_protocol(&self) -> Option<MirControlProtocol> {
        match self {
            Self::TaskAbort { .. }
            | Self::TaskCancelled { .. }
            | Self::TaskFailureRethrow { .. } => Some(MirControlProtocol::Abort),
            Self::Suspend { .. } | Self::TaskWait { .. } | Self::CownAcquire { .. } => {
                Some(MirControlProtocol::Suspend)
            }
            Self::Resume { .. } => Some(MirControlProtocol::Resume),
            _ => None,
        }
    }

    /// Continuation protocol required by an operation request statement.
    pub(crate) fn request_continuation_kind(&self) -> Option<MirContinuationKind> {
        match self {
            Self::HandlerRequest { .. } => Some(MirContinuationKind::Normal),
            Self::ResumableRequest { .. } => Some(MirContinuationKind::Resumable),
            Self::Suspend { .. } => Some(MirContinuationKind::Suspending),
            _ => None,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeIntrinsic {
    HasherNew,
    HasherFinish,
    Hash(Type),
    /// Inline native null address; never allocates or calls the runtime.
    CPointerNull(Type),
    CPointerIsNull,
    BatchNew(Type),
    BatchWorker,
    BatchNext(Type),
    BatchPush(Type),
    BatchFinish(Type),
    SeqNew,
    SeqPush(Type),
    SeqFinish(Type),
    Panic,
    CownNew(Type),
    #[allow(dead_code)]
    CownAcquire(Type),
    CownAcquireMany,
    CownPayload(Type),
    CownRelease,
    BytesNew,
    BytesFromString,
    BytesIsEmpty,
    BytesLength,
    BytesGet,
    BytesSlice,
    BytesConcat,
    BytesToString,
    BytesCursorNew,
    BytesCursorAdvance,
    MutBytesNew,
    MutBytesWithCapacity,
    MutBytesFromBytes,
    MutBytesLength,
    MutBytesCapacity,
    MutBytesPush,
    MutBytesGet,
    MutBytesSet,
    MutBytesPop,
    MutBytesClear,
    MutBytesReserve,
    MutBytesToBytes,
    MutBytesExtend,
    ListEmpty(Type),
    ListCons(Type),
    ListIsEmpty,
    ListHead(Type),
    ListTail(Type),
    ListLength,
    ListReverse(Type),
    MutListNew(Type),
    MutListPush(Type),
    MutListGet(Type),
    MutListSet(Type),
    MutListPop(Type),
    MutListLength,
    MutListCapacity,
    MutListIntoIter(Type),
    MutListCursorAdvance(Type),
    MutListToList(Type),
    MutMapNew(Type, Type),
    MutMapInsert(Type, Type),
    MutMapGet(Type, Type),
    MutMapRemove(Type, Type),
    MutMapContainsKey(Type),
    MutMapLength,
    MutMapCapacity,
    MutMapIsEmpty,
    MutMapIntoIter(Type, Type),
    MutMapCursorAdvance(Type, Type),
    MutMapToList(Type, Type),
    MutSetNew(Type),
    MutSetAdd(Type),
    MutSetRemove(Type),
    MutSetContains(Type),
    MutSetLength,
    MutSetCapacity,
    MutSetIsEmpty,
    MutSetIntoIter(Type),
    MutSetCursorAdvance(Type),
    MutSetToList(Type),
    MapEmpty(Type, Type),
    MapInsert(Type, Type),
    MapGet(Type, Type),
    MapRemove(Type, Type),
    MapContainsKey(Type),
    MapEntriesCursorNew(Type, Type),
    MapKeysCursorNew(Type, Type),
    MapValuesCursorNew(Type, Type),
    MapCursorAdvance(Type, Type),
    MapLength,
    MapIsEmpty,
    Show(Type),
    DebugString,
    DebugBytes,
    DebugDuration,
    DebugNativeId(Type),
    DebugPath(Type),
    DebugPathStatus,
    Echo,
    Print,
    Println,
    StringConcat,
    PartialEqual(Type),
    PartialCompare(Type),
    Compare(Type),
    StringEqual,
    StringNotEqual,
    StringIsEmpty,
    StringByteCount,
    StringStartsWith,
    StringEndsWith,
    StringContains,
    StringScalarCount,
    StringGraphemeCount,
    StringIsAscii,
    StringTrim,
    StringToUpper,
    StringToLower,
    StringSplit,
    Path(u8),
    StringReplace,
    StringGetByte,
    StringSlice,
    StringParse(Type),
    /// Lend a String payload as a NUL-terminated `CStr`; borrows the
    /// managed object and never copies or allocates.
    StringAsCString,
    /// Copy a NUL-terminated `CStr` into a fresh managed String; `None` for a
    /// null pointer or non-UTF-8 content.
    CStrAsString,
    /// Allocate a C-heap cell and store the initial value; the payload is the
    /// cell content type.
    CCellAlloc(Type),
    CCallbackNew(Type),
    CCallbackFunction,
    CCallbackContext,
    CCallbackClose,
    CCallbackFailed,
    /// Load the typed content of a C-heap cell.
    CCellRead(Type),
    /// Store a typed value into a C-heap cell.
    CCellWrite(Type),
    /// Release a C-heap cell; ownership is the caller's contract.
    CCellFree,
}

#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub(crate) enum MirConstant {
    /// Relocatable task-failure discriminator; encode only after linking.
    EffectOperation(crate::sema::EffectOperationId),
    Integer(u64),
    Float(f64),
    String(String),
    Boolean(bool),
    /// A constant `Option` payload used by the runtime resumable handler
    /// path. `None` carries no nested constant; the target type supplies the
    /// inactive payload's ABI shape during code generation.
    Option(Option<Box<MirConstant>>),
    /// A constant `Result` payload. The inactive side is materialized from
    /// the target type during code generation, just like an enum constructor.
    Result {
        is_ok: bool,
        value: Box<MirConstant>,
    },
    Tuple(Vec<MirConstant>),
    Struct(Vec<(String, MirConstant)>),
    Class(Vec<(String, MirConstant)>),
    /// Persistent collection literals used as resumable handler payloads.
    List(Vec<MirConstant>),
    Map(Vec<(MirConstant, MirConstant)>),
    /// Uniquely owned mutable collection literals used as resumable payloads.
    MutList(Vec<MirConstant>),
    MutMap(Vec<(MirConstant, MirConstant)>),
    MutSet(Vec<MirConstant>),
    Enum {
        variant: usize,
        fields: Vec<MirConstant>,
    },
}

#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub(crate) enum MirTerminator {
    Goto {
        target: MirBlockId,
        arguments: Vec<MirValueId>,
    },
    Branch {
        condition: MirValueId,
        then_block: MirBlockId,
        else_block: MirBlockId,
    },
    Return(Option<MirValueId>),
    Unreachable,
}

impl MirTerminator {
    #[allow(dead_code)]
    pub(crate) fn control_protocol(&self) -> Option<MirControlProtocol> {
        match self {
            Self::Return(_) => Some(MirControlProtocol::Return),
            Self::Goto { .. } | Self::Branch { .. } | Self::Unreachable => None,
        }
    }
}

impl MirProgram {
    pub(crate) fn lower(core: &CoreProgram) -> Result<Self, Diagnostic> {
        lower::lower_program(core)
    }

    pub(crate) fn verify(&self) -> Result<(), Diagnostic> {
        verifier::verify(self)
    }

    pub(crate) fn functions(&self) -> &[MirFunction] {
        &self.functions
    }

    pub(crate) fn types(&self) -> &TypeTable {
        &self.types
    }

    /// Drop functions that cannot run in this linked program.
    ///
    /// Only call this after modules are linked. Per-module artifacts must keep
    /// unused exports so importers can still call them.
    pub(crate) fn retain_reachable_functions(&mut self) {
        reachability::retain_reachable_functions(self);
    }

    /// Render the CFG in a compact, backend-independent form for diagnostics
    /// and lowering tests.
    #[allow(dead_code)]
    pub(crate) fn dump(&self) -> String {
        let mut output = self
            .functions
            .iter()
            .map(MirFunction::dump)
            .collect::<Vec<_>>()
            .join("\n\n");
        let capabilities = self.dump_continuation_capabilities();
        if !capabilities.is_empty() {
            output.push_str("\n\n; continuation capabilities\n");
            output.push_str(&capabilities);
        }
        output
    }
}
