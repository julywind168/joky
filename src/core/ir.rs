//! Typed slot IR with explicit control flow shared by all execution backends.
use crate::parser::ast::{
    BinOp, Expr, FnDef, Item, Literal, LogicalOp, Module as AstModule, Type, UnaryOp,
};
use crate::{Error, ErrorCode, ErrorPhase, Span, TypeRegistry};
use std::{collections::HashMap, sync::Arc};

mod calls;
mod control_flow;
mod enums;
mod inference;
mod records;
mod tuples;
use super::integer::IntegerType;
use control_flow::unreachable_error;
use inference::integer_literal;

pub(crate) enum Constant {
    Int32(i32),
    Int64(i64),
    UInt32(u32),
    UInt64(u64),
    Bool(bool),
    String(String),
}

#[derive(Clone, Debug)]
pub(crate) struct FunctionInfo {
    pub signature: crate::FunctionSignature,
    pub suspends: bool,
    /// Division/shift failures or calls that may propagate a native runtime error.
    pub may_fail: bool,
    pub frame_slots: usize,
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
}

pub(crate) struct ScopeBranch {
    pub(crate) name: String,
    pub(crate) args: Vec<Option<Slot>>,
    pub(crate) done: Slot,
}

pub(crate) enum Op {
    /// Copy a value, including snapshots of mutable variable reads.
    Copy(Slot, Slot),
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
        branches: Vec<ScopeBranch>,
        state: u32,
    },
    Binary(Slot, BinOp, Slot, Slot),
    Unary(Slot, UnaryOp, Slot),
    /// Wrapping integer conversion; slots retain source and target types.
    Convert(Slot, Slot),
    Call {
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
}

pub(crate) struct FunctionPlan {
    pub(crate) ops: Vec<Op>,
    pub(crate) op_spans: Vec<Span>,
    current_span: Span,
    return_type: Type,
    nominals: Arc<HashMap<String, Type>>,
    return_label: Option<usize>,
    loops: Vec<LoopTargets>,
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
        let nominals = Arc::new(super::types::nominal_types(types, module)?);
        let mut functions = HashMap::new();
        for item in &module.items {
            let (name, params, ret, suspends) = match item {
                Item::FnDef(f) => (&f.name, &f.params, &f.ret_ty, f.suspends),
                Item::ExternFn(f) => (&f.name, &f.params, &f.ret_ty, f.suspends),
            };
            for ty in params.iter().map(|p| &p.ty).chain(std::iter::once(ret)) {
                validate_nominal_type(ty, &nominals, item_span(item))?;
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
                types
                    .validate(&param.ty)
                    .map_err(|error| error.with_span(*item_span(item)))?;
            }
            types
                .validate(ret)
                .map_err(|error| error.with_span(*item_span(item)))?;
            IntegerType::check_declaration(name, *item_span(item))?;
            if name.starts_with("__joky_cps_")
                || name.starts_with("__joky_runtime_")
                || name.starts_with("joky_runtime_")
                || name.starts_with("joky_cps_")
                || name.starts_with("joky_vm_")
            {
                return Err(type_error(
                    item_span(item),
                    ErrorCode::ReservedName,
                    format!("Reserved runtime function name `{name}`"),
                ));
            }
            let info = FunctionInfo {
                signature: crate::FunctionSignature::new(
                    params.iter().map(|p| p.ty.clone()).collect(),
                    ret.clone(),
                ),
                suspends,
                may_fail: matches!(item, Item::ExternFn(_)),
                frame_slots: params.iter().map(|p| p.ty.parameter_slots()).sum(),
                span: *item_span(item),
            };
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
        let mut bodies = HashMap::new();
        for item in &module.items {
            if let Item::FnDef(f) = item {
                let plan = FunctionPlan::new(f, &functions, types, &nominals)?;
                bodies.insert(f.name.clone(), plan);
            }
        }
        for (name, body) in &bodies {
            functions.get_mut(name).unwrap().frame_slots = body.slot_count;
        }
        // A monotone fixed point handles recursion without changing pure loops.
        loop {
            let mut changed = false;
            for (name, body) in &bodies {
                if !functions[name].may_fail
                    && body.ops.iter().any(|op| match op {
                        Op::Binary(_, op, _, _) if op.can_fail() => true,
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
        Ok(Self { functions, bodies })
    }
}

impl FunctionPlan {
    fn new(
        f: &FnDef,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
        nominals: &Arc<HashMap<String, Type>>,
    ) -> Result<Self, Error> {
        let parameter_slots = functions[&f.name].parameter_slots();
        let mut plan = Self {
            ops: Vec::new(),
            op_spans: Vec::new(),
            current_span: f.span,
            return_type: f.ret_ty.clone(),
            nominals: nominals.clone(),
            return_label: None,
            loops: Vec::new(),
            result: None,
            slot_count: parameter_slots,
            state_count: 1,
            label_count: 0,
            span: f.span,
        };
        // Aggregate results have a fixed region immediately after parameters in CPS.
        // Direct lowering uses the same slots with an internal result pointer.
        if f.ret_ty.is_aggregate() {
            plan.result = Some(plan.slot(f.ret_ty.clone())?);
        }
        let mut vars = HashMap::new();
        let mut index = 0;
        for p in &f.params {
            let slot = (p.ty != Type::Unit).then(|| Slot {
                index,
                ty: p.ty.clone(),
            });
            index += p.ty.parameter_slots();
            if vars
                .insert(
                    p.name.clone(),
                    Binding {
                        slot,
                        mutable: false,
                    },
                )
                .is_some()
            {
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
        if let Some(exit) = plan.return_label {
            plan.emit(Op::Label(exit));
        }
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
        Ok(slot)
    }

    fn emit(&mut self, op: Op) {
        if let Op::Copy(dst, src) = &op {
            if dst.ty.is_aggregate() {
                for (dst, src) in dst.scalars().zip(src.scalars()) {
                    self.ops.push(Op::Copy(dst, src));
                    self.op_spans.push(self.current_span);
                }
                return;
            }
        }
        self.ops.push(op);
        self.op_spans.push(self.current_span);
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
        let result = self.expr_inner(expr, vars, functions, suspends, types, expected);
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
        if let Some(constructor) = self.enum_constructor(expr, vars) {
            return self.construct_enum(constructor, vars, functions, suspends, types);
        }
        match expr {
            Expr::Tuple { .. } | Expr::Destructure { .. } => {
                self.tuple_expr(expr, vars, functions, suspends, types, expected)
            }
            Expr::Match { .. } => self.match_expr(expr, vars, functions, suspends, types, expected),
            Expr::Record { .. } | Expr::Field { .. } | Expr::FieldAssign { .. } => {
                self.record_expr(expr, vars, functions, suspends, types)
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
            Expr::Ident { name, span } => {
                let binding = vars.get(name).ok_or_else(|| {
                    type_error(
                        span,
                        ErrorCode::UnknownName,
                        format!("Undefined variable `{name}`"),
                    )
                })?;
                if binding.mutable {
                    // Later operands/arguments may assign to this binding. Keep
                    // the value observed at this point, including across Pending.
                    self.snapshot(&binding.slot).map(Flow::Value)
                } else {
                    Ok(Flow::Value(binding.slot.clone()))
                }
            }
            Expr::FunctionRef { name, span } => {
                let function = functions.get(name).ok_or_else(|| {
                    type_error(
                        span,
                        ErrorCode::UnknownName,
                        format!("Undefined function `{name}`"),
                    )
                })?;
                if function.suspends
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
                let slot = if mutable {
                    self.snapshot(&value)?
                } else {
                    value
                };
                vars.insert(name.clone(), Binding { slot, mutable });
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
                if let (Some(dst), Some(src)) = (binding.slot, value) {
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
            Expr::Block { exprs, .. } => self.sequence(
                exprs,
                expected,
                &mut vars.clone(),
                functions,
                suspends,
                types,
            ),
            Expr::Sequence {
                exprs, trailing, ..
            } => {
                let flow = self.sequence(
                    exprs,
                    if *trailing { None } else { expected },
                    vars,
                    functions,
                    suspends,
                    types,
                )?;
                Ok(match flow {
                    Flow::Value(_) if *trailing => Flow::Value(None),
                    flow => flow,
                })
            }
            Expr::Unary { op, operand, span } => {
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
                };
                if !valid {
                    return Err(type_error(
                        span,
                        ErrorCode::TypeMismatch,
                        match op {
                            UnaryOp::Neg => "Unary `-` operand must be Int32 or Int64",
                            UnaryOp::Not => "Unary `!` operand must be Bool",
                            UnaryOp::BitNot => "Unary `~` operand must be an integer",
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
            Expr::Call { .. } => self.call_expr(expr, vars, functions, suspends, types),
            Expr::Scope { branches, span } => {
                if !suspends {
                    return Err(type_error(
                        span,
                        ErrorCode::InvalidCall,
                        "scope must be inside an @suspends function",
                    ));
                }
                if branches.is_empty() {
                    return Err(type_error(
                        span,
                        ErrorCode::InvalidCall,
                        "scope requires at least one branch",
                    ));
                }
                let mut planned = Vec::with_capacity(branches.len());
                for (branch_index, branch) in branches.iter().enumerate() {
                    let Expr::Call {
                        callee,
                        args,
                        span: branch_span,
                    } = branch
                    else {
                        return Err(type_error(
                            span,
                            ErrorCode::InvalidCall,
                            "scope branches must be function calls",
                        ));
                    };
                    check_call_root(callee, vars, branch_span)?;
                    if IntegerType::conversion(callee).is_some() {
                        return Err(type_error(
                            branch_span,
                            ErrorCode::InvalidCall,
                            "scope branches must be @suspends functions returning Unit",
                        ));
                    }
                    let info = functions.get(callee).ok_or_else(|| {
                        type_error(
                            branch_span,
                            ErrorCode::UnknownName,
                            format!("Undeclared function `{callee}`"),
                        )
                    })?;
                    if !info.suspends || info.signature.ret != Type::Unit {
                        return Err(type_error(
                            branch_span,
                            ErrorCode::InvalidCall,
                            format!(
                            "scope branch `{callee}` must be an @suspends function returning Unit"
                        ),
                        ));
                    }
                    if info.signature.params.len() != args.len() {
                        return Err(type_error(
                            branch_span,
                            ErrorCode::InvalidCall,
                            format!("Argument count mismatch for `{callee}`"),
                        ));
                    }
                    let mut values = Vec::with_capacity(args.len());
                    for (index, (arg, ty)) in args.iter().zip(&info.signature.params).enumerate() {
                        let Flow::Value(value) =
                            self.expr_expected(arg, vars, functions, suspends, types, Some(ty))?
                        else {
                            if let Some(next) = args
                                .get(index + 1)
                                .or_else(|| branches.get(branch_index + 1))
                            {
                                return Err(unreachable_error(next));
                            }
                            return Ok(Flow::Diverged);
                        };
                        if &slot_type(&value) != ty {
                            return Err(type_error(
                                &arg.span(),
                                ErrorCode::TypeMismatch,
                                format!("Argument type mismatch for `{callee}`"),
                            ));
                        }
                        extend_argument(&mut values, value);
                    }
                    let done = self.slot(Type::Bool)?;
                    planned.push(ScopeBranch {
                        name: callee.clone(),
                        args: values,
                        done,
                    });
                }
                let state = self.state_count;
                self.state_count += 1;
                self.emit(Op::Scope {
                    branches: planned,
                    state,
                });
                Ok(Flow::Value(None))
            }
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

fn check_call_root(
    callee: &str,
    vars: &HashMap<String, Binding>,
    span: &Span,
) -> Result<(), Error> {
    if let Some((root, _)) = callee.split_once('.') {
        if vars.contains_key(root) {
            return Err(type_error(
                span,
                ErrorCode::InvalidCall,
                format!("`{root}` is a local value; field and method calls are not supported"),
            ));
        }
    }
    Ok(())
}

fn validate_nominal_type(
    ty: &Type,
    nominals: &HashMap<String, Type>,
    span: &Span,
) -> Result<(), Error> {
    let mut pending = vec![ty];
    let mut tuples = std::collections::HashSet::new();
    while let Some(ty) = pending.pop() {
        if let Type::Tuple(tuple) = ty {
            if tuples.insert(Arc::as_ptr(tuple)) {
                pending.extend(&tuple.elements);
            }
        }
        if let Some((name, _)) = ty.nominal() {
            if nominals.get(name) != Some(ty) {
                return Err(type_error(
                    span,
                    ErrorCode::UnknownType,
                    format!("Type `{name}` does not belong to this source module"),
                ));
            }
        }
    }
    Ok(())
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
