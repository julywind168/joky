//! Semantic analysis module
//!
//! Performs type checking, name resolution, and semantic validation

mod calls;
mod methods;
pub(crate) mod path_intrinsics;
mod ranges;
pub(crate) use methods::{
    trait_method_name, CURSOR_METHOD, DEBUG_METHOD, DROP_METHOD, FROM_STRING_METHOD, HASH_METHOD,
    ORDERING_NAME, ORD_METHOD, PARTIAL_EQ_METHOD, PARTIAL_ORD_METHOD, SHOW_METHOD,
};
mod checker;
mod control_flow;
mod declarations;
mod drop;
pub(crate) mod effects;
mod expectation;
mod expr_checker;
mod free_names;
mod import_methods;
pub(crate) use import_methods::trait_key;
mod import_type_values;
mod import_types;
mod imports;
mod instances;
mod pattern;
mod pattern_checker;
mod prelude;
mod scope;
mod symbol_table;
mod type_table;
pub(crate) use type_table::TypeRemap;
mod debug;
mod dynamic;
mod intrinsic_constraints;
mod intrinsic_effects;
mod type_values;
mod types;
mod validation;

#[cfg(test)]
mod annotation_tests;
#[cfg(test)]
mod expectation_tests;
#[cfg(test)]
mod trait_tests;
#[cfg(test)]
mod type_values_tests;
#[cfg(test)]
mod types_tests;
#[cfg(test)]
mod validation_tests;

use crate::diagnostic::SemanticError;
use crate::syntax::Program;
use crate::Diagnostic;

pub(crate) use effects::{
    EffectGroupSet, EffectId, EffectMode, EffectOperation, EffectOperationId, EffectSet,
};
pub(crate) use type_table::{
    CheckedTypes, ClassLayout, ClosureCaptureBinding, EnumLayout, EnumVariantLayout,
    ExternalFunction, ModuleInterface, ModuleTypes, StructLayout, TypeTable,
};
pub(crate) use types::{type_name, Type};

use checker::Checker;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct GenericCallInstance {
    pub(crate) function: String,
    pub(crate) arguments: Vec<Type>,
    /// Number of leading source arguments that are compile-time type values.
    pub(crate) compile_time_arguments: usize,
}

/// Checks a program for semantic correctness and returns type information
///
/// # Errors
///
/// Returns a `Diagnostic` if:
/// - the main function is missing
/// - the main function is defined multiple times
/// - there are type errors
/// - there are undefined names
/// - other semantic rules are violated
pub(crate) fn check_program(program: &Program) -> Result<CheckedTypes, Diagnostic> {
    check_program_inner(program, true, None)
}

pub(crate) fn check_module(program: &Program) -> Result<CheckedTypes, Diagnostic> {
    check_program_inner(program, false, None)
}

pub(crate) fn check_module_with_context(
    program: &Program,
    context: &crate::module::ModuleCompileContext,
) -> Result<CheckedTypes, Diagnostic> {
    check_program_inner(program, false, Some(context))
}

fn check_program_inner(
    program: &Program,
    require_main: bool,
    module_context: Option<&crate::module::ModuleCompileContext>,
) -> Result<CheckedTypes, Diagnostic> {
    let original_program = program;
    let prelude = prelude::functions(
        u32::try_from(prelude::source_end(program) + 1)
            .map_err(|_| Diagnostic::codegen("source exceeds AST identity space"))?,
    )?;
    let mut prepared = program.clone();
    prepared.functions.extend(prelude.iter().cloned());
    let program = &prepared;
    // Only the package entry point requires `main`; imported library modules do not.
    let main_functions: Vec<_> = program
        .functions
        .iter()
        .filter(|function| function.name == "main")
        .collect();

    if require_main && main_functions.is_empty() {
        return Err(SemanticError::MissingMainFunction.into());
    }

    if main_functions.len() > 1 {
        return Err(SemanticError::DuplicateMainFunction {
            span: main_functions[1].span,
        }
        .into());
    }

    // Type checking
    let mut checker = Checker::new();
    let source_end = prelude::source_end(program);
    checker.next_import_node = u32::try_from(source_end + 1)
        .map_err(|_| Diagnostic::codegen("source exceeds AST identity space"))?;
    if let Some(context) = module_context {
        checker.configure_module_context(context);
    }
    // Validate foreign declarations before type-function/generic registration
    // can treat them as source templates.
    for function in program.functions.iter().filter(|f| f.foreign.is_some()) {
        if !function.type_parameters.is_empty()
            || function
                .return_type
                .as_ref()
                .is_some_and(|ty| ty.is_name("type"))
        {
            return Err(SemanticError::FunctionNotSupported {
                name: "extern C declarations cannot be generic or return a type value".into(),
                span: function.span,
            }
            .into());
        }
    }
    for intrinsic in &program.intrinsic_types {
        checker.validate_intrinsic_kind(intrinsic)?;
        checker.register_intrinsic_type(intrinsic);
    }

    for definition in &program.enums {
        checker.declare_enum(definition)?;
    }

    // Reserve effect IDs up front so function/method signatures can refer to
    // them while their operation types are resolved in the second pass.
    for effect in &program.effects {
        checker.declare_effect(effect)?;
    }

    // First pass: declare all named types so fields can forward-reference other types
    for definition in &program.structs {
        checker.declare_struct(definition)?;
    }
    for definition in &program.classes {
        checker.declare_class(definition)?;
    }
    // Type functions must be available while resolving field and function
    // annotations, regardless of their source declaration order.
    for function in &program.functions {
        if function
            .return_type
            .as_ref()
            .is_some_and(|ty| ty.is_name("type"))
        {
            checker.register_type_function(function)?;
        }
    }
    for definition in &program.traits {
        checker.declare_trait(definition)?;
    }
    // Imported effects must be available to field types and impl signatures.
    checker.import_dependency_effects()?;
    for definition in &program.structs {
        checker.define_struct(definition)?;
    }
    for definition in &program.classes {
        checker.define_class(definition)?;
    }
    for implementation in &program.impls {
        checker.define_impl(implementation)?;
    }
    for definition in &program.enums {
        checker.define_enum(definition)?;
    }
    checker.validate_struct_layouts(&program.structs)?;
    checker.validate_enum_layouts(&program.enums)?;
    // Resolve effect signatures after named value types are registered so an
    // operation may use user-defined structs and enums in its payload/result.
    for effect in &program.effects {
        checker.define_effect(effect)?;
    }
    checker.refresh_dynamic_effects()?;
    checker.bind_intrinsic_effects(program, module_context)?;
    for intrinsic in &program.intrinsic_types {
        checker.validate_intrinsic_constraints(intrinsic)?;
    }
    if let Some(context) = module_context {
        for (name, (module, ty)) in &context.type_bindings {
            let table = &context.dependency_types[module];
            let ty = checker.import_abi_type(*module, table, *ty, crate::Span::new(0, 0))?;
            checker.type_bindings.insert(name.clone(), ty);
        }
    }
    for constant in &program.constants {
        checker.register_constant(constant)?;
    }
    checker.check_constants()?;
    for function in &program.functions {
        if !function
            .return_type
            .as_ref()
            .is_some_and(|ty| ty.is_name("type"))
        {
            checker.register_function(function)?;
        }
    }

    // A for in an earlier method body can call advance before its body is
    // checked. Seed its effects from the declaration; checking the impl later
    // narrows this to the operations actually used.
    for implementation in program
        .impls
        .iter()
        .filter(|implementation| implementation.trait_name == "Cursor")
    {
        let methods = if let Some(info) = checker.structs.get_mut(&implementation.type_name) {
            &mut info.methods
        } else {
            &mut checker
                .classes
                .get_mut(&implementation.type_name)
                .expect("checked Cursor target")
                .methods
        };
        let signature = methods
            .get_mut(CURSOR_METHOD)
            .expect("checked Cursor method");
        for group in signature.declared_effects.iter() {
            for operation in checker.effects.all_operations(group) {
                signature.used_effects.insert(operation);
            }
        }
    }

    // Second pass: check type default values, methods, and function bodies
    for structure in &program.structs {
        checker.check_struct(structure)?;
    }
    for class in &program.classes {
        checker.check_class(class)?;
    }
    for implementation in &program.impls {
        checker.check_impl(implementation)?;
    }
    // Check non-main functions first so a missing `-> T` is reported at the
    // definition instead of as a confusing Unit use at a later call site.
    for function in program
        .functions
        .iter()
        .filter(|function| function.name != "main")
    {
        checker.check_function(function)?;
    }
    for function in program
        .functions
        .iter()
        .filter(|function| function.name == "main")
    {
        checker.check_function(function)?;
    }
    let queried_type = module_context
        .and_then(|context| context.type_query.as_ref())
        .map(|query| checker.resolve_type(query))
        .transpose()?;
    checker.prepare_instance_types(program)?;
    // Equality, Debug, and sorting use head/tail/get on checked cursors.
    // Their result layouts must exist before HIR.
    for id in 0..checker.list_types.len() {
        checker::intern_option(&mut checker.option_types, checker.list_types[id]);
        checker::intern_option(&mut checker.option_types, Type::List(id));
    }

    let struct_layouts = checker.struct_layouts();
    let mut struct_defaults = vec![Vec::new(); struct_layouts.len()];
    for structure in checker.structs.values() {
        struct_defaults[structure.id] = structure.defaults.clone();
    }
    let class_layouts = checker.class_layouts();
    let enum_layouts = checker.enum_layouts();
    let mut function_declared_effects: std::collections::HashMap<String, EffectGroupSet> = checker
        .functions
        .iter()
        .map(|(name, signature)| (name.clone(), signature.declared_effects.clone()))
        .collect();
    for info in checker.structs.values() {
        for (name, signature) in &info.methods {
            function_declared_effects.insert(
                format!("struct_{}_{}", info.id, name),
                signature.declared_effects.clone(),
            );
        }
    }
    for info in checker.classes.values() {
        for (name, signature) in &info.methods {
            function_declared_effects.insert(
                format!("class_{}_{}", info.id, name),
                signature.declared_effects.clone(),
            );
        }
    }
    let mut function_effects: std::collections::HashMap<String, EffectSet> = checker
        .functions
        .iter()
        .map(|(name, signature)| (name.clone(), signature.used_effects.clone()))
        .collect();
    // Methods use a type-qualified key so overloads on different receivers do
    // not collide with one another (or with free functions).  HIR uses the
    // same key when deciding whether a method can be inlined through a
    // resumable handler.
    for info in checker.structs.values() {
        for (name, signature) in &info.methods {
            function_effects.insert(
                format!("struct_{}_{}", info.id, name),
                signature.used_effects.clone(),
            );
        }
    }
    for info in checker.classes.values() {
        for (name, signature) in &info.methods {
            function_effects.insert(
                format!("class_{}_{}", info.id, name),
                signature.used_effects.clone(),
            );
        }
    }
    let effects = checker.effects.clone();
    let handler_operations = checker.handler_operations.clone();
    let effect_operations = checker.effect_operations.clone();
    let mut public_templates = std::collections::HashMap::new();
    for function in &program.functions {
        if function.visibility == crate::syntax::Visibility::Public
            && !function.type_parameters.is_empty()
            && checker.functions.contains_key(&function.name)
        {
            let signature = &checker.functions[&function.name];
            public_templates.insert(
                function.name.clone(),
                crate::module::generics::GenericTemplate {
                    parameters: signature.type_parameters.clone(),
                    bounds: signature.type_parameter_bounds.clone(),
                    abi: ExternalFunction {
                        region_contract: None,
                        parameters: signature.parameters.clone(),
                        parameter_borrows: signature.parameter_borrows.clone(),
                        return_type: signature.return_type,
                        suspends: signature.used_effects.may_suspend(&checker.effects),
                    },
                    effects: function
                        .effect_names
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                },
            );
        }
    }
    let explicit_hash_impls = TypeTable::explicit_hash_impls(&checker);
    let mut table = CheckedTypes::new(
        checker.types,
        checker.tuple_types,
        checker.c_pointers,
        checker.c_arrays,
        struct_layouts,
        struct_defaults,
        class_layouts,
        enum_layouts,
        checker.option_types,
        checker.result_types,
        checker.list_types,
        checker.maps,
        checker.cowns,
        checker.function_types,
        checker.generic_calls,
        checker.type_value_calls,
        program
            .constants
            .iter()
            .map(|constant| (constant.name.clone(), constant.value.clone()))
            .collect(),
        checker.constant_references,
        checker.show_impls,
        checker.associated_types,
        checker.trait_associated_impls,
        effects,
        function_declared_effects,
        function_effects,
        handler_operations,
        effect_operations,
        checker.closure_captures,
        checker.compile_time_bindings,
        checker.annotation_types,
        checker.external_symbols,
        checker.external_signatures,
    );
    table.dynamic_types = checker.dynamic_types;
    table.prelude_functions = prelude;
    table.local_bindings = checker.local_bindings;
    table.closure_capture_bindings = checker.closure_capture_bindings;
    table.for_into_cursor = checker.for_into_cursor;
    table.interface.trait_definitions = checker.traits;
    table
        .validate_c_layouts()
        .map_err(|name| SemanticError::FunctionNotSupported {
            name,
            span: crate::Span::new(0, source_end),
        })?;
    table.imported_constants = checker.imported_constants;
    table.interface.intrinsic_types = program.intrinsic_types.clone();
    for intrinsic in &mut table.interface.intrinsic_types {
        for method in &mut intrinsic.methods {
            *method = checker.intrinsic_methods[&intrinsic.name][&method.name].clone();
        }
    }
    table.queried_type = queried_type;
    table.interface.module_identity = checker.definition_module;
    table.resolved_method_calls = checker.resolved_method_calls;
    table.interface.method_symbols = checker.method_symbols;
    table.interface.methods = checker.imported_method_abis;
    table.trait_implementations = checker.trait_impls;
    table.explicit_hash_impls = explicit_hash_impls;
    table.interface.type_program = program
        .functions
        .iter()
        .any(|f| {
            f.visibility == crate::syntax::Visibility::Public
                && f.return_type.as_ref().is_some_and(|ty| ty.is_name("type"))
        })
        .then(|| Box::new(original_program.clone()));
    table.interface.module_imports = checker.import_aliases;
    table.interface.module_exports = checker.dependency_exports;
    table.interface.standard_modules = checker.standard_modules;
    table.interface.public_templates = public_templates;
    table.imported_templates = checker.imported_templates;
    for (name, request) in checker.generic_requests {
        let key = table.instance_name(&name, &request.arguments);
        table.interface.generic_requests.insert(key, request);
    }
    for constant in &program.constants {
        if constant.visibility == crate::syntax::Visibility::Public {
            let value =
                crate::module::constants::ConstantValue::from_expression(&constant.value, &table)?;
            table
                .interface
                .public_constants
                .insert(constant.name.clone(), value);
        }
    }
    table.prepare_struct_defaults();
    Ok(table)
}

#[cfg(test)]
mod tests {
    use crate::syntax;

    use super::*;

    fn check(source: &str) -> Result<CheckedTypes, Diagnostic> {
        check_program(&syntax::parse_program(source)?)
    }

    #[test]
    fn literals_default_to_int32_and_float64() {
        let program =
            syntax::parse_program("fn main() { let integer = 1; let float = 1.5 }").unwrap();
        let crate::syntax::ExprKind::Block(expressions) = &program.functions[0].body.kind else {
            panic!("expected a block");
        };
        let crate::syntax::ExprKind::Let { value: integer, .. } = &expressions[0].kind else {
            panic!("expected an integer binding");
        };
        let crate::syntax::ExprKind::Let { value: float, .. } = &expressions[1].kind else {
            panic!("expected a float binding");
        };
        let types = check_program(&program).unwrap();
        assert_eq!(types.get(integer), Type::I32);
        assert_eq!(types.get(float), Type::F64);
    }

    #[test]
    fn main_accepts_only_unit_or_result_unit_string() {
        assert!(check("fn main() -> Result(Unit, String) { Ok(()) }").is_ok());
        let error = check("fn main() -> Result(Int32, String) { Ok(1) }").unwrap_err();
        assert!(error
            .to_string()
            .contains("main may return only Result(Unit, String)"));
    }

    #[test]
    fn question_propagates_results_with_a_shared_error_type() {
        assert!(check(
            "fn read() -> Result(String, String) { Ok(\"ok\") }\n\
             fn main() -> Result(Unit, String) { let value = read()?; println(value); Ok(()) }"
        )
        .is_ok());
    }

    #[test]
    fn byte_string_literals_have_the_bytes_type() {
        let types = check("fn main() { b\"payload\" }").unwrap();
        let program = syntax::parse_program("fn main() { b\"payload\" }").unwrap();
        let crate::syntax::ExprKind::Block(expressions) = &program.functions[0].body.kind else {
            panic!("expected block");
        };
        assert_eq!(types.get(&expressions[0]), Type::Bytes);
    }

    #[test]
    fn closures_have_function_types_and_are_callable() {
        let types = check("fn main() { let f = fn (x: Int32) -> Int32 { x + 1 }; f(2) }").unwrap();
        let program =
            syntax::parse_program("fn main() { let f = fn (x: Int32) -> Int32 { x + 1 }; f(2) }")
                .unwrap();
        let crate::syntax::ExprKind::Block(expressions) = &program.functions[0].body.kind else {
            panic!("expected block");
        };
        let crate::syntax::ExprKind::Let { value, .. } = &expressions[0].kind else {
            panic!("expected closure binding");
        };
        assert!(matches!(types.get(value), Type::Function(_)));
    }

    #[test]
    fn closure_function_types_preserve_suspending_effects() {
        let program = syntax::parse_program(
            "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
             fn main() effects { time } {\n\
                 let f = fn () -> Unit { time.sleep(1ms) }; f()\n\
             }",
        )
        .unwrap();
        let types = super::check_program(&program).expect("suspending closure should type check");
        let crate::syntax::ExprKind::Block(expressions) = &program.functions[0].body.kind else {
            panic!("expected block");
        };
        let crate::syntax::ExprKind::Let { value, .. } = &expressions[0].kind else {
            panic!("expected closure binding");
        };
        let Type::Function(id) = types.get(value) else {
            panic!("expected function type")
        };
        let function_type = types.function_type(id);
        assert!(function_type.suspends);
    }

    #[test]
    fn function_type_annotations_match_closures() {
        check("fn main() { let f: fn(Int32) -> Int32 = fn (x: Int32) -> Int32 { x + 1 }; f(2) }")
            .expect("function type annotation should be accepted");
    }

    #[test]
    fn named_function_types_preserve_call_labels() {
        check(
            "fn main() { \
             let f: fn(left: Int32, right: Int32) -> Int32 = \
                 fn (x: Int32, y: Int32) -> Int32 { x + y }; \
             f(right: 2, left: 1) \
             }",
        )
        .expect("named function type should accept labeled calls");
        let error = check(
            "fn main() { \
             let f: fn(Int32, Int32) -> Int32 = fn (x: Int32, y: Int32) -> Int32 { x + y }; \
             f(right: 2, left: 1) \
             }",
        )
        .expect_err("positional function type should reject labels");
        assert!(error.to_string().contains("unknown function 'right'"));
    }

    #[test]
    fn inferred_closure_parameters_require_a_function_type_context() {
        let error = check("fn main() { let f = fn (value) { value }; f(1) }")
            .expect_err("untyped closure parameter should require context");
        assert!(error
            .to_string()
            .contains("closure parameter types require a function type context"));
    }

    #[test]
    fn ordinary_closures_reject_owned_captures_but_move_closures_allow_them() {
        let ordinary = check(
            "class Counter { var value: Int32 = 0 }\n\
             fn main() { let c = Counter(value: 1); let f = fn () -> Int32 { c.value }; f() }",
        );
        assert!(ordinary
            .expect_err("ordinary closure should reject class capture")
            .to_string()
            .contains("use 'move fn'"));
        check(
            "class Counter { var value: Int32 = 0 }\n\
             fn main() { let c = Counter(value: 1); let f = move fn () -> Int32 { c.value }; f() }",
        )
        .expect("move closure should accept class capture");
    }

    #[test]
    fn mutable_closure_capture_metadata_preserves_binding_identity_and_order() {
        let source =
            "fn main() { let a = 2; var z = 1; let f = move fn () -> Int32 { z = z + a; z }; () }";
        let program = syntax::parse_program(source).unwrap();
        let crate::syntax::ExprKind::Block(expressions) = &program.functions[0].body.kind else {
            panic!("expected block");
        };
        let crate::syntax::ExprKind::Let { value, .. } = &expressions[2].kind else {
            panic!("expected closure binding");
        };
        let types = check_program(&program).unwrap();
        assert_eq!(
            types.closure_capture_bindings(value.id),
            &[
                ClosureCaptureBinding {
                    name: "a".into(),
                    declaration: Some(expressions[0].id),
                    mutable: false,
                },
                ClosureCaptureBinding {
                    name: "z".into(),
                    declaration: Some(expressions[1].id),
                    mutable: true,
                },
            ]
        );
        assert_eq!(
            types.closure_captures(value.id),
            &[("a".into(), Type::I32), ("z".into(), Type::I32)]
        );
        let error = check("fn main() { var n = 1; let f = fn () -> Int32 { n }; () }")
            .expect_err("ordinary closure must reject a mutable capture");
        assert!(error.to_string().contains("use 'move fn'"));
        assert!(error.span().is_some());
    }

    #[test]
    fn nested_move_closures_cannot_consume_borrowed_mutable_capture_slots() {
        for initializer in ["1", "\"shared\""] {
            let source = format!("fn main() {{ var n = {initializer}; let f = move fn () -> Unit {{ let nested = move fn () -> Unit {{ let value = n; () }}; () }}; () }}");
            let error = check(&source).expect_err("captured slot is borrowed");
            assert!(error
                .to_string()
                .contains("out of a borrowed closure environment"));
            assert!(error.span().is_some());
        }
        check("fn main() { var n = 1; let f = move fn () -> Unit { var n = n; let nested = move fn () -> Unit { n = n + 1 }; () }; () }")
            .expect("new local binding can be moved into a nested closure");
    }

    #[test]
    fn closure_capture_rules_keep_function_ownership_and_callback_isolation() {
        let error =
            check("fn main() { let f = fn () -> Int32 { 1 }; let g = fn () -> Int32 { f() }; () }")
                .expect_err("function values are owned captures");
        assert!(error.to_string().contains("use 'move fn'"));
        check(
            "fn main() { let f = fn () -> Int32 { 1 }; let g = move fn () -> Int32 { f() }; () }",
        )
        .expect("explicit move accepts an owned function capture");
        let error = check("fn main() { var n: Int32 = 1; let callback = CCallback.new(move fn () -> Int32 { n = n + 1; n }, 0); () }")
            .expect_err("retained callbacks cannot mutate captured slots");
        assert!(
            error
                .to_string()
                .contains("CCallback cannot capture mutable bindings"),
            "{error}"
        );
    }

    #[test]
    fn legacy_lowercase_builtin_type_names_are_rejected() {
        for name in [
            "i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64", "f32", "f64", "bool", "string",
            "unit",
        ] {
            let source = format!("fn main() {{ let value: {name} = 0 }}");
            let error = check(&source).expect_err("legacy builtin type names must be rejected");
            assert!(
                error.to_string().contains("unknown type"),
                "{name}: {error}"
            );
        }
    }

    #[test]
    fn generic_functions_infer_and_accept_type_values() {
        check(
            "fn identity(T: type, value: T) -> T { value }\n\
             fn main() {\n\
                 let integer = identity(42)\n\
                 let boolean = identity(Bool, true)\n\
             }",
        )
        .unwrap();
    }

    #[test]
    fn generic_functions_accept_type_values_as_leading_arguments() {
        check(
            "fn identity(T: type, value: T) -> T { value }\n\
             fn main() {\n\
                 let value = identity(Int64, 42)\n\
             }",
        )
        .unwrap();
    }

    #[test]
    fn type_values_can_be_bound_and_composed() {
        check(
            "fn identity(T: type, value: T) -> T { value }\n\
             fn main() {\n\
                 let Number = Int64\n\
                 let Values = List(Number)\n\
                 let value = identity(Number, 42)\n\
                 let values = identity(Values, List(1, 2))\n\
             }",
        )
        .unwrap();
    }

    #[test]
    fn user_defined_type_functions_are_compile_time_only() {
        check(
            "fn BoxOf(T: type) -> type { List(T) }\n\
             fn identity(T: type, value: T) -> T { value }\n\
             fn main() {\n\
                 let BoxI32 = BoxOf(Int64)\n\
                 let value = identity(BoxI32, List(1, 2))\n\
             }",
        )
        .unwrap();
    }

    #[test]
    fn generic_definition_boundaries_are_diagnosed() {
        assert!(
            check("fn identity(T: type, T: type, value: T) -> T { value } fn main() {}").is_err()
        );
        assert!(check("fn main(T: type) {}").is_err());
        assert!(
            check("struct Box { fn identity(T: type, value: T) -> T { value } } fn main() {}")
                .is_err()
        );
        assert!(
            check("class Box { fn identity(T: type, value: T) -> T { value } } fn main() {}")
                .is_err()
        );
    }

    #[test]
    fn lists_accept_immutable_composites_and_reject_class_references() {
        check(
            "struct Entry { let name: String; let value: Int32 } \
             enum Item { Text(value: String); Number(value: Int32) } \
             fn ok() -> Result(Int32, String) { Ok(1) } \
             fn main() { \
                 let tuples = List((1, \"one\")); \
                 let structs = List(Entry(name: \"item\", value: 1)); \
                 let enums = List(Item.Text(value: \"text\")); \
                 let options = List(Some(\"value\")); \
                 let results = List(ok()); \
                 let nested = List(List(1, 2)) \
             }",
        )
        .unwrap();

        assert!(check("class Box { let value: Int32 } fn main() { List(Box(value: 1)) }").is_err());
        assert!(check(
            "class Box { let value: Int32 } \
             struct Wrapper { let value: Box } \
             fn main() { List(Wrapper(value: Box(value: 1))) }"
        )
        .is_err());
        assert!(check(
            "class Box { let value: Int32 } \
             enum MaybeBox { Some(value: Box); None } \
             fn main() { List(MaybeBox.None) }"
        )
        .is_err());
        assert!(check(
            "class Box { let value: Int32 } fn reject(value: List(Box)) {} fn main() {}"
        )
        .is_err());
        assert!(check(
            "class Box { let value: Int32 } \
             fn pass(T: type, value: T) -> List(T) { List(value) } \
             fn main() { pass(Box(value: 1)) }"
        )
        .is_err());
    }

    #[test]
    fn maps_require_hash_and_eq_keys() {
        check("fn main() { let values = Map.empty(String, Int32); values.insert(\"one\", 1) }")
            .unwrap();
        assert_eq!(
            check("fn main() { Map.empty(Float64, Int32) }")
                .unwrap_err()
                .to_string(),
            "type 'Float64' does not implement trait 'Hash'"
        );
        check("fn empty(K: type, V: type) -> Map(K, V) { Map.empty(K, V) } fn main() {}").unwrap();
        check(
            "fn empty(K: type + Hash + Eq, V: type) -> Map(K, V) { Map.empty(K, V) } fn main() {}",
        )
        .unwrap();
        check(
            "struct Point { let x: Int32 } \
             impl Hash for Point {} \
             impl Eq for Point {} \
             fn main() { Map.empty(Point, String) }",
        )
        .unwrap();
        assert!(check(
            "class Box { let value: Int32 } \
             impl Hash for Box {} \
             impl Eq for Box {} \
             fn main() { Map.empty(Box, Int32) }"
        )
        .is_err());
        assert!(check(
            "class Box { let value: Int32 } \
             impl Hash for Box {} \
             impl Eq for Box {} \
             fn make(K: type + Hash + Eq, V: type) -> Map(K, V) { Map.empty(K, V) } \
             fn main() { make(Box, Int32) }"
        )
        .is_err());
    }

    #[test]
    fn sets_share_map_key_constraints_and_representation() {
        check(
            "fn empty(T: type) -> Set(T) { Set.empty(T) }\n\
             fn main() {\n\
                 let values: Set(Int32) = empty(Int32)\n\
                 let updated = values.insert(1, true)\n\
                 updated.contains_key(1)\n\
             }",
        )
        .unwrap();
        assert!(check("class Box { let value: Int32 } fn main() { Set.empty(Box) }").is_err());
    }

    #[test]
    fn normal_do_with_handler_is_type_checked() {
        assert!(check(
            "eff Clock { fn now() -> Int64 }\n\
             fn read() -> Int64 effects { Clock } { Clock.now() }\n\
             fn main() { let value = do { read() } with { Clock.now() => 42 }; value }"
        )
        .is_ok());
        assert!(check(
            "eff Clock { fn now() -> Int64 }\n\
             fn main() { do { Clock.now() } with { Clock.now() => true } }"
        )
        .is_err());
        assert!(check(
            "eff Clock { fn now() -> Int64 }\n\
             fn fallback() -> Int64 effects { Clock } { Clock.now() }\n\
             fn main() { do { 1 } with { Clock.now() => fallback() } }"
        )
        .is_err());
    }

    #[test]
    fn effect_operation_calls_are_checked_and_declared() {
        assert!(check(
            "eff Clock { fn now() -> Int64 @suspends fn sleep(duration: Duration) -> Unit }\n\
             fn current() -> Int64 effects { Clock } { Clock.now() } fn main() effects { Clock } { current() }"
        )
        .is_ok());
        assert!(check(
            "eff Clock { fn now() -> Int64 }\nfn current() -> Int64 { Clock.now() } fn main() {}"
        )
        .is_err());
        assert!(check(
            "eff Clock { fn now() -> Int64 }\nfn current() -> Int64 effects { Clock } { Clock.missing() } fn main() {}"
        )
        .is_err());
    }

    #[test]
    fn option_annotations_constructors_and_match_are_type_checked() {
        check(
            "fn select(flag: Bool) -> Option(Int32) { if flag { Some(1) } else { None } }\
             fn main() { let value: Option(Int32) = select(true); match value { Some(number) => number; None => 0 } }"
        )
        .unwrap();
    }

    #[test]
    fn result_annotations_constructors_and_match_are_type_checked() {
        check(
            "fn parse(flag: Bool) -> Result(Int32, String) { if flag { Ok(1) } else { Err(\"bad\") } }\
             fn main() { let value: Result(Int32, String) = parse(true); match value { Ok(number) => number; Err(message) => 0 } }",
        )
        .unwrap();
    }

    #[test]
    fn nested_result_annotations_are_type_checked() {
        check("fn main() { let value: Result(Result(Int32, String), String) = Ok(Ok(1)) }")
            .unwrap();
    }

    #[test]
    fn all_numeric_annotations_are_supported() {
        assert!(check(concat!(
            "fn main() {",
            "let a: Int8 = -128; let b: Int16 = -32768; ",
            "let c: Int32 = -2147483648; let d: Int64 = -9223372036854775808; ",
            "let e: UInt8 = 255; let f: UInt16 = 65535; ",
            "let g: UInt32 = 4294967295; let h: UInt64 = 18446744073709551615; ",
            "let i: Float32 = 1.5; let j: Float64 = 1e300",
            "}"
        ))
        .is_ok());
    }

    #[test]
    fn suspending_time_accepts_duration_literals() {
        assert!(check(
            "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
             fn wait() -> Unit effects { time } { time.sleep(1h10m100s) }\n\
             fn main() effects { time } { wait() }"
        )
        .is_ok());
    }

    #[test]
    fn suspending_effect_handlers_cannot_suspend_again() {
        assert!(check(
            "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
             fn main() effects { time } {\n\
                 do { time.sleep(1ms) } with {\n\
                     time.sleep(duration) => time.sleep(duration)\n\
                 }\n\
             }",
        )
        .is_err());
    }

    #[test]
    fn suspending_effects_can_be_handled_with_immediate_resume() {
        assert!(check(
            "eff Input { @suspends fn read() -> Int32 }\n\
             fn main() {\n\
                 do { Input.read() } with { Input.read() => 42 }\n\
             }",
        )
        .is_ok());
    }

    #[test]
    fn normal_suspending_handlers_return_the_operation_value() {
        assert!(check(
            "eff Input { @suspends fn read() -> Int32 }\n\
             fn main() { do { Input.read() } with { Input.read() => 42 } }",
        )
        .is_ok());
    }

    #[test]
    fn resumable_handlers_implicitly_resume_with_a_value() {
        assert!(check(
            "eff Ask { @resumable fn question() -> Int32 }\n\
             fn main() { do { Ask.question() } with { Ask.question() => 42 } }",
        )
        .is_ok());
    }

    #[test]
    fn resumable_handlers_can_abort_from_a_block() {
        assert!(check(
            "eff Ask { @resumable fn question() -> Int32 }\n\
             fn main() {\n\
                 do { Ask.question() } with {\n\
                     Ask.question() => { println(\"aborted\"); abort 7 }\n\
                 }\n\
             }",
        )
        .is_ok());
    }

    #[test]
    fn resumable_abort_can_change_the_do_result_type() {
        assert!(check(
            "eff Ask { @resumable fn question() -> Int32 }\n\
             fn recover() -> String {\n\
                 do { Ask.question() } with { Ask.question() => abort \"fallback\" }\n\
             }\n\
             fn main() { println(recover()) }",
        )
        .is_ok());
    }

    #[test]
    fn suspending_and_resumable_annotations_are_independent() {
        assert!(check(
            "eff Ask { @suspends @resumable fn question() -> Int32 }\n\
             fn main() { do { Ask.question() } with { Ask.question() => 42 } }",
        )
        .is_ok());
    }

    #[test]
    fn abortive_handlers_can_use_an_explicit_abort_action() {
        assert!(check(
            "eff Failure { @aborts fn stop() -> Unit }\n\
             fn recover() -> Int32 {\n\
                 do { Failure.stop() } with {\n\
                     Failure.stop() => { println(\"handled\"); abort 7 }\n\
                 }\n\
             }\n\
             fn main() { println(recover()) }",
        )
        .is_ok());
    }

    #[test]
    fn rejects_suspending_effects_inside_a_cown_lease() {
        assert_eq!(
            check(
                "class Counter { var value: Int32 = 0 }\n\
                 eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
                 fn main() effects { time } {\n\
                     let counter = Cown.new(Counter(value: 0))\n\
                     when (counter) |state| { time.sleep(1ms) }\n\
                 }",
            )
            .unwrap_err()
            .to_string(),
            "function 'when body cannot perform a suspending effect operation' is not supported yet"
        );
    }

    #[test]
    fn numeric_literal_ranges_are_checked() {
        assert_eq!(
            check("fn main() { let value: Int8 = 128 }")
                .unwrap_err()
                .to_string(),
            "integer literal is out of range for Int8"
        );
        assert_eq!(
            check("fn main() { let value = 2147483648 }")
                .unwrap_err()
                .to_string(),
            "integer literal is out of range for Int32"
        );
    }

    #[test]
    fn integer_division_by_constant_zero_is_rejected() {
        assert_eq!(
            check("fn main() { let value: UInt64 = 8 / (2 - 2) }")
                .unwrap_err()
                .to_string(),
            "division by zero"
        );
        assert_eq!(
            check("fn main() { let value: Int32 = 8 % 0 }")
                .unwrap_err()
                .to_string(),
            "division by zero"
        );
        assert_eq!(
            check("fn main() { let value = 1.5 & 1.0 }")
                .unwrap_err()
                .to_string(),
            "bitwise operator requires integer operands"
        );
        assert_eq!(
            check("fn main() { (-2147483648).abs() }")
                .unwrap_err()
                .to_string(),
            "absolute value of the minimum integer is not representable"
        );
    }

    #[test]
    fn immutable_bindings_can_be_shadowed() {
        assert!(check("fn main() { let x = 100; let x = 200; x }").is_ok());
        assert!(check("fn main() { let x = \"first\"; let x = \"second\"; println(x) }").is_ok());
    }

    #[test]
    fn values_are_immutable() {
        assert!(check("fn main() { let x: Int64 = 100; x }").is_ok());
        assert!(check("fn main() { let x = 100; x = 200 }").is_err());
    }

    #[test]
    fn bindings_are_scoped_to_their_block() {
        assert_eq!(
            check("fn main() { { let x = 100 }; x }")
                .unwrap_err()
                .to_string(),
            "unknown value 'x'"
        );
    }

    #[test]
    fn calls_are_checked() {
        assert!(check("fn main() { println(\"hello\") }").is_ok());
        assert!(check("fn main() { panic(\"stop\") }").is_ok());
        assert_eq!(
            check("fn main() { panic(1) }").unwrap_err().to_string(),
            "expected String, found Int32"
        );
        assert!(check("fn main() { print(\"hello\") }").is_ok());
        assert!(
            check("fn add(a: Int32, b: Int32) -> Int32 { a + b } fn main() { add(1, 2) }").is_ok()
        );
        assert_eq!(
            check("fn add(a: Int32, b: Int32) -> Int32 { a + b } fn main() { add(1) }")
                .unwrap_err()
                .to_string(),
            "add expects exactly 2 argument(s)"
        );
        assert_eq!(
            check("fn add(a: Int32) -> Int32 { a } fn main() { add(\"one\") }")
                .unwrap_err()
                .to_string(),
            "expected Int32, found String"
        );
    }

    #[test]
    fn omitted_return_type_means_unit() {
        let error = "expected Unit, found Int32";
        assert_eq!(
            check("fn add(a: Int32, b: Int32) { a + b } fn main() { add(1, 2) }")
                .unwrap_err()
                .to_string(),
            error
        );
        assert_eq!(
            check("fn main() { println(\"{add(1, 2)}\") }\nfn add(a: Int32, b: Int32) { a + b }")
                .unwrap_err()
                .to_string(),
            error
        );
        assert_eq!(
            check("fn add(a: Int32, b: Int32) -> () { a + b } fn main() {}")
                .unwrap_err()
                .to_string(),
            error
        );
        assert!(check("fn announce() { println(\"hello\") } fn main() { announce() }").is_ok());
        assert!(
            check("fn add(a: Int32, b: Int32) -> Int32 { a + b } fn main() { add(1, 2) }").is_ok()
        );
        assert_eq!(
            check(
                "fn add(a: Int32, b: Int32) -> Int32 { a + b } fn main() { add(1, 2)\nprintln(\"ok\") }"
            )
            .unwrap_err()
            .to_string(),
            "this Int32 value is never used; assign it, discard it with 'let _', or delete the expression"
        );
        assert!(check(
            "fn add(a: Int32, b: Int32) -> Int32 { a + b } fn main() { let _ = add(1, 2)\nprintln(\"ok\") }"
        )
        .is_ok());
        assert_eq!(
            check("fn add(a: Int32, b: Int32) { a + b\n() } fn main() { add(1, 2) }")
                .unwrap_err()
                .to_string(),
            "this Int32 value is never used; assign it, discard it with 'let _', or delete the expression"
        );
    }

    #[test]
    fn constants_are_checked_and_expand_through_references() {
        assert!(check(
            "const BASE = 40\n\
             const ANSWER = BASE + 2\n\
             fn main() { println(ANSWER) }"
        )
        .is_ok());
        assert!(check(
            "const FIRST: Int32 = SECOND\n\
                 const SECOND: Int32 = FIRST\n\
                 fn main() {}"
        )
        .unwrap_err()
        .to_string()
        .contains("is part of a dependency cycle"));
        assert_eq!(
            check("const VALUE: Int32 = identity(1)\nfn main() {}")
                .unwrap_err()
                .to_string(),
            "constant value must be a pure constant expression"
        );
        assert!(check("const EMPTY = None\nfn main() {}")
            .unwrap_err()
            .to_string()
            .contains("None requires an Option(T) type context"));
    }

    #[test]
    fn show_accepts_builtin_values_and_constrains_generics() {
        assert!(check(
            "trait Show { fn show(&self) -> String }\n\
             fn log(T: type + Show, value: T) { println(value) }\n\
             fn main() { println(42); println(true); println(1.5); log(99) }"
        )
        .is_ok());
        assert!(check(
            "fn log(T: type, value: T) { println(value) }\n\
             fn main() { log(42) }"
        )
        .is_err());
        assert_eq!(
            check(
                "fn log(T: type + Show, value: T) { println(value) }\n\
                 fn main() { log(List(1)) }"
            )
            .unwrap_err()
            .to_string(),
            "type 'list' does not implement trait 'Show'"
        );
    }

    #[test]
    fn show_impls_are_registered_for_user_types() {
        check(
            "struct Point { let x: Int32 }\n\
             impl Show for Point { fn show() -> String { \"Point\" } }\n\
             fn main() { let point = Point(x: 1); println(point); point.show() }",
        )
        .unwrap();
        assert!(check(
            "struct Point { let x: Int32 }\n\
             fn main() { println(Point(x: 1)) }"
        )
        .is_err());
        assert_eq!(
            check(
                "struct Point { let x: Int32 }\n\
                 impl Show for Point { fn show() -> String { \"Point\" } }\n\
                 impl Show for Point { fn show() -> String { \"Point\" } }\n\
                 fn main() {}"
            )
            .unwrap_err()
            .to_string(),
            "trait 'Show' is implemented more than once for 'Point'"
        );
        assert!(check(
            "struct Point { let x: Int32 }\n\
             impl Inspect for Point { fn debug() -> String { \"Point\" } }\n\
             fn main() {}"
        )
        .is_err());
    }

    #[test]
    fn custom_trait_impls_satisfy_generic_bounds() {
        check(
            "trait Inspect { fn debug(&self) -> String }\n\
             struct Point { let x: Int32 }\n\
             class Counter { let value: Int32 }\n\
             impl Inspect for Point { fn debug() -> String { \"point\" } }\n\
             impl Inspect for Counter { fn debug() -> String { \"counter\" } }\n\
             fn dump(T: type + Inspect, value: T) -> String { value.debug() }\n\
             fn main() { println(dump(Point(x: 1))); println(dump(Counter(value: 1))) }",
        )
        .unwrap();
        assert!(check(
            "trait Inspect { fn debug(&self) -> String }\n\
             struct Point { let x: Int32 }\n\
             fn dump(T: type + Inspect, value: T) -> String { value.debug() }\n\
             fn main() { dump(Point(x: 1)) }"
        )
        .is_err());
    }

    #[test]
    fn trait_methods_support_self_parameters_and_multiple_requirements() {
        check(
            "trait Compare { fn equals(&self, other: Self) -> Bool }\n\
             trait Inspect { fn debug(&self) -> String fn type_name(&self) -> String }\n\
             struct Point { let x: Int32 }\n\
             impl Compare for Point { fn equals(other: Point) -> Bool { self.x == other.x } }\n\
             impl Inspect for Point {\n\
                 fn debug() -> String { \"point\" }\n\
                 fn type_name() -> String { \"Point\" }\n\
             }\n\
             fn same(T: type + Compare, left: T, right: T) -> Bool { left.equals(right) }\n\
             fn main() {\n\
                 let point = Point(x: 1)\n\
                 same(point, Point(x: 1))\n\
             }",
        )
        .unwrap();
    }

    #[test]
    fn free_functions_reject_self_named_parameters() {
        assert!(check("fn f(self) {} fn main() {}").is_err());
        assert!(check("fn f(x: Int32, self: String) {} fn main() {}").is_err());
    }

    #[test]
    fn trait_impls_reject_missing_or_mismatched_methods() {
        assert!(check(
            "trait Inspect { fn debug(&self) -> String fn type_name(&self) -> String }\n\
             struct Point { let x: Int32 }\n\
             impl Inspect for Point { fn debug() -> String { \"point\" } }\n\
             fn main() {}"
        )
        .is_err());
        assert!(check(
            "trait Compare { fn equals(&self, other: Self) -> Bool }\n\
             struct Point { let x: Int32 }\n\
             impl Compare for Point { fn equals(other: String) -> Bool { true } }\n\
             fn main() {}"
        )
        .is_err());
    }

    #[test]
    fn associated_types_resolve_in_generic_signatures() {
        check(
            "trait Iterator { type Item fn next(&self) -> Option(Item) }\n\
             struct Values { let value: Int32 }\n\
             impl Iterator for Values {\n\
                 type Item = Int32\n\
                 fn next() -> Option(Int32) { Some(self.value) }\n\
             }\n\
             fn first(I: type + Iterator, iter: I) -> Option(I.Item) { iter.next() }\n\
             fn main() { first(Values(value: 1)) }",
        )
        .unwrap();
        assert!(check(
            "trait Iterator { type Item fn next(&self) -> Option(Item) }\n\
             struct Values { let value: Int32 }\n\
             impl Iterator for Values { fn next() -> Option(Int32) { Some(self.value) } }\n\
             fn main() {}"
        )
        .is_err());
    }

    #[test]
    fn where_clauses_constrain_type_parameters_and_associated_types() {
        check(
            "fn show_value(T: type, value: T) -> String\n\
             where T: Show\n\
             { value.show() }\n\
             fn main() { show_value(1) }",
        )
        .unwrap();
        let error = check(
            "fn show_value(T: type, value: T) -> String\n\
             where T: Show\n\
             { value.show() }\n\
             struct Opaque { let value: Int32 }\n\
             fn main() { show_value(Opaque(value: 1)) }",
        )
        .unwrap_err();
        assert!(error.to_string().contains("Show"));

        check(
            "fn show_item(I: type + Cursor, cursor: I) -> String\n\
             where I.Item: Show\n\
             {\n\
                 match cursor.advance() {\n\
                     Some(pair) => pair.0.show()\n\
                     None => \"\"\n\
                 }\n\
             }\n\
             fn main() { show_item(List(1, 2)) }",
        )
        .unwrap();
        let error = check(
            "fn show_item(I: type + Cursor, cursor: I) -> String\n\
             where I.Item: Show\n\
             {\n\
                 match cursor.advance() {\n\
                     Some(pair) => pair.0.show()\n\
                     None => \"\"\n\
                 }\n\
             }\n\
             struct Opaque { let value: Int32 }\n\
             struct Values { let value: Opaque }\n\
             impl Cursor for Values {\n\
                 type Item = Opaque\n\
                 fn advance(self) -> Option((Opaque, Self)) { None }\n\
             }\n\
             fn main() { show_item(Values(value: Opaque(value: 1))) }",
        )
        .unwrap_err();
        assert!(error.to_string().contains("Show"));
    }

    #[test]
    fn for_accepts_into_cursor_containers() {
        check(
            "fn main() {\n\
                 var table: Map(Int32, Int32) = Map#{}\n\
                 table = table.insert(1, 10)\n\
                 for entry in table { entry.0 }\n\
             }",
        )
        .unwrap();
        check(
            "fn main() {\n\
                 var letters: Set(Int32) = Set#{}\n\
                 letters = letters.insert(7, true)\n\
                 for entry in letters {\n\
                     match entry { (key, _) => key }\n\
                 }\n\
             }",
        )
        .unwrap();
        check("fn main() { for i in 0..3 { i } }").unwrap();
        check(
            "fn main() {\n\
                 let items = MutList#{1, 2}\n\
                 for x in items { x }\n\
             }",
        )
        .unwrap();
    }

    #[test]
    fn if_expressions_are_type_checked() {
        let program = syntax::parse_program("fn main() { let x = if true 1 else 2; x }").unwrap();
        let types = check_program(&program).unwrap();
        let crate::syntax::ExprKind::Block(expressions) = &program.functions[0].body.kind else {
            panic!("expected a block");
        };
        let crate::syntax::ExprKind::Let { value, .. } = &expressions[0].kind else {
            panic!("expected a let expression");
        };
        assert_eq!(types.get(value), Type::I32);
        assert!(check("fn main() { if 1 10 else 20 }").is_err());
        assert_eq!(
            check("fn main() { if true 1 else \"two\" }")
                .unwrap_err()
                .to_string(),
            "expected Int32, found String"
        );
    }

    #[test]
    fn concurrent_expressions_have_stable_result_types() {
        let program = syntax::parse_program(
            "fn one() -> Int32 { 1 }\n\
             fn two() -> String { \"two\" }\n\
             fn main() { let values = parallel {\n| one()\n| two()\n}; values }",
        )
        .unwrap();
        let types = check_program(&program).unwrap();
        let crate::syntax::ExprKind::Block(expressions) = &program.functions[2].body.kind else {
            panic!("expected main block");
        };
        let crate::syntax::ExprKind::Let { value, .. } = &expressions[0].kind else {
            panic!("expected parallel binding");
        };
        assert!(matches!(types.get(value), Type::Tuple(_)));

        let error = check(
            "fn one() -> Int32 { 1 }\n\
             fn two() -> String { \"two\" }\n\
             fn main() { race {\n| one()\n| two()\n} }",
        )
        .expect_err("race arms must agree on their result type");
        assert_eq!(error.to_string(), "expected Int32, found String");
    }

    #[test]
    fn comparisons_and_while_are_type_checked() {
        assert!(check("fn main() { while 1 { } }").is_err());
        assert!(check("fn main() { while true { break } }").is_ok());
        assert_eq!(
            check("fn main() { 1 < \"two\" }").unwrap_err().to_string(),
            "ordered comparison requires operands of the same type implementing PartialOrd"
        );
    }

    #[test]
    fn loops_and_breaks_are_type_checked() {
        assert!(check("fn main() { let value = loop { break 42 }; value }").is_ok());
        assert!(check("fn main() { loop { break; break 1 } }").is_err());
        assert!(check("fn main() { while true { break } }").is_ok());
        assert!(check("fn main() { while true { break 1 } }").is_err());
        assert_eq!(
            check("fn main() { break }").unwrap_err().to_string(),
            "break is only valid inside a loop"
        );
        assert!(check("fn main() { while true { continue } }").is_ok());
        assert!(check("fn main() { loop { continue } }").is_ok());
        assert_eq!(
            check("fn main() { continue }").unwrap_err().to_string(),
            "continue is only valid inside a loop"
        );
        assert!(check("fn main() { for x in List(1, 2) { println(x) } }").is_ok());
        assert!(check("fn main() { var sum = 0; for x in List(1, 2) { sum = sum + x } }").is_ok());
        assert!(check("fn main() { let xs = for x in List(1, 2) { x }; xs.length() }").is_ok());
        assert!(
            check("fn main() { let xs = for x in List(1, 2) { println(x) }; xs.length() }")
                .is_err()
        );
        assert!(check(
            "fn main() { let xs: List(Unit) = for x in List(1, 2) { () }; xs.length() }"
        )
        .is_ok());
        assert!(check(
            "fn main() { let xs: List(Int32) = for x in List(1, 2) { continue }; xs.is_empty() }"
        )
        .is_ok());
    }

    #[test]
    fn tuples_are_type_checked() {
        assert!(check("fn main() { let pair: (Int32, String) = (1, \"ok\"); pair.0 } ").is_ok());
        assert!(check("fn main() { let pair = (1, 2); pair.0 } ").is_ok());
        assert!(check("fn main() { let pair = (1, 2); pair.0 = 3 } ").is_err());
        assert!(check("fn main() { let pair = (1, 2); pair.2 } ").is_err());
    }

    #[test]
    fn tuple_parameters_are_values() {
        assert!(check(
            "fn first(pair: (Int32, Int32)) -> Int32 { pair.0 } fn main() { first((1, 2)) }"
        )
        .is_ok());
    }

    #[test]
    fn structs_are_type_checked() {
        assert!(check(
            "struct Point { let x: Int32, let y: Int32 } \
             fn main() { let point: Point = Point(y: 2, x: 1); point.x }"
        )
        .is_ok());

        assert_eq!(
            check("struct Point { let x: Int32 } fn main() { Point(y: 1) }")
                .unwrap_err()
                .to_string(),
            "unknown function 'y'"
        );
        assert_eq!(
            check("struct Point { let x: Int32, let y: Int32 } fn main() { Point(x: 1) }")
                .unwrap_err()
                .to_string(),
            "Point expects exactly 2 argument(s)"
        );
    }

    #[test]
    fn implicit_initializers_use_field_defaults() {
        assert!(check(
            "struct Point { let x: Int32, let y: Int32 = 0 } fn main() { Point(x: 1).y }"
        )
        .is_ok());
        assert!(check(
            "class Counter { let name: String = \"ok\"; var value: Int32 = 0 } \
             fn main() { Counter() }"
        )
        .is_ok());
        assert!(check("struct Point { let x: Int32 } fn main() { Point() }").is_err());
    }

    #[test]
    fn classes_are_type_checked() {
        assert!(check(
            "class Counter { var value: Int32; fn increment() { self.value = self.value + 1 } } \
             fn main() { let counter = Counter(value: 0); counter.increment() }"
        )
        .is_ok());
        assert!(check(
            "class Counter { let value: Int32; fn increment() { self.value = 1 } } \
             fn main() { Counter(value: 0) }"
        )
        .is_err());
        assert!(check(
            "class Counter { var value: Int32 } \
             fn main() { let counter = Counter(value: 0); counter.value = 1 }"
        )
        .is_err());
    }

    #[test]
    fn classes_can_be_function_parameters_and_returns() {
        assert!(check(
            "class Counter { var value: Int32 } \
             fn identity(counter: Counter) -> Counter { counter } \
             fn main() { let counter = Counter(value: 1); identity(counter: counter).value }"
        )
        .is_ok());
    }

    #[test]
    fn classes_can_hold_other_class_references() {
        assert!(check(
            "class Child { let value: Int32 } \
             class Parent { let child: Child } \
             fn main() { Parent(child: Child(value: 1)).child.value }"
        )
        .is_ok());
    }

    #[test]
    fn class_let_fields_cannot_be_written_by_methods() {
        assert!(check(
            "class Counter { let value: Int32; fn reset() { self.value = 0 } } \
             fn main() { Counter(value: 1) }"
        )
        .is_err());
    }

    #[test]
    fn class_fields_cannot_be_written_externally() {
        assert!(check(
            "class Counter { var value: Int32 } \
             fn main() { let counter = Counter(value: 1); counter.value = 2 }"
        )
        .is_err());
    }

    #[test]
    fn class_methods_can_call_themselves() {
        assert!(check(
            "class Countdown { \
                 let start: Int32; \
                 fn sum(value: Int32) -> Int32 { if value == 0 0 else value + self.sum(value: value - 1) } \
             } \
             fn main() { Countdown(start: 3).sum(value: 3) }"
        )
        .is_ok());
    }

    #[test]
    fn methods_can_read_fields_without_self() {
        assert!(check(
            "struct Point { let x: Int32; fn value() -> Int32 { x } } \
             class Counter { let value: Int32; fn current() -> Int32 { value } } \
             fn main() { Point(x: 1).value() + Counter(value: 2).current() }"
        )
        .is_ok());
    }

    #[test]
    fn method_parameters_shadow_implicit_self_fields() {
        assert!(check(
            "class Example { \
                 let value: Int32; \
                 fn parameter(value: String) -> String { value } \
                 fn field(value: String) -> Int32 { self.value } \
             } \
             fn main() { let example = Example(value: 1); example.parameter(value: \"ok\") }"
        )
        .is_ok());
    }

    #[test]
    fn enums_are_constructed_and_matched_exhaustively() {
        assert!(check(
            "enum Shape { Circle(radius: Float32); Rectangle(width: Float32, height: Float32); Triangle } \
             fn area(shape: Shape) -> Float32 { \
                 match shape { \
                     Shape.Circle(radius) => radius * radius; \
                     Shape.Rectangle(width, height) => width * height; \
                     Shape.Triangle => 0.0 \
                 } \
             } \
             fn main() { area(shape: Shape.Circle(radius: 2.0)) }"
        )
        .is_ok());

        assert!(check(
            "enum Shape { Circle(radius: Float32); Triangle } \
             fn main() { match Shape.Triangle { Shape.Triangle => 0.0 } }"
        )
        .is_err());
    }

    #[test]
    fn wildcard_match_arms_are_exhaustive_and_final() {
        assert!(check(
            "enum Choice { First; Second } \
             fn main() { match Choice.First { Choice.First => 1; _ => 2 } }"
        )
        .is_ok());

        let unreachable = check(
            "enum Choice { First; Second } \
             fn main() { match Choice.First { _ => 1; Choice.First => 2 } }",
        )
        .unwrap_err();
        assert_eq!(unreachable.to_string(), "match arm is unreachable");
    }

    #[test]
    fn nested_result_and_option_patterns_check_and_cover() {
        assert!(check(
            "fn describe(value: Result(Option(Int32), String)) -> String { \
                 match value { \
                     Ok(Some(number)) => \"some\"; \
                     Ok(None) => \"none\"; \
                     Err(message) => message \
                 } \
             } \
             fn main() { describe(value: Ok(Some(1))) }"
        )
        .is_ok());

        // Qualified variants nest too.
        assert!(check(
            "enum Color { Red; Green } \
             fn describe(value: Result(Option(Color), String)) -> String { \
                 match value { \
                     Ok(Some(Color.Red)) => \"red\"; \
                     Ok(Some(Color.Green)) => \"green\"; \
                     Ok(None) => \"none\"; \
                     Err(message) => message \
                 } \
             } \
             fn main() { describe(value: Ok(Some(Color.Red))) }"
        )
        .is_ok());

        // A broader nested arm before a narrower one is unreachable.
        let unreachable = check(
            "fn main() { \
                 let value: Result(Option(Int32), String) = Ok(Some(1)); \
                 match value { \
                     Ok(wrapped) => 0; \
                     Ok(Some(number)) => number; \
                     Err(message) => 0 \
                 } \
             }",
        )
        .unwrap_err();
        assert_eq!(unreachable.to_string(), "match arm is unreachable");

        // Missing one nested case is non-exhaustive.
        assert!(check(
            "fn main() { \
                let value: Result(Option(Int32), String) = Ok(Some(1)); \
                match value { \
                    Ok(Some(number)) => number; \
                    Ok(None) => 0; \
                    Err(message) => 0 \
                } \
            }"
        )
        .is_ok());
        assert!(check(
            "fn main() { \
                let value: Result(Option(Int32), String) = Ok(Some(1)); \
                match value { \
                    Ok(Some(number)) => number; \
                    Err(message) => 0 \
                } \
            }"
        )
        .is_err());

        // Payload patterns must still match the payload type.
        assert!(check(
            "fn main() { \
                let value: Result(Int32, String) = Ok(1); \
                match value { \
                    Ok(Some(number)) => number; \
                    Ok(other) => 0; \
                    Err(message) => 0 \
                } \
            }"
        )
        .is_err());
    }

    #[test]
    fn tuple_patterns_check_payloads_and_top_level_matches() {
        assert!(check(
            "fn split(value: Result((Int32, Int32), String)) -> Int32 { \
                 match value { \
                     Ok((left, right)) => left + right; \
                     Err(message) => 0 \
                 } \
             } \
             fn main() { split(value: Ok((1, 2))) }"
        )
        .is_ok());
        assert!(check(
            "fn main() { \
                 let pair = (1, \"ok\"); \
                 match pair { \
                     (number, text) => number \
                 } \
             }"
        )
        .is_ok());
        assert!(check(
            "fn main() { \
                 let value: Result((Int32, Int32), String) = Ok((1, 2)); \
                 match value { \
                     Ok((left, right)) => left \
                 } \
             }"
        )
        .is_err());
        assert!(check(
            "fn main() { \
                 let pair = (1, 2, 3); \
                 match pair { \
                     (left, right) => left + right \
                 } \
             }"
        )
        .is_err());
    }

    #[test]
    fn match_pattern_bindings_cannot_repeat() {
        let error = check(
            "enum Pair { Values(left: Int32, right: Int32) } \
             fn main() { \
                 match Pair.Values(left: 1, right: 2) { \
                     Pair.Values(left: value, right: value) => value \
                 } \
             }",
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "pattern binding 'value' is defined more than once"
        );
    }

    #[test]
    fn nested_enum_patterns_are_checked_recursively() {
        assert!(check(
            "enum Inner { First(value: Int32); Second } \
             enum Outer { Some(value: Inner); None } \
             fn read(value: Outer) -> Int32 { \
                 match value { \
                     Outer.Some(Inner.First(value)) => value; \
                     Outer.Some(Inner.Second) => 2; \
                     Outer.None => 0 \
                 } \
             } \
             fn main() { read(value: Outer.Some(value: Inner.First(value: 1))) }"
        )
        .is_ok());

        assert!(check(
            "enum Inner { First; Second } \
             enum Outer { Some(value: Inner); None } \
             fn main() { \
                 match Outer.None { \
                     Outer.Some(value: Inner.First) => 1; \
                     Outer.None => 0 \
                 } \
             }"
        )
        .is_err());

        let unreachable = check(
            "enum Inner { First; Second } \
             enum Outer { Some(value: Inner); None } \
             fn main() { \
                 match Outer.None { \
                     Outer.Some(value: _) => 1; \
                     Outer.Some(value: Inner.First) => 2; \
                     Outer.None => 0 \
                 } \
             }",
        )
        .unwrap_err();
        assert_eq!(unreachable.to_string(), "match arm is unreachable");
    }

    #[test]
    fn fieldless_enum_variants_are_values_not_functions() {
        assert!(check("enum Shape { Triangle } fn main() { Shape.Triangle() }").is_err());
    }

    #[test]
    fn enum_value_layouts_cannot_be_empty_or_recursive() {
        assert!(check("enum Empty {} fn main() {}").is_err());
        assert!(check("enum List { Cons(tail: List); Nil } fn main() {}").is_err());
        assert!(check(
            "enum Left { Next(value: Right); End } \
             enum Right { Next(value: Left); End } \
             fn main() {}"
        )
        .is_err());
        assert!(check(
            "enum Node { Next(value: Wrapper); End } \
             struct Wrapper { let node: Node } \
             fn main() {}"
        )
        .is_err());
        assert!(check(
            "class Box { let node: Node } \
             enum Node { Next(value: Box); End } \
             fn main() {}"
        )
        .is_ok());
    }

    #[test]
    fn struct_layouts_allow_class_boundaries_but_reject_value_cycles() {
        assert!(check("struct Node { let next: Node } fn main() {}").is_err());
        assert!(check(
            "struct Left { let right: Right } \
             struct Right { let left: Left } \
             fn main() {}"
        )
        .is_err());
        assert!(check(
            "struct Node { let next: (Node, Int32) } \
             fn main() {}"
        )
        .is_err());
        assert!(check(
            "struct Wrapper { let value: Box } \
             class Box { let value: Int32 } \
             fn main() {}"
        )
        .is_ok());
    }

    #[test]
    fn option_and_result_standard_modules_check() {
        crate::sema::check_module(
            &syntax::parse_program(include_str!("../../std/joky/option.jk")).unwrap(),
        )
        .expect("joky/option");
        crate::sema::check_module(
            &syntax::parse_program(include_str!("../../std/joky/result.jk")).unwrap(),
        )
        .expect("joky/result");
        check(
            "fn main() { \
                 if !Some(1).is_some() { panic(\"option\") }; \
                 if !Ok(1).is_ok() { panic(\"result\") }; \
                 if Some(1).unwrap_or(0) != 1 { panic(\"unwrap\") } \
             }",
        )
        .expect("option/result methods");
    }
}
