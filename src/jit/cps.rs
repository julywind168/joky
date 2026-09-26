//! Linear, typed frame planning and CPS lowering for the expression language.
//! Every value, including call arguments and arithmetic temporaries, gets a
//! frame slot. Resumption therefore never depends on a previous native stack.

use std::collections::HashMap;

use cranelift_codegen::ir::{
    types, AbiParam, FuncRef, InstBuilder, MemFlagsData, Signature, Value,
};
use cranelift_frontend::{FunctionBuilder, Switch};
use cranelift_module::{FuncId, Linkage, Module};

use super::compiler::{jk_ty, lower_literal};
use crate::parser::ast::{BinOp, Expr, FnDef, Item, Literal, Module as AstModule, Type};
use crate::runtime::cps::{CpsFrame, DONE, INVALID_STATE};

#[derive(Clone)]
pub struct FunctionInfo {
    pub params: Vec<Type>,
    pub ret: Type,
    pub suspends: bool,
    pub frame_slots: usize,
}

#[derive(Clone)]
struct Slot {
    index: usize,
    ty: Type,
}

enum Op {
    Literal(Slot, Literal),
    Binary(Slot, BinOp, Slot, Slot),
    Call {
        name: String,
        args: Vec<Option<Slot>>,
        result: Option<Slot>,
        state: Option<u32>,
    },
}

pub struct Plan {
    ops: Vec<Op>,
    result: Option<Slot>,
    pub slot_count: usize,
    pub state_count: u32,
    pub locals: Vec<String>,
}

pub struct ModulePlan {
    pub functions: HashMap<String, FunctionInfo>,
    pub bodies: HashMap<String, Plan>,
}

impl ModulePlan {
    pub fn new(module: &AstModule) -> Result<Self, String> {
        Self::with_import_layouts(module, &HashMap::new())
    }

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
            if name.starts_with("__jk_cps_") {
                return Err(format!("Reserved runtime function name `{name}`"));
            }
            let info = FunctionInfo {
                params: params.iter().map(|p| p.ty.clone()).collect(),
                ret: ret.clone(),
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
            if count < info.params.len() || count >= i32::MAX as usize / 8 {
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
            locals: Vec::new(),
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
            plan.locals.push(p.name.clone());
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
        plan.locals.sort();
        plan.locals.dedup();
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
                let ty = match literal {
                    Literal::Int32(_) => Type::Int32,
                    Literal::Bool(_) => Type::Bool,
                    Literal::String(parts) => {
                        if parts
                            .iter()
                            .any(|p| matches!(p, crate::parser::ast::StringPart::Interpolated(_)))
                        {
                            return Err("String interpolation lowering is not implemented".into());
                        }
                        Type::String
                    }
                };
                let slot = self.slot(ty)?;
                self.ops.push(Op::Literal(slot.clone(), literal.clone()));
                Ok(Some(slot))
            }
            Expr::Ident(name) => vars
                .get(name)
                .cloned()
                .ok_or_else(|| format!("Undefined variable `{name}`")),
            Expr::Let { name, value } => {
                let value = self.expr(value, vars, functions, suspends)?;
                vars.insert(name.clone(), value);
                self.locals.push(name.clone());
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
                if info.params.len() != args.len() {
                    return Err(format!("Argument count mismatch for `{callee}`"));
                }
                let mut values = Vec::new();
                for (arg, ty) in args.iter().zip(&info.params) {
                    let value = self.expr(arg, vars, functions, suspends)?;
                    if &slot_type(&value) != ty {
                        return Err(format!("Argument type mismatch for `{callee}`"));
                    }
                    values.push(value);
                }
                let result = if info.ret == Type::Unit {
                    None
                } else {
                    Some(self.slot(info.ret.clone())?)
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
            Expr::Scope(_) => Err("scope is planned for phase 6".into()),
        }
    }
}

fn slot_type(slot: &Option<Slot>) -> Type {
    slot.as_ref().map_or(Type::Unit, |s| s.ty.clone())
}

pub fn signature<M: Module>(module: &M, info: &FunctionInfo) -> Signature {
    let mut sig = module.make_signature();
    if info.suspends {
        sig.params
            .push(AbiParam::new(module.target_config().pointer_type()));
        sig.returns.push(AbiParam::new(types::I32));
    } else {
        for ty in &info.params {
            if let Some(ty) = jk_ty(ty) {
                sig.params.push(AbiParam::new(ty));
            }
        }
        if let Some(ty) = jk_ty(&info.ret) {
            sig.returns.push(AbiParam::new(ty));
        }
    }
    sig
}

pub fn declare_helpers<M: Module>(module: &mut M) -> Result<HashMap<String, FuncId>, String> {
    let ptr = module.target_config().pointer_type();
    let mut ids = HashMap::new();
    for (name, params, returns) in [
        ("__jk_cps_slots", vec![ptr], vec![ptr]),
        ("__jk_cps_child", vec![ptr, ptr], vec![ptr]),
        ("__jk_cps_release_child", vec![ptr], vec![]),
    ] {
        let mut sig = module.make_signature();
        sig.params = params.into_iter().map(AbiParam::new).collect();
        sig.returns = returns.into_iter().map(AbiParam::new).collect();
        let id = module
            .declare_function(name, Linkage::Import, &sig)
            .map_err(|e| e.to_string())?;
        ids.insert(name.into(), id);
    }
    Ok(ids)
}

fn load(builder: &mut FunctionBuilder, slots: Value, slot: &Slot) -> Value {
    let value = builder.ins().load(
        types::I64,
        MemFlagsData::new(),
        slots,
        (slot.index * 8) as i32,
    );
    let ty = jk_ty(&slot.ty).unwrap();
    if ty == types::I64 {
        value
    } else {
        builder.ins().ireduce(ty, value)
    }
}

fn wide(builder: &mut FunctionBuilder, value: Value) -> Value {
    if builder.func.dfg.value_type(value) == types::I64 {
        value
    } else {
        builder.ins().uextend(types::I64, value)
    }
}

fn store(builder: &mut FunctionBuilder, slots: Value, slot: &Slot, value: Value) {
    let value = wide(builder, value);
    builder
        .ins()
        .store(MemFlagsData::new(), value, slots, (slot.index * 8) as i32);
}

fn helper(
    builder: &mut FunctionBuilder,
    refs: &HashMap<String, FuncRef>,
    name: &str,
    args: &[Value],
) -> Value {
    let call = builder.ins().call(refs[name], args);
    builder.inst_results(call)[0]
}

pub fn lower(
    builder: &mut FunctionBuilder,
    plan: &Plan,
    functions: &HashMap<String, FunctionInfo>,
    refs: &HashMap<String, FuncRef>,
    ptr_ty: types::Type,
) {
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    let frame = builder.block_params(entry)[0];
    let slots = helper(builder, refs, "__jk_cps_slots", &[frame]);
    let state_offset = std::mem::offset_of!(CpsFrame, state) as i32;
    let result_offset = std::mem::offset_of!(CpsFrame, result) as i32;
    let state = builder
        .ins()
        .load(types::I32, MemFlagsData::new(), frame, state_offset);
    let states: Vec<_> = (0..plan.state_count)
        .map(|_| builder.create_block())
        .collect();
    let invalid = builder.create_block();
    let mut switch = Switch::new();
    for (index, block) in states.iter().enumerate() {
        switch.set_entry(index as u128, *block);
    }
    switch.emit(builder, state, invalid);
    builder.switch_to_block(invalid);
    let error = builder.ins().iconst(types::I32, INVALID_STATE as i64);
    builder.ins().return_(&[error]);
    builder.switch_to_block(states[0]);

    for op in &plan.ops {
        match op {
            Op::Literal(dst, literal) => {
                let value = lower_literal(builder, literal).expect("validated literal");
                store(builder, slots, dst, value);
            }
            Op::Binary(dst, op, lhs, rhs) => {
                let lhs = load(builder, slots, lhs);
                let rhs = load(builder, slots, rhs);
                let value = match op {
                    BinOp::Add => builder.ins().iadd(lhs, rhs),
                    BinOp::Sub => builder.ins().isub(lhs, rhs),
                    BinOp::Mul => builder.ins().imul(lhs, rhs),
                    BinOp::Div => builder.ins().sdiv(lhs, rhs),
                };
                store(builder, slots, dst, value);
            }
            Op::Call {
                name,
                args,
                result,
                state: None,
            } => {
                let args: Vec<_> = args
                    .iter()
                    .flatten()
                    .map(|s| load(builder, slots, s))
                    .collect();
                let call = builder.ins().call(refs[name], &args);
                if let Some(dst) = result {
                    let value = builder.inst_results(call)[0];
                    store(builder, slots, dst, value);
                }
            }
            Op::Call {
                name,
                args,
                result,
                state: Some(state),
            } => {
                let count = builder
                    .ins()
                    .iconst(ptr_ty, functions[name].frame_slots as i64);
                let child = helper(builder, refs, "__jk_cps_child", &[frame, count]);
                let child_slots = helper(builder, refs, "__jk_cps_slots", &[child]);
                for (index, arg) in args.iter().enumerate() {
                    if let Some(arg) = arg {
                        let value = load(builder, slots, arg);
                        store(
                            builder,
                            child_slots,
                            &Slot {
                                index,
                                ty: arg.ty.clone(),
                            },
                            value,
                        );
                    }
                }
                let saved = builder.ins().iconst(types::I32, *state as i64);
                builder
                    .ins()
                    .store(MemFlagsData::new(), saved, frame, state_offset);
                builder.ins().jump(states[*state as usize], &[]);
                builder.switch_to_block(states[*state as usize]);
                // Re-enter only the child poll. Argument evaluation and side
                // effects before this point must not run again on resumption.
                let count = builder
                    .ins()
                    .iconst(ptr_ty, functions[name].frame_slots as i64);
                let child = helper(builder, refs, "__jk_cps_child", &[frame, count]);
                let call = builder.ins().call(refs[name], &[child]);
                let status = builder.inst_results(call)[0];
                let pending = builder.create_block();
                let ready = builder.create_block();
                builder.ins().brif(status, pending, &[], ready, &[]);
                builder.switch_to_block(pending);
                builder.ins().return_(&[status]);
                builder.switch_to_block(ready);
                if let Some(dst) = result {
                    let value =
                        builder
                            .ins()
                            .load(types::I64, MemFlagsData::new(), child, result_offset);
                    builder
                        .ins()
                        .store(MemFlagsData::new(), value, slots, (dst.index * 8) as i32);
                }
                builder.ins().call(refs["__jk_cps_release_child"], &[frame]);
            }
        }
    }
    if let Some(result) = &plan.result {
        let value = load(builder, slots, result);
        let value = wide(builder, value);
        builder
            .ins()
            .store(MemFlagsData::new(), value, frame, result_offset);
    }
    let done = builder.ins().iconst(types::I32, DONE as i64);
    builder
        .ins()
        .store(MemFlagsData::new(), done, frame, state_offset);
    let status = builder.ins().iconst(types::I32, 0);
    builder.ins().return_(&[status]);
    builder.seal_all_blocks();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_layouts_reject_invalid_sizes_and_non_imports() {
        let ast = crate::parser::parse(
            r#"
            extern fn host(x: Int32) @suspends -> Int32
            extern fn sync_host(x: Int32) -> Int32
            fn local(x: Int32) @suspends -> Int32 { host(x) }
        "#,
        )
        .unwrap();
        for (name, slots) in [
            ("host", 0),
            ("host", usize::MAX),
            ("missing", 1),
            ("local", 2),
            ("sync_host", 1),
        ] {
            assert!(
                ModulePlan::with_import_layouts(&ast, &HashMap::from([(name.into(), slots)]))
                    .is_err()
            );
        }
        let plan =
            ModulePlan::with_import_layouts(&ast, &HashMap::from([("host".into(), 10)])).unwrap();
        assert_eq!(plan.functions["host"].frame_slots, 10);
    }
}
