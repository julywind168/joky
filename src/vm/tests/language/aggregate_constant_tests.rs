//! Copy aggregate constants retain typed layouts and declaration scope.
use super::{VmEngine, VmMode};
use crate::{ErrorCode, ModuleSources};

const SOURCE: &str = include_str!("../../../../tests/fixtures/aggregate_constants.jk");
const VALUES: &str = include_str!("../../../../tests/fixtures/aggregate_constants_values.jk");

fn modules() -> ModuleSources {
    let mut modules = ModuleSources::new();
    modules
        .insert("aggregate_constants_values", VALUES)
        .unwrap();
    modules
}

#[test]
fn aggregate_constants_copy_project_and_survive_module_replacement() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source_with_modules(SOURCE, &modules())
            .unwrap_or_else(|error| {
                panic!(
                    "{mode:?}: {error} at {:?}",
                    error.span().and_then(|span| span.slice(SOURCE))
                )
            });
        assert_eq!(
            vm.call_int32_0("check")
                .unwrap_or_else(|error| panic!("{mode:?}: {error}")),
            42
        );
        let mut task = vm.call_async("pending", &[]).unwrap();
        assert!(!task.is_finished());
        let error = vm
            .load_source("const BAD: (Int32, Bool) = (1, 2)")
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::TypeMismatch);
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
        vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
        drop(vm);
        while !task.is_finished() {
            task.resume().unwrap();
        }
        assert_eq!(task.cps_result(), Some(42));
        drop(task);
        crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

#[test]
fn aggregate_constants_reject_other_owners_symbolic_types_and_invalid_initializers() {
    for (source, code, message) in [
        ("import joky/set.{type Set} const BAD: (Int32, Set<Int32>) = (1, Set.new::<Int32>())", ErrorCode::TypeMismatch, "constants require"),
        ("import joky/set.{type Set} struct S { values: Set<Int32> } struct P { s: S } const BAD: P = P { s: S { values: Set.new::<Int32>() } }", ErrorCode::TypeMismatch, "constants require"),
        ("const BAD: (Int32, &Int32) = (1, &1)", ErrorCode::TypeMismatch, "constants require"),
        ("struct P { p: Ptr } const BAD: P = P { p: Ptr.null() }", ErrorCode::TypeMismatch, "constants require"),
        ("import joky/fixed_array.{type FixedArray} import joky/set.{type Set} const BAD: FixedArray<Set<Int32>, 0> = []", ErrorCode::TypeMismatch, "constants require"),
        ("import joky/fixed_array.{type FixedArray} struct Marker<T> {} struct P<const N: UInt64> {} impl<const N: UInt64> P<N> { const BAD: Marker<FixedArray<Int32, N>> = Marker {} }", ErrorCode::TypeMismatch, "constants require"),
        ("enum E { A, Ref(&Int32) } const BAD: (E, Int32) = (E.A, 1)", ErrorCode::TypeMismatch, "constants require"),
        ("struct P<T> { x: T } impl<T> P<T> { const BAD: P<T> = P { x: 1 } }", ErrorCode::TypeMismatch, "constants require"),
        ("struct P<T> {} impl<T> P<T> { const BAD: P<T> = P {} }", ErrorCode::TypeMismatch, "constants require"),
        ("struct P<const N: UInt64> {} impl<const N: UInt64> P<N> { const BAD: P<N> = P {} }", ErrorCode::TypeMismatch, "constants require"),
        ("const BAD: (Int32, Bool) = (1, 2)", ErrorCode::TypeMismatch, "mismatch"),
        ("struct P { x: UInt8 } const BAD: P = P { x: 256 }", ErrorCode::TypeMismatch, "range"),
        ("struct P { x: Int32 } const BAD: P = P {}", ErrorCode::TypeMismatch, "Missing field"),
        ("struct P { x: Int32 } const BAD: P = P { x: 1, x: 2 }", ErrorCode::DuplicateDefinition, "Duplicate"),
        ("const BAD: Int32 = (1, 2).2", ErrorCode::UnknownName, "out of range"),
        ("const A: (Int32, Int32) = (1, B) const B: Int32 = A.0", ErrorCode::InvalidState, "Cyclic"),
        ("struct P { x: Int32 } const N: Int32 = P.DEFAULT.x impl P { const DEFAULT: P = P { x: N } }", ErrorCode::InvalidState, "Cyclic"),
        ("fn make() -> Int32 { 1 } const BAD: (Int32, Int32) = (make(), 2)", ErrorCode::InvalidState, "Constants allow"),
        ("const BAD: (Int32, Int32) = ({ let x = 1; x }, 2)", ErrorCode::InvalidState, "Constants allow"),
        ("struct P { x: Int32 } const BAD: P = if true { P { x: 1 } } else { P { x: 2 } }", ErrorCode::InvalidState, "Constants allow"),
    ] {
        let error = super::support::frontend_error(source, &ModuleSources::new());
        assert_eq!(error.code(), code, "{source}: {error}");
        assert!(error.message().contains(message), "{source}: {error}");
    }
}

#[test]
fn aggregate_constants_preserve_private_fields_in_values_and_construction() {
    for body in [
        "const BAD: UInt64 = cfg.DEFAULT.hidden",
        "const BAD: UInt64 = cfg.Settings.DEFAULT.hidden",
        "fn bad() -> UInt64 { cfg.DEFAULT.hidden }",
        "const BAD: cfg.Settings = cfg.Settings { hidden: 0, ..cfg.DEFAULT }",
        "const BAD: cfg.Settings = cfg.Settings { point: cfg.DEFAULT.point, pair: (7, true), hidden: 0 }",
    ] {
        let source = format!("import aggregate_constants_values as cfg {body}");
        let error = super::support::frontend_error(&source, &modules());
        assert_eq!(error.code(), ErrorCode::UnknownName, "{source}: {error}");
        assert!(error.message().contains("private"), "{source}: {error}");
    }
}

#[test]
fn aggregate_constants_bound_nesting_expansion_and_total_components() {
    let nested = |depth: usize| {
        let mut source = "struct R0 {}\n".to_owned();
        let mut value = "R0 {}".to_owned();
        for index in 1..depth {
            source += &format!("struct R{index} {{ value: R{} }}\n", index - 1);
            value = format!("R{index} {{ value: {value} }}");
        }
        source += &format!(
            "const VALUE: R{} = {value}\nfn check() -> Int32 {{ VALUE; 42 }}",
            depth - 1
        );
        source
    };
    let mut vm = VmEngine::new_interpreter();
    vm.load_source(&nested(32))
        .unwrap_or_else(|error| panic!("32 levels: {error}"));
    assert_eq!(vm.call_int32_0("check").unwrap(), 42);
    let deep = format!(
        "{} struct R32 {{ value: R31 }} const DEEP: R32 = R32 {{ value: VALUE }}",
        nested(32)
    );
    let error = super::support::frontend_error(&deep, &ModuleSources::new());
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert!(error.message().contains("aggregate nesting"), "{error}");

    // Repeated aliases expand a DAG. Empty leaves test the bound independently
    // of the existing scalar-layout limit and require no runtime allocations.
    let mut source = "struct R0 {} const C0: R0 = R0 {}\n".to_owned();
    for index in 1..=11 {
        let previous = index - 1;
        source += &format!("struct R{index} {{ left: R{previous}, right: R{previous} }}\nconst C{index}: R{index} = R{index} {{ left: C{previous}, right: C{previous} }}\n");
    }
    source += "fn check() -> Int32 { C11; 42 }\n";
    let exact = format!(
        "{source} struct Exact {{ value: R11 }} const EXACT: Exact = Exact {{ value: C11 }}"
    );
    vm.load_source(&exact)
        .unwrap_or_else(|error| panic!("4096 components: {error}"));
    assert_eq!(vm.call_int32_0("check").unwrap(), 42);
    let expanded = format!("{source}struct Wide {{ value: R11, unit: Unit }} const WIDE: Wide = Wide {{ value: C11, unit: {{}} }}");
    let error = super::support::frontend_error(&expanded, &ModuleSources::new());
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert!(error.message().contains("4096 components"), "{error}");
    // C0..C11 consume 8178 components; fourteen aliases leave 28 components.
    for index in 0..14 {
        source += &format!("const ALIAS{index}: R11 = C11\n");
    }
    for index in 0..28 {
        source += &format!("const EMPTY{index}: R0 = C0\n");
    }
    vm.load_source(&source)
        .unwrap_or_else(|error| panic!("65536 total components: {error}"));
    source += "struct Holder {} impl Holder { const EXTRA: R0 = C0 }";
    let error = super::support::frontend_error(&source, &ModuleSources::new());
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert!(error.message().contains("65536 components"), "{error}");
    assert_eq!(vm.call_int32_0("check").unwrap(), 42);
}
