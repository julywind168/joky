//! Typed slot IR with explicit control flow shared by all execution backends.
pub(crate) use crate::parser::ast::FloatKind;
use crate::parser::ast::{
    BinOp, CompilerIntrinsic, Expr, FnDef, Item, Literal, LogicalOp, Module as AstModule,
    ScopeKind, Type, UnaryOp,
};
use crate::{Error, ErrorCode, ErrorPhase, Span, TypeRegistry};
use std::{collections::HashMap, sync::Arc};

mod arenas;
mod arrays;
mod borrowing;
mod branches;
mod buffers;
mod fixed_arrays;
mod sequence_patterns;
pub(crate) use fixed_arrays::PlaceOp;
mod calls;
mod cleanup;
mod closures;
mod domain_schema;
mod domains;
mod when;
pub(crate) use cleanup::{CleanupOp, DropHook, ElementCleanup};
mod constants;
mod control_flow;
mod dyn_traits;
mod enums;
mod float;
mod function_values;
mod generics;
mod graph;
mod inference;
mod interpolation;
mod locals;
mod members;
mod memory;
mod patterns;
mod record_patterns;
mod records;
mod scripts;
mod text;
mod traits;
mod tuples;
use super::builtins::{Builtin, ExternAbi};
use super::integer::IntegerType;
use super::types::validate_nominal_type;
#[cfg(test)]
pub(crate) use buffers::storage_tests::{
    module as buffer_test_module, STORAGE as BUFFER_TEST_SOURCE,
};
pub(crate) use buffers::BufferOp;
use control_flow::unreachable_error;
use inference::integer_literal;

#[derive(Clone)]
pub(crate) enum Constant {
    NullPtr,
    Float32(f32),
    Float64(f64),
    Int8(i8),
    Int16(i16),
    Int32(i32),
    Int64(i64),
    UInt8(u8),
    UInt16(u16),
    UInt32(u32),
    Rune(u32),
    UInt64(u64),
    Bool(bool),
    String(String),
}

#[derive(Clone, Debug)]
pub(crate) struct FunctionInfo {
    pub closure_entry: bool,
    pub noescape: bool,
    pub noescape_params: Vec<usize>,
    /// Body-backed functions participate in borrow-effect inference.
    pub infer_noescape: bool,
    /// Parameter guarantees proved by the module fixed point, never source promises.
    pub inferred_noescape: Vec<usize>,
    /// None means unknown; Some lists parameters whose borrows may be stored.
    pub inferred_retained: Option<Vec<usize>>,
    /// Stored source parameter and destination explicit-arena parameter pairs.
    pub inferred_arena_retained: Vec<(usize, usize)>,
    /// A returned reference may point into scoped or relocatable aggregate storage.
    pub returns_interior: bool,
    pub compiler_intrinsic: Option<CompilerIntrinsic>,
    pub builtin: Option<Builtin>,
    pub extern_abi: ExternAbi,
    pub signature: crate::FunctionSignature,
    pub suspends: bool,
    /// Arithmetic/allocation failures or calls propagating a native runtime error.
    pub may_fail: bool,
    /// May allocate: preserves the existing ordinary-reference storage contract.
    pub uses_arena: bool,
    /// Needs task storage context, including deallocation without allocation.
    pub needs_arena: bool,
    pub frame_slots: usize,
    pub internal: bool,
    /// Compiler-checked persistent-root entry, independent of the public ABI.
    pub instance_entry: bool,
    /// Proved synchronous, host-free single-domain or scalar pair entry.
    pub domain_entry: bool,
    pub span: Span,
}

impl FunctionInfo {
    /// Conservative contract for calls whose implementation is selected at runtime.
    pub(crate) fn indirect(
        signature: crate::FunctionSignature,
        suspends: bool,
        span: Span,
    ) -> Self {
        Self {
            closure_entry: false,
            noescape: false,
            noescape_params: Vec::new(),
            infer_noescape: false,
            inferred_noescape: Vec::new(),
            inferred_retained: None,
            inferred_arena_retained: Vec::new(),
            returns_interior: true,
            compiler_intrinsic: None,
            signature,
            builtin: None,
            extern_abi: ExternAbi::Scalar,
            suspends,
            may_fail: true,
            uses_arena: true,
            needs_arena: true,
            frame_slots: 0,
            internal: true,
            instance_entry: false,
            domain_entry: false,
            span,
        }
    }

    pub(crate) fn parameter_noescape(&self, index: usize) -> bool {
        self.noescape
            || self.noescape_params.contains(&index)
            || self.inferred_noescape.contains(&index)
    }

    pub(crate) fn parameter_retained(&self, index: usize) -> bool {
        !self.parameter_noescape(index)
            && self
                .inferred_retained
                .as_ref()
                .is_none_or(|params| params.contains(&index))
    }

    fn can_borrow_argument(&self, index: usize) -> bool {
        !self.suspends && (self.infer_noescape || self.parameter_noescape(index))
    }

    pub(crate) fn parameter_slots(&self) -> usize {
        self.signature
            .params
            .iter()
            .map(Type::parameter_slots)
            .sum()
    }
    pub(crate) fn aggregate_result_offset(&self) -> usize {
        self.parameter_slots()
    }
}

#[derive(Clone)]
pub(crate) struct Slot {
    pub(crate) index: usize,
    pub(crate) ty: Type,
}

impl Slot {
    pub(crate) fn owner_key(&self) -> usize {
        self.scalars()
            .find(|s| s.ty.is_buffer())
            .expect("owning value")
            .index
    }

    pub(crate) fn field_path(&self, path: &[String]) -> Self {
        let mut result = self.clone();
        for name in path {
            let (offset, ty) = result.ty.inline_field(name).expect("checked field path");
            result.index += offset;
            result.ty = ty.clone();
        }
        result
    }

    /// Buffer ordinals within a cleanup group, independent of metadata lanes.
    pub(crate) fn owner_range(&self, field: &Slot) -> (usize, usize) {
        let first = self
            .scalars()
            .filter(|s| s.ty.is_buffer() && s.index < field.index)
            .count();
        let count = field.ty.buffer_elements().len();
        (first, count)
    }

    pub(crate) fn scalars(&self) -> impl Iterator<Item = Slot> + '_ {
        self.ty
            .value_lanes()
            .iter()
            .enumerate()
            .map(|(offset, ty)| Slot {
                index: self.index + offset,
                ty: ty.clone(),
            })
    }
}

#[derive(Clone)]
pub(crate) enum TextSource {
    Literal(String),
    Clone(Slot),
    /// Locale-independent shortest-round-trip formatting of a float scalar.
    Float(Slot),
}

#[derive(Clone)]
struct Binding {
    slot: Option<Slot>,
    mutable: bool,
    place: Option<Slot>,
}

pub(crate) struct ScopeBranch {
    /// Join accepts each result independently before polling another child.
    pub(crate) result: Option<Slot>,
    /// Register owning results atomically with acceptance, including partial joins.
    pub(crate) cleanup: Option<CleanupOp>,
    pub(crate) indirect: Option<Slot>,
    pub(crate) name: String,
    pub(crate) args: Vec<Option<Slot>>,
    pub(crate) done: Slot,
}

pub(crate) enum Op {
    Place(PlaceOp),
    /// A uniquely owned explicit arena, released by ordinary owner cleanup.
    ArenaNew {
        dst: Slot,
    },
    /// Fresh stable Copy storage tied to a checked explicit arena borrow.
    ArenaAlloc {
        dst: Slot,
        arena: Slot,
        value: Option<Slot>,
        target: Type,
    },
    /// Static closure move/call responsibility, erased after checking.
    Closure(Box<closures::ClosureOp>),
    /// Copy validated UTF-8 into a fresh task-owned buffer; this operation may fail.
    Text {
        dst: Slot,
        source: TextSource,
    },
    Assert {
        condition: Slot,
        message: Slot,
    },
    /// Report failure and leave through cleanup; there is no normal successor.
    Panic {
        message: Slot,
    },
    Cleanup(CleanupOp),
    /// Explicit non-Copy ownership and temporary control-block borrowing.
    Buffer(BufferOp),
    /// Checked construction capabilities, erased only after reference analysis.
    Graph(Box<graph::GraphOp>),
    /// Copy a value, including snapshots of mutable variable reads.
    Copy(Slot, Slot),
    /// Reference move or checked reborrow. No ownership crosses a host boundary.
    Reference {
        dst: Slot,
        src: Slot,
        moving: bool,
    },
    Reserve {
        dst: Slot,
        src: Slot,
    },
    /// Address and checked loan for an inline field of local storage.
    Project {
        dst: Slot,
        src: Slot,
        path: Vec<String>,
        reservation: bool,
    },
    /// Projection into a checked enum/tuple pattern. Byte offsets address storage;
    /// structural paths prove sibling bindings disjoint while retaining root loans.
    PatternProject {
        dst: Slot,
        src: Slot,
        offset: usize,
        path: Vec<usize>,
    },
    /// Function-owned storage, distinct from the task arena.
    Local {
        dst: Slot,
        value: Option<Slot>,
        target: Type,
    },
    /// Lexical lifetime marker, erased after reference analysis.
    EndLocal(Slot),
    Alloc {
        dst: Slot,
        value: Option<Slot>,
        target: Type,
    },
    ReadMemory {
        dst: Option<Slot>,
        reference: Slot,
        path: Vec<String>,
    },
    WriteMemory {
        reference: Slot,
        path: Vec<String>,
        value: Option<Slot>,
        target: Type,
    },
    /// Move a record/owning field out of compiler-private storage. The owner
    /// slot carries the unique capability; the storage contains its physical bits.
    ReadOwner {
        dst: Option<Slot>,
        reference: Slot,
        path: Vec<String>,
        owner: Slot,
        ownership_path: Vec<String>,
        detach: bool,
    },
    /// Synchronize private storage after moving a new value into its owner slot.
    StoreOwner {
        reference: Slot,
        path: Vec<String>,
        value: Option<Slot>,
        target: Type,
    },
    Label(usize),
    Jump(usize),
    Branch {
        condition: Slot,
        then_label: usize,
        else_label: usize,
    },
    Literal(Slot, Constant),
    FunctionRef(Slot, String),
    DomainModule(Slot),
    /// Trusted scalar slot encoding; never accepts references or owned values.
    DomainBits {
        dst: Slot,
        src: Slot,
        decode: bool,
    },
    /// Immutable concrete method entries in interface declaration order.
    DynTable {
        dst: Slot,
        table: Arc<DynTable>,
    },
    /// Load one typed entry; the data borrow remains an explicit call argument.
    DynMethod {
        dst: Slot,
        table: Slot,
        method: usize,
    },
    /// Deliver a branch result before ScopeFinish cancels remaining children.
    Scope {
        result: Option<Slot>,
        kind: ScopeKind,
        branches: Vec<ScopeBranch>,
        state: u32,
    },
    /// Run after result ownership registration, before any source continuation.
    ScopeFinish,
    Binary(Slot, BinOp, Slot, Slot),
    Unary(Slot, UnaryOp, Slot),
    /// Wrapping integer conversion or already-range-checked float truncation;
    /// slots retain source and target types.
    Convert(Slot, Slot),
    /// IEEE-exact unary float primitives shared by every backend.
    Float {
        dst: Slot,
        src: Slot,
        kind: FloatKind,
    },
    Call {
        /// A typed callee slot selects an indirect language call.
        indirect: Option<Slot>,
        name: String,
        args: Vec<Option<Slot>>,
        result: Option<Slot>,
        state: Option<u32>,
    },
    /// Open a sequential task scope; child tasks register until its join.
    TaskScopeEnter,
    /// Register one child task entry with captured inputs. Registration is
    /// the value snapshot point; capacity limits surface as task failures.
    TaskBranch {
        scope: u32,
        entry: String,
        captures: Vec<Option<Slot>>,
    },
    /// Suspend until every child task registered under `scope` has finished.
    /// Each join site owns one resume state.
    TaskJoin {
        scope: u32,
        state: u32,
    },
}

/// Normal completion (including Unit) is distinct from a control-flow exit.
enum Flow {
    Value(Option<Slot>),
    Diverged,
}

struct LoopTargets {
    header: usize,
    exit: usize,
    has_break: bool,
    local_depth: usize,
    /// Task scopes open when the loop was entered; breaks crossing scopes
    /// opened inside the loop route through their joins first.
    scope_depth: usize,
}

/// A compilation-local table has no ownership or host ABI entries.
pub(crate) struct DynTable {
    pub id: usize,
    pub self_type: Type,
    pub interface: Arc<crate::parser::ast::DynInterface>,
    pub methods: Vec<String>,
}

/// Erasure keeps the original capability and source location through borrow checking.
pub(crate) struct DynErasure {
    pub source: Slot,
    pub target: Slot,
    pub span: Span,
}

pub(crate) struct FunctionPlan {
    pub dyn_erasures: Vec<DynErasure>,
    source_function: String,
    suspending: bool,
    when_function: String,
    callable_modes: HashMap<usize, crate::parser::ast::CallableKind>,
    captured_owners: std::collections::HashSet<usize>,
    captured_fields: HashMap<usize, (Slot, Vec<String>)>,
    closure_places: HashMap<usize, Slot>,
    consuming_captures: bool,
    pub(crate) is_destructor: bool,
    unsafe_context: bool,
    pub(crate) ops: Vec<Op>,
    pub(crate) op_spans: Vec<Span>,
    current_span: Span,
    return_type: Type,
    scope: Arc<super::imports::ImportScope>,
    definition_scope: Option<Arc<super::imports::DefinitionScope>>,
    generics: std::rc::Rc<std::cell::RefCell<generics::GenericContext>>,
    instantiation_depth: usize,
    nominals: Arc<HashMap<String, Type>>,
    type_registry: TypeRegistry,
    return_label: Option<usize>,
    loops: Vec<LoopTargets>,
    graph_loop_depth: Option<usize>,
    graph_handles: std::collections::HashSet<usize>,
    borrowed_names: locals::BorrowedLocals,
    local_scopes: Vec<Vec<Slot>>,
    argument_scopes: Vec<std::collections::HashSet<usize>>,
    /// Implicit conversions must have a proof after all bodies have been analyzed.
    implicit_borrows: Vec<(String, usize, Span)>,
    pub(crate) domain_calls: Vec<(String, Span)>,
    task_scopes: Vec<branches::TaskScopeFrame>,
    task_scope_count: u32,
    /// Branch entry points planned for this function; the module driver
    /// registers them before capability and reference analysis.
    pub(crate) nested: Vec<NestedTask>,
    pub(crate) result: Option<Slot>,
    pub slot_count: usize,
    /// Stable native storage, indexed by a local destination or cleanup value slot.
    pub cps_local_slots: HashMap<usize, usize>,
    pub state_count: u32,
    pub label_count: usize,
    span: Span,
}

/// A planned branch entry point with its module-level function metadata.
pub(crate) struct NestedTask {
    pub(crate) name: String,
    pub(crate) info: FunctionInfo,
    pub(crate) plan: FunctionPlan,
}

pub(crate) struct ModulePlan {
    pub functions: HashMap<String, FunctionInfo>,
    pub bodies: HashMap<String, FunctionPlan>,
}

impl ModulePlan {
    pub fn with_import_layouts_and_types(
        module: &AstModule,
        imports: &HashMap<String, usize>,
        types: &TypeRegistry,
    ) -> Result<Self, Error> {
        let plan = Self::check_with_import_layouts_and_types(module, imports, types)?;
        plan.validate_dyn_tables()?;
        Ok(plan)
    }

    pub(crate) fn check_with_import_layouts_and_types(
        module: &AstModule,
        imports: &HashMap<String, usize>,
        types: &TypeRegistry,
    ) -> Result<Self, Error> {
        let scoped = types.compilation_scope().with_trait_environment(module);
        let _float_env = crate::runtime::float_env::Guard::enter();
        let types = &scoped;
        let nominals = Arc::new(super::types::nominal_types(types, module)?);
        let mut module = std::borrow::Cow::Borrowed(module);
        let members = std::rc::Rc::new(members::Members::collect(
            module.to_mut(),
            &nominals,
            types,
        )?);
        let module = module.as_ref();
        let mut functions = HashMap::new();
        for item in &module.items {
            let local = match item {
                Item::FnDef(f) => types.with_parameters(&f.type_params)?,
                Item::ExternFn(_) => types.clone(),
            };
            let (name, params, ret, suspends) = match item {
                Item::FnDef(f) => (&f.name, &f.params, &f.ret_ty, f.suspends),
                Item::ExternFn(f) => (&f.name, &f.params, &f.ret_ty, f.suspends),
            };
            dyn_traits::check_signature(
                params,
                ret,
                suspends,
                matches!(item, Item::ExternFn(_)),
                *item_span(item),
            )?;
            let builtin = Builtin::lookup(name);
            let extern_abi = match item {
                Item::ExternFn(f) => ExternAbi::classify(f, types, builtin.as_ref()),
                Item::FnDef(_) => ExternAbi::Scalar,
            };
            let borrowing_host = extern_abi == ExternAbi::Borrowing;
            for ty in params.iter().map(|p| &p.ty).chain(std::iter::once(ret)) {
                validate_nominal_type(ty, &nominals, item_span(item))?;
                if matches!(item, Item::ExternFn(_)) && *ty == Type::Rune {
                    return Err(type_error(
                        item_span(item),
                        ErrorCode::AbiError,
                        "Rune cannot cross an unchecked host ABI; use UInt32 and Rune.from_u32",
                    ));
                }
                if matches!(item, Item::ExternFn(_)) && ty.contains_functions() {
                    return Err(type_error(
                        item_span(item),
                        ErrorCode::AbiError,
                        "Language function values cannot cross a host ABI",
                    ));
                }
                if matches!(item, Item::ExternFn(_))
                    && ty.contains_buffers()
                    && !types.resource_factories.contains_key(name)
                    && builtin.is_none()
                    && !matches!(ty, Type::String)
                    && !(!suspends
                        && ty
                            .reference_type()
                            .is_some_and(|r| !r.mutable && r.target == Type::String))
                {
                    return Err(Error::new_with_code(
                        crate::ErrorPhase::Abi,
                        ErrorCode::AbiError,
                        "Buffer owners and views cannot cross a host ABI",
                    )
                    .with_span(*item_span(item)));
                }
                if matches!(item, Item::ExternFn(f) if !f.c_abi)
                    && !types.resource_io.contains_key(name)
                    && builtin.is_none()
                    && ((!borrowing_host
                        && ty.reference_type().is_some()
                        && !(!suspends
                            && ty
                                .reference_type()
                                .is_some_and(|r| !r.mutable && r.target == Type::String)))
                        || *ty == Type::Ptr)
                {
                    return Err(Error::new_with_code(
                        ErrorPhase::Abi,
                        ErrorCode::AbiError,
                        "References and Ptr require the explicit C ABI",
                    )
                    .with_span(*item_span(item)));
                }
                if matches!(item, Item::ExternFn(_))
                    && ty.is_aggregate()
                    && !types.resource_io.contains_key(name)
                    && builtin.is_none()
                {
                    return Err(Error::new_with_code(
                        ErrorPhase::Abi,
                        ErrorCode::AbiError,
                        "Aggregate parameters and returns are not supported by the host ABI",
                    )
                    .with_span(*item_span(item)));
                }
            }
            for param in params {
                local
                    .validate(&param.ty)
                    .map_err(|error| error.with_span(*item_span(item)))?;
            }
            local
                .validate(ret)
                .map_err(|error| error.with_span(*item_span(item)))?;
            if matches!(item, Item::ExternFn(_))
                && params
                    .iter()
                    .map(|p| &p.ty)
                    .chain(std::iter::once(ret))
                    .any(Type::contains_explicit_arena)
            {
                return Err(type_error(
                    item_span(item),
                    ErrorCode::InvalidCall,
                    "Arena capabilities cannot cross a host boundary",
                ));
            }
            let source_name = if matches!(item, Item::FnDef(f) if f.scope.is_some())
                && !members.functions.contains(name)
            {
                name.rsplit('.').next().unwrap()
            } else {
                name.as_str()
            };
            IntegerType::check_declaration(source_name, *item_span(item))?;
            if source_name.starts_with("__joky_dns_")
                || source_name.starts_with("joky_dns_")
                || source_name.starts_with("__joky_tcp_")
                || source_name.starts_with("__joky_vm_")
                || source_name.starts_with("__joky_resource_")
                || source_name.starts_with("joky_byte_request_")
                || source_name.starts_with("__joky_arena_")
                || source_name.starts_with("__joky_explicit_arena_")
                || source_name.starts_with("__joky_buffer_")
                || source_name.starts_with("__joky_place_")
                || source_name.starts_with("__joky_cps_")
                || source_name.starts_with("__joky_generic_")
                || source_name.starts_with("__joky_closure_")
                || source_name.starts_with("__joky_callable_")
                || source_name.starts_with("__joky_indirect_")
                || source_name.starts_with("__joky_runtime_")
                || source_name.starts_with("joky_runtime_")
                || source_name.starts_with("joky_cps_")
                || source_name.starts_with("joky_vm_")
                || source_name.starts_with("__joky_instance_")
                || source_name.starts_with("__joky_string_")
                || source_name.starts_with("__joky_user_text_v2_")
                || source_name.starts_with("joky_string_result_")
            {
                return Err(type_error(
                    item_span(item),
                    ErrorCode::ReservedName,
                    format!("Reserved runtime function name `{name}`"),
                ));
            }
            if let Some(builtin) = &builtin {
                builtin.validate_declaration(item)?;
            }
            let info = FunctionInfo {
                closure_entry: false,
                noescape: builtin.is_some()
                    || types.resource_io.contains_key(name)
                    || matches!(item, Item::FnDef(f) if f.noescape.as_ref().is_some_and(|effect| effect.all)),
                noescape_params: if let Item::FnDef(f) = item {
                    let mut indexes = Vec::new();
                    for name in f.noescape.iter().flat_map(|effect| &effect.params) {
                        let index =
                            params.iter().position(|p| &p.name == name).ok_or_else(|| {
                                type_error(
                                    item_span(item),
                                    ErrorCode::UnknownName,
                                    format!("Unknown @noescape parameter '{name}'"),
                                )
                            })?;
                        if indexes.contains(&index) {
                            return Err(type_error(
                                item_span(item),
                                ErrorCode::DuplicateDefinition,
                                "Duplicate @noescape parameter",
                            ));
                        }
                        indexes.push(index);
                    }
                    indexes
                } else {
                    Vec::new()
                },
                infer_noescape: matches!(item, Item::FnDef(_))
                    && !members.traits.requirements.contains(name),
                inferred_noescape: Vec::new(),
                inferred_retained: None,
                inferred_arena_retained: Vec::new(),
                returns_interior: true,
                compiler_intrinsic: match item {
                    Item::FnDef(f) => f.compiler_intrinsic.clone(),
                    _ => None,
                },
                extern_abi,
                instance_entry: false,
                domain_entry: false,
                internal: matches!(item, Item::FnDef(f) if f.buffer_intrinsic.is_some())
                    || members.functions.contains(name)
                    || matches!(item, Item::FnDef(f) if f.scope.is_some())
                    || module
                        .scope
                        .as_ref()
                        .is_some_and(|scope| scope.lazy_hosts.contains(name)),
                signature: crate::FunctionSignature::new(
                    params.iter().map(|p| p.ty.clone()).collect(),
                    ret.clone(),
                ),
                suspends,
                // Abstract requirements have no body and promise no storage purity.
                // Concrete instances are replanned against actual impl functions.
                may_fail: matches!(item, Item::ExternFn(_))
                    || members.traits.requirements.contains(name),
                uses_arena: builtin.as_ref().is_some_and(Builtin::needs_adapter)
                    || types.resource_factories.contains_key(name)
                    || types.text_ffi.contains_key(name)
                    || members.traits.requirements.contains(name)
                    || matches!(item, Item::ExternFn(_))
                        && params
                            .iter()
                            .map(|p| &p.ty)
                            .chain(std::iter::once(ret))
                            .any(|ty| {
                                *ty == Type::String
                                    || ty
                                        .reference_type()
                                        .is_some_and(|r| r.target == Type::String)
                            }),
                needs_arena: builtin.as_ref().is_some_and(Builtin::needs_adapter)
                    || types.resource_factories.contains_key(name)
                    || types.text_ffi.contains_key(name)
                    || members.traits.requirements.contains(name)
                    || matches!(item, Item::ExternFn(_))
                        && params
                            .iter()
                            .map(|p| &p.ty)
                            .chain(std::iter::once(ret))
                            .any(|ty| {
                                *ty == Type::String
                                    || ty
                                        .reference_type()
                                        .is_some_and(|r| r.target == Type::String)
                            }),
                frame_slots: params.iter().map(|p| p.ty.parameter_slots()).sum::<usize>()
                    + builtin.as_ref().map_or_else(
                        || {
                            usize::from(
                                extern_abi.byte_io()
                                    == Some(crate::runtime::byte_request::ByteIoKind::Read),
                            )
                        },
                        Builtin::result_slots,
                    ),
                builtin,
                span: *item_span(item),
            };
            if let Item::ExternFn(f) = item {
                extern_abi.validate(f, &info.signature, &nominals, types)?;
            }
            if functions.insert(name.clone(), info).is_some() {
                return Err(type_error(
                    item_span(item),
                    ErrorCode::DuplicateDefinition,
                    format!("Duplicate function `{name}`"),
                ));
            }
        }
        for (name, &count) in imports {
            let info = functions.get_mut(name).ok_or_else(|| {
                Error::new_with_code(
                    ErrorPhase::TypeCheck,
                    ErrorCode::InvalidCall,
                    format!("CPS import `{name}` is not declared"),
                )
            })?;
            if !info.suspends
                || info.extern_abi == ExternAbi::Borrowing
                || module
                    .items
                    .iter()
                    .any(|item| matches!(item, Item::FnDef(f) if &f.name == name))
            {
                return Err(type_error(
                    &info.span,
                    ErrorCode::InvalidCall,
                    format!("`{name}` is not a suspending import"),
                ));
            }
            if count < info.signature.params.len() || count >= i32::MAX as usize / 8 {
                return Err(type_error(
                    &info.span,
                    ErrorCode::InvalidCall,
                    format!("Invalid CPS frame size for `{name}`"),
                ));
            }
            info.frame_slots = count;
        }
        let generics = std::rc::Rc::new(std::cell::RefCell::new(generics::GenericContext::new(
            module,
        )));
        crate::core::source::validate_module_path(&module.logical_name)?;
        generics.borrow_mut().members = members.clone();
        when::validate_states(module, &functions, types, &nominals, generics.clone())?;
        constants::validate_all(&members, &functions, types, &nominals, generics.clone())?;
        let mut template_bodies = HashMap::new();
        // Check every template using rigid parameters, including unused templates.
        for item in &module.items {
            if let Item::FnDef(f) = item {
                if !f.type_params.is_empty() && !members.traits.requirements.contains(&f.name) {
                    let local = types.with_parameters(&f.type_params)?;
                    let plan =
                        FunctionPlan::new(f, &functions, &local, &nominals, generics.clone(), 0)?;
                    template_bodies.insert(f.name.clone(), plan);
                }
            }
        }
        let mut template_generated = std::mem::take(&mut generics.borrow_mut().generated_bodies);
        generics.borrow_mut().checking = false;
        let mut bodies = HashMap::new();
        for item in &module.items {
            if let Item::FnDef(f) = item {
                if f.type_params.is_empty() && !f.scope.as_ref().is_some_and(|s| s.lazy_functions) {
                    let plan =
                        FunctionPlan::new(f, &functions, types, &nominals, generics.clone(), 0)?;
                    bodies.insert(f.name.clone(), plan);
                }
            }
        }
        let mut next = 0;
        loop {
            // Branch bodies and drop hooks can be the only callers of lazy standard-library
            // functions. Publish them before computing the dependency closure,
            // including branches created by newly instantiated generic bodies.
            let mut tasks = std::mem::take(&mut generics.borrow_mut().generated_bodies);
            for body in bodies.values_mut() {
                take_nested_tasks(body, &mut tasks);
            }
            for NestedTask { name, info, plan } in tasks {
                functions.insert(name.clone(), info);
                bodies.insert(name, plan);
            }
            let mut library_dependencies = std::collections::BTreeSet::new();
            for body in bodies.values() {
                collect_plan_dependencies(body, &mut library_dependencies);
            }
            let mut added_library = false;
            for item in &module.items {
                if let Item::FnDef(f) = item {
                    if f.type_params.is_empty()
                        && f.scope.as_ref().is_some_and(|s| s.lazy_functions)
                        && !bodies.contains_key(&f.name)
                        && library_dependencies.contains(&f.name)
                    {
                        let plan = FunctionPlan::new(
                            f,
                            &functions,
                            types,
                            &nominals,
                            generics.clone(),
                            0,
                        )?;
                        bodies.insert(f.name.clone(), plan);
                        added_library = true;
                    }
                }
            }
            if added_library {
                continue;
            }
            let instance = generics.borrow().instances.get(next).cloned();
            let Some(instance) = instance else { break };
            next += 1;
            let definition = generics.borrow().definition(&instance, types)?;
            let mut info = functions[&instance.template].clone();
            info.signature = instance.signature.clone();
            info.compiler_intrinsic = definition.compiler_intrinsic.clone();
            info.frame_slots = info.parameter_slots();
            info.internal = true;
            functions.insert(instance.name.clone(), info);
            let plan = FunctionPlan::new(
                &definition,
                &functions,
                types,
                &nominals,
                generics.clone(),
                instance.depth,
            )?;
            bodies.insert(instance.name, plan);
        }
        // Prove calls in unused templates without emitting their lazy helper
        // closure or instantiating executable generic bodies just for checking.
        let mut template_helpers = std::collections::BTreeSet::new();
        generics.borrow_mut().checking = true;
        loop {
            let mut dependencies = std::collections::BTreeSet::new();
            for body in template_bodies.values() {
                collect_plan_dependencies(body, &mut dependencies);
            }
            let mut added = false;
            for item in &module.items {
                if let Item::FnDef(f) = item {
                    if f.type_params.is_empty()
                        && f.scope.as_ref().is_some_and(|scope| scope.lazy_functions)
                        && dependencies.contains(&f.name)
                        && !bodies.contains_key(&f.name)
                        && !template_bodies.contains_key(&f.name)
                    {
                        let plan = FunctionPlan::new(
                            f,
                            &functions,
                            types,
                            &nominals,
                            generics.clone(),
                            0,
                        )?;
                        template_bodies.insert(f.name.clone(), plan);
                        template_helpers.insert(f.name.clone());
                        added = true;
                    }
                }
            }
            if !added {
                break;
            }
        }
        generics.borrow_mut().checking = false;
        // Uninstantiated template branches also participate in capability and
        // reference analysis, then are removed together with their templates.
        let mut template_tasks: Vec<String> = Vec::new();
        let mut tasks = std::mem::take(&mut generics.borrow_mut().generated_bodies);
        tasks.append(&mut template_generated);
        template_tasks.extend(tasks.iter().map(|task| task.name.clone()));
        for plan in template_bodies.values_mut() {
            let mut taken = Vec::new();
            take_nested_tasks(plan, &mut taken);
            template_tasks.extend(taken.iter().map(|task| task.name.clone()));
            tasks.extend(taken);
        }
        for NestedTask {
            name,
            info,
            plan: body,
        } in tasks
        {
            functions.insert(name.clone(), info);
            bodies.insert(name, body);
        }
        for (index, signature) in generics.borrow().callables.iter().enumerate() {
            functions.insert(
                format!("__joky_indirect_{index}"),
                FunctionInfo::indirect(
                    signature.signature.clone(),
                    signature.suspends,
                    Span::empty(0),
                ),
            );
        }
        for (index, signature) in generics.borrow().abstract_callables.iter().enumerate() {
            let mut info = FunctionInfo::indirect(signature.clone(), false, Span::empty(0));
            info.noescape_params = vec![0];
            info.frame_slots = info.parameter_slots();
            functions.insert(format!("__joky_callable_{index}"), info);
        }
        for (index, slot) in generics.borrow().dyn_contracts.iter().enumerate() {
            let mut info = FunctionInfo::indirect(slot.signature.clone(), false, slot.span);
            info.noescape_params = slot.noescape_params.clone();
            info.frame_slots = info.parameter_slots();
            functions.insert(format!("__joky_indirect_dyn_{index}"), info);
        }
        // Check capability propagation through unused templates too. They are
        // removed only after the whole module has passed capability and reference checks.
        bodies.extend(template_bodies);
        for (name, body) in &bodies {
            functions.get_mut(name).unwrap().frame_slots = body.slot_count;
        }
        // A monotone fixed point handles recursion without changing pure loops.
        loop {
            let mut changed = false;
            for (name, body) in &bodies {
                if !functions[name].may_fail
                    && body.ops.iter().any(|op| {
                        op.cleanups().any(|cleanup| {
                            let CleanupOp::Register {
                                hooks, elements, ..
                            } = cleanup;
                            !elements.is_empty()
                                || hooks.iter().any(|hook| functions[&hook.name].may_fail)
                        }) || match op {
                            Op::Assert { .. }
                            | Op::Panic { .. }
                            | Op::Alloc { .. }
                            | Op::ArenaNew { .. }
                            | Op::ArenaAlloc { .. }
                            | Op::Text { .. }
                            | Op::Place(_) => true,
                            Op::Buffer(BufferOp::SwapReferences { target, .. })
                                if target.is_place() =>
                            {
                                true
                            }
                            Op::Buffer(BufferOp::Storage { intrinsic, .. }) => intrinsic.may_fail(),
                            Op::Binary(_, op, lhs, _) if op.can_fail() && !lhs.ty.is_float() => {
                                true
                            }
                            Op::Buffer(BufferOp::End(_)) => true,
                            Op::Call { name, .. } => functions[name].may_fail,
                            Op::Scope { branches, .. } => branches
                                .iter()
                                .any(|branch| functions[&branch.name].may_fail),
                            // Registration can fail on capacity limits; joins
                            // observe child failure and cancellation.
                            Op::TaskBranch { .. } | Op::TaskJoin { .. } => true,
                            _ => false,
                        }
                    })
                {
                    functions.get_mut(name).unwrap().may_fail = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        memory::check_module(&mut functions, &bodies)?;
        for body in bodies.values_mut() {
            body.lower_graph_ops();
            body.erase_local_ends();
            let mut spans = Vec::new();
            let mut ops = Vec::new();
            for (op, span) in body.ops.drain(..).zip(body.op_spans.drain(..)) {
                if !matches!(op, Op::Closure(_)) {
                    ops.push(op);
                    spans.push(span);
                }
            }
            body.ops = ops;
            body.op_spans = spans;
        }
        for name in generics.borrow().templates.keys() {
            functions.remove(name);
            bodies.remove(name);
        }
        for name in template_tasks.iter().chain(&template_helpers) {
            functions.remove(name);
            bodies.remove(name);
        }
        for item in &module.items {
            if let Item::FnDef(f) = item {
                if f.scope.as_ref().is_some_and(|s| s.lazy_functions)
                    && !bodies.contains_key(&f.name)
                {
                    functions.remove(&f.name);
                }
            }
        }
        if let Some(scope) = &module.scope {
            let used: std::collections::HashSet<_> = bodies
                .values()
                .flat_map(|body| &body.ops)
                .flat_map(|op| match op {
                    Op::Call { name, .. } | Op::FunctionRef(_, name) => vec![name],
                    Op::Scope { branches, .. } => {
                        branches.iter().map(|branch| &branch.name).collect()
                    }
                    Op::TaskBranch { entry, .. } => vec![entry],
                    _ => Vec::new(),
                })
                .collect();
            functions.retain(|name, _| !scope.lazy_hosts.contains(name) || used.contains(name));
        }
        for (name, body) in &mut bodies {
            let info = functions.get_mut(name).unwrap();
            if info.suspends {
                body.reserve_cps_locals()?;
            }
            info.frame_slots = body.slot_count;
        }
        Ok(Self { functions, bodies })
    }
}

impl FunctionPlan {
    fn source_nominals(&self) -> &HashMap<String, Type> {
        self.definition_scope
            .as_ref()
            .map_or(&self.nominals, |scope| &scope.nominals)
    }

    /// Lookup a source spelling without exposing canonical dependency names.
    fn named_type(&self, name: &str) -> Option<&Type> {
        if name.contains('.') && !self.scope.types.contains_key(name) {
            return None;
        }
        if let Some(ty) = self
            .scope
            .types
            .get(name)
            .and_then(|binding| binding.resolved.as_ref())
            .and_then(super::types::TypeDefinition::explicit_arena)
        {
            return Some(ty);
        }
        if self
            .scope
            .types
            .get(name)
            .is_some_and(|binding| binding.resolved.is_some())
        {
            return None;
        }
        self.source_nominals().get(name)
    }

    fn new(
        f: &FnDef,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
        nominals: &Arc<HashMap<String, Type>>,
        generics: std::rc::Rc<std::cell::RefCell<generics::GenericContext>>,
        instantiation_depth: usize,
    ) -> Result<Self, Error> {
        for ty in f
            .params
            .iter()
            .map(|p| &p.ty)
            .chain(std::iter::once(&f.ret_ty))
        {
            if ty.contains_fixed_array() && (!fixed_arrays::eligible(ty) || f.suspends) {
                return Err(type_error(
                    &f.span,
                    ErrorCode::TypeMismatch,
                    format!(
                        "FixedArray is not supported in this context: function `{}` uses {ty}",
                        f.name
                    ),
                ));
            }
        }
        when::validate_blocks(f)?;
        let prepared = super::json_derive::prepare_encoding(f);
        let f = prepared.as_ref();
        // Specialization and synthesized branch entries can expose dyn types
        // absent from the original generic declaration.
        dyn_traits::check_signature(&f.params, &f.ret_ty, f.suspends, false, f.span)?;
        // Branch entries are planned before their FunctionInfo is registered;
        // parameter storage is derivable from the declaration alone.
        let parameter_slots = functions.get(&f.name).map_or_else(
            || f.params.iter().map(|p| p.ty.parameter_slots()).sum(),
            FunctionInfo::parameter_slots,
        );
        let is_destructor = generics
            .borrow()
            .members
            .traits
            .definitions
            .iter()
            .any(|d| {
                d.is_drop
                    && f.name
                        .contains(&format!("joky/impl/{}/", d.self_parameter.id))
            });
        let when_function = generics
            .borrow()
            .when_parents
            .get(&f.name)
            .cloned()
            .unwrap_or_else(|| f.name.clone());
        let mut plan = Self {
            dyn_erasures: Vec::new(),
            when_function,
            source_function: f.name.clone(),
            suspending: f.suspends,
            callable_modes: Default::default(),
            captured_owners: Default::default(),
            captured_fields: Default::default(),
            closure_places: Default::default(),
            consuming_captures: false,
            is_destructor,
            unsafe_context: false,
            ops: Vec::new(),
            op_spans: Vec::new(),
            current_span: f.span,
            return_type: f.ret_ty.clone(),
            scope: f.scope.as_ref().map_or_else(
                || generics.borrow().scope.clone(),
                |scope| scope.imports.clone(),
            ),
            definition_scope: f.scope.clone(),
            generics,
            instantiation_depth,
            // Substituted arguments may come from the consumer's other modules.
            // Validate their identities there; source lookup stays in the
            // definition scope, which was checked with rigid parameters first.
            type_registry: types.clone(),
            nominals: if instantiation_depth > 0 {
                nominals.clone()
            } else {
                f.scope
                    .as_ref()
                    .map_or_else(|| nominals.clone(), |scope| scope.nominals.clone())
            },
            return_label: None,
            loops: Vec::new(),
            graph_loop_depth: None,
            graph_handles: Default::default(),
            borrowed_names: locals::borrowed_names(&f.body),
            local_scopes: vec![Vec::new()],
            argument_scopes: Vec::new(),
            implicit_borrows: Vec::new(),
            domain_calls: Vec::new(),
            task_scopes: Vec::new(),
            task_scope_count: 0,
            nested: Vec::new(),
            result: None,
            slot_count: parameter_slots,
            cps_local_slots: HashMap::new(),
            state_count: 1,
            label_count: 0,
            span: f.span,
        };
        if fixed_arrays::intrinsic(&f.compiler_intrinsic) {
            return Ok(plan);
        }
        if let Some(CompilerIntrinsic::DirectoryType(ty)) = &f.compiler_intrinsic {
            plan.result = Some(plan.directory_type(ty, f.span)?);
            plan.resolve_cleanups()?;
            return Ok(plan);
        }
        if matches!(
            f.compiler_intrinsic,
            Some(
                CompilerIntrinsic::DomainReadOnly
                    | CompilerIntrinsic::DomainModule
                    | CompilerIntrinsic::DomainPack
                    | CompilerIntrinsic::DomainUnpack
            )
        ) {
            let mut index = 0;
            let values: Vec<_> = f
                .params
                .iter()
                .map(|p| {
                    let slot = (p.ty != Type::Unit).then(|| Slot {
                        index,
                        ty: p.ty.clone(),
                    });
                    index += p.ty.parameter_slots();
                    slot
                })
                .collect();
            let info = &functions[&f.name];
            let Some(Flow::Value(result)) = plan.source_domain_call(domains::DomainCall {
                info,
                args: &[],
                values: &values,
                span: f.span,
                vars: &mut HashMap::new(),
                functions,
                suspends: f.suspends,
                types,
            })?
            else {
                unreachable!()
            };
            plan.result = result;
            plan.resolve_cleanups()?;
            return Ok(plan);
        }
        if matches!(
            f.compiler_intrinsic,
            Some(
                CompilerIntrinsic::ScriptSchema(_)
                    | CompilerIntrinsic::ScriptPack
                    | CompilerIntrinsic::ScriptUnpack
            )
        ) {
            let mut index = 0;
            let values: Vec<_> = f
                .params
                .iter()
                .map(|p| {
                    let slot = (p.ty != Type::Unit).then(|| Slot {
                        index,
                        ty: p.ty.clone(),
                    });
                    index += p.ty.parameter_slots();
                    slot
                })
                .collect();
            let info = &functions[&f.name];
            let Some(Flow::Value(result)) = plan.script_call(info, &values, f.span)? else {
                unreachable!()
            };
            plan.result = result;
            plan.resolve_cleanups()?;
            return Ok(plan);
        }
        if let Some(intrinsic) = f.buffer_intrinsic {
            plan.storage_body(f, intrinsic)?;
            return Ok(plan);
        }
        if matches!(
            f.compiler_intrinsic,
            Some(CompilerIntrinsic::MemReplace | CompilerIntrinsic::MemSwap)
        ) {
            // Direct calls lower at their use site. Explicit function values
            // specialize a concrete Copy signature and need an executable body.
            if f.type_params.is_empty() {
                let Type::Reference(r) = &f.params[0].ty else {
                    unreachable!()
                };
                let target = plan.pointee(&r.target, types)?;
                let left = Slot {
                    index: 0,
                    ty: Type::reference(target.clone(), true),
                };
                let right = Slot {
                    index: 1,
                    ty: f.params[1].ty.clone(),
                };
                if matches!(f.compiler_intrinsic, Some(CompilerIntrinsic::MemSwap)) {
                    plan.emit(Op::Buffer(BufferOp::SwapReferences {
                        left,
                        right,
                        target,
                    }));
                } else {
                    let dst = if target == Type::Unit {
                        None
                    } else {
                        Some(plan.slot(target.clone())?)
                    };
                    plan.emit(Op::ReadMemory {
                        dst: dst.clone(),
                        reference: left.clone(),
                        path: vec![],
                    });
                    plan.emit(Op::WriteMemory {
                        reference: left,
                        path: vec![],
                        value: (target != Type::Unit).then_some(right),
                        target,
                    });
                    plan.result = dst;
                }
            }
            return Ok(plan);
        }
        if matches!(
            f.compiler_intrinsic,
            Some(CompilerIntrinsic::Assert | CompilerIntrinsic::Panic)
        ) {
            let panic = matches!(f.compiler_intrinsic, Some(CompilerIntrinsic::Panic));
            let message_type = Type::reference(Type::String, false);
            let params = if panic {
                vec![&message_type]
            } else {
                vec![&Type::Bool, &message_type]
            };
            if f.suspends || f.ret_ty != Type::Unit || !f.params.iter().map(|p| &p.ty).eq(params) {
                return Err(type_error(
                    &f.span,
                    ErrorCode::InvalidCall,
                    "Invalid compiler check signature",
                ));
            }
            let message = Slot {
                index: usize::from(!panic),
                ty: message_type,
            };
            if panic {
                plan.emit(Op::Panic { message });
            } else {
                plan.emit(Op::Assert {
                    condition: Slot {
                        index: 0,
                        ty: Type::Bool,
                    },
                    message,
                });
            }
            return Ok(plan);
        }
        if let Some(CompilerIntrinsic::PlainCopy(ty) | CompilerIntrinsic::ReferenceFree(ty)) =
            &f.compiler_intrinsic
        {
            if f.suspends || f.ret_ty != Type::Unit || !f.params.is_empty() {
                return Err(type_error(
                    &f.span,
                    ErrorCode::InvalidCall,
                    "Invalid compiler type qualification signature",
                ));
            }
            if matches!(
                f.compiler_intrinsic,
                Some(CompilerIntrinsic::ReferenceFree(_))
            ) {
                generics::validate_reference_free(ty, f.span)?;
            } else {
                generics::validate_plain_copy(ty, f.span)?;
            }
            return Ok(plan);
        }
        if matches!(
            f.compiler_intrinsic,
            Some(CompilerIntrinsic::RuneFromValidU32)
        ) {
            if f.suspends
                || f.ret_ty != Type::Rune
                || !f.params.iter().map(|p| &p.ty).eq([&Type::UInt32])
            {
                return Err(type_error(
                    &f.span,
                    ErrorCode::InvalidCall,
                    "Invalid compiler Rune construction signature",
                ));
            }
            let dst = plan.slot(Type::Rune)?;
            plan.emit(Op::Convert(
                dst.clone(),
                Slot {
                    index: 0,
                    ty: Type::UInt32,
                },
            ));
            plan.result = Some(dst);
            return Ok(plan);
        }
        if plan.float_body(f)? {
            return Ok(plan);
        }
        if plan.text_body(f)? {
            return Ok(plan);
        }
        if plan.arena_body(f)? {
            return Ok(plan);
        }
        // Aggregate results have a fixed region immediately after parameters in CPS.
        // Direct lowering uses the same slots with an internal result pointer.
        if f.ret_ty.is_aggregate() {
            plan.result = Some(plan.slot(f.ret_ty.clone())?);
        }
        let mut vars = HashMap::new();
        let mut index = 0;
        // Register every incoming owner before materializing any parameter
        // place, which may fail or become an interpreter slicing boundary.
        for p in &f.params {
            if p.ty.is_owner() {
                plan.register_owner(
                    Slot {
                        index,
                        ty: p.ty.clone(),
                    },
                    None,
                );
            }
            index += p.ty.parameter_slots();
        }
        index = 0;
        for p in &f.params {
            let slot = (p.ty != Type::Unit).then(|| Slot {
                index,
                ty: p.ty.clone(),
            });
            index += p.ty.parameter_slots();
            let binding = plan.local_binding(
                &p.name,
                Binding {
                    slot,
                    mutable: false,
                    place: None,
                },
            )?;
            if vars.insert(p.name.clone(), binding).is_some() {
                return Err(type_error(
                    &f.span,
                    ErrorCode::DuplicateDefinition,
                    format!("Duplicate parameter `{}`", p.name),
                ));
            }
        }
        let declared = {
            let context = plan.generics.borrow();
            context
                .instances
                .iter()
                .find(|i| i.name == f.name)
                .map(|i| context.templates[&i.template].params.clone())
        };
        if let Some(params) = declared {
            let mut index = 0;
            for (p, actual) in params.iter().zip(&f.params) {
                if let Some((mode, _)) = closures::callable_signature(&p.ty) {
                    plan.callable_modes.insert(index, mode);
                }
                index += actual.ty.parameter_slots();
            }
        }
        plan.bind_closure_captures(&mut vars)?;
        let flow = plan.expr_expected(
            &f.body,
            &mut vars,
            functions,
            f.suspends,
            types,
            Some(&f.ret_ty),
        )?;
        if let Flow::Value(value) = flow {
            if slot_type(&value) != f.ret_ty {
                return Err(type_error(
                    &f.span,
                    ErrorCode::TypeMismatch,
                    format!(
                        "Return type mismatch for `{}`: expected {}, got {}",
                        f.name,
                        f.ret_ty,
                        slot_type(&value),
                    ),
                ));
            }
            if plan.return_label.is_some() || f.ret_ty.is_aggregate() {
                if let (Some(dst), Some(src)) = (&plan.result, value) {
                    plan.emit(Op::Copy(dst.clone(), src));
                }
            } else {
                plan.result = value;
            }
        }
        plan.end_local_scope();
        if let Some(exit) = plan.return_label {
            plan.emit(Op::Label(exit));
        }
        plan.resolve_cleanups()?;
        Ok(plan)
    }

    fn slot(&mut self, ty: Type) -> Result<Slot, Error> {
        self.check_fixed_array(&ty)?;
        if ty.contains_explicit_arena() {
            self.type_registry
                .validate(&ty)
                .map_err(|error| error.with_span(self.current_span))?;
        }
        let width = ty.value_lanes().len();
        if self
            .slot_count
            .checked_add(width)
            .is_none_or(|end| end >= (i32::MAX as usize / 8))
        {
            return Err(type_error(
                &self.span,
                ErrorCode::InvalidState,
                "CPS frame is too large",
            ));
        }
        let slot = Slot {
            index: self.slot_count,
            ty,
        };
        self.slot_count += width;
        if slot.ty.is_owner() || slot.ty.is_closure_value() || slot.ty.is_place() {
            self.local_scopes.last_mut().unwrap().push(slot.clone());
        }
        Ok(slot)
    }

    fn emit(&mut self, op: Op) {
        if let Op::Closure(op) = &op {
            if let closures::ClosureOp::Move { dst, src } = op.as_ref() {
                if let Some(mode) = self.callable_modes.get(&src.index).copied() {
                    self.callable_modes.insert(dst.index, mode);
                }
                if let Some(place) = self.closure_places.get(&src.index).cloned() {
                    self.closure_places.entry(dst.index).or_insert(place);
                }
            }
        }

        if let Op::EndLocal(slot) = &op {
            if slot.ty.is_place() {
                self.ops.push(Op::Place(PlaceOp::End(slot.clone())));
                self.op_spans.push(self.current_span);
                return;
            }
            if slot.ty.is_closure_value() {
                self.ops.push(Op::Closure(Box::new(closures::ClosureOp::End(
                    slot.clone(),
                ))));
                self.op_spans.push(self.current_span);
                if !slot.ty.is_owner() {
                    return;
                }
            }
            if slot.ty.is_owner() {
                self.ops.push(Op::Buffer(BufferOp::End(slot.clone())));
                self.op_spans.push(self.current_span);
                return;
            }
        }
        if let Op::Copy(dst, src) = &op {
            if dst.ty.is_place() {
                let op = if matches!(dst.ty, Type::FixedArrayInit(_)) {
                    PlaceOp::MoveInit {
                        dst: dst.clone(),
                        src: src.clone(),
                        publish: false,
                    }
                } else {
                    PlaceOp::Copy {
                        dst: dst.clone(),
                        src: src.clone(),
                    }
                };
                self.ops.push(Op::Place(op));
                self.op_spans.push(self.current_span);
                return;
            }
            if let Some(place) = self.closure_places.get(&src.index).cloned() {
                self.closure_places.entry(dst.index).or_insert(place);
            }
            if let Some(mode) = self.callable_modes.get(&src.index).copied() {
                self.callable_modes.insert(dst.index, mode);
            }
            if dst.ty.is_closure_value() && dst.index != src.index {
                self.ops
                    .push(Op::Closure(Box::new(closures::ClosureOp::Move {
                        dst: dst.clone(),
                        src: src.clone(),
                    })));
                self.op_spans.push(self.current_span);
            }
            if dst.ty.is_owner() {
                self.ops.push(Op::Buffer(BufferOp::Move {
                    dst: dst.clone(),
                    src: src.clone(),
                }));
                self.op_spans.push(self.current_span);
                if dst.ty.is_aggregate() {
                    self.register_owner(dst.clone(), None);
                }
                return;
            }
            if dst.ty.reference_type().is_some() {
                self.ops.push(Op::Reference {
                    dst: dst.clone(),
                    src: src.clone(),
                    moving: true,
                });
                self.op_spans.push(self.current_span);
                return;
            }
            if dst.ty.is_aggregate() {
                for (dst, src) in dst.scalars().zip(src.scalars()) {
                    self.emit(Op::Copy(dst, src));
                }
                return;
            }
        }
        let owner = match &op {
            Op::Call {
                result: Some(dst), ..
            }
            | Op::Scope {
                result: Some(dst), ..
            }
            | Op::ReadOwner { dst: Some(dst), .. }
            | Op::Buffer(BufferOp::Extract { dst, .. } | BufferOp::Detach { dst, .. })
                if dst.ty.is_owner() =>
            {
                Some(dst.clone())
            }
            _ => None,
        };
        self.ops.push(op);
        self.op_spans.push(self.current_span);
        if let Some(owner) = owner {
            self.register_owner(owner, None);
        }
    }

    fn expr(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<Flow, Error> {
        self.expr_expected(expr, vars, functions, suspends, types, None)
    }

    fn expr_expected(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
        expected: Option<&Type>,
    ) -> Result<Flow, Error> {
        let previous = self.current_span;
        self.current_span = expr.span();
        // Blocks and sequences do not need the value planner's large frame on
        // every nesting level. Keep small-stack compilation independent of the
        // number of expression variants handled by expr_inner.
        let result = if matches!(
            expr,
            Expr::Unsafe { .. } | Expr::Graph { .. } | Expr::Block { .. } | Expr::Sequence { .. }
        ) {
            self.block_expr(expr, vars, functions, suspends, types, expected)
        } else {
            self.expr_inner(expr, vars, functions, suspends, types, expected)
        }
        .and_then(|flow| self.coerce_reference(flow, expected));
        self.current_span = previous;
        result
    }

    /// Capture the old value before RHS effects, using the resolved address once.
    fn compound_snapshot(
        &mut self,
        op: &Option<BinOp>,
        ty: Type,
        original: Option<Slot>,
        memory: Option<(Slot, Vec<String>)>,
        span: &Span,
    ) -> Result<Option<Slot>, Error> {
        let Some(op) = op else { return Ok(None) };
        if !(ty.integer().is_some()
            || (ty.is_float() && matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div)))
        {
            return Err(type_error(
                span,
                ErrorCode::TypeMismatch,
                "Compound assignment requires a supported numeric type",
            ));
        }
        let dst = self.slot(ty)?;
        if let Some((reference, path)) = memory {
            self.emit(Op::ReadMemory {
                dst: Some(dst.clone()),
                reference,
                path,
            });
        } else {
            self.emit(Op::Copy(dst.clone(), original.unwrap()));
        }
        Ok(Some(dst))
    }

    fn compound_value(
        &mut self,
        op: &Option<BinOp>,
        old: Option<Slot>,
        value: Option<Slot>,
        span: &Span,
    ) -> Result<Option<Slot>, Error> {
        let Some(op) = op else { return Ok(value) };
        let left = old.unwrap();
        let right = value.filter(|value| value.ty == left.ty).ok_or_else(|| {
            type_error(
                span,
                ErrorCode::TypeMismatch,
                "Compound assignment operands must have the same type",
            )
        })?;
        let dst = self.slot(left.ty.clone())?;
        self.emit(Op::Binary(dst.clone(), op.clone(), left, right));
        Ok(Some(dst))
    }

    fn expr_inner(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
        expected: Option<&Type>,
    ) -> Result<Flow, Error> {
        if let Expr::Call {
            callee, args, span, ..
        } = expr
        {
            if callee == "$interpolation_append" {
                let Expr::Unary { operand, .. } = &args[0] else {
                    unreachable!("compiler-authored interpolation borrow")
                };
                return self.interpolation_append(
                    (operand, &args[1], *span),
                    vars,
                    functions,
                    suspends,
                    types,
                );
            }
        }
        self.check_graph_expression(expr, vars)?;
        let type_path = match expr {
            Expr::Record { name, .. } => Some(name.clone()),
            Expr::Call { callee, .. } => callee.contains('.').then(|| callee.clone()),
            Expr::Field { .. } => expression_path(expr),
            Expr::GenericName(name) => Some(name.name.clone()),
            Expr::MethodCall(call) => expression_path(&call.receiver),
            _ => None,
        };
        if type_path
            .as_ref()
            .and_then(|path| path.split('.').next())
            .is_some_and(|root| !vars.contains_key(root) && types.is_parameter_name(root))
        {
            return Err(type_error(
                &expr.span(),
                ErrorCode::TypeMismatch,
                "Unconstrained type parameters have no constructors or associated members",
            ));
        }
        // A `Module.NAME` constant read must resolve before field projection,
        // which would otherwise treat the module alias as an undefined variable.
        if matches!(expr, Expr::Field { .. }) {
            if let Some(flow) = self.module_constant_read(expr, vars, functions, suspends, types)? {
                return Ok(flow);
            }
        }
        if let Some(flow) = self.dyn_expr(expr, vars, functions, suspends, types)? {
            return Ok(flow);
        }
        // Keep the larger memory planner off scalar/identifier recursion paths.
        // This also preserves compilation on the interpreter's small-stack hosts.
        if matches!(
            expr,
            Expr::Call { .. }
                | Expr::Field { .. }
                | Expr::FieldAssign { .. }
                | Expr::Unary {
                    op: UnaryOp::Deref | UnaryOp::Borrow | UnaryOp::BorrowMut,
                    ..
                }
        ) {
            if let Some(flow) =
                self.memory_expr(expr, vars, functions, suspends, types, expected)?
            {
                return Ok(flow);
            }
        }
        if let Some(flow) = self.member_expr(expr, vars, functions, suspends, types)? {
            return Ok(flow);
        }
        if let Some(constructor) = self.enum_constructor(expr, vars)? {
            return self.enum_expr(constructor, vars, functions, suspends, types, expected);
        }
        if let Some((name, info)) = self.function_value_info(expr, vars, functions, types, true)? {
            self.check_drop_call(&name, expr.span())?;
            let target = self.slot(Type::function_with_effect(
                info.signature.params,
                info.signature.ret,
                info.suspends,
            ))?;
            self.emit(Op::FunctionRef(target.clone(), name));
            return Ok(Flow::Value(Some(target)));
        }
        if matches!(expr, Expr::Field { type_args, .. } if !type_args.is_empty()) {
            return Err(type_error(
                &expr.span(),
                ErrorCode::InvalidCall,
                "Explicit type arguments require a generic enum constructor",
            ));
        }
        // Every value resolution has declined a `Module.NAME` field read, so
        // the receiver is a module alias rather than a variable. Report the
        // missing constant instead of an undefined variable at the alias.
        if let Expr::Field { .. } = expr {
            if let Some(path) = expression_path(expr) {
                if let Some(root) = path.split('.').next() {
                    if !vars.contains_key(root) && self.scope.modules.contains_key(root) {
                        return Err(type_error(
                            &expr.span(),
                            ErrorCode::UnknownName,
                            format!("Undeclared constant `{path}`"),
                        ));
                    }
                }
            }
        }
        match expr {
            Expr::When(block) => self.when_expr(block, vars, functions, suspends, types),
            Expr::AnonymousFunction(function) => {
                self.anonymous_function(function, vars, functions, suspends, types, expected)
            }
            Expr::Array { .. } => self.array_expr(expr, vars, functions, suspends, types, expected),
            Expr::Tuple { .. } | Expr::Destructure { .. } => {
                self.tuple_expr(expr, vars, functions, suspends, types, expected)
            }
            Expr::Match { .. } => self.match_expr(expr, vars, functions, suspends, types, expected),
            Expr::Record { .. } | Expr::Field { .. } | Expr::FieldAssign { .. } => {
                self.record_expr(expr, vars, functions, suspends, types, expected)
            }
            Expr::Literal { literal, .. } => {
                let (ty, value) = match literal {
                    Literal::Float(text) => inference::float_literal(text, expected, expr.span())?,
                    Literal::Float32(n) => (Type::Float32, Constant::Float32(*n)),
                    Literal::Float64(n) => (Type::Float64, Constant::Float64(*n)),
                    Literal::Int8(n) => (Type::Int8, Constant::Int8(*n)),
                    Literal::Int16(n) => (Type::Int16, Constant::Int16(*n)),
                    Literal::Int32(n) => (Type::Int32, Constant::Int32(*n)),
                    Literal::Int64(n) => (Type::Int64, Constant::Int64(*n)),
                    Literal::UInt8(n) => (Type::UInt8, Constant::UInt8(*n)),
                    Literal::UInt16(n) => (Type::UInt16, Constant::UInt16(*n)),
                    Literal::UInt32(n) => (Type::UInt32, Constant::UInt32(*n)),
                    Literal::Rune(n) => {
                        if char::from_u32(*n).is_none() {
                            return Err(type_error(
                                &expr.span(),
                                ErrorCode::TypeMismatch,
                                "Rune literal must be a Unicode scalar",
                            ));
                        }
                        (Type::Rune, Constant::Rune(*n))
                    }
                    Literal::UInt64(n) => (Type::UInt64, Constant::UInt64(*n)),
                    Literal::ConstParameter(_) if self.generics.borrow().checking => {
                        (Type::UInt64, Constant::UInt64(0))
                    }
                    Literal::ConstParameter(_) => {
                        return Err(type_error(
                            &expr.span(),
                            ErrorCode::TypeMismatch,
                            "Unsubstituted constant parameter",
                        ))
                    }
                    Literal::Integer {
                        magnitude,
                        negative,
                    } => integer_literal(*magnitude, *negative, expected, expr.span())?,
                    Literal::Bool(b) => (Type::Bool, Constant::Bool(*b)),
                    Literal::String(text) => (Type::String, Constant::String(text.clone())),
                };
                let slot = self.slot(ty)?;
                if let Constant::String(text) = value {
                    self.emit(Op::Text {
                        dst: slot.clone(),
                        source: TextSource::Literal(text),
                    });
                    self.register_owner(slot.clone(), None);
                } else {
                    self.emit(Op::Literal(slot.clone(), value));
                }
                Ok(Flow::Value(Some(slot)))
            }
            Expr::GenericName(name) => Err(type_error(
                &name.span,
                ErrorCode::InvalidCall,
                "Explicit type arguments require a generic enum constructor",
            )),
            Expr::Ident { name, span } => {
                // A local binding shadows a bare module constant; only an
                // unshadowed name reaches the module's constant namespace.
                if !vars.contains_key(name) {
                    if let Some(flow) =
                        self.module_constant_expr(name, vars, functions, suspends, types)?
                    {
                        return Ok(flow);
                    }
                }
                let binding = vars.get(name).ok_or_else(|| {
                    type_error(
                        span,
                        ErrorCode::UnknownName,
                        format!("Undefined variable `{name}`"),
                    )
                })?;
                if let Some(place) = &binding.place {
                    if let Some(owner) = binding.slot.as_ref().filter(|slot| slot.ty.is_owner()) {
                        return self
                            .read_owner(place.clone(), owner, vec![], owner.ty.clone())
                            .map(Flow::Value);
                    }
                    if binding.slot.as_ref().is_some_and(|s| {
                        s.ty.is_closure_value() && self.captured_owners.contains(&s.index)
                    }) {
                        return Err(type_error(
                            span,
                            ErrorCode::InvalidAssignment,
                            if self.consuming_captures {
                                "Move capture requires a consuming closure entry"
                            } else {
                                "Borrowed closure captures cannot consume non-Copy values; use move fn"
                            },
                        ));
                    }
                    let value = self.read_local(place.clone())?;
                    if let (Some(source), Some(target)) = (&binding.slot, &value) {
                        if source.ty.is_closure_value() {
                            self.emit(Op::Closure(Box::new(closures::ClosureOp::Move {
                                dst: target.clone(),
                                src: source.clone(),
                            })));
                        }
                    }
                    return Ok(Flow::Value(value));
                }
                if slot_type(&binding.slot).is_owner()
                    || binding.mutable
                        && slot_type(&binding.slot).reference_type().is_none()
                        && !slot_type(&binding.slot).is_closure_value()
                {
                    // Later operands/arguments may assign to this binding. Keep
                    // the value observed at this point, including across Pending.
                    self.snapshot(&binding.slot).map(Flow::Value)
                } else {
                    Ok(Flow::Value(binding.slot.clone()))
                }
            }
            Expr::FunctionRef { name, span } => {
                if self.generics.borrow().templates.contains_key(name) {
                    return Err(type_error(
                        span,
                        ErrorCode::InvalidCall,
                        "Generic functions require a concrete wrapper for native registration",
                    ));
                }
                let function = functions.get(name).ok_or_else(|| {
                    type_error(
                        span,
                        ErrorCode::UnknownName,
                        format!("Undefined function `{name}`"),
                    )
                })?;
                if function.internal
                    || function.extern_abi == ExternAbi::C
                    || function.suspends
                    || function.signature.ret != Type::Int32
                    || function.signature.params.len() > 2
                    || function
                        .signature
                        .params
                        .iter()
                        .any(|ty| *ty != Type::Int32)
                {
                    return Err(type_error(span, ErrorCode::InvalidCall, format!(
                        "Function reference `{name}` must target a synchronous Int32 function with zero to two Int32 arguments"
                    )));
                }
                let slot = self.slot(Type::Named("vm.Function".into()))?;
                self.emit(Op::FunctionRef(slot.clone(), name.clone()));
                Ok(Flow::Value(Some(slot)))
            }
            Expr::Let {
                name,
                annotation,
                value,
                span,
            }
            | Expr::Var {
                name,
                annotation,
                value,
                span,
            } => {
                if let Some(annotation) = annotation {
                    validate_nominal_type(annotation, &self.nominals, span)?;
                    types
                        .validate(annotation)
                        .map_err(|error| error.with_span(*span))?;
                }
                let Flow::Value(value) = self.expr_expected(
                    value,
                    vars,
                    functions,
                    suspends,
                    types,
                    annotation.as_ref(),
                )?
                else {
                    return Ok(Flow::Diverged);
                };
                if let Some(expected) = annotation {
                    if *expected != slot_type(&value) {
                        return Err(type_error(
                            span,
                            ErrorCode::TypeMismatch,
                            format!(
                            "Type annotation mismatch for `{name}`: expected {expected}, got {}",
                            slot_type(&value)
                        ),
                        ));
                    }
                }
                let mutable = matches!(expr, Expr::Var { .. });
                // A mutable binding owns its storage; assigning to it must not
                // change its initializer or another binding sharing that value.
                let slot = if slot_type(&value).reference_type().is_some() {
                    self.reference_value(value, true)?
                } else if mutable || slot_type(&value).is_closure_value() {
                    self.snapshot(&value)?
                } else {
                    value
                };
                let binding = self.local_binding(
                    name,
                    Binding {
                        slot,
                        mutable,
                        place: None,
                    },
                )?;
                vars.insert(name.clone(), binding);
                Ok(Flow::Value(None))
            }
            Expr::Assign {
                name,
                value,
                op,
                span,
            } => {
                let binding = vars.get(name).cloned().ok_or_else(|| {
                    type_error(
                        span,
                        ErrorCode::UnknownName,
                        format!("Undefined variable `{name}`"),
                    )
                })?;
                if !binding.mutable {
                    return Err(type_error(
                        span,
                        ErrorCode::InvalidAssignment,
                        format!("Cannot assign to immutable binding `{name}`; declare it with var"),
                    ));
                }
                let is_owner = slot_type(&binding.slot).is_owner();
                let place = binding
                    .place
                    .clone()
                    .map(|p| {
                        if is_owner {
                            Ok(p)
                        } else {
                            self.reserve_write(p)
                        }
                    })
                    .transpose()?;
                let old = self.compound_snapshot(
                    op,
                    slot_type(&binding.slot),
                    binding.slot.clone(),
                    place.clone().map(|reference| (reference, vec![])),
                    span,
                )?;
                let Flow::Value(value) = self.expr_expected(
                    value,
                    vars,
                    functions,
                    suspends,
                    types,
                    Some(&slot_type(&binding.slot)),
                )?
                else {
                    return Ok(Flow::Diverged);
                };
                let value = self.compound_value(op, old, value, span)?;
                if slot_type(&binding.slot) != slot_type(&value) {
                    return Err(type_error(
                        span,
                        ErrorCode::TypeMismatch,
                        format!(
                            "Assignment type mismatch for `{name}`: expected {}, got {}",
                            slot_type(&binding.slot),
                            slot_type(&value)
                        ),
                    ));
                }
                if is_owner
                    && binding.slot.as_ref().is_some_and(|s| {
                        self.captured_owners.contains(&s.index)
                            || self.captured_fields.contains_key(&s.index)
                    })
                {
                    self.replace_capture(place.unwrap(), value.unwrap())?;
                    return Ok(Flow::Value(None));
                }
                if is_owner {
                    let dst = binding.slot.unwrap();
                    self.emit(Op::Copy(dst.clone(), value.unwrap()));
                    if let Some(reference) = place {
                        self.emit(Op::StoreOwner {
                            reference: reference.clone(),
                            path: vec![],
                            value: Some(dst.clone()),
                            target: dst.ty.clone(),
                        });
                        self.register_owner(dst, Some(reference));
                    }
                } else if let Some(reference) = place {
                    if let (Some(dst), Some(src)) = (&binding.slot, &value) {
                        if dst.ty.is_closure_value() {
                            self.emit(Op::Closure(Box::new(closures::ClosureOp::Move {
                                dst: dst.clone(),
                                src: src.clone(),
                            })));
                        }
                    }
                    self.emit(Op::WriteMemory {
                        reference,
                        path: vec![],
                        value,
                        target: slot_type(&binding.slot),
                    });
                } else if let (Some(dst), Some(src)) = (binding.slot, value) {
                    self.emit(Op::Copy(dst, src));
                }
                Ok(Flow::Value(None))
            }
            Expr::While { .. }
            | Expr::If { .. }
            | Expr::Logical { .. }
            | Expr::Return { .. }
            | Expr::Break { .. }
            | Expr::Continue { .. } => {
                self.control_flow_expr(expr, vars, functions, suspends, types, expected)
            }
            Expr::Unsafe { .. }
            | Expr::Graph { .. }
            | Expr::Block { .. }
            | Expr::Sequence { .. } => {
                unreachable!("block forms are dispatched before value expressions")
            }
            Expr::Unary { op, operand, span } => {
                if matches!(op, UnaryOp::Borrow | UnaryOp::BorrowMut) {
                    return self.borrow_local(
                        operand,
                        *op == UnaryOp::BorrowMut,
                        vars,
                        suspends,
                        *span,
                    );
                }
                let Flow::Value(operand) =
                    self.expr_expected(operand, vars, functions, suspends, types, expected)?
                else {
                    return Ok(Flow::Diverged);
                };
                let ty = slot_type(&operand);
                let valid = match op {
                    UnaryOp::Neg => {
                        ty.is_float() || ty.integer().is_some_and(|integer| integer.signed())
                    }
                    UnaryOp::Not => ty == Type::Bool,
                    UnaryOp::BitNot => ty.integer().is_some(),
                    UnaryOp::Borrow | UnaryOp::BorrowMut | UnaryOp::Deref => unreachable!(),
                };
                if !valid {
                    return Err(type_error(
                        span,
                        ErrorCode::TypeMismatch,
                        match op {
                            UnaryOp::Neg => "Unary `-` operand must be a signed integer or float",
                            UnaryOp::Not => "Unary `!` operand must be Bool",
                            UnaryOp::BitNot => "Unary `~` operand must be an integer",
                            UnaryOp::Borrow | UnaryOp::BorrowMut | UnaryOp::Deref => unreachable!(),
                        },
                    ));
                }
                let dst = self.slot(ty)?;
                self.emit(Op::Unary(dst.clone(), op.clone(), operand.unwrap()));
                Ok(Flow::Value(Some(dst)))
            }
            Expr::BinOp { lhs, op, rhs, span } => {
                let kind = if op.is_comparison() {
                    "Comparison"
                } else {
                    "Arithmetic"
                };
                let hint = self.binary_hint(lhs, rhs, vars, functions, types);
                let operand_type = hint
                    .as_ref()
                    .or(expected.filter(|ty| !op.is_comparison() && ty.is_numeric()));
                let Flow::Value(left) =
                    self.expr_expected(lhs, vars, functions, suspends, types, operand_type)?
                else {
                    return Err(unreachable_error(rhs));
                };
                let left = left.ok_or_else(|| {
                    type_error(span, ErrorCode::TypeMismatch, "Binary operand is Unit")
                })?;
                let equality = matches!(op, BinOp::Eq | BinOp::Ne);
                if !(left.ty.integer().is_some()
                    || (left.ty.is_float()
                        && (op.is_comparison()
                            || matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div))))
                    && !(equality && matches!(left.ty, Type::Bool | Type::Rune))
                {
                    return Err(type_error(
                        span,
                        ErrorCode::TypeMismatch,
                        format!(
                            "{kind} operands must have the same supported numeric type{}",
                            if matches!(op, BinOp::Eq | BinOp::Ne) {
                                " or both Bool or Rune"
                            } else {
                                ""
                            }
                        ),
                    ));
                }
                let Flow::Value(right) =
                    self.expr_expected(rhs, vars, functions, suspends, types, Some(&left.ty))?
                else {
                    return Ok(Flow::Diverged);
                };
                let right = right.ok_or_else(|| {
                    type_error(span, ErrorCode::TypeMismatch, "Binary operand is Unit")
                })?;
                if left.ty != right.ty {
                    return Err(type_error(
                        span,
                        ErrorCode::TypeMismatch,
                        format!(
                            "Binary operands must have the same type, got {} and {}",
                            left.ty, right.ty
                        ),
                    ));
                }
                let dst = self.slot(if op.is_comparison() {
                    Type::Bool
                } else {
                    left.ty.clone()
                })?;
                self.emit(Op::Binary(dst.clone(), op.clone(), left, right));
                Ok(Flow::Value(Some(dst)))
            }
            Expr::MethodCall(_) => unreachable!("method calls resolve before expression planning"),
            Expr::Invoke { callee, args, span } => {
                self.invoke_expr(callee, args, *span, vars, functions, suspends, types)
            }
            Expr::Call { .. } => self.call_expr(expr, vars, functions, suspends, types),
            Expr::Scope { .. } => self.scope_expr(expr, vars, functions, suspends, types),
            Expr::TaskScope { .. } => {
                self.task_scope_expr(expr, vars, functions, suspends, types, expected)
            }
            Expr::Branch { .. } => self.branch_expr(expr, vars, functions, suspends, types),
        }
    }

    fn snapshot(&mut self, value: &Option<Slot>) -> Result<Option<Slot>, Error> {
        value
            .as_ref()
            .map(|src| {
                let dst = self.slot(src.ty.clone())?;
                self.emit(Op::Copy(dst.clone(), src.clone()));
                Ok(dst)
            })
            .transpose()
    }
}

fn slot_type(slot: &Option<Slot>) -> Type {
    slot.as_ref().map_or(Type::Unit, |s| s.ty.clone())
}

/// A match pattern binding owns the variant payload: hints address slot 0,
/// lowering addresses the payload's frame slot. Unit payloads bind no storage.
fn payload_binding(ty: &Type, index: usize) -> Binding {
    Binding {
        place: None,
        slot: (ty != &Type::Unit).then_some(Slot {
            index,
            ty: ty.clone(),
        }),
        mutable: false,
    }
}

/// Explicit arguments on a namespace or variant path. Qualified paths retain
/// the parser's field representation, while simple paths use GenericName.
fn expression_type_arguments(expr: &Expr) -> Option<&[crate::parser::ast::GenericArgument]> {
    match expr {
        Expr::GenericName(name) => Some(&name.type_args),
        Expr::Field { type_args, .. } if !type_args.is_empty() => Some(type_args),
        _ => None,
    }
}

fn expression_path(mut expr: &Expr) -> Option<String> {
    let mut segments = Vec::new();
    loop {
        match expr {
            Expr::Ident { name, .. } => {
                segments.push(name.as_str());
                segments.reverse();
                return Some(segments.join("."));
            }
            Expr::GenericName(name) => {
                segments.push(name.name.as_str());
                segments.reverse();
                return Some(segments.join("."));
            }
            Expr::Field {
                base,
                name,
                type_args,
                ..
            } if segments.is_empty() || type_args.is_empty() => {
                segments.push(name.as_str());
                expr = base;
            }
            _ => return None,
        }
    }
}

impl FunctionPlan {
    fn check_call_root(
        &self,
        callee: &str,
        vars: &HashMap<String, Binding>,
        span: &Span,
    ) -> Result<(), Error> {
        let root = callee.split('.').next().unwrap();
        let imported = self.scope.functions.contains_key(callee)
            || self.scope.constructors.contains_key(callee);
        if vars.contains_key(root) && (callee.contains('.') || imported) {
            return Err(type_error(
                span,
                ErrorCode::InvalidCall,
                format!("'{root}' is a local value and cannot be called as a function"),
            ));
        }
        Ok(())
    }
}

fn item_span(item: &Item) -> &Span {
    match item {
        Item::FnDef(function) => &function.span,
        Item::ExternFn(function) => &function.span,
    }
}

/// Drain branch entry points planned inside `plan`, children before parents.
/// Dependencies needed both for execution and for checking unused templates.
fn collect_plan_dependencies(
    plan: &FunctionPlan,
    dependencies: &mut std::collections::BTreeSet<String>,
) {
    let mut pending = vec![plan];
    while let Some(plan) = pending.pop() {
        pending.extend(plan.nested.iter().map(|task| &task.plan));
        for op in &plan.ops {
            for CleanupOp::Register {
                hooks, elements, ..
            } in op.cleanups()
            {
                dependencies.extend(
                    hooks
                        .iter()
                        .chain(elements.iter().flat_map(|e| &e.hooks))
                        .map(|hook| hook.name.clone()),
                );
            }

            match op {
                Op::Call { name, .. } | Op::FunctionRef(_, name) => {
                    dependencies.insert(name.clone());
                }
                Op::Scope { branches, .. } => {
                    dependencies.extend(branches.iter().map(|branch| branch.name.clone()));
                }
                Op::TaskBranch { entry, .. } => {
                    dependencies.insert(entry.clone());
                }
                _ => {}
            }
        }
    }
}

fn take_nested_tasks(plan: &mut FunctionPlan, out: &mut Vec<NestedTask>) {
    for mut task in plan.nested.drain(..) {
        take_nested_tasks(&mut task.plan, out);
        out.push(task);
    }
}

fn type_error(span: &Span, code: ErrorCode, message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::TypeCheck, code, message).with_span(*span)
}

fn extend_argument(args: &mut Vec<Option<Slot>>, value: Option<Slot>) {
    match value {
        Some(slot) if slot.ty.is_aggregate() || slot.ty.dyn_interface().is_some() => {
            args.extend(slot.scalars().map(Some))
        }
        value => args.push(value),
    }
}

#[cfg(test)]
mod tests;
