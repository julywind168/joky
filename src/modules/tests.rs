use super::*;
use crate::{
    core::resolver::{self, BindingSpec},
    runtime::cps::CpsFrame,
    vm::{VmEngine, VmMode},
    FunctionSignature, Span,
};

extern "C" fn increment(value: i32) -> i32 {
    value + 1
}

fn direct_binding() -> NativeBinding {
    NativeBinding {
        spec: BindingSpec::with_symbol(
            "math.next",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            "host_increment",
        ),
        entry: Some(HostEntry::Direct(increment as *const u8)),
        owner: None,
    }
}

fn registry(binding: NativeBinding) -> Rc<ModuleRegistry> {
    let mut registry = ModuleRegistry::default();
    unsafe { registry.register_module("user/math", [binding]) }.unwrap();
    Rc::new(registry)
}

#[test]
fn injected_modules_run_and_reload_in_both_vm_modes() {
    let registry = registry(direct_binding());
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode_and_registry(mode, registry.clone());
        let source = "import user/math fn answer() -> Int32 { math.next(41) }";
        vm.load_source(source).unwrap();
        assert_eq!(vm.call_int32_0("answer").unwrap(), 42);
        let version = vm.module_version();
        for invalid in [
            "import user/missing fn answer() -> Int32 { 0 }",
            "import user/math fn answer() -> Int32 { math.next(true) }",
        ] {
            assert!(vm.load_source(invalid).is_err());
            assert_eq!(vm.module_version(), version);
            assert_eq!(vm.call_int32_0("answer").unwrap(), 42);
        }
        vm.load_source("import user/math fn answer() -> Int32 { math.next(99) }")
            .unwrap();
        assert_eq!(vm.call_int32_0("answer").unwrap(), 100);
        assert_ne!(vm.module_version(), version);
    }
}

#[test]
fn registry_rejects_conflicts_atomically() {
    let mut registry = ModuleRegistry::default();
    unsafe { registry.register_module("user/math", [direct_binding()]) }.unwrap();
    let mut second = direct_binding();
    second.spec.name = "second.next".into();
    second.spec.symbol = Some("second_increment".into());
    let duplicate_module =
        unsafe { registry.register_module("user/math", [second.clone()]) }.unwrap_err();
    assert_eq!(duplicate_module.code(), ErrorCode::DuplicateDefinition);
    let duplicate_name =
        unsafe { registry.register_module("user/other", [second.clone(), direct_binding()]) }
            .unwrap_err();
    assert_eq!(duplicate_name.code(), ErrorCode::DuplicateDefinition);
    assert!(registry.binding("second.next").is_none());
    assert!(registry.metadata().module("user/other").is_none());
    second.spec.symbol = Some("host_increment".into());
    let duplicate_symbol =
        unsafe { registry.register_module("user/other", [second.clone()]) }.unwrap_err();
    assert_eq!(
        (duplicate_symbol.phase(), duplicate_symbol.code()),
        (ErrorPhase::Abi, ErrorCode::AbiError)
    );
    second.spec.symbol = Some("second_increment".into());
    unsafe { registry.register_module("user/other", [second]) }.unwrap();
    assert_eq!(
        registry
            .metadata()
            .binding("math.next")
            .unwrap()
            .symbol
            .as_deref(),
        Some("host_increment")
    );
    assert!(registry.binding("second.next").is_some());
}

#[test]
fn native_entries_require_a_matching_abi_and_valid_pointer() {
    let mut registry = ModuleRegistry::default();
    let mut null = direct_binding();
    null.entry = Some(HostEntry::Direct(std::ptr::null()));
    let mut mismatch = direct_binding();
    mismatch.spec.suspends = true;
    for binding in [null, mismatch] {
        let error = unsafe { registry.register_module("user/math", [binding]) }.unwrap_err();
        assert_eq!(
            (error.phase(), error.code()),
            (ErrorPhase::Abi, ErrorCode::AbiError)
        );
        assert!(registry.metadata().module("user/math").is_none());
    }
    for slots in [0, i32::MAX as usize / 8] {
        let mut binding = cps_binding(Rc::new(()));
        binding.entry = Some(HostEntry::Cps {
            entry: yield_value,
            frame_slots: Some(slots),
        });
        let error = unsafe { registry.register_module("user/math", [binding]) }.unwrap_err();
        assert_eq!(
            (error.phase(), error.code()),
            (ErrorPhase::Abi, ErrorCode::AbiError)
        );
        assert!(registry.metadata().module("user/math").is_none());
    }
    unsafe { registry.register_module("user/math", [direct_binding()]) }.unwrap();
}

#[test]
fn import_errors_keep_the_offending_source_span() {
    let mut binding = direct_binding();
    binding.spec.name = "next".into();
    let registry = registry(binding);
    for (source, label, code) in [
        (
            "import user/absent fn f() {}",
            "import user/absent",
            ErrorCode::InvalidImport,
        ),
        (
            "import user/math import user/math fn f() {}",
            "import user/math",
            ErrorCode::InvalidImport,
        ),
        (
            "import user/math extern fn next(value: Int32) -> Int32",
            "import user/math",
            ErrorCode::DuplicateDefinition,
        ),
        (
            "import user/math fn next(value: Int32) -> Int32 { value }",
            "import user/math",
            ErrorCode::DuplicateDefinition,
        ),
    ] {
        let module = crate::parser::parse(source).unwrap();
        let error = resolver::resolve(&module, registry.metadata()).unwrap_err();
        let start = source.rfind(label).unwrap();
        assert_eq!((error.phase(), error.code()), (ErrorPhase::Resolve, code));
        assert_eq!(error.span(), Some(Span::new(start, start + label.len())));
    }
}

#[test]
fn explicit_host_aliases_use_canonical_registry_signatures() {
    let mut binding = direct_binding();
    binding.spec.name = "next".into();
    let registry = registry(binding);
    let valid = "type Count = Int32 extern fn next(value: Count) -> Count fn answer() -> Count { next(41) }";
    let invalid = "type Flag = Bool extern fn next(value: Flag) -> Int32";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode_and_registry(mode, registry.clone());
        vm.load_source(valid).unwrap();
        assert_eq!(vm.call_int32_0("answer").unwrap(), 42);
        assert_eq!(
            vm.load_source(invalid).unwrap_err().code(),
            ErrorCode::AbiError
        );
        assert_eq!(vm.call_int32_0("answer").unwrap(), 42);
    }
    for (source, success) in [(valid, true), (invalid, false)] {
        let mut compiler = crate::aot::AotCompiler::with_symbol_prefix_and_registry(
            "alias_host",
            "",
            registry.clone(),
        )
        .unwrap();
        let result = compiler.compile_module(&crate::parser::parse(source).unwrap());
        if success {
            result.unwrap();
        } else {
            assert_eq!(result.unwrap_err().code(), ErrorCode::AbiError);
        }
    }
}

#[test]
fn explicit_extern_declarations_cannot_override_registry_signatures() {
    let mut binding = direct_binding();
    binding.spec.name = "next".into();
    let registry = registry(binding);
    for source in [
        "extern fn next(value: Bool) -> Int32 fn answer() -> Int32 { next(true) }",
        "extern fn next(value: Int32) @suspends -> Int32 fn answer() @suspends -> Int32 { next(41) }",
    ] {
        let ast = crate::parser::parse(source).unwrap();
        let Item::ExternFn(f) = &ast.items[0] else { panic!("expected extern") };
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode_and_registry(mode, registry.clone());
            vm.load_source("fn answer() -> Int32 { 42 }").unwrap();
            let error = vm.load_source(source).unwrap_err();
            assert_eq!((error.phase(), error.code()), (ErrorPhase::Abi, ErrorCode::AbiError));
            assert_eq!(error.span(), Some(f.span));
            assert_eq!(vm.call_int32_0("answer").unwrap(), 42);
        }
        let mut compiler = crate::aot::AotCompiler::with_symbol_prefix_and_registry("invalid", "", registry.clone()).unwrap();
        let error = compiler.compile_module(&ast).unwrap_err();
        assert_eq!((error.phase(), error.code()), (ErrorPhase::Abi, ErrorCode::AbiError));
        assert_eq!(error.span(), Some(f.span));
    }
}

#[test]
fn unrelated_imports_do_not_enable_vm_call_rewrites() {
    let registry = registry(direct_binding());
    let module = crate::parser::parse("import user/math fn f() { vm.call_int32(1) }").unwrap();
    let resolved = resolver::resolve(&module, registry.metadata()).unwrap();
    let Item::FnDef(f) = &resolved.items[0] else {
        panic!("expected function")
    };
    let crate::parser::ast::Expr::Block { exprs, .. } = &f.body else {
        panic!("expected block")
    };
    assert!(
        matches!(&exprs[0], crate::parser::ast::Expr::Call { callee, .. } if callee == "vm.call_int32")
    );
}

unsafe extern "C" fn yield_value(frame: *mut CpsFrame) -> i32 {
    let frame = &mut *frame;
    if frame.state == 0 {
        frame.slots[3] = frame.slots[0] + 1;
        frame.state = 1;
        1
    } else {
        frame.result = frame.slots[3];
        0
    }
}

fn cps_binding(owner: Rc<dyn Any>) -> NativeBinding {
    let mut binding = direct_binding();
    binding.spec.suspends = true;
    binding.entry = Some(HostEntry::Cps {
        entry: yield_value,
        frame_slots: Some(4),
    });
    binding.owner = Some(owner);
    binding
}

#[test]
fn imported_cps_entries_keep_layouts_and_owners_after_reload_and_drop() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let owner = Rc::new(());
        let weak = Rc::downgrade(&owner);
        let registry = registry(cps_binding(owner));
        let mut vm = VmEngine::with_mode_and_registry(mode, registry.clone());
        drop(registry);
        vm.load_source("import user/math fn answer() @suspends -> Int32 { math.next(41) }")
            .unwrap();
        let mut task = vm.call_async("answer", &[]).unwrap();
        assert!(!task.is_finished());
        vm.load_source("fn answer() -> Int32 { 0 }").unwrap();
        drop(vm);
        assert!(weak.upgrade().is_some());
        task.resume().unwrap();
        assert_eq!(task.cps_result().unwrap(), 42);
        drop(task);
        assert!(weak.upgrade().is_none());
    }
}

#[test]
fn symbol_only_imports_fail_before_publishing_vm_code() {
    let mut binding = direct_binding();
    binding.entry = None;
    let registry = registry(binding);
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode_and_registry(mode, registry.clone());
        vm.load_source("fn answer() -> Int32 { 42 }").unwrap();
        let error = vm
            .load_source("import user/math fn answer() -> Int32 { math.next(41) }")
            .unwrap_err();
        assert_eq!(
            (error.phase(), error.code()),
            (ErrorPhase::Abi, ErrorCode::HostError)
        );
        assert_eq!(
            error
                .span()
                .unwrap()
                .slice("import user/math fn answer() -> Int32 { math.next(41) }"),
            Some("import user/math")
        );
        assert_eq!(vm.call_int32_0("answer").unwrap(), 42);
    }
}

#[test]
fn registry_types_and_native_signatures_match_across_backends() {
    extern "C" fn token() -> u64 {
        42
    }
    extern "C" fn value(token: u64) -> i32 {
        token as i32
    }
    let mut registry = ModuleRegistry::default();
    let token_type = Type::Named("user.Token".into());
    unsafe {
        registry.register_module(
            "user/tokens",
            [
                NativeBinding {
                    spec: BindingSpec::with_symbol(
                        "tokens.create",
                        FunctionSignature::new(vec![], token_type.clone()),
                        "token_create",
                    ),
                    entry: Some(HostEntry::Direct(token as *const u8)),
                    owner: None,
                },
                NativeBinding {
                    spec: BindingSpec::with_symbol(
                        "tokens.value",
                        FunctionSignature::new(vec![token_type], Type::Int32),
                        "token_value",
                    ),
                    entry: Some(HostEntry::Direct(value as *const u8)),
                    owner: None,
                },
            ],
        )
    }
    .unwrap();
    let registry = Rc::new(registry);
    let source = "import user/tokens as t.{type Token as Handle, create as make} fn answer() -> Int32 { let token: Handle = make() let same: t.Token = token t.value(same) }";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode_and_registry(mode, registry.clone());
        vm.load_source(source).unwrap();
        assert_eq!(vm.call_int32_0("answer").unwrap(), 42);
    }
    let mut aot =
        crate::aot::AotCompiler::with_symbol_prefix_and_registry("tokens", "", registry).unwrap();
    aot.compile_module(&crate::parser::parse(source).unwrap())
        .unwrap();
}

#[test]
fn standard_registry_is_shared_and_cps_layout_conflicts_are_reported() {
    assert!(Rc::ptr_eq(
        &crate::stdlib::module_registry(),
        &crate::stdlib::module_registry()
    ));
    let registry = registry(cps_binding(Rc::new(())));
    let ast =
        crate::parser::parse("import user/math fn answer() @suspends -> Int32 { math.next(41) }")
            .unwrap();
    let mut compiler =
        crate::aot::AotCompiler::with_symbol_prefix_and_registry("layout", "", registry).unwrap();
    let error = compiler
        .compile_with_import_layouts(&ast, &HashMap::from([("math.next".into(), 1)]))
        .unwrap_err();
    assert_eq!(
        (error.phase(), error.code()),
        (ErrorPhase::Abi, ErrorCode::AbiError)
    );
}

#[test]
fn imported_aliases_preserve_pending_calls_and_owners_after_reload() {
    use std::cell::Cell;
    thread_local! { static STARTS: Cell<u32> = const { Cell::new(0) }; }
    unsafe extern "C" fn pending(frame: *mut CpsFrame) -> i32 {
        let frame = &mut *frame;
        if frame.state == 0 {
            STARTS.with(|starts| starts.set(starts.get() + 1));
            frame.slots[3] = frame.slots[0] + 1;
        }
        if frame.state < 3 {
            frame.state += 1;
            1
        } else {
            frame.result = frame.slots[3];
            0
        }
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        STARTS.with(|starts| starts.set(0));
        let owner = Rc::new(());
        let weak = Rc::downgrade(&owner);
        let mut binding = cps_binding(owner);
        binding.entry = Some(HostEntry::Cps {
            entry: pending,
            frame_slots: Some(4),
        });
        let mut vm = VmEngine::with_mode_and_registry(mode, registry(binding));
        vm.load_source("import user/math as m.{next as plus} fn answer() @suspends -> Int32 { let math = 99 match Some(plus(40)) { | Some(n) => m.next(n) | None => 0 } }").unwrap();
        let mut task = vm.call_async("answer", &[]).unwrap();
        assert!(!task.is_finished());
        task.resume().unwrap();
        assert_eq!(STARTS.with(Cell::get), 1);
        vm.set_prelude_enabled(false);
        vm.load_source("fn answer() -> Int32 { 0 }").unwrap();
        drop(vm);
        assert!(weak.upgrade().is_some());
        for _ in 0..8 {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        assert_eq!(task.cps_result(), Some(42));
        assert_eq!(STARTS.with(Cell::get), 2);
        drop(task);
        assert!(weak.upgrade().is_none());
    }
}
