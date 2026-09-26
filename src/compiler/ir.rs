//! Typed linear IR shared by direct and CPS code generation.
use crate::parser::ast::{BinOp, Expr, FnDef, Item, Literal, Module as AstModule, Type};
use std::collections::HashMap;

pub(super) enum Constant {
    Int32(i32),
    Bool(bool),
    String(String),
}

#[derive(Clone, Debug)]
pub(crate) struct FunctionInfo {
    pub signature: crate::FunctionSignature,
    pub suspends: bool,
    pub frame_slots: usize,
}

#[derive(Clone)]
pub(super) struct Slot {
    pub(super) index: usize,
    pub(super) ty: Type,
}

pub(super) struct ScopeBranch {
    pub(super) name: String,
    pub(super) args: Vec<Option<Slot>>,
    pub(super) done: Slot,
}

pub(super) enum Op {
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
    pub(super) ops: Vec<Op>,
    pub(super) result: Option<Slot>,
    pub slot_count: usize,
    pub state_count: u32,
}

pub(crate) struct ModulePlan {
    pub functions: HashMap<String, FunctionInfo>,
    pub bodies: HashMap<String, Plan>,
}

impl ModulePlan {
    pub fn with_import_layouts(
        module: &AstModule,
        imports: &HashMap<String, usize>,
    ) -> Result<Self, String> {
        let mut functions = HashMap::new();
        for item in &module.items {
            let (name, params, ret, suspends) = match item {
                Item::FnDef(f) => (&f.name, &f.params, &f.ret_ty, f.suspends),
                Item::ExternFn(f) => (&f.name, &f.params, &f.ret_ty, f.suspends),
            };
            if name.starts_with("__joky_cps_")
                || name.starts_with("joky_cps_")
                || name.starts_with("joky_vm_")
            {
                return Err(format!("Reserved runtime function name `{name}`"));
            }
            let info = FunctionInfo {
                signature: crate::FunctionSignature::new(
                    params.iter().map(|p| p.ty.clone()).collect(),
                    ret.clone(),
                ),
                suspends,
                frame_slots: params.len(),
            };
            if functions.insert(name.clone(), info).is_some() {
                return Err(format!("Duplicate function `{name}`"));
            }
        }
        for (name, &count) in imports {
            let info = functions
                .get_mut(name)
                .ok_or_else(|| format!("CPS import `{name}` is not declared"))?;
            if !info.suspends
                || module
                    .items
                    .iter()
                    .any(|item| matches!(item, Item::FnDef(f) if &f.name == name))
            {
                return Err(format!("`{name}` is not a suspending import"));
            }
            if count < info.signature.params.len() || count >= i32::MAX as usize / 8 {
                return Err(format!("Invalid CPS frame size for `{name}`"));
            }
            info.frame_slots = count;
        }
        let mut bodies = HashMap::new();
        for item in &module.items {
            if let Item::FnDef(f) = item {
                let plan = Plan::new(f, &functions)?;
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
    fn new(f: &FnDef, functions: &HashMap<String, FunctionInfo>) -> Result<Self, String> {
        let mut plan = Self {
            ops: Vec::new(),
            result: None,
            slot_count: f.params.len(),
            state_count: 1,
        };
        let mut vars = HashMap::new();
        for (index, p) in f.params.iter().enumerate() {
            let slot = (p.ty != Type::Unit).then(|| Slot {
                index,
                ty: p.ty.clone(),
            });
            if vars.insert(p.name.clone(), slot).is_some() {
                return Err(format!("Duplicate parameter `{}`", p.name));
            }
        }
        plan.result = plan.expr(&f.body, &mut vars, functions, f.suspends)?;
        if slot_type(&plan.result) != f.ret_ty {
            return Err(format!(
                "Return type mismatch for `{}`: expected {}, got {}",
                f.name,
                f.ret_ty,
                slot_type(&plan.result)
            ));
        }
        Ok(plan)
    }

    fn slot(&mut self, ty: Type) -> Result<Slot, String> {
        if self.slot_count >= (i32::MAX as usize / 8) {
            return Err("CPS frame is too large".into());
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
    ) -> Result<Option<Slot>, String> {
        match expr {
            Expr::Literal(literal) => {
                let (ty, value) = match literal {
                    Literal::Int32(n) => (Type::Int32, Constant::Int32(*n)),
                    Literal::Bool(b) => (Type::Bool, Constant::Bool(*b)),
                    Literal::String(text) => (Type::String, Constant::String(text.clone())),
                };
                let slot = self.slot(ty)?;
                self.ops.push(Op::Literal(slot.clone(), value));
                Ok(Some(slot))
            }
            Expr::Ident(name) => vars
                .get(name)
                .cloned()
                .ok_or_else(|| format!("Undefined variable `{name}`")),
            Expr::FunctionRef(name) => {
                let function = functions
                    .get(name)
                    .ok_or_else(|| format!("Undefined function `{name}`"))?;
                if function.suspends
                    || function.signature.ret != Type::Int32
                    || function.signature.params.len() > 2
                    || function
                        .signature
                        .params
                        .iter()
                        .any(|ty| *ty != Type::Int32)
                {
                    return Err(format!(
                        "Function reference `{name}` must target a synchronous Int32 function with zero to two Int32 arguments"
                    ));
                }
                let slot = self.slot(Type::Named("vm.Function".into()))?;
                self.ops.push(Op::FunctionRef(slot.clone(), name.clone()));
                Ok(Some(slot))
            }
            Expr::Let {
                name,
                annotation,
                value,
            } => {
                let value = self.expr(value, vars, functions, suspends)?;
                if let Some(expected) = annotation {
                    if *expected != slot_type(&value) {
                        return Err(format!(
                            "Type annotation mismatch for `{name}`: expected {expected}, got {}",
                            slot_type(&value)
                        ));
                    }
                }
                vars.insert(name.clone(), value);
                Ok(None)
            }
            Expr::Block(stmts) => {
                let mut vars = vars.clone();
                let mut result = None;
                for stmt in stmts {
                    result = self.expr(stmt, &mut vars, functions, suspends)?;
                }
                Ok(result)
            }
            Expr::BinOp { lhs, op, rhs } => {
                let lhs = self
                    .expr(lhs, vars, functions, suspends)?
                    .ok_or("Arithmetic operand is Unit")?;
                let rhs = self
                    .expr(rhs, vars, functions, suspends)?
                    .ok_or("Arithmetic operand is Unit")?;
                if lhs.ty != Type::Int32 || rhs.ty != Type::Int32 {
                    return Err("Arithmetic operands must be Int32".into());
                }
                let dst = self.slot(Type::Int32)?;
                self.ops.push(Op::Binary(dst.clone(), op.clone(), lhs, rhs));
                Ok(Some(dst))
            }
            Expr::Call { callee, args } => {
                let info = functions
                    .get(callee)
                    .ok_or_else(|| format!("Undeclared function `{callee}`"))?;
                if info.suspends && !suspends {
                    return Err(format!("Caller of `{callee}` must be marked @suspends"));
                }
                if info.signature.params.len() != args.len() {
                    return Err(format!("Argument count mismatch for `{callee}`"));
                }
                let mut values = Vec::new();
                for (arg, ty) in args.iter().zip(&info.signature.params) {
                    let value = self.expr(arg, vars, functions, suspends)?;
                    if &slot_type(&value) != ty {
                        return Err(format!("Argument type mismatch for `{callee}`"));
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
            Expr::Scope(branches) => {
                if !suspends {
                    return Err("scope must be inside an @suspends function".into());
                }
                if branches.is_empty() {
                    return Err("scope requires at least one branch".into());
                }
                let mut planned = Vec::with_capacity(branches.len());
                for branch in branches {
                    let Expr::Call { callee, args } = branch else {
                        return Err("scope branches must be function calls".into());
                    };
                    let info = functions
                        .get(callee)
                        .ok_or_else(|| format!("Undeclared function `{callee}`"))?;
                    if !info.suspends || info.signature.ret != Type::Unit {
                        return Err(format!(
                            "scope branch `{callee}` must be an @suspends function returning Unit"
                        ));
                    }
                    if info.signature.params.len() != args.len() {
                        return Err(format!("Argument count mismatch for `{callee}`"));
                    }
                    let mut values = Vec::with_capacity(args.len());
                    for (arg, ty) in args.iter().zip(&info.signature.params) {
                        let value = self.expr(arg, vars, functions, suspends)?;
                        if &slot_type(&value) != ty {
                            return Err(format!("Argument type mismatch for `{callee}`"));
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
}

fn slot_type(slot: &Option<Slot>) -> Type {
    slot.as_ref().map_or(Type::Unit, |s| s.ty.clone())
}
