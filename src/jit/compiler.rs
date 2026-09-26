//! Joky-Lite JIT compiler based on Cranelift.

use super::cps::{self, ModulePlan};
use cranelift_codegen::ir::{types, FuncRef, InstBuilder, Value};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{default_libcall_names, FuncId, Linkage, Module};
use std::collections::HashMap;

use crate::parser::ast::{BinOp, Expr, FnDef, Literal, Module as JkModule, StringPart, Type};

// ── Type mapping ──────────────────────────────────────────────────────────────

pub fn jk_ty(ty: &Type) -> Option<types::Type> {
    match ty {
        Type::Int32 => Some(types::I32),
        Type::Bool => Some(types::I8),
        Type::Unit => None,
        Type::String | Type::Named(_) => Some(types::I64),
    }
}

// ── Compiler ────────────────────────────────────────────────────────────────

struct OwnedModule(Option<JITModule>);

impl std::ops::Deref for OwnedModule {
    type Target = JITModule;
    fn deref(&self) -> &JITModule {
        self.0.as_ref().unwrap()
    }
}
impl std::ops::DerefMut for OwnedModule {
    fn deref_mut(&mut self) -> &mut JITModule {
        self.0.as_mut().unwrap()
    }
}
impl Drop for OwnedModule {
    fn drop(&mut self) {
        // VM calls borrow the compiler; suspended calls hold an Rc to it.
        // No generated entry remains callable after the final owner drops.
        unsafe {
            self.0.take().unwrap().free_memory();
        }
    }
}

pub struct JitCompiler {
    module: OwnedModule,
    ctx: Context,
    builder_ctx: FunctionBuilderContext,
    func_ids: HashMap<String, FuncId>,
    cps_layouts: HashMap<String, CpsFrameLayout>,
    code_owners: Vec<std::rc::Rc<dyn std::any::Any>>,
}

/// Logical frame layout for a suspending function.
/// State 0 starts execution; subsequent states resume pending child calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpsFrameLayout {
    pub function: String,
    pub state_count: u32,
    pub locals: Vec<String>,
    pub slot_count: usize,
}

impl JitCompiler {
    /// Create a JIT compiler with named host function pointers.
    pub fn new(host_symbols: &[(&str, *const u8)]) -> Result<Self, String> {
        let mut builder = JITBuilder::new(default_libcall_names())
            .map_err(|e| format!("Failed to create JITBuilder: {}", e))?;
        // Make host symbols available to imported functions.
        for (name, ptr) in host_symbols {
            builder.symbol(*name, *ptr);
        }
        for (name, ptr) in crate::runtime::cps::symbols() {
            builder.symbol(name, ptr);
        }
        let module = JITModule::new(builder);
        Ok(JitCompiler {
            module: OwnedModule(Some(module)),
            ctx: Context::new(),
            builder_ctx: FunctionBuilderContext::new(),
            func_ids: HashMap::new(),
            cps_layouts: HashMap::new(),
            code_owners: Vec::new(),
        })
    }

    /// Create a compiler without host symbols.
    pub fn new_simple() -> Result<Self, String> {
        Self::new(&[])
    }

    pub fn compile_module(&mut self, jk_module: &JkModule) -> Result<Vec<String>, String> {
        self.compile_with_import_layouts(jk_module, &HashMap::new())
    }

    pub fn retain_code_owner(&mut self, owner: std::rc::Rc<dyn std::any::Any>) {
        self.code_owners.push(owner);
    }

    pub fn compile_with_import_layouts(
        &mut self,
        jk_module: &JkModule,
        imports: &HashMap<String, usize>,
    ) -> Result<Vec<String>, String> {
        let plans = ModulePlan::with_import_layouts(jk_module, imports)?;
        self.cps_layouts.clear();
        self.func_ids
            .extend(cps::declare_helpers(&mut *self.module)?);
        for item in &jk_module.items {
            let (name, linkage) = match item {
                crate::parser::ast::Item::FnDef(f) => (&f.name, Linkage::Export),
                crate::parser::ast::Item::ExternFn(f) => (&f.name, Linkage::Import),
            };
            let sig = cps::signature(&*self.module, &plans.functions[name]);
            let id = self
                .module
                .declare_function(name, linkage, &sig)
                .map_err(|e| format!("Failed to declare {name}: {e}"))?;
            self.func_ids.insert(name.clone(), id);
        }
        for (name, plan) in &plans.bodies {
            if plans.functions[name].suspends {
                self.cps_layouts.insert(
                    name.clone(),
                    CpsFrameLayout {
                        function: name.clone(),
                        state_count: plan.state_count,
                        locals: plan.locals.clone(),
                        slot_count: plan.slot_count,
                    },
                );
            }
        }

        let mut compiled = Vec::new();
        for item in &jk_module.items {
            if let crate::parser::ast::Item::FnDef(f) = item {
                self.define_function(f, &plans)?;
                compiled.push(f.name.clone());
            }
        }

        self.module
            .finalize_definitions()
            .map_err(|e| format!("finalize_definitions failed: {}", e))?;
        Ok(compiled)
    }

    /// Returned pointer is valid only while this compiler is alive.
    pub fn get_fn_ptr(&self, name: &str) -> Option<*const u8> {
        let id = self.func_ids.get(name)?;
        Some(self.module.get_finalized_function(*id))
    }

    pub fn cps_layout(&self, name: &str) -> Option<&CpsFrameLayout> {
        self.cps_layouts.get(name)
    }

    fn define_function(&mut self, func: &FnDef, plans: &ModulePlan) -> Result<(), String> {
        let id = *self
            .func_ids
            .get(&func.name)
            .ok_or_else(|| format!("Function {} is not declared", func.name))?;

        self.ctx.clear();
        self.ctx.func.signature = cps::signature(&*self.module, &plans.functions[&func.name]);
        let target_cfg = self.module.target_config();

        {
            let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut self.builder_ctx);

            let mut func_refs: HashMap<String, FuncRef> = HashMap::new();
            for (name, &fid) in &self.func_ids {
                let fref = self.module.declare_func_in_func(fid, builder.func);
                func_refs.insert(name.clone(), fref);
            }

            if func.suspends {
                cps::lower(
                    &mut builder,
                    &plans.bodies[&func.name],
                    &plans.functions,
                    &func_refs,
                    target_cfg.pointer_type(),
                );
            } else {
                let entry = builder.create_block();
                builder.append_block_params_for_function_params(entry);
                builder.switch_to_block(entry);
                builder.seal_block(entry);

                let mut vars: HashMap<String, Value> = HashMap::new();
                let mut i = 0;
                for p in &func.params {
                    if jk_ty(&p.ty).is_some() {
                        vars.insert(p.name.clone(), builder.block_params(entry)[i]);
                        i += 1;
                    }
                }

                let result = lower_expr(&mut builder, &func_refs, &vars, &func.body)?;
                match result {
                    Some(v) => {
                        builder.ins().return_(&[v]);
                    }
                    None => {
                        builder.ins().return_(&[]);
                    }
                }
            }
            builder.finalize(target_cfg);
        }

        self.module
            .define_function(id, &mut self.ctx)
            .map_err(|e| format!("define_function {} failed: {:?}", func.name, e))?;
        Ok(())
    }
}

// ── Expression lowering ───────────────────────────────────────────────────────

pub fn lower_expr(
    builder: &mut FunctionBuilder,
    func_refs: &HashMap<String, FuncRef>,
    vars: &HashMap<String, Value>,
    expr: &Expr,
) -> Result<Option<Value>, String> {
    match expr {
        Expr::Literal(lit) => lower_literal(builder, lit).map(Some),

        Expr::Ident(name) => vars
            .get(name)
            .copied()
            .map(Some)
            .ok_or_else(|| format!("Undefined variable `{}`", name)),

        Expr::BinOp { op, lhs, rhs } => {
            let l = lower_expr(builder, func_refs, vars, lhs)?
                .ok_or("Left arithmetic operand is Unit")?;
            let r = lower_expr(builder, func_refs, vars, rhs)?
                .ok_or("Right arithmetic operand is Unit")?;
            let v = match op {
                BinOp::Add => builder.ins().iadd(l, r),
                BinOp::Sub => builder.ins().isub(l, r),
                BinOp::Mul => builder.ins().imul(l, r),
                BinOp::Div => builder.ins().sdiv(l, r),
            };
            Ok(Some(v))
        }

        Expr::Block(stmts) => {
            let mut local_vars = vars.clone();
            let mut last = None;
            for stmt in stmts {
                last = lower_expr_mut(builder, func_refs, &mut local_vars, stmt)?;
            }
            Ok(last)
        }

        Expr::Call { callee, args } => {
            let fref = *func_refs
                .get(callee)
                .ok_or_else(|| format!("Undeclared function `{}`", callee))?;
            let mut arg_vals = Vec::new();
            for a in args {
                if let Some(v) = lower_expr(builder, func_refs, vars, a)? {
                    arg_vals.push(v);
                }
            }
            let call = builder.ins().call(fref, &arg_vals);
            Ok(builder.inst_results(call).first().copied())
        }

        Expr::Let { name, value } => {
            let _ = name;
            lower_expr(builder, func_refs, vars, value)
        }

        Expr::Scope(_) => Err("scope is planned for phase 6".to_string()),
    }
}

pub fn lower_expr_mut(
    builder: &mut FunctionBuilder,
    func_refs: &HashMap<String, FuncRef>,
    vars: &mut HashMap<String, Value>,
    expr: &Expr,
) -> Result<Option<Value>, String> {
    if let Expr::Let { name, value } = expr {
        let v = lower_expr(builder, func_refs, vars, value)?;
        if let Some(val) = v {
            vars.insert(name.clone(), val);
        }
        return Ok(None);
    }
    lower_expr(builder, func_refs, vars, expr)
}

pub fn lower_literal(builder: &mut FunctionBuilder, lit: &Literal) -> Result<Value, String> {
    match lit {
        Literal::Int32(n) => Ok(builder.ins().iconst(types::I32, *n as i64)),
        Literal::Bool(b) => Ok(builder.ins().iconst(types::I8, *b as i64)),
        Literal::String(parts) => {
            let mut s = String::new();
            for part in parts {
                match part {
                    StringPart::Literal(l) => s.push_str(l),
                    StringPart::Interpolated(_) => s.push_str("{?}"),
                }
            }
            let leaked: &'static str = Box::leak(s.into_boxed_str());
            Ok(builder.ins().iconst(types::I64, leaked.as_ptr() as i64))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser;

    unsafe extern "C" fn host_sleep(_frame: *mut crate::runtime::cps::CpsFrame) -> i32 {
        0
    }

    #[test]
    fn suspending_functions_have_cps_layouts() {
        let source = r#"
            extern fn host_sleep(ms: Int32) @suspends
            fn delayed(x: Int32) @suspends -> Int32 {
                let y = x + 1
                host_sleep(y)
                y + 1
            }
        "#;
        let module = parser::parse(source).unwrap();
        let ptr = host_sleep as *const u8;
        let mut compiler = JitCompiler::new(&[("host_sleep", ptr)]).unwrap();
        compiler.compile_module(&module).unwrap();
        let layout = compiler.cps_layout("delayed").unwrap();
        assert_eq!(layout.state_count, 2);
        assert_eq!(layout.locals, vec!["x", "y"]);
    }

    #[test]
    fn bool_values_and_shadowed_locals_survive_multiple_states() {
        use crate::runtime::cps::{CpsEntry, CpsFrame, INVALID_STATE};
        unsafe extern "C" fn yield_once(frame: *mut CpsFrame) -> i32 {
            let frame = &mut *frame;
            if frame.state == 0 {
                frame.state = 1;
                1
            } else {
                0
            }
        }
        extern "C" fn identity(value: bool) -> bool {
            value
        }
        let ast = parser::parse(
            r#"
            extern fn yield_once() @suspends
            extern fn identity(value: Bool) -> Bool
            fn f(value: Bool) @suspends -> Bool {
                let saved = identity(value)
                let local = { let saved = false yield_once() saved }
                yield_once()
                yield_once()
                saved
            }
        "#,
        )
        .unwrap();
        let mut compiler = JitCompiler::new(&[
            ("yield_once", yield_once as *const u8),
            ("identity", identity as *const u8),
        ])
        .unwrap();
        compiler.compile_module(&ast).unwrap();
        let entry: CpsEntry = unsafe { std::mem::transmute(compiler.get_fn_ptr("f").unwrap()) };
        let mut frame = CpsFrame::new(compiler.cps_layout("f").unwrap().slot_count);
        frame.slots[0] = 1;
        for state in 1..=3 {
            assert_eq!(unsafe { entry(&mut frame) }, 1);
            assert_eq!(frame.state, state);
        }
        assert_eq!(unsafe { entry(&mut frame) }, 0);
        assert_eq!(frame.result, 1);
        assert_eq!(unsafe { entry(&mut frame) }, INVALID_STATE);
        frame.state = 999;
        assert_eq!(unsafe { entry(&mut frame) }, INVALID_STATE);
    }
}
