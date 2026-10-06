//! Module constants: declaration, import, evaluation and rejection contract.
use joky::vm::{VmEngine, VmMode};
use joky::{ErrorCode, ModuleSources};

/// Assert that loading `source` fails with `code` and a message containing `text`.
fn reject(source: &str, code: ErrorCode, text: &str) {
    let error = VmEngine::with_mode(VmMode::Jit)
        .load_source(source)
        .unwrap_err();
    assert_eq!(error.code(), code, "{source}: {}", error.message());
    assert!(
        error.message().contains(text),
        "{source}: expected {text:?}, got {:?}",
        error.message()
    );
}

fn sources(entries: &[(&str, &str)]) -> ModuleSources {
    let mut modules = ModuleSources::new();
    for (path, source) in entries {
        modules.insert(*path, *source).unwrap();
    }
    modules
}

fn load(entry: &str, modules: &ModuleSources) -> Result<VmEngine, joky::Error> {
    let mut vm = VmEngine::with_mode(VmMode::Jit);
    vm.load_source_with_modules(entry, modules)?;
    Ok(vm)
}

/// Load `entry` against `modules`, returning the rejection it must produce.
fn load_error(entry: &str, modules: &ModuleSources) -> joky::Error {
    match load(entry, modules) {
        Ok(_) => panic!("{entry} must be rejected"),
        Err(error) => error,
    }
}

fn evaluate(entry: &str, modules: &ModuleSources) -> i32 {
    load(entry, modules)
        .unwrap_or_else(|error| panic!("{error}"))
        .call_int32_0("main")
        .unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn local_module_constants_evaluate_and_forward_reference() {
    let modules = ModuleSources::new();
    assert_eq!(
        evaluate(
            "const MAX: Int32 = 64 const DOUBLE: Int32 = MAX * 2 fn main() -> Int32 { DOUBLE }",
            &modules,
        ),
        128
    );
}

#[test]
fn imported_module_constants_keep_declaration_identity() {
    let modules = sources(&[(
        "app/config",
        "pub const LIMIT: Int32 = 4096 const PRIVATE_MAX: Int32 = 7 pub fn limit() -> Int32 { LIMIT + PRIVATE_MAX }",
    )]);
    // The qualified read, the renamed import and the defining module's own
    // private dependency all resolve to the same declarations.
    assert_eq!(
        evaluate(
            "import app/config as cfg.{LIMIT as CAP} fn main() -> Int32 { cfg.LIMIT + CAP + cfg.limit() }",
            &modules,
        ),
        4096 + 4096 + 4103
    );
}

#[test]
fn module_constants_cover_every_scalar_kind() {
    let source = concat!(
        "const F32: Float32 = 1.5 ",
        "const F64: Float64 = 2.25 ",
        "const I8: Int8 = int8(100) ",
        "const I16: Int16 = int16(-32000) ",
        "const I32: Int32 = -7 ",
        "const I64: Int64 = int64(1) ",
        "const U8: UInt8 = uint8(200) ",
        "const U16: UInt16 = uint16(60000) ",
        "const U32: UInt32 = uint32(4000000) ",
        "const U64: UInt64 = uint64(0) ",
        "const CH: Rune = 'A' ",
        "const FLAG: Bool = !false ",
        "fn main() -> Int32 { I32 }",
    );
    assert_eq!(evaluate(source, &ModuleSources::new()), -7);
}

#[test]
fn module_constant_expressions_reject_unsupported_forms() {
    // The initializer whitelist is the associated-constant one: no calls, no
    // locals, no control flow, and no non-scalar types.
    reject(
        "const A: Int32 = 1 fn helper() -> Int32 { 1 } const B: Int32 = helper()",
        ErrorCode::InvalidState,
        "Constants allow scalar literals",
    );
    reject(
        "const A: String = \"x\" ",
        ErrorCode::TypeMismatch,
        "integer, float, Rune or Bool",
    );
    // A bare function name is not a constant binding.
    reject(
        "fn f() -> Int32 { 1 } const A: Int32 = f fn main() -> Int32 { A }",
        ErrorCode::UnknownName,
        "Unknown module constant `f`",
    );
    // A local binding shadows the constant rather than being rejected, so the
    // shadowing case is a positive test.
    assert_eq!(
        evaluate(
            "const A: Int32 = 1 fn main() -> Int32 { let A = 2 A }",
            &ModuleSources::new()
        ),
        2
    );
}

#[test]
fn module_constant_arithmetic_follows_the_shared_scalar_kernel() {
    // Integer division by zero and negative shifts are compile-time errors.
    reject(
        "const A: Int32 = 1 / 0 fn main() -> Int32 { A }",
        ErrorCode::InvalidState,
        "Invalid division or shift",
    );
    reject(
        "const A: Int32 = 1 << -1 fn main() -> Int32 { A }",
        ErrorCode::InvalidState,
        "Invalid division or shift",
    );
    // Integer overflow wraps at the target width instead of trapping.
    assert_eq!(
        evaluate(
            "const A: Int32 = 4096 * 4096 * 4096 * 4096 fn main() -> Int32 { A }",
            &ModuleSources::new(),
        ),
        0
    );
    // Float division follows IEEE rules: infinity is a value, not an error.
    assert_eq!(
        evaluate(
            "const F: Float64 = 1.0 / 0.0 fn main() -> Int32 { if F > 1.0e308 { 1 } else { 0 } }",
            &ModuleSources::new(),
        ),
        1
    );
    // The declared type still governs the initializer.
    reject(
        "const A: Int32 = 1.5 fn main() -> Int32 { A }",
        ErrorCode::TypeMismatch,
        "mismatch",
    );
    // `const` counts as a declaration: imports cannot follow one.
    reject(
        "const A: Int32 = 1 import joky/io fn main() -> Int32 { A }",
        ErrorCode::Generic,
        "Imports must precede all declarations",
    );
}

#[test]
fn module_constant_cycles_report_the_declaration() {
    for (source, expected) in [
        (
            "const A: Int32 = B const B: Int32 = A fn main() -> Int32 { A }",
            "Cyclic",
        ),
        ("const A: Int32 = A fn main() -> Int32 { A }", "Cyclic"),
    ] {
        let error = VmEngine::with_mode(VmMode::Jit)
            .load_source(source)
            .unwrap_err();
        assert!(
            error.message().contains(expected),
            "{source}: {}",
            error.message()
        );
    }
}

#[test]
fn constant_expressions_beyond_256_syntax_nodes_are_rejected() {
    // `1 + 1 + …` with `count` additions holds `1 + 2 * count` syntax nodes:
    // 127 additions reach 255 nodes, 128 reach 257 and cross the budget.
    let chain = |count: usize| {
        let mut source = String::from("const SUM: Int32 = 1");
        for _ in 0..count {
            source.push_str(" + 1");
        }
        source.push_str(" fn main() -> Int32 { SUM }");
        source
    };
    VmEngine::with_mode(VmMode::Jit)
        .load_source(&chain(127))
        .unwrap_or_else(|error| panic!("255 nodes must be accepted: {error}"));
    let error = VmEngine::with_mode(VmMode::Jit)
        .load_source(&chain(128))
        .unwrap_err();
    assert!(
        error.message().contains("256 syntax nodes"),
        "{}",
        error.message()
    );
}

#[test]
fn unused_constant_declarations_are_still_checked() {
    // Declaring a constant is enough to require a valid initializer; the
    // declaration must not escape checking because no body reads it.
    let error = VmEngine::with_mode(VmMode::Jit)
        .load_source("const UNUSED: Int32 = UNKNOWN fn main() -> Int32 { 0 }")
        .unwrap_err();
    assert!(error.message().contains("Unknown"), "{}", error.message());
}

#[test]
fn constant_dependency_depth_beyond_the_limit_is_rejected() {
    // 32 dependency levels are allowed; the 33rd is rejected with a stable
    // resource-limit diagnostic rather than unbounded recursion.
    let chain = |count: usize| {
        let mut source = String::from("const C0: Int32 = 1 ");
        for i in 1..count {
            source.push_str(&format!("const C{i}: Int32 = C{} ", i - 1));
        }
        source.push_str(&format!("fn main() -> Int32 {{ C{} }}", count - 1));
        source
    };
    VmEngine::with_mode(VmMode::Jit)
        .load_source(&chain(32))
        .unwrap_or_else(|error| panic!("32 levels must be accepted: {error}"));
    let error = VmEngine::with_mode(VmMode::Jit)
        .load_source(&chain(40))
        .unwrap_err();
    assert!(error.message().contains("32 levels"), "{}", error.message());
}

#[test]
fn private_module_constants_are_not_readable_across_modules() {
    let modules = sources(&[(
        "app/config",
        "const SECRET: Int32 = 7 pub fn reveal() -> Int32 { SECRET }",
    )]);
    // The defining module reads its own private constant through a public API.
    assert_eq!(
        evaluate(
            "import app/config as cfg fn main() -> Int32 { cfg.reveal() }",
            &modules
        ),
        7
    );
    // A consumer cannot name it, though the declaration exists: the module
    // never exports the spelling, so the read is an undeclared constant.
    let error = load_error(
        "import app/config as cfg fn main() -> Int32 { cfg.SECRET }",
        &modules,
    );
    assert_eq!(error.code(), ErrorCode::UnknownName);
    assert!(
        error.message().contains("Undeclared constant `cfg.SECRET`"),
        "{}",
        error.message()
    );
}

#[test]
fn module_constants_read_identically_in_jit_and_interpreter() {
    let source =
        "const BASE: Int32 = 6 const SCALED: Int32 = BASE * 7 fn main() -> Int32 { SCALED }";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            vm.call_int32_0("main")
                .unwrap_or_else(|error| panic!("{error}")),
            42,
            "{mode:?}"
        );
    }
}

#[test]
fn selector_import_of_a_missing_constant_reports_the_export_error() {
    let modules = sources(&[("app/config", "pub const LIMIT: Int32 = 1")]);
    let error = load_error(
        "import app/config as cfg.{MISSING} fn main() -> Int32 { cfg.LIMIT }",
        &modules,
    );
    assert_eq!(error.code(), ErrorCode::InvalidImport);
    assert!(
        error.message().contains("does not export value 'MISSING'"),
        "{}",
        error.message()
    );
}

#[test]
fn constant_initializers_use_imported_constants_by_every_spelling() {
    let modules = sources(&[
        (
            "app/config",
            "pub const LIMIT: Int32 = 100 pub const SCALE: Int32 = 3",
        ),
        (
            "app/derived",
            concat!(
                "import app/config as cfg.{LIMIT as CAP}\n",
                "const LOCAL: Int32 = cfg.SCALE * CAP\n",
                "pub fn total() -> Int32 { LOCAL }\n",
            ),
        ),
    ]);
    // A source module's own constant initializer resolves the renamed
    // selector import and the alias-qualified spelling alike.
    assert_eq!(
        evaluate(
            "import app/derived as d fn main() -> Int32 { d.total() }",
            &modules,
        ),
        300
    );
}

#[test]
fn public_constants_may_depend_on_private_constants() {
    let modules = sources(&[(
        "app/config",
        "const SECRET: Int32 = 2048 pub const LIMIT: Int32 = SECRET * 2",
    )]);
    // Only the computed value is exported; the private name stays unbound.
    assert_eq!(
        evaluate(
            "import app/config as cfg fn main() -> Int32 { cfg.LIMIT }",
            &modules,
        ),
        4096
    );
    let error = load_error(
        "import app/config as cfg const USE: Int32 = cfg.SECRET fn main() -> Int32 { USE }",
        &modules,
    );
    assert!(
        error.message().contains("constant `cfg.SECRET`"),
        "{}",
        error.message()
    );
}

#[test]
fn module_and_associated_constants_share_one_dependency_graph() {
    // Module constants and associated constants feed each other through the
    // single declaration-identity graph, in both directions.
    assert_eq!(
        evaluate(
            concat!(
                "const BASE: Int32 = 5\n",
                "impl Int32 {\n",
                "    const SHIFTED: Int32 = BASE * 3\n",
                "}\n",
                "const TOTAL: Int32 = Int32.SHIFTED + BASE\n",
                "fn main() -> Int32 { TOTAL }\n",
            ),
            &ModuleSources::new(),
        ),
        20
    );
    reject(
        concat!(
            "const A: Int32 = Int32.B\n",
            "impl Int32 {\n",
            "    const B: Int32 = A\n",
            "}\n",
            "fn main() -> Int32 { A }\n",
        ),
        ErrorCode::InvalidState,
        "Cyclic",
    );
}

#[test]
fn imported_constants_follow_the_value_namespace_rules() {
    let modules = sources(&[
        ("app/first", "pub const VALUE: Int32 = 1"),
        ("app/second", "pub const OTHER: Int32 = 2"),
    ]);
    // An imported constant cannot reuse a module alias name.
    let error = load_error(
        "import app/first.{VALUE as second}\nimport app/second as second\nfn main() -> Int32 { second }",
        &modules,
    );
    assert_eq!(error.code(), ErrorCode::DuplicateDefinition);
    assert!(
        error.message().contains("Module alias 'second'"),
        "{}",
        error.message()
    );
    // Integer conversions and memory intrinsics stay reserved.
    let error = load_error(
        "import app/first.{VALUE as int32}\nfn main() -> Int32 { int32 }",
        &modules,
    );
    assert_eq!(error.code(), ErrorCode::ReservedName);
    assert!(error.message().contains("Reserved"), "{}", error.message());
    // A local binding shadows the module prefix: the projection reads the
    // binding's fields and never falls back to module lookup.
    let error = load_error(
        concat!(
            "import app/first as cfg\n",
            "fn main() -> Int32 {\n",
            "    let cfg = 1\n",
            "    cfg.VALUE\n",
            "}\n",
        ),
        &modules,
    );
    assert!(
        error.message().contains("no field") || error.message().contains("fields"),
        "{}",
        error.message()
    );
}

#[test]
fn diamond_dependencies_resolve_through_shared_declarations() {
    // base is imported through two different modules; the joined constant
    // still resolves to one declaration identity, not two spellings.
    let modules = sources(&[
        ("app/base", "pub const B: Int32 = 10"),
        (
            "app/left",
            "import app/base.{B}\npub const L: Int32 = B + 1",
        ),
        (
            "app/right",
            "import app/base as base\npub const R: Int32 = base.B + 2",
        ),
        (
            "app/top",
            concat!(
                "import app/left.{L}\n",
                "import app/right as right\n",
                "pub const T: Int32 = L * right.R\n",
            ),
        ),
    ]);
    assert_eq!(
        evaluate("import app/top.{T} fn main() -> Int32 { T }", &modules),
        132
    );
}

#[test]
fn module_constants_conflict_with_values_in_the_same_namespace() {
    reject(
        "const DUP: Int32 = 1 fn DUP() -> Int32 { 0 }",
        ErrorCode::DuplicateDefinition,
        "already defined",
    );
    // Bare enum constructors share the value namespace with constants.
    reject(
        "enum Light { On }\nconst On: Int32 = 1 fn main() -> Int32 { On }",
        ErrorCode::DuplicateDefinition,
        "already defined",
    );
    // Two constants under one name are rejected inside the same module, but
    // different modules keep private same-name constants without colliding.
    reject(
        "const DUP: Int32 = 1 const DUP: Int32 = 2 fn main() -> Int32 { DUP }",
        ErrorCode::DuplicateDefinition,
        "Duplicate module constant",
    );
    let modules = sources(&[
        (
            "app/first",
            "const SHARED: Int32 = 1 pub fn a() -> Int32 { SHARED }",
        ),
        (
            "app/second",
            "const SHARED: Int32 = 2 pub fn b() -> Int32 { SHARED }",
        ),
    ]);
    assert_eq!(
        evaluate(
            "import app/first as f
import app/second as s
fn main() -> Int32 { f.a() + s.b() }",
            &modules,
        ),
        3
    );
}
