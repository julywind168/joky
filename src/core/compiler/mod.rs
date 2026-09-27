//! Shared semantic analysis and Cranelift code generation.
pub(crate) mod ir;
mod lower;

use crate::parser::ast::{Item, Module as AstModule};
use crate::{resolver::BindingRegistry, Error, ErrorCode, ErrorPhase, TypeRegistry};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataDescription, FuncId, Linkage, Module};
use ir::{Constant, FunctionInfo, ModulePlan, Op};
use std::collections::{HashMap, HashSet};

/// Compile an already-resolved module (see resolver::resolve) and its Cranelift
/// contexts. Compilation contexts are temporary; only exports survive.
pub(crate) fn compile_with_bindings<M: Module>(
    module: &mut M,
    ast: &AstModule,
    imports: &HashMap<String, usize>,
    symbol_prefix: &str,
    bindings: &BindingRegistry,
    types: &TypeRegistry,
) -> Result<HashMap<String, (FuncId, FunctionInfo)>, Error> {
    bindings.validate_declarations(ast)?;
    let plan = ModulePlan::with_import_layouts_and_types(ast, imports, types)?;
    let mut ids = lower::declare_helpers(module)?;
    let mut declared_symbols: HashSet<_> = ids.keys().cloned().collect();
    for item in &ast.items {
        let (name, linkage) = match item {
            Item::FnDef(f) => (&f.name, Linkage::Export),
            Item::ExternFn(f) => (&f.name, Linkage::Import),
        };
        let signature = lower::signature(module, &plan.functions[name]);
        let symbol = if linkage == Linkage::Import {
            bindings
                .binding(name)
                .and_then(|binding| binding.symbol.clone())
        } else {
            None
        }
        .unwrap_or_else(|| format!("{symbol_prefix}{name}"));
        if !declared_symbols.insert(symbol.clone()) {
            let span = match item {
                Item::FnDef(f) => f.span,
                Item::ExternFn(f) => f.span,
            };
            return Err(Error::new_with_code(
                ErrorPhase::Abi,
                ErrorCode::AbiError,
                format!("Native symbol `{symbol}` conflicts with another declaration"),
            )
            .with_span(span));
        }
        let id = module
            .declare_function(&symbol, linkage, &signature)
            .map_err(|error| compile_error(error.to_string()))?;
        ids.insert(name.clone(), id);
    }
    let mut ctx = Context::new();
    let mut builder_ctx = FunctionBuilderContext::new();
    let mut exports = HashMap::new();
    let mut data_ids = HashMap::new();
    for body in plan.bodies.values() {
        for op in &body.ops {
            if let Op::Literal(_, Constant::String(text)) = op {
                if data_ids.contains_key(text) {
                    continue;
                }
                let id = module
                    .declare_anonymous_data(false, false)
                    .map_err(|error| compile_error(error.to_string()))?;
                let mut data = DataDescription::new();
                data.define(crate::runtime::string::encode(text));
                data.set_align(8);
                module
                    .define_data(id, &data)
                    .map_err(|error| compile_error(error.to_string()))?;
                data_ids.insert(text.clone(), id);
            }
        }
    }
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
            let strings = data_ids
                .iter()
                .map(|(text, &id)| (text.clone(), module.declare_data_in_func(id, builder.func)))
                .collect();
            lower::lower(
                &mut builder,
                info,
                &plan.bodies[&f.name],
                &plan.functions,
                &refs,
                target.pointer_type(),
                &strings,
            );
            builder.finalize(target);
        }
        module
            .define_function(ids[&f.name], &mut ctx)
            .map_err(|e| compile_error(format!("Failed to define {}: {e}", f.name)))?;
        exports.insert(f.name.clone(), (ids[&f.name], info.clone()));
    }
    Ok(exports)
}

fn compile_error(message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Compile, ErrorCode::LinkError, message)
}

#[cfg(test)]
mod tests {
    use super::ir::ModulePlan;
    use crate::{ErrorCode, TypeRegistry};
    use std::collections::HashMap;

    #[test]
    fn jit_and_aot_reject_incorrect_annotations_and_reserved_names() {
        for attribute in ["", "@suspends"] {
            for (source, message) in [
                (
                    format!("fn f() {attribute} {{ let x: Bool = 1 }}"),
                    "Type annotation mismatch",
                ),
                (
                    format!("fn f() {attribute} {{ let x: Int32 = {{}} }}"),
                    "Type annotation mismatch",
                ),
                (
                    format!("fn joky_cps_frame_new() {attribute} {{}}"),
                    "Reserved runtime",
                ),
                (
                    format!("fn __joky_cps_slots() {attribute} {{}}"),
                    "Reserved runtime",
                ),
            ] {
                let ast = crate::parser::parse(&source).unwrap();
                let error = crate::jit::compile_with_registry(
                    &ast,
                    &[],
                    &HashMap::new(),
                    vec![],
                    crate::runtime::suspension::ModuleId(1),
                    &crate::resolver::BindingRegistry::default(),
                    &TypeRegistry::default(),
                )
                .err()
                .expect("JIT must reject unsupported semantics");
                assert!(error.message().contains(message), "{source}: {error}");
                let error = crate::aot::AotCompiler::new("invalid")
                    .unwrap()
                    .compile_module(&ast)
                    .unwrap_err();
                assert!(error.message().contains(message), "{source}: {error}");
            }
        }
    }

    #[test]
    fn jit_and_aot_preserve_expression_error_codes_and_spans() {
        let source = "fn f() -> Int32 { let value: Bool = 1 }";
        let ast = crate::parser::parse(source).unwrap();
        let expected_start = source.find("let value").unwrap();
        let expected_end = source.find('1').unwrap() + 1;
        let jit_error = crate::jit::compile_with_registry(
            &ast,
            &[],
            &HashMap::new(),
            vec![],
            crate::runtime::suspension::ModuleId(2),
            &crate::resolver::BindingRegistry::default(),
            &TypeRegistry::default(),
        )
        .err()
        .expect("JIT should reject the mismatched annotation");
        let aot_error = crate::aot::AotCompiler::new("diagnostics")
            .unwrap()
            .compile_module(&ast)
            .unwrap_err();
        for error in [jit_error, aot_error] {
            assert_eq!(error.code(), ErrorCode::TypeMismatch);
            assert_eq!(
                error.span(),
                Some(crate::Span::new(expected_start, expected_end))
            );
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
            assert!(ModulePlan::with_import_layouts_and_types(
                &ast,
                &HashMap::from([(name.into(), slots)]),
                &TypeRegistry::default(),
            )
            .is_err());
        }
        let plan = ModulePlan::with_import_layouts_and_types(
            &ast,
            &HashMap::from([("host".into(), 10)]),
            &TypeRegistry::default(),
        )
        .unwrap();
        assert_eq!(plan.functions["host"].frame_slots, 10);
    }
}
