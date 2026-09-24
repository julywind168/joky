//! Lowering of HIR (`CoreProgram`) into typed, CFG-oriented MIR.
//!
//! The lowerer walks each core function's expression tree and emits typed
//! MIR statements, making branches, loops, and transfers explicit. Literal
//! values become typed `Const` statements; more complex scalar and aggregate
//! expressions are lowered into the corresponding MIR statements.

use std::collections::{HashMap, HashSet};

use super::*;
use crate::hir::{
    CoreCollectionLiteral, CoreExpr, CoreExprKind, CoreFunction, CorePattern, CorePatternField,
};
use crate::sema::CheckedTypes;

mod cleanup;
mod control_flow;
mod cursor;
mod expressions;
mod externals;
mod for_loop;
mod functions;
mod ownership;
mod pattern_helpers;
mod patterns;
mod support;
mod synthetic_handlers;
mod tasks;

use self::cleanup::insert_scope_edge_drops;
use self::externals::{
    build_external_stub, collect_external_symbols, register_external_function_ids,
};
pub(crate) use self::functions::finalize_function_continuation_spills;
use self::functions::lower_function;
pub(crate) use self::functions::statement_destination;
use self::pattern_helpers::{ordered_pattern_fields, resolved_pattern_variant};
use self::support::{
    method_key, resolve_argument_index, resolve_field_access, resumable_forward_parameter,
    resumable_scalar_transform, HandlerTarget, LoopFrame, ResumableHandlerTarget,
};
use self::synthetic_handlers::collect_synthetic_handlers;

/// Build a `MirProgram` from a lowered core program.
pub(crate) fn lower_program(core: &CoreProgram) -> Result<MirProgram, Diagnostic> {
    let mut handler_function_ids = HashMap::new();
    let mut handler_specs = Vec::new();
    let mut next_function_id = core.functions().len();
    let mut known_function_names = core
        .functions()
        .iter()
        .map(|function| function.name.clone())
        .collect::<HashSet<_>>();
    // Builtins and option/result constructors are resolved by sema rather
    // than represented as user functions.  Keep them out of synthetic
    // handler capture lists just like ordinary function names.
    known_function_names.extend(
        ["println", "Some", "None", "Ok", "Err"]
            .into_iter()
            .map(str::to_owned),
    );
    for function in core.functions() {
        collect_synthetic_handlers(
            &function.body,
            function.module,
            core.types(),
            &known_function_names,
            &mut handler_function_ids,
            &mut handler_specs,
            &mut next_function_id,
        );
    }
    let mut function_ids = core
        .functions()
        .iter()
        .enumerate()
        .map(|(id, function)| {
            let key = function
                .receiver
                .map(|receiver| method_key(receiver, &function.name))
                .unwrap_or_else(|| function.name.clone());
            (key, MirFunctionId(id))
        })
        .collect::<HashMap<_, _>>();
    for spec in &handler_specs {
        function_ids.insert(spec.function.name.clone(), spec.id);
    }
    let external_symbols = collect_external_symbols(core.types());
    register_external_function_ids(&external_symbols, &mut function_ids, next_function_id);
    let mut function_names = core
        .functions()
        .iter()
        .filter(|function| function.receiver.is_none())
        .map(|function| function.name.clone())
        .collect::<HashSet<_>>();
    function_names.extend(handler_specs.iter().map(|spec| spec.function.name.clone()));
    let mut function_parameters = core
        .functions()
        .iter()
        .enumerate()
        .map(|(id, f)| {
            (
                MirFunctionId(id),
                f.parameters
                    .iter()
                    .map(|p| p.name.clone())
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<HashMap<_, _>>();
    for spec in &handler_specs {
        function_parameters.insert(
            spec.id,
            spec.function
                .parameters
                .iter()
                .map(|parameter| parameter.name.clone())
                .collect(),
        );
    }
    for symbol in &external_symbols {
        let Some(id) = function_ids
            .get(&crate::module::symbol_key(symbol))
            .copied()
        else {
            continue;
        };
        let Some(signature) = core.types().external_signature(symbol) else {
            return Err(Diagnostic::codegen(format!(
                "missing export signature for '{}'",
                symbol.name
            )));
        };
        function_parameters.insert(
            id,
            signature
                .parameters
                .iter()
                .map(|(name, _)| name.clone())
                .collect(),
        );
    }
    // Keep bodies for both free functions and methods.  Resumable lowering
    // may inline a method while a lexical handler is active, just like it
    // already does for a named free-function call.
    let function_bodies = core
        .functions()
        .iter()
        .map(|function| {
            let key = function
                .receiver
                .map(|receiver| method_key(receiver, &function.name))
                .unwrap_or_else(|| function.name.clone());
            (key, function)
        })
        .collect::<HashMap<_, _>>();
    let closure_ids = core
        .functions()
        .iter()
        .filter(|function| function.name.starts_with("__closure_"))
        .map(|function| (function.name.clone(), MirFunctionId(function.id.0)))
        .collect::<HashMap<_, _>>();
    let mut functions = core
        .functions()
        .iter()
        .map(|function| {
            lower_function(
                function,
                MirFunctionId(
                    core.functions()
                        .iter()
                        .position(|candidate| std::ptr::eq(candidate, function))
                        .expect("function belongs to core program"),
                ),
                &function_ids,
                &function_names,
                &function_parameters,
                &function_bodies,
                &closure_ids,
                core.types(),
                core.struct_defaults(),
                core.class_defaults(),
                &handler_function_ids,
                &core.source_spans,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut synthetic_cursor = 0;
    while synthetic_cursor < handler_specs.len() {
        let spec = &handler_specs[synthetic_cursor];
        functions.push(lower_function(
            &spec.function,
            spec.id,
            &function_ids,
            &function_names,
            &function_parameters,
            &function_bodies,
            &closure_ids,
            core.types(),
            core.struct_defaults(),
            core.class_defaults(),
            &handler_function_ids,
            &core.source_spans,
        )?);
        synthetic_cursor += 1;
    }

    for symbol in &external_symbols {
        let id = *function_ids
            .get(&crate::module::symbol_key(symbol))
            .expect("external stub id");
        let signature = core
            .types()
            .external_signature(symbol)
            .expect("external signature");
        functions.push(build_external_stub(id, symbol, signature, core.types())?);
    }

    crate::mir::suspending_analysis::materialize_task_waits(&mut functions);

    // Phase 2: Compute and propagate suspending property through call graph
    let suspending_map = crate::mir::suspending_analysis::compute_suspending_functions(&functions);
    crate::mir::suspending_analysis::propagate_suspending_property(&mut functions, &suspending_map);
    crate::mir::suspending_analysis::materialize_direct_call_continuations(
        &mut functions,
        &suspending_map,
    );

    let program = MirProgram {
        functions,
        types: core.types().module.types.clone(),
    };

    Ok(program)
}

fn resumable_runtime_constant(value: &CoreExpr, types: &CheckedTypes) -> Option<MirConstant> {
    let value = match &value.kind {
        CoreExprKind::Call {
            callee, arguments, ..
        } if matches!(&callee.kind, CoreExprKind::Name(name) if name == "resume")
            && arguments.len() == 1 =>
        {
            &arguments[0].value
        }
        _ => value,
    };
    match &value.kind {
        CoreExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            let MirConstant::Boolean(condition) = resumable_runtime_constant(condition, types)?
            else {
                return None;
            };
            if condition {
                resumable_runtime_constant(then_branch, types)
            } else {
                resumable_runtime_constant(else_branch, types)
            }
        }
        CoreExprKind::Unary { op, expression } => {
            let value = resumable_runtime_constant(expression, types)?;
            match (op, value) {
                (crate::syntax::UnaryOp::Not, MirConstant::Boolean(value)) => {
                    Some(MirConstant::Boolean(!value))
                }
                (crate::syntax::UnaryOp::BitNot, MirConstant::Integer(value)) => {
                    Some(MirConstant::Integer(!value))
                }
                _ => None,
            }
        }
        CoreExprKind::Binary { op, left, right } => {
            let left = resumable_runtime_constant(left, types)?;
            let right = resumable_runtime_constant(right, types)?;
            resumable_constant_binary(*op, left, right)
        }
        CoreExprKind::Integer(value) => Some(MirConstant::Integer(*value)),
        CoreExprKind::Float(value) => Some(MirConstant::Float(*value)),
        CoreExprKind::Boolean(value) => Some(MirConstant::Boolean(*value)),
        CoreExprKind::String(value) => Some(MirConstant::String(value.clone())),
        CoreExprKind::Name(name) if name == "None" => Some(MirConstant::Option(None)),
        CoreExprKind::Tuple(values) => Some(MirConstant::Tuple(
            values
                .iter()
                .map(|value| resumable_runtime_constant(value, types))
                .collect::<Option<Vec<_>>>()?,
        )),
        CoreExprKind::StructInit { fields, .. } => Some(MirConstant::Struct(
            fields
                .iter()
                .map(|(name, value)| {
                    Some((name.clone(), resumable_runtime_constant(value, types)?))
                })
                .collect::<Option<Vec<_>>>()?,
        )),
        CoreExprKind::CollectionLiteral(literal) => match literal {
            CoreCollectionLiteral::List(values) => Some(MirConstant::List(
                values
                    .iter()
                    .map(|value| resumable_runtime_constant(value, types))
                    .collect::<Option<Vec<_>>>()?,
            )),
            // Set is represented as Map<T, Bool> in the type table.
            CoreCollectionLiteral::Set(values) => Some(MirConstant::Map(
                values
                    .iter()
                    .map(|value| {
                        Some((
                            resumable_runtime_constant(value, types)?,
                            MirConstant::Boolean(true),
                        ))
                    })
                    .collect::<Option<Vec<_>>>()?,
            )),
            CoreCollectionLiteral::Map(entries) => Some(MirConstant::Map(
                entries
                    .iter()
                    .map(|entry| {
                        Some((
                            resumable_runtime_constant(&entry.key, types)?,
                            resumable_runtime_constant(&entry.value, types)?,
                        ))
                    })
                    .collect::<Option<Vec<_>>>()?,
            )),
            CoreCollectionLiteral::MutList(values) => Some(MirConstant::MutList(
                values
                    .iter()
                    .map(|value| resumable_runtime_constant(value, types))
                    .collect::<Option<Vec<_>>>()?,
            )),
            CoreCollectionLiteral::MutMap(entries) => Some(MirConstant::MutMap(
                entries
                    .iter()
                    .map(|entry| {
                        Some((
                            resumable_runtime_constant(&entry.key, types)?,
                            resumable_runtime_constant(&entry.value, types)?,
                        ))
                    })
                    .collect::<Option<Vec<_>>>()?,
            )),
            CoreCollectionLiteral::MutSet(values) => Some(MirConstant::MutSet(
                values
                    .iter()
                    .map(|value| resumable_runtime_constant(value, types))
                    .collect::<Option<Vec<_>>>()?,
            )),
        },
        CoreExprKind::Field {
            value: enum_value,
            access: crate::syntax::FieldAccess::Name(variant_name),
        } => {
            if !matches!(enum_value.kind, CoreExprKind::Name(_)) {
                return None;
            }
            let Type::Enum(enum_id) = value.ty else {
                return None;
            };
            let variant = types
                .enum_variants(enum_id)
                .iter()
                .position(|candidate| candidate.name == *variant_name)?;
            if !types.enum_variants(enum_id)[variant].fields.is_empty() {
                return None;
            }
            Some(MirConstant::Enum {
                variant,
                fields: Vec::new(),
            })
        }
        CoreExprKind::Call {
            callee, arguments, ..
        } if arguments.len() == 1
            && matches!(&callee.kind, CoreExprKind::Name(name) if matches!(name.as_str(), "Some" | "Ok" | "Err")) =>
        {
            let CoreExprKind::Name(name) = &callee.kind else {
                return None;
            };
            let nested = resumable_runtime_constant(&arguments[0].value, types)?;
            match name.as_str() {
                "Some" => Some(MirConstant::Option(Some(Box::new(nested)))),
                "Ok" => Some(MirConstant::Result {
                    is_ok: true,
                    value: Box::new(nested),
                }),
                "Err" => Some(MirConstant::Result {
                    is_ok: false,
                    value: Box::new(nested),
                }),
                _ => None,
            }
        }
        CoreExprKind::Call {
            callee, arguments, ..
        } => {
            if let (CoreExprKind::Name(_), Type::Struct(_) | Type::Class(_)) =
                (&callee.kind, value.ty)
            {
                let names = match value.ty {
                    Type::Struct(id) => types.struct_fields(id),
                    Type::Class(id) => types.class_fields(id),
                    _ => unreachable!(),
                }
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>();
                let mut used = vec![false; names.len()];
                let mut next_positional = 0;
                let mut fields = vec![None; names.len()];
                for argument in arguments {
                    let index = resolve_argument_index(
                        argument.label.as_deref(),
                        &names,
                        &mut used,
                        &mut next_positional,
                    )
                    .ok()?;
                    fields[index] = Some((
                        names[index].clone(),
                        resumable_runtime_constant(&argument.value, types)?,
                    ));
                }
                let fields = fields.into_iter().collect::<Option<Vec<_>>>()?;
                return Some(match value.ty {
                    Type::Struct(_) => MirConstant::Struct(fields),
                    Type::Class(_) => MirConstant::Class(fields),
                    _ => unreachable!(),
                });
            }
            let CoreExprKind::Field {
                value: enum_value,
                access: crate::syntax::FieldAccess::Name(variant_name),
            } = &callee.kind
            else {
                return None;
            };
            if !matches!(enum_value.kind, CoreExprKind::Name(_)) {
                return None;
            }
            let Type::Enum(enum_id) = value.ty else {
                return None;
            };
            let variants = types.enum_variants(enum_id);
            let variant = variants
                .iter()
                .position(|candidate| candidate.name == *variant_name)?;
            let names = variants[variant]
                .fields
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>();
            let mut used = vec![false; names.len()];
            let mut next_positional = 0;
            let mut fields = vec![None; names.len()];
            for argument in arguments {
                let index = resolve_argument_index(
                    argument.label.as_deref(),
                    &names,
                    &mut used,
                    &mut next_positional,
                )
                .ok()?;
                fields[index] = Some(resumable_runtime_constant(&argument.value, types)?);
            }
            Some(MirConstant::Enum {
                variant,
                fields: fields.into_iter().collect::<Option<Vec<_>>>()?,
            })
        }
        _ => None,
    }
}

fn resumable_constant_binary(
    op: crate::syntax::BinaryOp,
    left: MirConstant,
    right: MirConstant,
) -> Option<MirConstant> {
    match (left, right) {
        (MirConstant::Integer(left), MirConstant::Integer(right)) => {
            let value = match op {
                crate::syntax::BinaryOp::Add => left.checked_add(right)?,
                crate::syntax::BinaryOp::Subtract => left.checked_sub(right)?,
                crate::syntax::BinaryOp::Multiply => left.checked_mul(right)?,
                crate::syntax::BinaryOp::Divide => left.checked_div(right)?,
                crate::syntax::BinaryOp::Remainder => left.checked_rem(right)?,
                crate::syntax::BinaryOp::ShiftLeft => left.wrapping_shl((right & 63) as u32),
                crate::syntax::BinaryOp::ShiftRight => left.wrapping_shr((right & 63) as u32),
                crate::syntax::BinaryOp::BitAnd => left & right,
                crate::syntax::BinaryOp::BitXor => left ^ right,
                crate::syntax::BinaryOp::BitOr => left | right,
                crate::syntax::BinaryOp::Equal => return Some(MirConstant::Boolean(left == right)),
                crate::syntax::BinaryOp::NotEqual => {
                    return Some(MirConstant::Boolean(left != right));
                }
                crate::syntax::BinaryOp::Less => return Some(MirConstant::Boolean(left < right)),
                crate::syntax::BinaryOp::LessEqual => {
                    return Some(MirConstant::Boolean(left <= right));
                }
                crate::syntax::BinaryOp::Greater => {
                    return Some(MirConstant::Boolean(left > right));
                }
                crate::syntax::BinaryOp::GreaterEqual => {
                    return Some(MirConstant::Boolean(left >= right));
                }
                crate::syntax::BinaryOp::And | crate::syntax::BinaryOp::Or => return None,
            };
            Some(MirConstant::Integer(value))
        }
        (MirConstant::Float(left), MirConstant::Float(right)) => {
            let value = match op {
                crate::syntax::BinaryOp::Add => MirConstant::Float(left + right),
                crate::syntax::BinaryOp::Subtract => MirConstant::Float(left - right),
                crate::syntax::BinaryOp::Multiply => MirConstant::Float(left * right),
                crate::syntax::BinaryOp::Divide => MirConstant::Float(left / right),
                crate::syntax::BinaryOp::Remainder => MirConstant::Float(left % right),
                crate::syntax::BinaryOp::ShiftLeft
                | crate::syntax::BinaryOp::ShiftRight
                | crate::syntax::BinaryOp::BitAnd
                | crate::syntax::BinaryOp::BitXor
                | crate::syntax::BinaryOp::BitOr => return None,
                crate::syntax::BinaryOp::Equal => MirConstant::Boolean(left == right),
                crate::syntax::BinaryOp::NotEqual => MirConstant::Boolean(left != right),
                crate::syntax::BinaryOp::Less => MirConstant::Boolean(left < right),
                crate::syntax::BinaryOp::LessEqual => MirConstant::Boolean(left <= right),
                crate::syntax::BinaryOp::Greater => MirConstant::Boolean(left > right),
                crate::syntax::BinaryOp::GreaterEqual => MirConstant::Boolean(left >= right),
                crate::syntax::BinaryOp::And | crate::syntax::BinaryOp::Or => return None,
            };
            Some(value)
        }
        (MirConstant::Boolean(left), MirConstant::Boolean(right)) => match op {
            crate::syntax::BinaryOp::And => Some(MirConstant::Boolean(left && right)),
            crate::syntax::BinaryOp::Or => Some(MirConstant::Boolean(left || right)),
            crate::syntax::BinaryOp::Equal => Some(MirConstant::Boolean(left == right)),
            crate::syntax::BinaryOp::NotEqual => Some(MirConstant::Boolean(left != right)),
            _ => None,
        },
        _ => None,
    }
}

#[derive(Clone)]
struct LocalClosureBinding {
    function_name: String,
    captures: Vec<(String, Type, MirLocalId)>,
}

struct Lowerer<'a> {
    source_spans: &'a HashMap<crate::syntax::NodeId, crate::Span>,
    current_span: Option<crate::Span>,
    value_spans: Vec<Option<crate::Span>>,
    mutable_locals: HashMap<MirLocalId, crate::Span>,
    source_locals: HashMap<crate::syntax::NodeId, MirLocalId>,
    assignments: HashSet<MirValueId>,
    capture_state: Option<(MirLocalId, crate::hir::CoreClosureState)>,
    blocks: Vec<MirBlock>,
    /// Lexical lease depth at each block's creation. Transfers to an outer
    /// handler/loop must release only the leases they leave behind.
    block_cown_depths: Vec<usize>,
    block_task_depths: Vec<usize>,
    cown_leases: Vec<MirValueId>,
    current: MirBlockId,
    scope_depth: usize,
    next_value: usize,
    next_scope: usize,
    next_task: usize,
    next_continuation: usize,
    value_types: Vec<Type>,
    value_ownership: Vec<MirOwnership>,
    continuations: Vec<MirContinuation>,
    loops: Vec<LoopFrame>,
    handlers: Vec<HashMap<crate::sema::EffectOperationId, HandlerTarget>>,
    resumable_handlers: Vec<HashMap<crate::sema::EffectOperationId, ResumableHandlerTarget>>,
    inline_functions: Vec<String>,
    struct_defaults: &'a [Vec<Option<CoreExpr>>],
    class_defaults: &'a [Vec<Option<CoreExpr>>],
    function_ids: &'a HashMap<String, MirFunctionId>,
    function_parameters: &'a HashMap<MirFunctionId, Vec<String>>,
    function_bodies: &'a HashMap<String, &'a CoreFunction>,
    closure_ids: &'a HashMap<String, MirFunctionId>,
    types: &'a CheckedTypes,
    locals: Vec<MirLocal>,
    entry_locals: HashSet<MirLocalId>,
    bindings: Vec<HashMap<String, MirLocalId>>,
    /// Stable closure targets and the declaration-time locals backing their
    /// captures. Call-site shadowing must not change an inlined snapshot.
    closure_bindings: HashMap<String, LocalClosureBinding>,
    receiver_local: Option<MirLocalId>,
    task_scopes: Vec<MirScopeId>,
    branch_regions: HashSet<MirScopeId>,
    function_name: String,
    is_task: bool,
    return_type: Type,
    handler_function_ids: &'a HashMap<crate::syntax::NodeId, MirFunctionId>,
}

impl Lowerer<'_> {
    fn local_closure_binding(
        &self,
        function_name: &str,
        captures: &[(String, Type)],
    ) -> Option<LocalClosureBinding> {
        if self
            .function_bodies
            .get(function_name)
            .is_some_and(|function| function.closure_state.is_some())
        {
            return None;
        }
        let captures = captures
            .iter()
            .map(|(name, ty)| {
                (!self.types.is_owned(*ty))
                    .then(|| self.resolve_local(name))
                    .flatten()
                    .map(|local| (name.clone(), *ty, local))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(LocalClosureBinding {
            function_name: function_name.to_owned(),
            captures,
        })
    }

    fn new_local(&mut self, name: &str, ty: Type) -> MirLocalId {
        self.new_local_with_ownership(name, ty, ownership_for_type(ty, self.types))
    }

    fn new_local_with_ownership(
        &mut self,
        name: &str,
        ty: Type,
        ownership: MirOwnership,
    ) -> MirLocalId {
        let id = MirLocalId(self.locals.len());
        self.locals.push(MirLocal {
            id,
            name: name.to_owned(),
            ty,
            ownership,
            scope_depth: self.scope_depth,
        });
        id
    }

    fn bind_local(&mut self, name: &str, local: MirLocalId) {
        self.bindings
            .last_mut()
            .expect("local scope")
            .insert(name.to_owned(), local);
    }

    fn resolve_local(&self, name: &str) -> Option<MirLocalId> {
        self.bindings
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }

    fn resolve_expression_local(&self, expression: &CoreExpr, name: &str) -> Option<MirLocalId> {
        self.types
            .local_bindings
            .get(&expression.id)
            .and_then(|declaration| self.source_locals.get(declaration).copied())
            .or_else(|| self.resolve_local(name))
    }

    fn is_open(&self) -> bool {
        self.blocks[self.current.0].terminator.is_none()
    }

    fn new_block(&mut self) -> MirBlockId {
        let id = MirBlockId(self.blocks.len());
        self.block_cown_depths.push(self.cown_leases.len());
        self.block_task_depths.push(self.task_scopes.len());
        self.blocks.push(MirBlock {
            id,
            scoped: false,
            scope_depth: self.scope_depth,
            statements: Vec::new(),
            terminator: None,
        });
        id
    }

    fn switch_to(&mut self, block: MirBlockId) {
        self.current = block;
        self.scope_depth = self.blocks[block.0].scope_depth;
    }

    fn mark_scoped(&mut self, block: MirBlockId) {
        self.blocks[block.0].scoped = true;
        self.blocks[block.0].scope_depth = self.scope_depth + 1;
    }

    fn terminate(&mut self, terminator: MirTerminator) -> Result<(), Diagnostic> {
        if matches!(terminator, MirTerminator::Return(_)) {
            self.release_cown_leases_from(0);
            let scopes = self.task_scopes.clone();
            for scope in scopes.into_iter().rev() {
                self.push_statement(MirStatement::ScopeExit { scope });
            }
        } else if let MirTerminator::Goto { target, .. } = &terminator {
            let depth = self.block_cown_depths[target.0];
            if depth < self.cown_leases.len() {
                self.release_cown_leases_from(depth);
            }
            let depth = self.block_task_depths[target.0];
            // Iterate by index: the loop body mutates self via push_statement,
            // so task_scopes cannot stay borrowed across iterations.
            for index in (depth.min(self.task_scopes.len())..self.task_scopes.len()).rev() {
                let scope = self.task_scopes[index];
                if !self.blocks[self.current.0]
                    .statements
                    .iter()
                    .any(|s| matches!(s,MirStatement::ScopeExit {scope:closed} if *closed == scope))
                {
                    self.push_statement(MirStatement::ScopeExit { scope });
                }
            }
        }
        let block = &mut self.blocks[self.current.0];
        if block.terminator.is_some() {
            return Err(Diagnostic::codegen("MIR block already has a terminator"));
        }
        block.terminator = Some(terminator);
        Ok(())
    }

    /// Emit cleanup for this control-flow edge without changing lexical state:
    /// sibling paths still own the leases while their bodies are lowered.
    fn release_cown_leases_from(&mut self, depth: usize) {
        // Iterate by index: the loop body mutates self via push_statement,
        // so cown_leases cannot stay borrowed across iterations.
        for index in (depth..self.cown_leases.len()).rev() {
            let cown = self.cown_leases[index];
            let destination = self.next_value(Type::Unit);
            self.push_statement(MirStatement::RuntimeCall {
                destination,
                intrinsic: RuntimeIntrinsic::CownRelease,
                arguments: vec![MirCallArgument {
                    parameter: 0,
                    value: cown,
                }],
            });
        }
    }

    fn next_value(&mut self, ty: Type) -> MirValueId {
        self.next_value_with_ownership(ty, ownership_for_type(ty, self.types))
    }

    fn next_value_with_ownership(&mut self, ty: Type, ownership: MirOwnership) -> MirValueId {
        let value = MirValueId(self.next_value);
        self.next_value += 1;
        self.value_types.push(ty);
        self.value_spans.push(self.current_span);
        self.value_ownership.push(ownership);
        value
    }

    fn push_statement(&mut self, statement: MirStatement) {
        self.blocks[self.current.0].statements.push(statement);
    }

    fn new_task_scope(&mut self) -> MirScopeId {
        let scope = MirScopeId(self.next_scope);
        self.next_scope += 1;
        scope
    }

    fn new_task(&mut self) -> MirTaskId {
        let task = MirTaskId(self.next_task);
        self.next_task += 1;
        task
    }

    fn new_continuation(
        &mut self,
        operation: crate::sema::EffectOperationId,
        suspend_block: MirBlockId,
        resume_block: MirBlockId,
        destination: MirValueId,
    ) -> MirContinuationId {
        self.new_continuation_with_kind(
            operation,
            suspend_block,
            resume_block,
            destination,
            MirContinuationKind::Suspending,
        )
    }

    fn new_continuation_with_kind(
        &mut self,
        operation: crate::sema::EffectOperationId,
        suspend_block: MirBlockId,
        resume_block: MirBlockId,
        destination: MirValueId,
        kind: MirContinuationKind,
    ) -> MirContinuationId {
        let id = MirContinuationId(self.next_continuation);
        self.next_continuation += 1;
        self.continuations.push(MirContinuation {
            id,
            kind,
            operation: Some(operation),
            callee: None,
            suspend_block,
            resume_block,
            resume_destination: Some(destination),
            // The first generation is sufficient until continuations can be
            // reused by a real scheduler state machine.
            generation: 0,
            locals_before_suspend: self.locals.len(),
            spill_values: Vec::new(),
            spill_slots: Vec::new(),
            frame_slots: Vec::new(),
        });
        id
    }

    fn discard_value(&mut self, value: Option<MirValueId>) {
        let Some(value) = value else {
            return;
        };
        if !matches!(
            self.value_ownership[value.0],
            MirOwnership::Owned | MirOwnership::Shared
        ) {
            return;
        }
        let destination = self.next_value(Type::Unit);
        self.push_statement(MirStatement::Drop { destination, value });
    }
}
