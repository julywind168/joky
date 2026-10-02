//! Typed slot IR with explicit control flow shared by all execution backends.
use crate::parser::ast::{
    BinOp, CompilerIntrinsic, Expr, FnDef, Item, Literal, LogicalOp, Module as AstModule,
    ScopeKind, Type, UnaryOp,
};
use crate::{Error, ErrorCode, ErrorPhase, Span, TypeRegistry};
use std::{collections::HashMap, sync::Arc};

mod borrowing;
mod buffers;
mod calls;
mod cleanup;
pub(crate) use cleanup::{CleanupOp, DropHook, ElementCleanup};
mod constants;
mod control_flow;
mod enums;
mod function_values;
mod generics;
mod graph;
mod inference;
mod locals;
mod members;
mod memory;
mod patterns;
mod records;
mod traits;
mod tuples;
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
    Int32(i32),
    Int64(i64),
    UInt32(u32),
    UInt64(u64),
    Bool(bool),
    String(String),
}

#[derive(Clone, Debug)]
pub(crate) struct FunctionInfo {
    pub compiler_intrinsic: Option<CompilerIntrinsic>,
    pub c_abi: bool,
    pub borrowing_host: bool,
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
    pub span: Span,
}

impl FunctionInfo {
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
            let Type::Record(record) = &result.ty else {
                unreachable!("checked field path")
            };
            let field = record
                .fields
                .iter()
                .find(|field| &field.name == name)
                .unwrap();
            result.index += field.offset;
            result.ty = field.ty.clone();
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
            .scalar_types()
            .iter()
            .enumerate()
            .map(|(offset, ty)| Slot {
                index: self.index + offset,
                ty: ty.clone(),
            })
    }
}

#[derive(Clone)]
struct Binding {
    slot: Option<Slot>,
    mutable: bool,
    place: Option<Slot>,
}

pub(crate) struct ScopeBranch {
    pub(crate) indirect: Option<Slot>,
    pub(crate) name: String,
    pub(crate) args: Vec<Option<Slot>>,
    pub(crate) done: Slot,
}

pub(crate) enum Op {
    Require {
        condition: Slot,
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
    Scope {
        kind: ScopeKind,
        branches: Vec<ScopeBranch>,
        state: u32,
    },
    Binary(Slot, BinOp, Slot, Slot),
    Unary(Slot, UnaryOp, Slot),
    /// Wrapping integer conversion; slots retain source and target types.
    Convert(Slot, Slot),
    Call {
        /// A typed callee slot selects an indirect language call.
        indirect: Option<Slot>,
        name: String,
        args: Vec<Option<Slot>>,
        result: Option<Slot>,
        state: Option<u32>,
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
}

pub(crate) struct FunctionPlan {
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
    pub(crate) result: Option<Slot>,
    pub slot_count: usize,
    pub state_count: u32,
    pub label_count: usize,
    span: Span,
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
        let scoped = types.compilation_scope();
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
            let borrowing_host =
                matches!(item, Item::ExternFn(f) if super::ffi::is_borrowing_import(f));
            for ty in params.iter().map(|p| &p.ty).chain(std::iter::once(ret)) {
                validate_nominal_type(ty, &nominals, item_span(item))?;
                if matches!(item, Item::ExternFn(_)) && ty.contains_functions() {
                    return Err(type_error(
                        item_span(item),
                        ErrorCode::AbiError,
                        "Language function values cannot cross a host ABI",
                    ));
                }
                if matches!(item, Item::ExternFn(_)) && ty.contains_buffers() {
                    return Err(Error::new_with_code(
                        crate::ErrorPhase::Abi,
                        ErrorCode::AbiError,
                        "Buffer owners and views cannot cross a host ABI",
                    )
                    .with_span(*item_span(item)));
                }
                if matches!(item, Item::ExternFn(f) if !f.c_abi)
                    && ((!borrowing_host && ty.reference_type().is_some()) || *ty == Type::Ptr)
                {
                    return Err(Error::new_with_code(
                        ErrorPhase::Abi,
                        ErrorCode::AbiError,
                        "References and Ptr require the explicit C ABI",
                    )
                    .with_span(*item_span(item)));
                }
                if matches!(item, Item::ExternFn(_)) && ty.is_aggregate() {
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
            let source_name = if matches!(item, Item::FnDef(f) if f.scope.is_some())
                && !members.functions.contains(name)
            {
                name.rsplit('.').next().unwrap()
            } else {
                name.as_str()
            };
            IntegerType::check_declaration(source_name, *item_span(item))?;
            if source_name.starts_with("__joky_arena_")
                || source_name.starts_with("__joky_buffer_")
                || source_name.starts_with("__joky_cps_")
                || source_name.starts_with("__joky_generic_")
                || source_name.starts_with("__joky_indirect_")
                || source_name.starts_with("__joky_runtime_")
                || source_name.starts_with("joky_runtime_")
                || source_name.starts_with("joky_cps_")
                || source_name.starts_with("joky_vm_")
            {
                return Err(type_error(
                    item_span(item),
                    ErrorCode::ReservedName,
                    format!("Reserved runtime function name `{name}`"),
                ));
            }
            let info = FunctionInfo {
                compiler_intrinsic: match item {
                    Item::FnDef(f) => f.compiler_intrinsic.clone(),
                    _ => None,
                },
                c_abi: matches!(item, Item::ExternFn(f) if f.c_abi),
                borrowing_host,
                internal: matches!(item, Item::FnDef(f) if f.buffer_intrinsic.is_some())
                    || members.functions.contains(name)
                    || matches!(item, Item::FnDef(f) if f.scope.is_some()),
                signature: crate::FunctionSignature::new(
                    params.iter().map(|p| p.ty.clone()).collect(),
                    ret.clone(),
                ),
                suspends,
                // Abstract requirements have no body and promise no storage purity.
                // Concrete instances are replanned against actual impl functions.
                may_fail: matches!(item, Item::ExternFn(_))
                    || members.traits.requirements.contains(name),
                uses_arena: members.traits.requirements.contains(name),
                needs_arena: members.traits.requirements.contains(name),
                frame_slots: params.iter().map(|p| p.ty.parameter_slots()).sum(),
                span: *item_span(item),
            };
            if info.borrowing_host {
                super::ffi::validate_borrowing_signature(
                    &info.signature,
                    &nominals,
                    types,
                    info.span,
                )?;
            } else if info.c_abi {
                super::ffi::validate_signature(
                    &info.signature,
                    suspends,
                    &nominals,
                    types,
                    info.span,
                )?;
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
                || info.borrowing_host
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
        generics.borrow_mut().members = members.clone();
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
        generics.borrow_mut().checking = false;
        let mut bodies = HashMap::new();
        for item in &module.items {
            if let Item::FnDef(f) = item {
                if f.type_params.is_empty() {
                    let plan =
                        FunctionPlan::new(f, &functions, types, &nominals, generics.clone(), 0)?;
                    bodies.insert(f.name.clone(), plan);
                }
            }
        }
        let mut next = 0;
        loop {
            let instance = generics.borrow().instances.get(next).cloned();
            let Some(instance) = instance else { break };
            next += 1;
            let definition = generics.borrow().definition(&instance, types)?;
            let mut info = functions[&instance.template].clone();
            info.signature = instance.signature.clone();
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
        for (index, signature) in generics.borrow().callables.iter().enumerate() {
            functions.insert(
                format!("__joky_indirect_{index}"),
                FunctionInfo {
                    compiler_intrinsic: None,
                    signature: signature.signature.clone(),
                    c_abi: false,
                    borrowing_host: false,
                    suspends: signature.suspends,
                    may_fail: true,
                    uses_arena: true,
                    needs_arena: true,
                    frame_slots: 0,
                    internal: true,
                    span: Span::empty(0),
                },
            );
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
                    && body.ops.iter().any(|op| match op {
                        Op::Require { .. } | Op::Alloc { .. } => true,
                        Op::Buffer(BufferOp::Storage { intrinsic, .. }) => intrinsic.may_fail(),
                        Op::Binary(_, op, _, _) if op.can_fail() => true,
                        Op::Cleanup(CleanupOp::Register { hooks, .. }) => {
                            hooks.iter().any(|hook| functions[&hook.name].may_fail)
                        }
                        Op::Buffer(BufferOp::End(_)) => true,
                        Op::Call { name, .. } => functions[name].may_fail,
                        Op::Scope { branches, .. } => branches
                            .iter()
                            .any(|branch| functions[&branch.name].may_fail),
                        _ => false,
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
        }
        for name in generics.borrow().templates.keys() {
            functions.remove(name);
            bodies.remove(name);
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
        let parameter_slots = functions[&f.name].parameter_slots();
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
        let mut plan = Self {
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
            result: None,
            slot_count: parameter_slots,
            state_count: 1,
            label_count: 0,
            span: f.span,
        };
        if let Some(intrinsic) = f.buffer_intrinsic {
            plan.storage_body(f, intrinsic)?;
            return Ok(plan);
        }
        if matches!(f.compiler_intrinsic, Some(CompilerIntrinsic::Require)) {
            if f.suspends
                || f.ret_ty != Type::Unit
                || !f
                    .params
                    .iter()
                    .map(|p| &p.ty)
                    .eq([&Type::Bool, &Type::String])
            {
                return Err(type_error(
                    &f.span,
                    ErrorCode::InvalidCall,
                    "Invalid compiler require signature",
                ));
            }
            plan.emit(Op::Require {
                condition: Slot {
                    index: 0,
                    ty: Type::Bool,
                },
                message: Slot {
                    index: 1,
                    ty: Type::String,
                },
            });
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
        let width = ty.scalar_types().len();
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
        if slot.ty.is_owner() {
            self.local_scopes.last_mut().unwrap().push(slot.clone());
        }
        Ok(slot)
    }

    fn emit(&mut self, op: Op) {
        if let Op::EndLocal(slot) = &op {
            if slot.ty.is_owner() {
                self.ops.push(Op::Buffer(BufferOp::End(slot.clone())));
                self.op_spans.push(self.current_span);
                return;
            }
        }
        if let Op::Copy(dst, src) = &op {
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
            | Op::ReadOwner { dst: Some(dst), .. }
            | Op::Buffer(BufferOp::Extract { dst, .. })
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

    fn expr_inner(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
        expected: Option<&Type>,
    ) -> Result<Flow, Error> {
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
        // Keep the larger memory planner off scalar/identifier recursion paths.
        // This also preserves compilation on the interpreter's small-stack hosts.
        if matches!(
            expr,
            Expr::Call { .. } | Expr::Field { .. } | Expr::FieldAssign { .. }
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
        if let Some(constructor) = self.enum_constructor(expr, vars) {
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
        match expr {
            Expr::Tuple { .. } | Expr::Destructure { .. } => {
                self.tuple_expr(expr, vars, functions, suspends, types, expected)
            }
            Expr::Match { .. } => self.match_expr(expr, vars, functions, suspends, types, expected),
            Expr::Record { .. } | Expr::Field { .. } | Expr::FieldAssign { .. } => {
                self.record_expr(expr, vars, functions, suspends, types, expected)
            }
            Expr::Literal { literal, .. } => {
                let (ty, value) = match literal {
                    Literal::Int32(n) => (Type::Int32, Constant::Int32(*n)),
                    Literal::Int64(n) => (Type::Int64, Constant::Int64(*n)),
                    Literal::UInt32(n) => (Type::UInt32, Constant::UInt32(*n)),
                    Literal::UInt64(n) => (Type::UInt64, Constant::UInt64(*n)),
                    Literal::Integer {
                        magnitude,
                        negative,
                    } => integer_literal(*magnitude, *negative, expected, expr.span())?,
                    Literal::Bool(b) => (Type::Bool, Constant::Bool(*b)),
                    Literal::String(text) => (Type::String, Constant::String(text.clone())),
                };
                let slot = self.slot(ty)?;
                self.emit(Op::Literal(slot.clone(), value));
                Ok(Flow::Value(Some(slot)))
            }
            Expr::GenericName(name) => Err(type_error(
                &name.span,
                ErrorCode::InvalidCall,
                "Explicit type arguments require a generic enum constructor",
            )),
            Expr::Ident { name, span } => {
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
                    return self.read_local(place.clone()).map(Flow::Value);
                }
                if slot_type(&binding.slot).is_owner()
                    || binding.mutable && slot_type(&binding.slot).reference_type().is_none()
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
                    || function.c_abi
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
                } else if mutable {
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
            Expr::Assign { name, value, span } => {
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
                    UnaryOp::Neg => ty.integer().is_some_and(|integer| integer.signed()),
                    UnaryOp::Not => ty == Type::Bool,
                    UnaryOp::BitNot => ty.integer().is_some(),
                    UnaryOp::Borrow | UnaryOp::BorrowMut => unreachable!(),
                };
                if !valid {
                    return Err(type_error(
                        span,
                        ErrorCode::TypeMismatch,
                        match op {
                            UnaryOp::Neg => "Unary `-` operand must be Int32 or Int64",
                            UnaryOp::Not => "Unary `!` operand must be Bool",
                            UnaryOp::BitNot => "Unary `~` operand must be an integer",
                            UnaryOp::Borrow | UnaryOp::BorrowMut => unreachable!(),
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
                    .or(expected.filter(|ty| !op.is_comparison() && ty.integer().is_some()));
                let Flow::Value(left) =
                    self.expr_expected(lhs, vars, functions, suspends, types, operand_type)?
                else {
                    return Err(unreachable_error(rhs));
                };
                let left = left.ok_or_else(|| {
                    type_error(span, ErrorCode::TypeMismatch, "Binary operand is Unit")
                })?;
                let equality = matches!(op, BinOp::Eq | BinOp::Ne);
                if left.ty.integer().is_none() && !(equality && left.ty == Type::Bool) {
                    return Err(type_error(
                        span,
                        ErrorCode::TypeMismatch,
                        format!(
                            "{kind} operands must have the same integer type{}",
                            if matches!(op, BinOp::Eq | BinOp::Ne) {
                                " or both Bool"
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
fn expression_type_arguments(expr: &Expr) -> Option<&[Type]> {
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

fn type_error(span: &Span, code: ErrorCode, message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::TypeCheck, code, message).with_span(*span)
}

fn extend_argument(args: &mut Vec<Option<Slot>>, value: Option<Slot>) {
    match value {
        Some(slot) if slot.ty.is_aggregate() => args.extend(slot.scalars().map(Some)),
        value => args.push(value),
    }
}

#[cfg(test)]
mod tests;
