//! Typed slot IR with explicit control flow shared by all execution backends.
use crate::parser::ast::{
    BinOp, Expr, FnDef, Item, Literal, LogicalOp, Module as AstModule, Type, UnaryOp,
};
use crate::{Error, ErrorCode, ErrorPhase, Span, TypeRegistry};
use std::collections::HashMap;

pub(crate) enum Constant {
    Int32(i32),
    Bool(bool),
    String(String),
}

#[derive(Clone, Debug)]
pub(crate) struct FunctionInfo {
    pub signature: crate::FunctionSignature,
    pub suspends: bool,
    pub frame_slots: usize,
    pub span: Span,
}

#[derive(Clone)]
pub(crate) struct Slot {
    pub(crate) index: usize,
    pub(crate) ty: Type,
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
    Call {
        name: String,
        args: Vec<Option<Slot>>,
        result: Option<Slot>,
        state: Option<u32>,
    },
}

pub(crate) struct FunctionPlan {
    pub(crate) ops: Vec<Op>,
    pub(crate) op_spans: Vec<Span>,
    current_span: Span,
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
        let mut functions = HashMap::new();
        for item in &module.items {
            let (name, params, ret, suspends) = match item {
                Item::FnDef(f) => (&f.name, &f.params, &f.ret_ty, f.suspends),
                Item::ExternFn(f) => (&f.name, &f.params, &f.ret_ty, f.suspends),
            };
            for param in params {
                types
                    .validate(&param.ty)
                    .map_err(|error| error.with_span(*item_span(item)))?;
            }
            types
                .validate(ret)
                .map_err(|error| error.with_span(*item_span(item)))?;
            if name.starts_with("__joky_cps_")
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
                frame_slots: params.len(),
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
                let plan = FunctionPlan::new(f, &functions, types)?;
                bodies.insert(f.name.clone(), plan);
            }
        }
        for (name, body) in &bodies {
            functions.get_mut(name).unwrap().frame_slots = body.slot_count;
        }
        Ok(Self { functions, bodies })
    }
}

impl FunctionPlan {
    fn new(
        f: &FnDef,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
    ) -> Result<Self, Error> {
        let mut plan = Self {
            ops: Vec::new(),
            op_spans: Vec::new(),
            current_span: f.span,
            result: None,
            slot_count: f.params.len(),
            state_count: 1,
            label_count: 0,
            span: f.span,
        };
        let mut vars = HashMap::new();
        for (index, p) in f.params.iter().enumerate() {
            let slot = (p.ty != Type::Unit).then(|| Slot {
                index,
                ty: p.ty.clone(),
            });
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
        plan.result = plan.expr(&f.body, &mut vars, functions, f.suspends, types)?;
        if slot_type(&plan.result) != f.ret_ty {
            return Err(type_error(
                &f.span,
                ErrorCode::TypeMismatch,
                format!(
                    "Return type mismatch for `{}`: expected {}, got {}",
                    f.name,
                    f.ret_ty,
                    slot_type(&plan.result)
                ),
            ));
        }
        Ok(plan)
    }

    fn slot(&mut self, ty: Type) -> Result<Slot, Error> {
        if self.slot_count >= (i32::MAX as usize / 8) {
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
        self.slot_count += 1;
        Ok(slot)
    }

    fn emit(&mut self, op: Op) {
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
    ) -> Result<Option<Slot>, Error> {
        let previous = self.current_span;
        self.current_span = expr.span();
        let result = self.expr_inner(expr, vars, functions, suspends, types);
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
    ) -> Result<Option<Slot>, Error> {
        match expr {
            Expr::Literal { literal, .. } => {
                let (ty, value) = match literal {
                    Literal::Int32(n) => (Type::Int32, Constant::Int32(*n)),
                    Literal::Bool(b) => (Type::Bool, Constant::Bool(*b)),
                    Literal::String(text) => (Type::String, Constant::String(text.clone())),
                };
                let slot = self.slot(ty)?;
                self.emit(Op::Literal(slot.clone(), value));
                Ok(Some(slot))
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
                    self.snapshot(&binding.slot)
                } else {
                    Ok(binding.slot.clone())
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
                Ok(Some(slot))
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
                    types
                        .validate(annotation)
                        .map_err(|error| error.with_span(*span))?;
                }
                let value = self.expr(value, vars, functions, suspends, types)?;
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
                Ok(None)
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
                let value = self.expr(value, vars, functions, suspends, types)?;
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
                Ok(None)
            }
            Expr::While {
                condition, body, ..
            } => {
                let header = self.label_count;
                let body_label = header + 1;
                let exit = header + 2;
                self.label_count += 3;
                self.emit(Op::Label(header));
                // Condition-local declarations cannot escape an iteration.
                let test = self.expr(condition, &mut vars.clone(), functions, suspends, types)?;
                if slot_type(&test) != Type::Bool {
                    return Err(type_error(
                        &condition.span(),
                        ErrorCode::TypeMismatch,
                        "while condition must be Bool",
                    ));
                }
                self.emit(Op::Branch {
                    condition: test.unwrap(),
                    then_label: body_label,
                    else_label: exit,
                });
                self.emit(Op::Label(body_label));
                self.expr(body, &mut vars.clone(), functions, suspends, types)?;
                self.emit(Op::Jump(header));
                self.emit(Op::Label(exit));
                Ok(None)
            }
            Expr::If { .. } | Expr::Logical { .. } => {
                self.conditional_expr(expr, vars, functions, suspends, types)
            }
            Expr::Block { exprs: stmts, .. } => {
                let mut vars = vars.clone();
                let mut result = None;
                for stmt in stmts {
                    result = self.expr(stmt, &mut vars, functions, suspends, types)?;
                }
                Ok(result)
            }
            Expr::Sequence {
                exprs, trailing, ..
            } => {
                let mut result = None;
                for expr in exprs {
                    result = self.expr(expr, vars, functions, suspends, types)?;
                }
                Ok(if *trailing { None } else { result })
            }
            Expr::Unary { op, operand, span } => {
                let operand = self.expr(operand, vars, functions, suspends, types)?;
                let (symbol, expected) = match op {
                    UnaryOp::Neg => ("-", Type::Int32),
                    UnaryOp::Not => ("!", Type::Bool),
                };
                if operand.as_ref().map(|slot| &slot.ty) != Some(&expected) {
                    return Err(type_error(
                        span,
                        ErrorCode::TypeMismatch,
                        format!("Unary `{symbol}` operand must be {expected}"),
                    ));
                }
                let dst = self.slot(expected)?;
                self.emit(Op::Unary(dst.clone(), op.clone(), operand.unwrap()));
                Ok(Some(dst))
            }
            Expr::BinOp { lhs, op, rhs, span } => {
                let kind = if op.is_comparison() {
                    "Comparison"
                } else {
                    "Arithmetic"
                };
                let lhs = self
                    .expr(lhs, vars, functions, suspends, types)?
                    .ok_or_else(|| {
                        type_error(
                            span,
                            ErrorCode::TypeMismatch,
                            format!("{kind} operand is Unit"),
                        )
                    })?;
                let rhs = self
                    .expr(rhs, vars, functions, suspends, types)?
                    .ok_or_else(|| {
                        type_error(
                            span,
                            ErrorCode::TypeMismatch,
                            format!("{kind} operand is Unit"),
                        )
                    })?;
                let equality = matches!(op, BinOp::Eq | BinOp::Ne);
                let valid = lhs.ty == rhs.ty
                    && (lhs.ty == Type::Int32 || (equality && lhs.ty == Type::Bool));
                if !valid {
                    let expected = if equality {
                        "both Int32 or both Bool"
                    } else {
                        "Int32"
                    };
                    return Err(type_error(
                        span,
                        ErrorCode::TypeMismatch,
                        format!("{kind} operands must be {expected}"),
                    ));
                }
                let dst = self.slot(if op.is_comparison() {
                    Type::Bool
                } else {
                    Type::Int32
                })?;
                self.emit(Op::Binary(dst.clone(), op.clone(), lhs, rhs));
                Ok(Some(dst))
            }
            Expr::Call { callee, args, span } => {
                let info = functions.get(callee).ok_or_else(|| {
                    type_error(
                        span,
                        ErrorCode::UnknownName,
                        format!("Undeclared function `{callee}`"),
                    )
                })?;
                if info.suspends && !suspends {
                    return Err(type_error(
                        span,
                        ErrorCode::InvalidCall,
                        format!("Caller of `{callee}` must be marked @suspends"),
                    ));
                }
                if info.signature.params.len() != args.len() {
                    return Err(type_error(
                        span,
                        ErrorCode::InvalidCall,
                        format!("Argument count mismatch for `{callee}`"),
                    ));
                }
                let mut values = Vec::new();
                for (arg, ty) in args.iter().zip(&info.signature.params) {
                    let value = self.expr(arg, vars, functions, suspends, types)?;
                    if &slot_type(&value) != ty {
                        return Err(type_error(
                            &arg.span(),
                            ErrorCode::TypeMismatch,
                            format!("Argument type mismatch for `{callee}`"),
                        ));
                    }
                    values.push(value);
                }
                let result = if info.signature.ret == Type::Unit {
                    None
                } else {
                    Some(self.slot(info.signature.ret.clone())?)
                };
                let state = info.suspends.then(|| {
                    let state = self.state_count;
                    self.state_count += 1;
                    state
                });
                self.emit(Op::Call {
                    name: callee.clone(),
                    args: values,
                    result: result.clone(),
                    state,
                });
                Ok(result)
            }
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
                for branch in branches {
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
                    for (arg, ty) in args.iter().zip(&info.signature.params) {
                        let value = self.expr(arg, vars, functions, suspends, types)?;
                        if &slot_type(&value) != ty {
                            return Err(type_error(
                                &arg.span(),
                                ErrorCode::TypeMismatch,
                                format!("Argument type mismatch for `{callee}`"),
                            ));
                        }
                        values.push(value);
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
                Ok(None)
            }
        }
    }

    /// Lower conditional evaluation using the existing branch and copy IR.
    fn conditional_expr(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<Option<Slot>, Error> {
        match expr {
            Expr::If {
                condition,
                then_branch,
                else_branch,
                span,
            } => {
                let test = self.expr(condition, &mut vars.clone(), functions, suspends, types)?;
                if slot_type(&test) != Type::Bool {
                    return Err(type_error(
                        &condition.span(),
                        ErrorCode::TypeMismatch,
                        "if condition must be Bool",
                    ));
                }
                let then_label = self.label_count;
                let else_label = then_label + 1;
                let exit = then_label + 2;
                self.label_count += 3;
                self.emit(Op::Branch {
                    condition: test.unwrap(),
                    then_label,
                    else_label,
                });
                self.emit(Op::Label(then_label));
                let then_value =
                    self.expr(then_branch, &mut vars.clone(), functions, suspends, types)?;
                if else_branch.is_none() && then_value.is_some() {
                    return Err(type_error(
                        &then_branch.span(),
                        ErrorCode::TypeMismatch,
                        "if without else must have a Unit then branch",
                    ));
                }
                // Each incoming path writes the same result slot. Native direct
                // lowering joins it in SSA; CPS and bytecode retain it in frames.
                let result = self.snapshot(&then_value)?;
                self.emit(Op::Jump(exit));
                self.emit(Op::Label(else_label));
                if let Some(branch) = else_branch {
                    let else_value =
                        self.expr(branch, &mut vars.clone(), functions, suspends, types)?;
                    if slot_type(&then_value) != slot_type(&else_value) {
                        return Err(type_error(
                            span,
                            ErrorCode::TypeMismatch,
                            format!(
                                "if branches must have the same type, got {} and {}",
                                slot_type(&then_value),
                                slot_type(&else_value)
                            ),
                        ));
                    }
                    if let (Some(dst), Some(src)) = (&result, else_value) {
                        self.emit(Op::Copy(dst.clone(), src));
                    }
                }
                self.emit(Op::Label(exit));
                Ok(result)
            }
            Expr::Logical { op, lhs, rhs, .. } => {
                let left = self.expr(lhs, vars, functions, suspends, types)?;
                if slot_type(&left) != Type::Bool {
                    return Err(type_error(
                        &lhs.span(),
                        ErrorCode::TypeMismatch,
                        "Logical operand must be Bool",
                    ));
                }
                let result = self.snapshot(&left)?;
                let right_label = self.label_count;
                let exit = right_label + 1;
                self.label_count += 2;
                let (then_label, else_label) = match op {
                    LogicalOp::And => (right_label, exit),
                    LogicalOp::Or => (exit, right_label),
                };
                self.emit(Op::Branch {
                    condition: left.unwrap(),
                    then_label,
                    else_label,
                });
                self.emit(Op::Label(right_label));
                // The RHS is checked even when it cannot execute. Its local
                // declarations cannot escape a path that might be skipped.
                let right = self.expr(rhs, &mut vars.clone(), functions, suspends, types)?;
                if slot_type(&right) != Type::Bool {
                    return Err(type_error(
                        &rhs.span(),
                        ErrorCode::TypeMismatch,
                        "Logical operand must be Bool",
                    ));
                }
                self.emit(Op::Copy(result.as_ref().unwrap().clone(), right.unwrap()));
                self.emit(Op::Label(exit));
                Ok(result)
            }
            _ => unreachable!("expected a conditional expression"),
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

fn item_span(item: &Item) -> &Span {
    match item {
        Item::FnDef(function) => &function.span,
        Item::ExternFn(function) => &function.span,
    }
}

fn type_error(span: &Span, code: ErrorCode, message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::TypeCheck, code, message).with_span(*span)
}

#[cfg(test)]
mod tests;
