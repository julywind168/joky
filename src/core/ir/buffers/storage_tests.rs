use super::*;
use crate::{
    runtime::arena::{LIMIT, LIVE, MAX_BYTES},
    vm::{VmEngine, VmMode},
};

// These markers can only be installed by the compiler, never by a source path,
// name, extern declaration or annotation. Public array registration is separate.
pub(crate) fn module(source: &str) -> AstModule {
    let mut ast = crate::parser::parse(&format!("type Array<T> = T\n{source}")).unwrap();
    let alias = &mut ast.type_aliases[0];
    alias.target = Type::Buffer(Arc::new(Type::Parameter(alias.type_params[0].clone())));
    // This fixture specializes Copy primitives to Int32. Owner-only views
    // execute through the owning_elements fixture in all three backends.
    for intrinsic in BufferIntrinsic::ALL.into_iter().filter(|i| {
        !matches!(
            i,
            BufferIntrinsic::BorrowRef | BufferIntrinsic::BorrowMutRef
        )
    }) {
        let name = intrinsic.symbol().strip_prefix("__joky_buffer_").unwrap();
        let mut item = crate::parser::parse(&format!("fn {name}() {{}}"))
            .unwrap()
            .items
            .remove(0);
        let Item::FnDef(f) = &mut item else {
            unreachable!()
        };
        f.buffer_intrinsic = Some(intrinsic);
        f.buffer_element = Some(Type::Int32);
        let signature = intrinsic.signature(&Type::Int32);
        f.params = signature
            .params
            .into_iter()
            .enumerate()
            .map(|(i, ty)| crate::parser::ast::Param {
                name: format!("arg{i}"),
                ty,
                span: f.span,
            })
            .collect();
        f.ret_ty = signature.ret;
        ast.items.push(item);
    }
    TypeRegistry::default().resolve_aliases(&ast).unwrap()
}

pub(crate) const STORAGE: &str = r#"
    fn make() -> Array<Int32> {
        var b = allocate(2)
        initialize_next(&mut b, -3)
        initialize_next(&mut b, 42)
        b
    }
    fn forward(b: Array<Int32>) -> Array<Int32> { b }
    fn grow(b: &mut Array<Int32>) {
        var next = allocate(4)
        var i: UInt64 = 0
        let length = len(b)
        while i < length {
            let value = load(b, i)
            initialize_next(&mut next, value)
            i = i + 1
        }
        replace(b, next)
    }
    fn check() -> Int32 {
        var b = forward(make())
        grow(&mut b)
        store(&mut b, 0, -9)
        initialize_next(&mut b, 9)
        let answer = load(&b, 0) + load(&b, 1) + load(&b, 2)
        truncate(&mut b, 1)
        if len(&b) == 1 && capacity(&b) == 4 && capacity_limit() > 4 { answer } else { 0 }
    }
    fn empty() -> Int32 {
        let b = allocate(0)
        if len(&b) == 0 && capacity(&b) == 0 { 42 } else { 0 }
    }
"#;

#[test]
fn buffer_storage_executes_moves_growth_and_initialized_prefix() {
    let ast = module(STORAGE);
    let plan =
        ModulePlan::with_import_layouts_and_types(&ast, &HashMap::new(), &TypeRegistry::default())
            .unwrap();
    assert!(plan.functions["check"].uses_arena && plan.functions["check"].may_fail);
    assert!(!plan.functions["load"].uses_arena && plan.functions["load"].may_fail);
    for name in ["release", "replace"] {
        assert!(plan.functions[name].needs_arena);
        assert!(!plan.functions[name].uses_arena);
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_module(&ast).unwrap();
        for name in ["check", "empty"] {
            assert_eq!(vm.call_int32_0(name).unwrap(), 42);
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}

#[test]
fn buffer_storage_rejects_bounds_and_overflow_without_leaking() {
    for (body, code, message) in [
        (
            "let b = allocate(1) load(&b, 0);",
            ErrorCode::RuntimeError,
            "initialized prefix",
        ),
        (
            "var b = allocate(1) store(&mut b, 0, 42);",
            ErrorCode::RuntimeError,
            "initialized prefix",
        ),
        (
            "var b = allocate(0) initialize_next(&mut b, 42);",
            ErrorCode::RuntimeError,
            "exceeds capacity",
        ),
        (
            "var b = allocate(2) initialize_next(&mut b, 42) truncate(&mut b, 2);",
            ErrorCode::RuntimeError,
            "cannot increase",
        ),
        (
            "let b = allocate(2) load(&b, 18446744073709551615);",
            ErrorCode::RuntimeError,
            "initialized prefix",
        ),
        (
            "let b = allocate(18446744073709551615);",
            ErrorCode::ResourceLimit,
            "address range",
        ),
        (
            "let b = allocate(16777216);",
            ErrorCode::ResourceLimit,
            "64 MiB",
        ),
    ] {
        let ast = module(&format!("fn check() -> Int32 {{ {body} 42 }}"));
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_module(&ast).unwrap();
            let error = vm.call_int32_0("check").unwrap_err();
            assert_eq!(error.code(), code, "{body}: {error:?}");
            assert!(error.message().contains(message), "{body}: {error:?}");
            assert!(error.span().is_some());
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}

#[test]
fn buffer_storage_allocation_failures_release_every_charged_block() {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            LIMIT.with(|n| n.set(MAX_BYTES));
        }
    }
    let ast = module(STORAGE);
    let record = std::mem::size_of::<(std::ptr::NonNull<u8>, std::alloc::Layout)>();
    let control = std::mem::size_of::<crate::runtime::buffer::Buffer>() + record;
    let payload = 2 * std::mem::size_of::<i32>() + record;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_module(&ast).unwrap();
        for limit in [0, control, control + payload, 2 * control + payload] {
            let _restore = Restore;
            LIMIT.with(|n| n.set(limit));
            let error = vm.call_int32_0("check").unwrap_err();
            assert_eq!(error.code(), ErrorCode::ResourceLimit);
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}
