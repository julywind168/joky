//! AOT compiler.
//!
//! Compile Joky-Lite to object files for linking with the system C compiler.
//! Shares CPS lowering and the platform calling convention with the JIT.

use crate::jit::cps::{self, ModulePlan};
use cranelift_codegen::ir::{FuncRef, InstBuilder, Value};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{default_libcall_names, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use std::collections::HashMap;
use std::path::Path;

use crate::jit::compiler::{jk_ty, lower_expr};
use crate::parser::ast::{FnDef, Module as JkModule};

pub struct AotCompiler {
    module: ObjectModule,
    ctx: Context,
    builder_ctx: FunctionBuilderContext,
    func_ids: HashMap<String, FuncId>,
    cps_layouts: HashMap<String, usize>,
}

impl AotCompiler {
    pub fn new(output_name: &str) -> Result<Self, String> {
        let mut flag_builder = settings::builder();
        // Emit position-independent code for shared libraries and PIE executables.
        flag_builder.set("is_pic", "true").unwrap();
        flag_builder.set("use_colocated_libcalls", "false").unwrap();

        let isa_builder =
            cranelift_native::builder().map_err(|e| format!("Unsupported platform: {}", e))?;
        let isa = isa_builder
            .finish(settings::Flags::new(flag_builder))
            .map_err(|e| format!("Failed to build ISA: {}", e))?;

        let obj_builder = ObjectBuilder::new(
            isa,
            output_name.as_bytes().to_vec(),
            default_libcall_names(),
        )
        .map_err(|e| format!("Failed to create ObjectBuilder: {}", e))?;
        let module = ObjectModule::new(obj_builder);

        Ok(AotCompiler {
            module,
            ctx: Context::new(),
            builder_ctx: FunctionBuilderContext::new(),
            func_ids: HashMap::new(),
            cps_layouts: HashMap::new(),
        })
    }

    /// Declare and define every function in a module.
    pub fn compile_module(&mut self, jk_module: &JkModule) -> Result<Vec<String>, String> {
        self.compile_with_import_layouts(jk_module, &HashMap::new())
    }

    /// Slot count required when registering an exported CPS function.
    pub fn cps_frame_slots(&self, name: &str) -> Option<usize> {
        self.cps_layouts.get(name).copied()
    }

    pub fn compile_with_import_layouts(
        &mut self,
        jk_module: &JkModule,
        imports: &HashMap<String, usize>,
    ) -> Result<Vec<String>, String> {
        let plans = ModulePlan::with_import_layouts(jk_module, imports)?;
        self.cps_layouts = plans
            .bodies
            .iter()
            .filter(|(name, _)| plans.functions[*name].suspends)
            .map(|(name, body)| (name.clone(), body.slot_count))
            .collect();
        self.func_ids
            .extend(cps::declare_helpers(&mut self.module)?);
        for item in &jk_module.items {
            let (name, linkage) = match item {
                crate::parser::ast::Item::FnDef(f) => (&f.name, Linkage::Export),
                crate::parser::ast::Item::ExternFn(f) => (&f.name, Linkage::Import),
            };
            let sig = cps::signature(&self.module, &plans.functions[name]);
            let id = self
                .module
                .declare_function(name, linkage, &sig)
                .map_err(|e| format!("Failed to declare {name}: {e}"))?;
            self.func_ids.insert(name.clone(), id);
        }

        // Define function bodies after all declarations are available.
        let mut compiled = Vec::new();
        for item in &jk_module.items {
            if let crate::parser::ast::Item::FnDef(f) = item {
                self.define_function(f, &plans)?;
                compiled.push(f.name.clone());
            }
        }
        Ok(compiled)
    }

    /// Write the compiled object to disk.
    pub fn write_object(self, path: &Path) -> Result<(), String> {
        let product = self.module.finish();
        let bytes = product
            .emit()
            .map_err(|e| format!("Failed to emit object: {}", e))?;
        std::fs::write(path, &bytes)
            .map_err(|e| format!("Failed to write {}: {}", path.display(), e))?;
        Ok(())
    }

    fn define_function(&mut self, func: &FnDef, plans: &ModulePlan) -> Result<(), String> {
        let id = *self
            .func_ids
            .get(&func.name)
            .ok_or_else(|| format!("Function {} is not declared", func.name))?;

        self.ctx.clear();
        self.ctx.func.signature = cps::signature(&self.module, &plans.functions[&func.name]);

        let target_cfg = self.module.target_config();

        {
            let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut self.builder_ctx);

            // Declare all known function references.
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

#[cfg(test)]
mod tests;
