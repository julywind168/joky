use super::*;

const HELPERS: &str = r#"
    fn read(a: &Array<Int32>) -> Int32 { 7 }
    fn write(a: &mut Array<Int32>) {}
    fn grow(a: &mut Array<Int32>) { let unused = alloc(1) }
    fn consume(a: Array<Int32>) {}
    fn forward(a: Array<Int32>) -> Array<Int32> { a }
    fn view(a: &Array<Int32>) -> &Array<Int32> { a }
    fn exclusive(a: &mut Array<Int32>) -> &mut Array<Int32> { a }

"#;

// Only compiler-owned definitions can introduce the internal Buffer identity.
// Ownership-only cases deliberately avoid depending on the source array library.
fn module(source: &str) -> Result<AstModule, Error> {
    let mut ast = crate::parser::parse(&format!("type Array<T> = T\n{source}")).unwrap();
    let alias = &mut ast.type_aliases[0];
    alias.target = Type::Buffer(Arc::new(Type::Parameter(alias.type_params[0].clone())));
    TypeRegistry::default().resolve_aliases(&ast)
}

fn plan(source: &str) -> Result<ModulePlan, Error> {
    let module = module(&format!("{HELPERS}\n{source}"))?;
    ModulePlan::with_import_layouts_and_types(&module, &HashMap::new(), &TypeRegistry::default())
}

fn accepts(source: &str) {
    if let Err(error) = plan(source) {
        panic!("{source}\n{error:?}");
    }
}

fn rejects(source: &str, message: &str) {
    let error = match plan(source) {
        Ok(_) => panic!("unexpectedly accepted: {source}"),
        Err(error) => error,
    };
    assert!(error.message().contains(message), "{source}\n{error:?}");
    assert!(error.span().is_some(), "missing source location: {error:?}");
}

#[test]
fn buffer_types_are_noncopy_and_have_no_public_memory_layout() {
    let owner = Type::Buffer(Arc::new(Type::Int32));
    let shared = Type::reference(owner.clone(), false);
    let exclusive = Type::reference(owner.clone(), true);
    assert!(!owner.is_copy_value());
    assert!(shared.is_copy_value());
    assert!(!exclusive.is_copy_value());
    for ty in [&owner, &shared, &exclusive] {
        assert!(ty.contains_buffers());
        assert!(!ty.is_copy_storage_value());
        assert_eq!(ty.parameter_slots(), 1);
        assert!(ty.memory_layout().is_err());
        assert!(TypeRegistry::default().validate(ty).is_ok());
    }
    assert_eq!(owner.to_string(), "Buffer<Int32>");
    assert!(TypeRegistry::default()
        .validate(&Type::Buffer(Arc::new(Type::String)))
        .is_err());
    assert!(TypeRegistry::default()
        .validate(&Type::reference(shared, false))
        .is_err());
}

#[test]
fn buffer_aliases_normalize_and_reject_unsupported_elements_and_storage() {
    accepts("struct Box { value: Array<Int32> extra: Array<Int32> } fn f(a: Box) -> Box { a }");
    accepts("type Numbers = Array<Int32> fn f(a: Numbers) -> Numbers { a }");
    accepts("struct Box { value: Array<Int32> } fn f(a: Array<Int32>) -> Box { Box { value: a } }");
    accepts("struct Box { tag: Unit value: Array<Int32> } fn f(a: Array<Int32>) -> Box { Box { tag: {}, value: a } }");
    for source in [
        "type Wrong = Array<String> fn f() {}",
        "struct Box { value: &Array<Int32> } fn f() {}",
        "fn f(a: Array<Int32>) { let bad = (a, 1) }",
        "fn f(a: &Array<Int32>) { let bad = (a, 1) }",
        "fn id<T>(value: T) -> T { value } fn f(a: &Array<Int32>) { let bad = id(a) }",
    ] {
        assert!(plan(source).is_err(), "unexpectedly accepted: {source}");
    }
}

#[test]
fn buffer_moves_cover_bindings_arguments_returns_and_reinitialization() {
    accepts("fn f(input: Array<Int32>) -> Array<Int32> { let a = input let b = a forward(b) }");
    accepts("fn f(input: Array<Int32>, next: Array<Int32>) { var a = input consume(a) a = next write(&mut a) }");
    accepts("fn f(a: Array<Int32>, b: Array<Int32>, flag: Bool) -> Array<Int32> { if flag { a } else { b } }");
    rejects("fn f(a: Array<Int32>) { let b = a consume(a) }", "moved");
    rejects("fn f(a: Array<Int32>) { consume(a) consume(a) }", "moved");
    rejects(
        "fn twice(a: Array<Int32>, b: Array<Int32>) {} fn f(a: Array<Int32>) { twice(a, a) }",
        "moved",
    );
    rejects("fn f(a: Array<Int32>) { a; read(&a); }", "moved");
    let plan = plan("fn f(a: Array<Int32>) -> Array<Int32> { a }").unwrap();
    assert!(plan.functions["f"].internal);
    assert!(plan.bodies["f"]
        .ops
        .iter()
        .any(|op| matches!(op, Op::Buffer(BufferOp::Move { .. }))));
    assert!(!plan.bodies["f"]
        .ops
        .iter()
        .any(|op| matches!(op, Op::Copy(dst, _) if dst.ty.is_buffer())));
}

#[test]
fn buffer_borrows_end_at_last_use_and_reborrow_in_helpers() {
    accepts("fn f(input: Array<Int32>) { var a = input let r = &a let s = r read(r); read(s); write(&mut a) grow(&mut a) write(&mut a) }");
    accepts("fn f(a: &mut Array<Int32>) { write(a) grow(a) write(a) }");
    accepts("fn f(input: Array<Int32>) { var a = input let m = &mut a let s = share(m) read(s); write(m) }");
    accepts("fn f(a: &Array<Int32>) -> &Array<Int32> { view(a) }");
    accepts("fn f(a: &mut Array<Int32>) -> &mut Array<Int32> { exclusive(a) }");
    rejects("fn f(a: Array<Int32>) { write(&mut a) }", "var binding");
    rejects(
        "fn f(input: Array<Int32>) { var a = input let r = &a write(&mut a) read(r); }",
        "live borrow",
    );
    rejects(
        "fn f(input: Array<Int32>) { var a = input let r = &mut a read(&a); write(r) }",
        "live borrow",
    );
}

#[test]
fn live_buffer_views_block_moves_overwrite_and_lexical_escape() {
    rejects(
        "fn f(a: Array<Int32>) { let r = &a consume(a) read(r); }",
        "live borrow",
    );
    rejects(
        "fn f(input: Array<Int32>, b: Array<Int32>) { var a = input let r = &a a = b read(r); }",
        "live borrow",
    );
    rejects(
        "fn f(a: Array<Int32>) { let r = { let local = a; &local } read(r); }",
        "live borrow",
    );
    rejects(
        "fn f(a: Array<Int32>) -> &Array<Int32> { &a }",
        "borrowed input",
    );
    rejects(
        "fn f(a: Array<Int32>, other: &Array<Int32>) -> &Array<Int32> { &a }",
        "live borrow",
    );
    rejects(
        "fn f(a: &Array<Int32>) -> Array<Int32> { memory.read(a) }",
        "control blocks",
    );
    rejects(
        "fn f(a: &mut Array<Int32>, value: Array<Int32>) { memory.write(a, value) }",
        "control blocks",
    );
}

#[test]
fn buffer_loans_follow_returned_views_and_argument_evaluation_order() {
    rejects("fn dependent(a: &Array<Int32>, p: &Int32) -> &Array<Int32> { a } fn f(input: Array<Int32>) { var a = input var n = 1 let r = dependent(&a, &n) n = 2 read(r); }", "live borrow");
    accepts("fn dependent(a: &Array<Int32>, p: &Int32) -> &Array<Int32> { a } fn f(input: Array<Int32>) { var a = input var n = 1 let r = dependent(&a, &n) read(r); n = 2 }");
    rejects(
        "fn f(input: Array<Int32>) { var a = input let r = view(&a) write(&mut a) read(r); }",
        "live borrow",
    );
    rejects("fn pair(a: &mut Array<Int32>, b: Int32) {} fn f(input: Array<Int32>) { var a = input pair(&mut a, read(&a)) }", "live borrow");
    accepts("fn pair(a: &mut Array<Int32>, b: Int32) {} fn f(input: Array<Int32>) { var a = input let n = read(&a) pair(&mut a, n) }");
    rejects(
        "fn pair(a: &Array<Int32>, b: Array<Int32>) {} fn f(a: Array<Int32>) { pair(&a, a) }",
        "live borrow",
    );
}

#[test]
fn buffer_flow_checks_branches_loops_break_continue_and_early_returns() {
    rejects(
        "fn f(a: Array<Int32>, flag: Bool) { if flag { consume(a) } consume(a) }",
        "moved",
    );
    rejects(
        "fn f(a: Array<Int32>, flag: Bool) { while flag { consume(a) } }",
        "moved",
    );
    accepts("fn f(input: Array<Int32>, flag: Bool) -> Array<Int32> { var a = input while flag { a = forward(a) } a }");
    accepts("fn f(input: Array<Int32>, flag: Bool) -> Array<Int32> { var a = input while flag { let b = a a = forward(b) if flag { continue } break } a }");
    accepts("fn f(a: Array<Int32>, flag: Bool) -> Array<Int32> { if flag { return a } a }");
    accepts("fn f(a: Array<Int32>, flag: Bool) { while flag { consume(a) break } }");
}

#[test]
fn buffer_allocation_effect_does_not_retain_views_or_relax_ordinary_references() {
    let plan =
        plan("fn f(input: Array<Int32>) { var a = input grow(&mut a) grow(&mut a) }").unwrap();
    assert!(plan.functions["grow"].uses_arena);
    assert!(plan.functions["f"].uses_arena);
    rejects("fn mixed(a: &mut Array<Int32>, value: &mut Int32) { let p = alloc(1) } fn f(input: Array<Int32>) { var a = input let p = alloc(1) mixed(&mut a, p) memory.write(p, 2) }", "retained by the task arena");
    rejects("fn mixed(a: &mut Array<Int32>, value: &mut Int32) { let p = alloc(1) } fn f(input: Array<Int32>) { var a = input var n = 0 mixed(&mut a, &mut n) }", "local borrow");
    rejects("fn f(a: &Array<Int32>) { let p = alloc(a) }", "Copy data");
}

#[test]
fn buffer_owners_and_views_do_not_enter_suspending_or_host_signatures() {
    rejects("fn f(a: Array<Int32>) @suspends {}", "synchronous");
    rejects("fn f(a: &Array<Int32>) @suspends {}", "synchronous");
    for declaration in [
        "extern fn host(a: Array<Int32>)",
        "extern fn host(a: &Array<Int32>) @suspends",
        "unsafe extern \"C\" fn host(a: &Array<Int32>)",
        "unsafe extern \"C\" fn host() -> Array<Int32>",
    ] {
        rejects(declaration, "host ABI");
    }
    rejects("fn f() -> Array<Int32> { f() }", "moved owner input");
    rejects("fn f() -> &Array<Int32> { f() }", "borrowed input");
}

#[test]
fn buffer_helpers_compile_but_remain_internal_in_all_backends() {
    let ast = module("fn f(a: Array<Int32>) -> Array<Int32> { a }").unwrap();
    let plan =
        ModulePlan::with_import_layouts_and_types(&ast, &HashMap::new(), &TypeRegistry::default())
            .unwrap();
    assert!(plan.functions["f"].internal);
    for mode in [crate::vm::VmMode::Jit, crate::vm::VmMode::Interpreter] {
        let mut vm = crate::vm::VmEngine::with_mode(mode);
        vm.load_module(&ast).unwrap();
        assert!(vm.call_int32_0("f").is_err());
    }
    let mut compiler = crate::aot::AotCompiler::new("buffers").unwrap();
    compiler.compile_module(&ast).unwrap();
}

#[test]
fn buffer_registration_and_identity_cannot_be_forged_by_spelling() {
    let owner = Type::Buffer(Arc::new(Type::Int32));
    for ty in [
        owner.clone(),
        Type::reference(owner.clone(), false),
        Type::reference(owner, true),
    ] {
        for signature in [
            crate::FunctionSignature::new(vec![ty.clone()], Type::Unit),
            crate::FunctionSignature::new(vec![], ty),
        ] {
            assert!(
                crate::modules::validate_host_types(&mut TypeRegistry::default(), &signature)
                    .is_err()
            );
            assert!(crate::core::ffi::validate_registration(&signature).is_err());
            assert!(crate::core::ffi::validate_borrowing_registration(&signature).is_err());
        }
    }
    let types = TypeRegistry::default();
    for source in [
        "fn f(a: Buffer<Int32>) {}",
        "fn f(a: Array<Int32>) {}",
        "import joky/array as array fn f() { array.allocate(1); }",
        "import joky/internal/buffer as b fn f() { b.allocate(1); }",
    ] {
        let ast = crate::parser::parse(source).unwrap();
        let result = types.resolve_aliases(&ast).and_then(|ast| {
            ModulePlan::with_import_layouts_and_types(&ast, &HashMap::new(), &types)
        });
        assert!(result.is_err(), "{source}");
    }
    // Existing user type names remain ordinary Copy records, not capabilities.
    let ast = types
        .resolve_aliases(
            &crate::parser::parse("struct Buffer { value: Int32 } fn f(a: Buffer) -> Buffer { a }")
                .unwrap(),
        )
        .unwrap();
    let plan = ModulePlan::with_import_layouts_and_types(&ast, &HashMap::new(), &types).unwrap();
    assert!(!plan.functions["f"].internal);
}

#[test]
fn owning_element_validation_bounds_shared_nested_array_dags() {
    let mut source =
        "struct Branch<T> { first: Array<T> second: Array<T> } type L0 = Int32\n".to_owned();
    for level in 1..=24 {
        source.push_str(&format!("type L{level} = Branch<L{}>\n", level - 1));
    }
    source.push_str("fn f(a: Array<L24>) -> Array<L24> { a }");
    accepts(&source);
}
