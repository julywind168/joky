use super::*;
use crate::{sema, syntax};

fn lower(source: &str) -> CoreProgram {
    let program = syntax::parse_program(source).unwrap();
    let types = sema::check_program(&program).unwrap();
    CoreProgram::lower(program, types).unwrap()
}

#[test]
fn lowering_preserves_local_binding_mutability() {
    let core = lower(
        "fn identity(T: type, input: T) -> T { var value = input; value } \
         fn main() { let initial = 1; var value = identity(initial); let value = value }",
    );
    let main = core.functions().iter().find(|f| f.name == "main").unwrap();
    let CoreExprKind::Block(expressions) = &main.body.kind else {
        panic!("expected block");
    };
    assert!(matches!(
        &expressions[0].kind,
        CoreExprKind::Let { mutable: false, .. }
    ));
    assert!(matches!(
        &expressions[1].kind,
        CoreExprKind::Let { mutable: true, .. }
    ));
    assert!(matches!(
        &expressions[2].kind,
        CoreExprKind::Let { mutable: false, .. }
    ));
    let identity = core
        .functions()
        .iter()
        .find(|f| f.name.starts_with("identity$$"))
        .unwrap();
    let CoreExprKind::Block(expressions) = &identity.body.kind else {
        panic!("expected block");
    };
    assert!(matches!(
        &expressions[0].kind,
        CoreExprKind::Let { name, mutable: true, .. } if name == "value"
    ));
}

#[test]
fn lowering_consumes_resolved_annotations_instead_of_source_spelling() {
    let mut program = syntax::parse_program(
        r#"
        fn values(input: ListOf(Int64)) -> ListOf(Int64) { input }
        fn ListOf(T: type) -> type { List(T) }
        fn main() { let result = values(List(1)) }
    "#,
    )
    .unwrap();
    let types = sema::check_program(&program).unwrap();
    program.functions[0].parameters[0].ty.kind = syntax::TypeExpr::Name("no-such-type".to_owned());
    program.functions[0].return_type.as_mut().unwrap().kind =
        syntax::TypeExpr::Name("no-such-type".to_owned());
    let core = CoreProgram::lower(program, types).unwrap();
    let values = &core.functions()[0];
    assert!(matches!(values.parameters[0].ty, Type::List(_)));
    assert_eq!(values.return_type, values.parameters[0].ty);
}

#[test]
fn type_functions_and_type_bindings_are_erased_before_runtime_lowering() {
    let core = lower(
        r#"
        fn Wrap(T: type) -> type { List(T) }
        fn Number() -> type { Int64 }
        fn identity(T: type, value: T) -> T { value }
        fn main() {
            let N = Number()
            let X = Wrap(N)
            let value: X = identity(X, List(1, 2))
        }
    "#,
    );
    assert_eq!(core.functions().len(), 2);
    assert!(!core
        .functions()
        .iter()
        .any(|f| f.name == "Number" || f.name == "Wrap"));
    let main = core.functions().iter().find(|f| f.name == "main").unwrap();
    let CoreExprKind::Block(expressions) = &main.body.kind else {
        panic!("block")
    };
    assert!(matches!(expressions[0].kind, CoreExprKind::Unit));
    assert!(matches!(expressions[1].kind, CoreExprKind::Unit));
    let CoreExprKind::Let { value, .. } = &expressions[2].kind else {
        panic!("let")
    };
    let CoreExprKind::Call { arguments, .. } = &value.kind else {
        panic!("call")
    };
    assert_eq!(arguments.len(), 1);
    let identity = core
        .functions()
        .iter()
        .find(|f| f.name.starts_with("identity$$"))
        .unwrap();
    assert_eq!(identity.parameters.len(), 1);
}

#[test]
fn type_value_and_inferred_calls_share_one_generic_instance() {
    let core = lower(
        r#"
        fn identity(T: type, value: T) -> T { value }
        fn forward(T: type, value: T) -> T { identity(T, value) }
        fn main() {
            let a = identity(Int64, 1)
            let b = identity(Int64, 2)
            let c: Int64 = identity(3)
            let d = forward(Int64, 4)
        }
    "#,
    );
    assert_eq!(
        core.functions()
            .iter()
            .filter(|f| f.name.starts_with("identity$$"))
            .count(),
        1
    );
}

#[test]
fn lowers_effect_metadata_for_functions() {
    let core = lower(
        "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
         fn wait() -> Unit effects { time } { time.sleep(1000ms) } fn main() {}",
    );
    let wait = core
        .functions()
        .iter()
        .find(|function| function.name == "wait")
        .expect("effect function");
    assert_eq!(wait.declared_effects.iter().count(), 1);
    assert!(wait.may_suspend);
}

#[test]
fn lowers_do_with_handlers() {
    let core = lower(
        "eff Clock { fn now() -> Int64 }\n\
         fn main() { let value = do { Clock.now() } with { Clock.now() => 42 }; value }",
    );
    let CoreExprKind::Block(expressions) = &core.functions()[0].body.kind else {
        panic!("expected function body");
    };
    let CoreExprKind::Let { value, .. } = &expressions[0].kind else {
        panic!("expected let");
    };
    let CoreExprKind::Do { handlers, .. } = &value.kind else {
        panic!("expected do");
    };
    assert_eq!(handlers.len(), 1);
    assert_eq!(handlers[0].operation.operation, 0);

    assert!(core.functions()[0].used_effects.is_empty());
}

#[test]
fn lowers_resumable_handler_value_without_the_resume_marker_call() {
    let core = lower(
        "eff Ask { @resumable fn question(prompt: String) -> String }\n\
         fn main() -> String { do { Ask.question(\"Joky\") } with { Ask.question(prompt) => resume(prompt) } }",
    );
    let CoreExprKind::Block(expressions) = &core.functions()[0].body.kind else {
        panic!("expected function block");
    };
    let CoreExprKind::Do { handlers, .. } = &expressions[0].kind else {
        panic!("expected do expression");
    };
    assert!(handlers[0].resumes);
    assert!(matches!(handlers[0].value.kind, CoreExprKind::Name(ref name) if name == "prompt"));
    assert!(!core
        .functions()
        .iter()
        .any(|function| function.name.starts_with("__task_")));
}

#[test]
fn normal_handler_body_with_normal_call_stays_in_the_parent_function() {
    let core = lower(
        "eff Clock { fn now() -> Int64 }\n\
         fn read() -> Int64 effects { Clock } { Clock.now() }\n\
         fn main() -> Int64 { do { read() } with { Clock.now() => 42 } }",
    );
    assert!(!core
        .functions()
        .iter()
        .any(|function| function.name.starts_with("__task_")));
}

#[test]
fn normal_handler_body_with_non_normal_call_materializes_a_task() {
    let core = lower(
        "eff Clock { fn now() -> Int64 }\n\
         eff Failure { @aborts fn stop() -> Unit }\n\
         fn fail() -> Int64 effects { Failure } { Failure.stop(); 0 }\n\
         fn main() -> Int64 effects { Failure } { do { fail() } with { Clock.now() => 42 } }",
    );
    assert!(core
        .functions()
        .iter()
        .any(|function| function.name.starts_with("__task_main_")));
}

#[test]
fn lowers_typed_function_bodies() {
    let core = lower("fn add(a: Int32, b: Int32) -> Int32 { a + b } fn main() { add(1, 2) }");
    assert_eq!(core.functions().len(), 2);
    assert_eq!(core.functions()[0].return_type, Type::I32);
    assert!(core.functions()[0].used_effects.is_empty());
    assert!(!core.functions()[0].may_suspend);
    assert_eq!(core.functions()[1].body.ty, Type::I32);
}

#[test]
fn decodes_graph_module_id_from_imported_symbol() {
    assert_eq!(module_id_for_function("main"), CoreModuleId(0));
    assert_eq!(module_id_for_function("m7_exposed"), CoreModuleId(7));
    assert_eq!(module_id_for_function("m7_nested_name"), CoreModuleId(7));
    assert_eq!(module_id_for_function("m_missing"), CoreModuleId(0));
}

#[test]
fn lowers_control_flow_and_patterns() {
    let core = lower(
        r#"
            enum Maybe {
                Some(value: Int32)
                None
            }

            fn main() {
                let selected = if true 1 else 2
                let result = match Maybe.Some(value: selected) {
                    Maybe.Some(value: value) => value
                    Maybe.None => 0
                }
                let loop_result = loop { break result }
                if loop_result == 1 { println("ok") } else { println("bad") }
            }
        "#,
    );
    let body = &core.functions()[0].body;
    let CoreExprKind::Block(expressions) = &body.kind else {
        panic!("expected main body block");
    };
    assert!(matches!(&expressions[0].kind, CoreExprKind::Let { .. }));
    assert!(
        matches!(&expressions[0].kind, CoreExprKind::Let { value, .. } if matches!(value.kind, CoreExprKind::If { .. }))
    );
    assert!(
        matches!(&expressions[1].kind, CoreExprKind::Let { value, .. } if matches!(value.kind, CoreExprKind::Match { .. }))
    );
    assert!(
        matches!(&expressions[2].kind, CoreExprKind::Let { value, .. } if matches!(value.kind, CoreExprKind::Loop { .. }))
    );
}

#[test]
fn lowers_struct_and_class_methods_with_receivers() {
    let core = lower(
        r#"
            class Counter {
                var value: Int32 = 0
                fn current() -> Int32 { self.value }
            }
            struct Point {
                let x: Int32 = 1
                fn value() -> Int32 { x }
            }
            fn main() {
                let counter = Counter()
                let point = Point()
                println("ok")
            }
        "#,
    );
    let counter = core
        .functions()
        .iter()
        .find(|function| function.name == "current")
        .expect("class method");
    assert_eq!(counter.receiver, Some(Type::Class(0)));
    let point = core
        .functions()
        .iter()
        .find(|function| function.name == "value")
        .expect("struct method");
    assert_eq!(point.receiver, Some(Type::Struct(0)));
}

#[test]
fn lowers_default_field_initializers() {
    let core = lower(
        r#"
            class Counter { var value: Int32 = 0 }
            struct Point {
                let x: Int32 = 1
                let y: Int32 = 2
            }
            fn main() {
                let counter = Counter()
                let point = Point()
                println("ok")
            }
        "#,
    );
    assert_eq!(core.struct_defaults()[0].len(), 2);
    assert_eq!(core.class_defaults()[0].len(), 1);
    assert!(core
        .struct_defaults()
        .iter()
        .chain(core.class_defaults())
        .flat_map(|fields| fields.iter().flatten())
        .all(|expression| expression.ty == Type::I32));
}
