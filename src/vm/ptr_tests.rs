//! Opaque C pointers never acquire reference or resource ownership semantics.
use super::{VmEngine, VmMode};
use crate::{FunctionSignature, Type};

extern "C" fn identity(p: *mut u8) -> *mut u8 {
    p
}

#[test]
fn ptr_intrinsics_aliases_modules_and_layouts_are_consistent() {
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "foreign",
            r#"
        pub type Address = Ptr
        unsafe extern "C" fn identity(p: Address) -> Address
        pub fn forward<T>(p: T) -> T { p }
        pub fn empty() -> Address { Ptr.null() }
        pub fn check(p: Address) -> Bool { Ptr.is_null(unsafe { identity(p) }) }
    "#,
        )
        .unwrap();
    let source = r#"
        import foreign.{type Address, forward, empty, check}
        fn f() -> Int32 {
            let p: Address = forward(empty())
            if check(p) && Ptr.is_null(p) { 42 } else { 0 }
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        unsafe {
            vm.register_c_fn(
                "identity",
                FunctionSignature::new(vec![Type::Ptr], Type::Ptr),
                identity as *const u8,
                None,
            )
            .unwrap();
        }
        vm.load_source_with_modules(source, &modules)
            .unwrap_or_else(|e| panic!("{mode:?}: {e} {:?}", e.span()));
        assert_eq!(vm.call_int32_0("f").unwrap(), 42);
    }
    let ast = crate::TypeRegistry::default()
        .resolve_with_modules(&crate::parser::parse(source).unwrap(), &modules)
        .unwrap();
    crate::aot::AotCompiler::new("ptr-imports")
        .unwrap()
        .compile_module(&ast)
        .unwrap();
    let header = crate::ffi::c_header(&ast).unwrap();
    assert!(header.contains("void * identity(void *arg0);"), "{header}");
    for width in [4, 8] {
        let target = crate::memory_layout::MemoryLayoutTarget {
            pointer_size: width,
            pointer_align: width,
            ..Default::default()
        };
        let layout = Type::Ptr.memory_layout_for(target).unwrap();
        assert_eq!((layout.size, layout.align), (width, width));
    }
    assert_eq!(Type::Ptr.to_string(), "Ptr");
}

#[test]
fn ptr_rejects_unsafe_bypasses_and_invalid_operations_in_all_backends() {
    let prefix = "unsafe extern \"C\" fn identity(p: Ptr) -> Ptr ";
    for body in [
        "fn f() { identity(Ptr.null()); }",
        "fn f() { unsafe { identity(Ptr.null()); }; identity(Ptr.null()); }",
        "fn bad() { identity(Ptr.null()); } fn f() { unsafe { bad() } }",
        "fn f() { let p: Ptr = 0; }",
        "fn f() { let p: Ptr = alloc(1); }",
        "fn f() { let p: &Int32 = Ptr.null(); }",
        "fn f() -> &Int32 { unsafe { identity(Ptr.null()) } }",
        "fn f() { unsafe { identity(alloc(1)); } }",
        "fn f() { let p = Ptr.null(); unsafe { p + 1; } }",
        "fn f() { let p = Ptr.null(); unsafe { p.x; } }",
        "fn f() { unsafe { int64(Ptr.null()); } }",
        "fn f() { unsafe { uint64(Ptr.null()); } }",
        "fn f() { unsafe { (*(Ptr.null())); } }",
        "fn f() { unsafe { (*(Ptr.null()) = 1); } }",
        "fn f() { (&*(Ptr.null())); }",
        "fn f() { Ptr.null(0); }",
        "fn f() { Ptr.is_null(); }",
        "fn f() { Ptr.is_null(0); }",
        "fn f() { Ptr.is_null(Ptr.null(), Ptr.null()); }",
        "fn f() { Ptr.null::<Int32>(); }",
        "fn f() { Ptr.is_null::<Int32>(Ptr.null()); }",
        "fn f(p: Ptr<Int32>) {}",
        "type Ptr = Int32",
        "struct Ptr { value: Int32 }",
        "impl Ptr { fn null() -> Ptr { Ptr.null() } }",
        "extern fn host(p: Ptr)",
        "extern fn host() -> Ptr",
        "extern fn host() @suspends -> Ptr",
        "unsafe extern \"C\" fn host() @suspends -> Ptr",
        "unsafe extern \"C\" fn host(p: &Ptr)",
        "unsafe extern \"C\" fn host(p: Ptr, q: Ptr, r: Ptr, s: Ptr, t: Ptr)",
        "#repr(C) struct Record { value: Ptr }",
    ] {
        let source = format!("{prefix}{body}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            unsafe {
                vm.register_c_fn(
                    "identity",
                    FunctionSignature::new(vec![Type::Ptr], Type::Ptr),
                    identity as *const u8,
                    None,
                )
                .unwrap();
            }
            vm.load_source("fn intact() -> Int32 { 42 }").unwrap();
            let version = vm.module_version();
            let error = vm.load_source(&source).expect_err(body);
            assert!(error.span().is_some(), "{mode:?}: {body}: {error}");
            assert_eq!(vm.module_version(), version);
            assert_eq!(vm.call_int32_0("intact").unwrap(), 42);
        }
        if let Ok(ast) = crate::parser::parse(&source) {
            assert!(
                crate::aot::AotCompiler::new("ptr-invalid")
                    .unwrap()
                    .compile_module(&ast)
                    .is_err(),
                "{body}"
            );
        }
    }
}

#[test]
fn ptr_registration_keeps_legacy_host_abi_separate() {
    use super::support::delayed_value;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        for signature in [
            FunctionSignature::new(vec![Type::Ptr], Type::Unit),
            FunctionSignature::new(vec![], Type::Ptr),
        ] {
            assert!(
                unsafe { vm.register_fn("legacy", signature.clone(), identity as *const u8) }
                    .is_err()
            );
            assert!(
                unsafe { vm.register_suspending_fn("legacy", signature, delayed_value) }.is_err()
            );
        }
        for signature in [
            FunctionSignature::new(vec![Type::Ptr; 5], Type::Ptr),
            FunctionSignature::new(vec![Type::Ptr], Type::Bool),
            FunctionSignature::new(vec![Type::reference(Type::Ptr, false)], Type::Ptr),
        ] {
            assert!(
                unsafe { vm.register_c_fn("bad", signature, identity as *const u8, None) }.is_err()
            );
        }
        for name in ["Ptr.null", "Ptr.is_null"] {
            assert!(unsafe {
                vm.register_c_fn(
                    name,
                    FunctionSignature::new(vec![Type::Ptr], Type::Ptr),
                    identity as *const u8,
                    None,
                )
            }
            .is_err());
        }
        assert!(vm.register_opaque_type("Ptr").is_err());
    }
}

#[test]
fn ptr_values_survive_pending_returns_reload_and_cancel_without_ownership() {
    use super::support::{delayed_value, EVENTS, READY};
    use crate::runtime::suspension::ContinuationStatus;
    use std::{cell::Cell, rc::Rc};
    thread_local! {
        static ADDRESS: Cell<*mut u8> = const { Cell::new(std::ptr::null_mut()) };
        static CREATES: Cell<usize> = const { Cell::new(0) };
        static RELEASES: Cell<usize> = const { Cell::new(0) };
        static READS: Cell<usize> = const { Cell::new(0) };
    }
    extern "C" fn create() -> *mut u8 {
        CREATES.with(|n| n.set(n.get() + 1));
        ADDRESS.with(Cell::get)
    }
    unsafe extern "C" fn read(p: *mut u8) -> i32 {
        READS.with(|n| n.set(n.get() + 1));
        *p.cast::<i32>()
    }
    extern "C" fn release(_p: *mut u8) {
        RELEASES.with(|n| n.set(n.get() + 1));
    }
    extern "C" fn fail() -> *mut u8 {
        crate::runtime::error::record_error(crate::Error::new_with_code(
            crate::ErrorPhase::Runtime,
            crate::ErrorCode::HostError,
            "C pointer result failed",
        ));
        ADDRESS.with(Cell::get)
    }
    struct Owner {
        dropped: Rc<Cell<bool>>,
        value: i32,
    }
    impl Drop for Owner {
        fn drop(&mut self) {
            self.dropped.set(true);
        }
    }
    let source = r#"
        unsafe extern "C" fn ptr_create() -> Ptr
        unsafe extern "C" fn ptr_read(p: Ptr) -> Int32
        unsafe extern "C" fn ptr_release(p: Ptr)
        unsafe extern "C" fn ptr_fail() -> Ptr
        extern fn delayed_value(n: Int32) @suspends -> Int32
        struct Saved { p: Ptr }
        fn pass(p: Ptr) @suspends -> Ptr { delayed_value(0); p }
        fn run() @suspends -> Int32 {
            let p = unsafe { ptr_create() }
            let saved = alloc(Saved { p: p })
            let q = pass(saved.p)
            let n = unsafe { ptr_read(q) }
            unsafe { ptr_release(p) }
            n
        }
        fn failure() -> Int32 { unsafe { ptr_read(ptr_fail()) } }
        fn forgotten() -> Int32 { let p = unsafe { ptr_create() }; if Ptr.is_null(p) { 0 } else { 42 } }
        fn trap(n: Int32) -> Int32 { let p = unsafe { ptr_create() }; unsafe { ptr_read(p) } / n }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        READY.with(|v| v.set(false));
        EVENTS.with(|v| v.borrow_mut().clear());
        CREATES.with(|v| v.set(0));
        RELEASES.with(|v| v.set(0));
        READS.with(|v| v.set(0));
        let dropped = Rc::new(Cell::new(false));
        let owner = Rc::new(Owner {
            dropped: dropped.clone(),
            value: 42,
        });
        ADDRESS.with(|p| p.set(std::ptr::addr_of!(owner.value).cast_mut().cast()));
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            for (name, params, ret, entry) in [
                ("ptr_create", vec![], Type::Ptr, create as *const u8),
                ("ptr_read", vec![Type::Ptr], Type::Int32, read as *const u8),
                (
                    "ptr_release",
                    vec![Type::Ptr],
                    Type::Unit,
                    release as *const u8,
                ),
                ("ptr_fail", vec![], Type::Ptr, fail as *const u8),
            ] {
                vm.register_c_fn(
                    name,
                    FunctionSignature::new(params, ret),
                    entry,
                    Some(owner.clone()),
                )
                .unwrap();
            }
            vm.register_suspending_fn(
                "delayed_value",
                FunctionSignature::new(vec![Type::Int32], Type::Int32),
                delayed_value,
            )
            .unwrap();
        }
        drop(owner);
        vm.load_source(source)
            .unwrap_or_else(|e| panic!("{mode:?}: {e} {:?}", e.span()));
        assert_eq!(
            vm.call_int32_0("failure").unwrap_err().code(),
            crate::ErrorCode::HostError
        );
        READS.with(|n| assert_eq!(n.get(), 0));
        assert_eq!(vm.call_int32_0("forgotten").unwrap(), 42);
        assert!(vm.call_int32_1("trap", 0).is_err());
        RELEASES.with(|n| assert_eq!(n.get(), 0));
        let mut task = vm.call_async("run", &[]).unwrap();
        let cancelled = vm.call_async("run", &[]).unwrap();
        CREATES.with(|n| assert_eq!(n.get(), 4));
        for _ in 0..3 {
            assert_eq!(task.status(), ContinuationStatus::Pending);
            task.resume().unwrap();
            CREATES.with(|n| assert_eq!(n.get(), 4));
        }
        drop(cancelled);
        RELEASES.with(|n| assert_eq!(n.get(), 0));
        vm.load_source("fn intact() -> Int32 { 42 }").unwrap();
        drop(vm);
        assert!(!dropped.get());
        READY.with(|v| v.set(true));
        task.resume().unwrap();
        assert_eq!(task.cps_result(), Some(42));
        RELEASES.with(|n| assert_eq!(n.get(), 1));
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
        drop(task);
        assert!(dropped.get());
        ADDRESS.with(|p| p.set(std::ptr::null_mut()));
    }
}
