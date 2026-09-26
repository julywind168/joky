/// AOT 编译器（阶段3）
///
/// 把 Joky-Lite 源码编译成 .o 目标文件，再用系统 cc 链接成可执行文件。
/// 与 JIT 共用相同的 lowering 逻辑，调用约定一致（System V / Windows x64）。
use cranelift_codegen::ir::{types, AbiParam, FuncRef, InstBuilder, Value};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{default_libcall_names, FuncId, Linkage, Module};
use cranelift_native;
use cranelift_object::{ObjectBuilder, ObjectModule};
use std::collections::HashMap;
use std::path::Path;

use crate::parser::ast::{BinOp, Expr, FnDef, Literal, Module as JkModule, Param, StringPart, Type};

// ── 类型映射（与 jit/compiler.rs 一致）────────────────────────────────────

fn jk_ty(ty: &Type) -> Option<types::Type> {
    match ty {
        Type::Int32          => Some(types::I32),
        Type::Bool           => Some(types::I8),
        Type::Unit           => None,
        Type::String
        | Type::Named(_)     => Some(types::I64),
    }
}

fn build_sig(module: &ObjectModule, params: &[Param], ret: &Type) -> cranelift_codegen::ir::Signature {
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

// ── AOT 编译器 ────────────────────────────────────────────────────────────

pub struct AotCompiler {
    module: ObjectModule,
    ctx: Context,
    builder_ctx: FunctionBuilderContext,
    func_ids: HashMap<String, FuncId>,
}

impl AotCompiler {
    pub fn new(output_name: &str) -> Result<Self, String> {
        let mut flag_builder = settings::builder();
        // 生成位置无关代码（PIE 可执行文件需要）
        flag_builder.set("is_pic", "false").unwrap();
        flag_builder.set("use_colocated_libcalls", "false").unwrap();

        let isa_builder = cranelift_native::builder()
            .map_err(|e| format!("不支持当前平台: {}", e))?;
        let isa = isa_builder
            .finish(settings::Flags::new(flag_builder))
            .map_err(|e| format!("构建 ISA 失败: {}", e))?;

        let obj_builder = ObjectBuilder::new(isa, output_name.as_bytes().to_vec(), default_libcall_names())
            .map_err(|e| format!("创建 ObjectBuilder 失败: {}", e))?;
        let module = ObjectModule::new(obj_builder);

        Ok(AotCompiler {
            module,
            ctx: Context::new(),
            builder_ctx: FunctionBuilderContext::new(),
            func_ids: HashMap::new(),
        })
    }

    /// 编译整个模块，声明并定义所有函数
    pub fn compile_module(&mut self, jk_module: &JkModule) -> Result<Vec<String>, String> {
        // 第一遍：声明
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

        // 第二遍：定义
        let mut compiled = Vec::new();
        for item in &jk_module.items {
            if let crate::parser::ast::Item::FnDef(f) = item {
                self.define_function(f)?;
                compiled.push(f.name.clone());
            }
        }
        Ok(compiled)
    }

    /// 将目标文件写到磁盘，返回路径
    pub fn write_object(self, path: &Path) -> Result<(), String> {
        let product = self.module.finish();
        let bytes = product.emit()
            .map_err(|e| format!("emit 目标文件失败: {}", e))?;
        std::fs::write(path, &bytes)
            .map_err(|e| format!("写入 {} 失败: {}", path.display(), e))?;
        Ok(())
    }

    fn define_function(&mut self, func: &FnDef) -> Result<(), String> {
        let id = *self.func_ids.get(&func.name)
            .ok_or_else(|| format!("函数 {} 未声明", func.name))?;

        self.ctx.clear();
        self.ctx.func.signature = build_sig(&self.module, &func.params, &func.ret_ty);

        let target_cfg = self.module.target_config();

        {
            let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut self.builder_ctx);

            // 预先引入所有已知函数引用
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

// ── 表达式 lowering（与 jit/compiler.rs 共用相同逻辑）──────────────────────

fn lower_expr(
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

fn lower_expr_mut(
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

fn lower_literal(builder: &mut FunctionBuilder, lit: &Literal) -> Result<Value, String> {
    match lit {
        Literal::Int32(n) => Ok(builder.ins().iconst(types::I32, *n as i64)),
        Literal::Bool(b)  => Ok(builder.ins().iconst(types::I8, *b as i64)),
        Literal::String(parts) => {
            // AOT 中字符串字面量作为全局数据段处理；阶段2简化：leak 指针
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
