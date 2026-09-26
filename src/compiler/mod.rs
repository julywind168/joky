//! Shared semantic analysis and Cranelift code generation.
pub(crate) mod ir;
mod lower;

use crate::parser::ast::{Item, Module as AstModule};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Linkage, Module};
use ir::{FunctionInfo, ModulePlan};
use std::collections::HashMap;

/// Compilation contexts are temporary; only exports survive this function.
pub(crate) fn compile<M: Module>(
    module: &mut M,
    ast: &AstModule,
    imports: &HashMap<String, usize>,
    symbol_prefix: &str,
) -> Result<HashMap<String, (FuncId, FunctionInfo)>, String> {
    let plan = ModulePlan::with_import_layouts(ast, imports)?;
    let mut ids = lower::declare_helpers(module)?;
    for item in &ast.items {
        let (name, linkage) = match item {
            Item::FnDef(f) => (&f.name, Linkage::Export),
            Item::ExternFn(f) => (&f.name, Linkage::Import),
        };
        let signature = lower::signature(module, &plan.functions[name]);
        let id = module
            .declare_function(&format!("{symbol_prefix}{name}"), linkage, &signature)
            .map_err(|e| e.to_string())?;
        ids.insert(name.clone(), id);
    }
    let mut ctx = Context::new();
    let mut builder_ctx = FunctionBuilderContext::new();
    let mut exports = HashMap::new();
    for item in &ast.items {
        let Item::FnDef(f) = item else { continue };
        ctx.clear();
        let info = &plan.functions[&f.name];
        ctx.func.signature = lower::signature(module, info);
        let target = module.target_config();
        {
            let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
            let refs = ids
                .iter()
                .map(|(name, &id)| (name.clone(), module.declare_func_in_func(id, builder.func)))
                .collect();
            lower::lower(
                &mut builder,
                info,
                &plan.bodies[&f.name],
                &plan.functions,
                &refs,
                target.pointer_type(),
            );
            builder.finalize(target);
        }
        module
            .define_function(ids[&f.name], &mut ctx)
            .map_err(|e| format!("Failed to define {}: {e}", f.name))?;
        exports.insert(f.name.clone(), (ids[&f.name], info.clone()));
    }
    Ok(exports)
}

#[cfg(test)]
mod tests {
    use super::ir::ModulePlan;
    use std::collections::HashMap;

    #[test]
    fn jit_and_aot_reject_strings_and_incorrect_annotations() {
        for attribute in ["", "@suspends"] {
            for (source, message) in [
                (
                    format!("fn f() {attribute} {{ let x = \"text\" }}"),
                    "String literals",
                ),
                (
                    format!("fn f(x: String) {attribute} -> String {{ x }}"),
                    "String ABI",
                ),
                (
                    format!("extern fn host(x: String) {attribute} fn f() {{}}"),
                    "String ABI",
                ),
                (
                    format!("fn f() {attribute} {{ let x: Bool = 1 }}"),
                    "Type annotation mismatch",
                ),
                (
                    format!("fn f() {attribute} {{ let x: Int32 = {{}} }}"),
                    "Type annotation mismatch",
                ),
                (
                    format!("fn jk_cps_frame_new() {attribute} {{}}"),
                    "Reserved runtime",
                ),
                (
                    format!("fn __jk_cps_slots() {attribute} {{}}"),
                    "Reserved runtime",
                ),
            ] {
                let ast = crate::parser::parse(&source).unwrap();
                let error = crate::jit::compile(
                    &ast,
                    &[],
                    &HashMap::new(),
                    vec![],
                    crate::runtime::suspension::ModuleId(1),
                )
                .err()
                .expect("JIT must reject unsupported semantics");
                assert!(error.contains(message), "{source}: {error}");
                let error = crate::aot::AotCompiler::new("invalid")
                    .unwrap()
                    .compile_module(&ast)
                    .unwrap_err();
                assert!(error.contains(message), "{source}: {error}");
            }
        }
    }

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
