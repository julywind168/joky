//! Explicit arena scope ownership and provenance across the VM backends.
use super::{support::frontend_error, InterpreterLimits, VmEngine, VmMode};
use crate::runtime::arena::{LIMIT, LIVE};

const SOURCE: &str = include_str!("../../tests/fixtures/explicit_arena.jk");
const IMPORT: &str = "import joky/arena.{type Arena}\n";

fn live(expected: usize) {
    LIVE.with(|count| assert_eq!(count.get(), expected));
}

struct Limit(usize);
impl Limit {
    fn new(limit: usize) -> Self {
        Self(LIMIT.with(|current| current.replace(limit)))
    }
}
impl Drop for Limit {
    fn drop(&mut self) {
        LIMIT.with(|current| current.set(self.0));
    }
}

#[test]
fn explicit_arena_executes_values_helpers_nested_scopes_and_jumps() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE)
            .unwrap_or_else(|error| panic!("{error}"));
        for name in [
            "explicit_basic",
            "explicit_helper",
            "explicit_values",
            "explicit_nested",
            "explicit_task_reference",
            "explicit_helper_storage",
            "explicit_arena_parameters",
            "explicit_move",
            "explicit_drop",
            "explicit_loop",
            "explicit_jumps",
            "explicit_return",
        ] {
            assert_eq!(vm.call_int32_0(name).unwrap(), 42, "{mode:?}: {name}");
            live(0);
        }
        for _ in 0..3 {
            let error = vm.call_int32_1("explicit_failure", 0).unwrap_err();
            assert_eq!(error.code(), crate::ErrorCode::RuntimeError);
            live(0);
            assert_eq!(vm.call_int32_1("explicit_failure", 1).unwrap(), 42);
            live(0);
        }
    }
}

#[test]
fn explicit_arena_scope_releases_payloads_before_task_completion() {
    extern "C" fn live_count() -> i32 {
        LIVE.with(|count| count.get() as i32)
    }
    let source = format!(
        r#"{IMPORT}
        extern fn live_count() -> Int32;
        fn f() -> Int32 {{
            let task_value = alloc(1)
            var count = live_count()
            {{
                let arena = Arena.new()
                let value = arena.alloc(42)
                count = count * 10 + live_count()
                (*value);
            }}
            count * 10 + live_count() + (*task_value) - 1
        }}"#
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_fn(
                "live_count",
                crate::FunctionSignature::new(vec![], crate::Type::Int32),
                live_count as *const u8,
            )
            .unwrap();
        }
        vm.load_source(&source)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(vm.call_int32_0("f").unwrap(), 131, "{mode:?}");
        live(0);
    }
}

#[test]
fn explicit_arena_refunds_scopes_and_shares_the_task_limit() {
    let _limit = Limit::new(8192);
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(vm.call_int32_1("explicit_many", 1000).unwrap(), 42);
        live(0);
        let error = vm.call_int32_0("explicit_shared_budget").unwrap_err();
        assert_eq!(error.code(), crate::ErrorCode::ResourceLimit, "{mode:?}");
        live(0);
        assert_eq!(vm.call_int32_0("explicit_basic").unwrap(), 42);
        live(0);
    }
}

#[test]
fn explicit_arena_drop_refunds_payloads_within_the_scope() {
    let source = format!(
        r#"{IMPORT}
        import joky/mem
        fn f() -> Int32 {{
            let arena = Arena.new()
            var index = 0
            while index < 150 {{ arena.alloc(index); index = index + 1 }}
            mem.drop(arena)
            let next = Arena.new()
            index = 0
            while index < 150 {{ next.alloc(index); index = index + 1 }}
            42
        }}"#
    );
    let _limit = Limit::new(8192);
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(&source)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(vm.call_int32_0("f").unwrap(), 42);
        live(0);
    }
}

#[test]
fn explicit_arena_rejects_escaping_references_and_capability_forgery() {
    for body in [
        "fn bad() -> &mut Int32 { let arena = Arena.new(); arena.alloc(42) }",
        "fn bad() -> (Option<&Int32>,) { let arena = Arena.new(); let p = arena.alloc(42); (Some((&*p)),) }",
        "fn bad() { let arena = Arena.new(); let p = arena.alloc(42); alloc((&*p,)); }",
        "fn make(arena: &Arena) -> &mut Int32 { arena.alloc(42) } fn bad() -> &mut Int32 { let arena = Arena.new(); make(&arena) }",
        "fn bad() { let arena = Arena.new(); let p = arena.alloc(42); let moved = arena; (*p); }",
        "import joky/mem fn bad() { let arena = Arena.new(); let p = arena.alloc(42); mem.drop(arena); (*p); }",
        "fn bad() { let outer = Arena.new(); { let inner = Arena.new(); let p = inner.alloc(42); outer.alloc((&*p,)); } }",
        "fn store(arena: &Arena, p: &Int32) { arena.alloc((p,)); } fn bad() { let outer = Arena.new(); { let inner = Arena.new(); let p = inner.alloc(42); store(&outer, p); } }",
        "fn bad() -> Int32 { let value = { let arena = Arena.new(); arena.alloc(42) }; (*value) }",
        "fn bad() { let arena = Arena.new(); let p = arena.alloc(42); let q: &Int32 = p; let exclusive: &mut Int32 = q; }",
        "fn bad(arena: &Arena, shared: &Int32) -> &mut Int32 { shared }",
        "fn bad(arena: &mut Arena) {}",
        "fn bad() -> Arena { Arena.new() }",
        "fn bad() { let arena = Arena.new(); let copy = (arena,); }",
        "fn identity<T>(value: T) -> T { value } fn bad() { identity(Arena.new()); }",
        "fn bad() { let arena = Arena.new(); arena.alloc(\"owned\"); }",
        "fn tick() @suspends {} fn bad() @suspends { let arena = Arena.new(); tick(); arena.alloc(42); }",
        "fn tick() @suspends {} fn bad() @suspends { let arena = Arena.new(); let p = arena.alloc(42); tick(); (*p); }",
        "fn bad() -> Int32 { let f = { let arena = Arena.new(); let p: &Int32 = arena.alloc(42); let f = || { (*p) }; f }; f() }",
        "fn bad() -> Int32 { let f = { let arena = Arena.new(); let p: &Int32 = arena.alloc(42); let f = move || { (*p) }; f }; f() }",
        "import joky/mem fn bad() -> Int32 { let arena = Arena.new(); let p: &Int32 = arena.alloc(42); let f = || { (*p) }; mem.drop(arena); f() }",
        "import joky/mem fn bad() -> Int32 { let arena = Arena.new(); let p: &Int32 = arena.alloc(42); let f = move || { (*p) }; mem.drop(arena); f() }",
        "trait Read { fn value(&self) @noescape -> Int32; } struct Point { x: Int32 } impl Read for Point { fn value(&self) -> Int32 { self.x } } fn bad() -> Int32 { let view = { let arena = Arena.new(); let point = arena.alloc(Point { x: 42 }); let view: &dyn Read = &*point; view }; view.value() }",
    ] {
        let source = format!("{IMPORT}{body}");
        let error = frontend_error(&source, &crate::ModuleSources::new());
        assert!(error.span().is_some(), "{error}\n{source}");
        assert!(
            matches!(
                error.code(),
                crate::ErrorCode::InvalidAssignment | crate::ErrorCode::TypeMismatch | crate::ErrorCode::InvalidCall
            ),
            "unexpected rejection: {error}\n{source}"
        );
    }
}

#[test]
fn explicit_arena_escape_diagnostics_match_backend_entry_points() {
    let source =
        format!("{IMPORT}fn bad() -> &mut Int32 {{ let arena = Arena.new(); arena.alloc(42) }}");
    let expected = frontend_error(&source, &crate::ModuleSources::new());
    let check = |error: crate::Error| {
        assert_eq!(error.code(), expected.code());
        assert_eq!(error.message(), expected.message());
        assert_eq!(error.span(), expected.span());
    };
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        check(VmEngine::with_mode(mode).load_source(&source).unwrap_err());
    }
    check(
        crate::aot::AotCompiler::new("explicit-arena-rejection")
            .unwrap()
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap_err(),
    );
}

#[test]
fn explicit_arena_interpreter_cancellation_releases_active_synchronous_helpers() {
    cancellation_prefixes(false);
}

#[test]
#[ignore = "exhaustive: explicit arena cancellation at every interpreter instruction"]
fn exhaustive_explicit_arena_interpreter_cancellation() {
    cancellation_prefixes(true);
}

fn cancellation_prefixes(exhaustive: bool) {
    let source = format!("{SOURCE}\nfn run() @suspends -> Int32 {{ explicit_helper() }}");
    let mut vm = VmEngine::with_mode(VmMode::Interpreter);
    vm.load_source(&source)
        .unwrap_or_else(|error| panic!("{error}"));
    vm.set_interpreter_limits(InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    let mut trace = vec![];
    let mut task = vm.call_async("run", &[]).unwrap();
    for _ in 0..200 {
        trace.push(LIVE.with(|count| count.get()));
        if task.is_finished() {
            break;
        }
        task.resume().unwrap();
    }
    assert_eq!(task.cps_result(), Some(42));
    drop(task);
    live(0);
    let prefixes = if exhaustive {
        (0..trace.len()).collect::<Vec<_>>()
    } else {
        vec![
            0,
            trace.iter().position(|count| *count == 1).unwrap(),
            trace.iter().position(|count| *count > 1).unwrap(),
            trace.iter().rposition(|count| *count > 0).unwrap(),
        ]
    };
    for prefix in prefixes {
        let mut task = vm.call_async("run", &[]).unwrap();
        for _ in 0..prefix {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        live(trace[prefix]);
        if task.is_finished() {
            assert_eq!(task.cps_result(), Some(42));
        }
        drop(task);
        live(0);
    }
}
