//! Typed linear IR shared by direct and CPS code generation.
use crate::parser::ast::{BinOp, Expr, FnDef, Item, Literal, Module as AstModule, Type};
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

pub(crate) struct ScopeBranch {
    pub(crate) name: String,
    pub(crate) args: Vec<Option<Slot>>,
    pub(crate) done: Slot,
}

pub(crate) enum Op {
    Literal(Slot, Constant),
    FunctionRef(Slot, String),
    Scope {
        branches: Vec<ScopeBranch>,
        state: u32,
    },
    Binary(Slot, BinOp, Slot, Slot),
    Call {
        name: String,
        args: Vec<Option<Slot>>,
        result: Option<Slot>,
        state: Option<u32>,
    },
}

pub(crate) struct Plan {
    pub(crate) ops: Vec<Op>,
    pub(crate) result: Option<Slot>,
    pub slot_count: usize,
    pub state_count: u32,
    span: Span,
}

pub(crate) struct ModulePlan {
    pub functions: HashMap<String, FunctionInfo>,
    pub bodies: HashMap<String, Plan>,
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
                types.validate(&param.ty).map_err(|error| {
                    type_error_at_code(item_span(item), ErrorCode::UnknownType, error)
                })?;
            }
            types.validate(ret).map_err(|error| {
                type_error_at_code(item_span(item), ErrorCode::UnknownType, error)
            })?;
            if name.starts_with("__joky_cps_")
                || name.starts_with("joky_cps_")
                || name.starts_with("joky_vm_")
            {
                return Err(type_error_at_code(
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
                return Err(type_error_at_code(
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
                return Err(type_error_at_code(
                    &info.span,
                    ErrorCode::InvalidCall,
                    format!("`{name}` is not a suspending import"),
                ));
            }
            if count < info.signature.params.len() || count >= i32::MAX as usize / 8 {
                return Err(type_error_at_code(
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
                let plan = Plan::new(f, &functions, types)?;
                bodies.insert(f.name.clone(), plan);
            }
        }
        for (name, body) in &bodies {
            functions.get_mut(name).unwrap().frame_slots = body.slot_count;
        }
        Ok(Self { functions, bodies })
    }
}

impl Plan {
    fn new(
        f: &FnDef,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
    ) -> Result<Self, Error> {
        let mut plan = Self {
            ops: Vec::new(),
            result: None,
            slot_count: f.params.len(),
            state_count: 1,
            span: f.span,
        };
        let mut vars = HashMap::new();
        for (index, p) in f.params.iter().enumerate() {
            let slot = (p.ty != Type::Unit).then(|| Slot {
                index,
                ty: p.ty.clone(),
            });
            if vars.insert(p.name.clone(), slot).is_some() {
                return Err(type_error_at_code(
                    &f.span,
                    ErrorCode::DuplicateDefinition,
                    format!("Duplicate parameter `{}`", p.name),
                ));
            }
        }
        plan.result = plan.expr(&f.body, &mut vars, functions, f.suspends, types)?;
        if slot_type(&plan.result) != f.ret_ty {
            return Err(type_error_at_code(
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
            return Err(self.error("CPS frame is too large"));
        }
        let slot = Slot {
            index: self.slot_count,
            ty,
        };
        self.slot_count += 1;
        Ok(slot)
    }

    fn expr(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Option<Slot>>,
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
                self.ops.push(Op::Literal(slot.clone(), value));
                Ok(Some(slot))
            }
            Expr::Ident { name, span } => vars.get(name).cloned().ok_or_else(|| {
                self.error_at_code(
                    span,
                    ErrorCode::UnknownName,
                    format!("Undefined variable `{name}`"),
                )
            }),
            Expr::FunctionRef { name, span } => {
                let function = functions.get(name).ok_or_else(|| {
                    self.error_at_code(
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
                    return Err(self.error_at_code(span, ErrorCode::InvalidCall, format!(
                        "Function reference `{name}` must target a synchronous Int32 function with zero to two Int32 arguments"
                    )));
                }
                let slot = self.slot(Type::Named("vm.Function".into()))?;
                self.ops.push(Op::FunctionRef(slot.clone(), name.clone()));
                Ok(Some(slot))
            }
            Expr::Let {
                name,
                annotation,
                value,
                span,
            } => {
                if let Some(annotation) = annotation {
                    types
                        .validate(annotation)
                        .map_err(|error| self.error_at_code(span, ErrorCode::UnknownType, error))?;
                }
                let value = self.expr(value, vars, functions, suspends, types)?;
                if let Some(expected) = annotation {
                    if *expected != slot_type(&value) {
                        return Err(self.error_at_code(
                            span,
                            ErrorCode::TypeMismatch,
                            format!(
                            "Type annotation mismatch for `{name}`: expected {expected}, got {}",
                            slot_type(&value)
                        ),
                        ));
                    }
                }
                vars.insert(name.clone(), value);
                Ok(None)
            }
            Expr::Block { exprs: stmts, .. } => {
                let mut vars = vars.clone();
                let mut result = None;
                for stmt in stmts {
                    result = self.expr(stmt, &mut vars, functions, suspends, types)?;
                }
                Ok(result)
            }
            Expr::BinOp { lhs, op, rhs, span } => {
                let lhs = self
                    .expr(lhs, vars, functions, suspends, types)?
                    .ok_or_else(|| {
                        self.error_at_code(
                            span,
                            ErrorCode::TypeMismatch,
                            "Arithmetic operand is Unit",
                        )
                    })?;
                let rhs = self
                    .expr(rhs, vars, functions, suspends, types)?
                    .ok_or_else(|| {
                        self.error_at_code(
                            span,
                            ErrorCode::TypeMismatch,
                            "Arithmetic operand is Unit",
                        )
                    })?;
                if lhs.ty != Type::Int32 || rhs.ty != Type::Int32 {
                    return Err(self.error_at_code(
                        span,
                        ErrorCode::TypeMismatch,
                        "Arithmetic operands must be Int32",
                    ));
                }
                let dst = self.slot(Type::Int32)?;
                self.ops.push(Op::Binary(dst.clone(), op.clone(), lhs, rhs));
                Ok(Some(dst))
            }
            Expr::Call { callee, args, span } => {
                let info = functions.get(callee).ok_or_else(|| {
                    self.error_at_code(
                        span,
                        ErrorCode::UnknownName,
                        format!("Undeclared function `{callee}`"),
                    )
                })?;
                if info.suspends && !suspends {
                    return Err(self.error_at_code(
                        span,
                        ErrorCode::InvalidCall,
                        format!("Caller of `{callee}` must be marked @suspends"),
                    ));
                }
                if info.signature.params.len() != args.len() {
                    return Err(self.error_at_code(
                        span,
                        ErrorCode::InvalidCall,
                        format!("Argument count mismatch for `{callee}`"),
                    ));
                }
                let mut values = Vec::new();
                for (arg, ty) in args.iter().zip(&info.signature.params) {
                    let value = self.expr(arg, vars, functions, suspends, types)?;
                    if &slot_type(&value) != ty {
                        return Err(self.error_at_code(
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
                self.ops.push(Op::Call {
                    name: callee.clone(),
                    args: values,
                    result: result.clone(),
                    state,
                });
                Ok(result)
            }
            Expr::Scope { branches, span } => {
                if !suspends {
                    return Err(self.error_at_code(
                        span,
                        ErrorCode::InvalidCall,
                        "scope must be inside an @suspends function",
                    ));
                }
                if branches.is_empty() {
                    return Err(self.error_at_code(
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
                        return Err(self.error_at_code(
                            span,
                            ErrorCode::InvalidCall,
                            "scope branches must be function calls",
                        ));
                    };
                    let info = functions.get(callee).ok_or_else(|| {
                        self.error_at_code(
                            branch_span,
                            ErrorCode::UnknownName,
                            format!("Undeclared function `{callee}`"),
                        )
                    })?;
                    if !info.suspends || info.signature.ret != Type::Unit {
                        return Err(self.error_at_code(
                            branch_span,
                            ErrorCode::InvalidCall,
                            format!(
                            "scope branch `{callee}` must be an @suspends function returning Unit"
                        ),
                        ));
                    }
                    if info.signature.params.len() != args.len() {
                        return Err(self.error_at_code(
                            branch_span,
                            ErrorCode::InvalidCall,
                            format!("Argument count mismatch for `{callee}`"),
                        ));
                    }
                    let mut values = Vec::with_capacity(args.len());
                    for (arg, ty) in args.iter().zip(&info.signature.params) {
                        let value = self.expr(arg, vars, functions, suspends, types)?;
                        if &slot_type(&value) != ty {
                            return Err(self.error_at_code(
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
                self.ops.push(Op::Scope {
                    branches: planned,
                    state,
                });
                Ok(None)
            }
        }
    }

    fn error(&self, message: impl Into<String>) -> Error {
        type_error_at(&self.span, message)
    }

    fn error_at_code(&self, span: &Span, code: ErrorCode, message: impl Into<String>) -> Error {
        type_error_at_code(span, code, message)
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

fn type_error(message: impl Into<String>) -> Error {
    Error::new(ErrorPhase::TypeCheck, message)
}

fn type_error_at(span: &Span, message: impl Into<String>) -> Error {
    type_error(message).with_span(*span)
}

fn type_error_at_code(span: &Span, code: ErrorCode, message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::TypeCheck, code, message).with_span(*span)
}
