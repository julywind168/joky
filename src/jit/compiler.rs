/// Joky-Lite JIT 编译器（基于 Cranelift 0.136.x）
use cranelift_codegen::ir::{types, AbiParam, FuncRef, InstBuilder, Value};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{default_libcall_names, FuncId, Linkage, Module};
use std::collections::HashMap;

use crate::parser::ast::{BinOp, Expr, FnDef, Literal, Module as JkModule, Param, StringPart, Type};

// ── 类型映射 ──────────────────────────────────────────────────────────────

pub fn jk_ty(ty: &Type) -> Option<types::Type> {
    match ty {
        Type::Int32         => Some(types::I32),
        Type::Bool          => Some(types::I8),
        Type::Unit          => None,
        Type::String
        | Type::Named(_)    => Some(types::I64),
    }
}

pub fn build_sig(module: &JITModule, params: &[Param], ret: &Type) -> cranelift_codegen::ir::Signature {
    let mut sig = module.make_signature();
    for p in params {
        if let Some(t) = jk_ty(&p.ty) {
            sig.params.push(AbiParam::new(t));
        }
    }
    if let Some(t) = jk_ty(ret) {
        sig.returns.push(AbiParam::new(t));
    }
    sig
}

// ── 编译器 ────────────────────────────────────────────────────────────────

pub struct JitCompiler {
    module: JITModule,
    ctx: Context,
    builder_ctx: FunctionBuilderContext,
    func_ids: HashMap<String, FuncId>,
}

impl JitCompiler {
    /// 创建 JIT 编译器，`host_symbols` 为需要注入的宿主函数名 → 函数指针
    pub fn new(host_symbols: &[(&str, *const u8)]) -> Result<Self, String> {
        let mut builder = JITBuilder::new(default_libcall_names())
            .map_err(|e| format!("无法创建 JITBuilder: {}", e))?;
        // 注入宿主函数符号，使 Import 声明可以解析
        for (name, ptr) in host_symbols {
            builder.symbol(*name, *ptr);
        }
        let module = JITModule::new(builder);
        Ok(JitCompiler {
            module,
            ctx: Context::new(),
            builder_ctx: FunctionBuilderContext::new(),
            func_ids: HashMap::new(),
        })
    }

    /// 无宿主符号的便捷构造（向后兼容）
    pub fn new_simple() -> Result<Self, String> {
        Self::new(&[])
    }

    pub fn compile_module(&mut self, jk_module: &JkModule) -> Result<Vec<String>, String> {
        for item in &jk_module.items {
            match item {
                crate::parser::ast::Item::FnDef(f) => {
                    let sig = build_sig(&self.module, &f.params, &f.ret_ty);
                    let id = self.module
                        .declare_function(&f.name, Linkage::Export, &sig)
                        .map_err(|e| format!("声明 {} 失败: {}", f.name, e))?;
                    self.func_ids.insert(f.name.clone(), id);
                }
                crate::parser::ast::Item::ExternFn(e) => {
                    let sig = build_sig(&self.module, &e.params, &e.ret_ty);
                    let id = self.module
                        .declare_function(&e.name, Linkage::Import, &sig)
                        .map_err(|e2| format!("声明 extern {} 失败: {}", e.name, e2))?;
                    self.func_ids.insert(e.name.clone(), id);
                }
            }
        }

        let mut compiled = Vec::new();
        for item in &jk_module.items {
            if let crate::parser::ast::Item::FnDef(f) = item {
                self.define_function(f)?;
                compiled.push(f.name.clone());
            }
        }

        self.module.finalize_definitions()
            .map_err(|e| format!("finalize_definitions 失败: {}", e))?;
        Ok(compiled)
    }

    pub fn get_fn_ptr(&self, name: &str) -> Option<*const u8> {
        let id = self.func_ids.get(name)?;
        Some(self.module.get_finalized_function(*id) as *const u8)
    }

    fn define_function(&mut self, func: &FnDef) -> Result<(), String> {
        let id = *self.func_ids.get(&func.name)
            .ok_or_else(|| format!("函数 {} 未声明", func.name))?;

        self.ctx.clear();
        self.ctx.func.signature = build_sig(&self.module, &func.params, &func.ret_ty);
        let target_cfg = self.module.target_config();

        {
            let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut self.builder_ctx);

            let mut func_refs: HashMap<String, FuncRef> = HashMap::new();
            for (name, &fid) in &self.func_ids {
                let fref = self.module.declare_func_in_func(fid, builder.func);
                func_refs.insert(name.clone(), fref);
            }

            let entry = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);
            builder.seal_block(entry);

            let mut vars: HashMap<String, Value> = HashMap::new();
            for (i, p) in func.params.iter().enumerate() {
                if jk_ty(&p.ty).is_some() {
                    vars.insert(p.name.clone(), builder.block_params(entry)[i]);
                }
            }

            let result = lower_expr(&mut builder, &func_refs, &vars, &func.body)?;
            match result {
                Some(v) => { builder.ins().return_(&[v]); }
                None    => { builder.ins().return_(&[]); }
            }
            builder.finalize(target_cfg);
        }

        self.module
            .define_function(id, &mut self.ctx)
            .map_err(|e| format!("define_function {} 失败: {:?}", func.name, e))?;
        Ok(())
    }
}

// ── 表达式 lowering ───────────────────────────────────────────────────────

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
            .ok_or_else(|| format!("未定义变量 `{}`", name)),

        Expr::BinOp { op, lhs, rhs } => {
            let l = lower_expr(builder, func_refs, vars, lhs)?.ok_or("BinOp 左侧为 Unit")?;
            let r = lower_expr(builder, func_refs, vars, rhs)?.ok_or("BinOp 右侧为 Unit")?;
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
                .ok_or_else(|| format!("未声明函数 `{}`", callee))?;
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

        Expr::Scope(_) => Err("scope 在阶段6实现".to_string()),
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
        Literal::Bool(b)  => Ok(builder.ins().iconst(types::I8, *b as i64)),
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
