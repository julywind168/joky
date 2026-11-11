use super::support::*;
use super::VmMode;
use crate::runtime::arena::LIVE;

const SOURCE: &str = include_str!("../../../../tests/fixtures/range_slices.jk");

#[test]
fn range_slices_resume_once_and_release_borrows_on_cancel() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        assert_eq!(vm.call_int32_0("compute").unwrap(), 42);
        let error = vm.call_int32_0("bad_open_end").unwrap_err();
        assert!(
            error
                .message()
                .contains("FixedArray index or slice is out of bounds"),
            "{error}"
        );
        for (entry, pending, completed) in [
            (
                "pending",
                &[-99, -1, -1][..],
                &[-99, -1, -1, 0, 0, 4, 4][..],
            ),
            ("pending_open", &[0, 1, 1][..], &[0, 1, 1][..]),
        ] {
            for cancel in [false, true] {
                EVENTS.with(|events| events.borrow_mut().clear());
                READY.with(|ready| ready.set(false));
                let mut task = vm.call_async(entry, &[]).unwrap();
                for _ in 0..3 {
                    task.resume().unwrap();
                }
                EVENTS.with(|events| assert_eq!(*events.borrow(), pending));
                if !cancel {
                    READY.with(|ready| ready.set(true));
                    for _ in 0..30 {
                        if task.is_finished() {
                            break;
                        }
                        task.resume().unwrap();
                    }
                    assert_eq!(task.cps_result(), Some(42));
                }
                drop(task);
                EVENTS.with(|events| {
                    assert_eq!(*events.borrow(), if cancel { pending } else { completed })
                });
                LIVE.with(|live| assert_eq!(live.get(), 0));
            }
        }
    }
}

#[test]
fn range_slices_reject_invalid_types_conflicting_loans_and_escapes() {
    for source in [
        "fn bad() { let a = [1, 2]; let n: Int32 = 1; let s = &a[0..n]; }",
        "fn bad() { let a = [1, 2]; let s = &a[-1..2]; }",
        "fn bad() { let a = [1, 2]; let s = &a[0.0..1.0]; }",
        "fn bad() { let a = [1, 2]; let r = 0..2; let s = &a[r]; }",
        "fn bad() { let a = [1, 2]; let s = a[0..2]; }",
        "fn bad() { let a = [1, 2]; let s = a[..]; }",
        "fn bad() { let a = [1, 2]; let s = &a[..=]; }",
        "fn bad() { let a = [1, 2]; let s = &a[1..=]; }",
        "fn bad() { let a = [1, 2]; let s = &a[..1; 0]; }",
        "fn bad() { let a = [1, 2]; let n: Int32 = 1; let s = &a[n..]; }",
        "fn bad() { let r = 1.. }",
        "fn bad() { let a = [1, 2]; let s = &mut a[0..2]; }",
        "fn bad() { var a = [1, 2]; let r = &a; let s = &mut r[0..2]; }",
        "fn bad() { var a = [1, 2]; let s = &a[0..2]; a.push(3); s[0]; }",
        "fn bad() { var a = [1, 2]; let s = &mut a[0..1]; let t = &mut a[1..2]; s[0] += t[0]; }",
        "fn bad() { var a = [1, 2]; let s = &mut a[0..{ a.push(3); 2 }]; s[0] += 1; }",
        "import joky/slice.{type Slice} fn bad() -> &Slice<Int32> { let a = [1, 2]; &a[0..2] }",
        "fn bad() { let s = &\"abc\"[0..2]; }",
    ] {
        let error = frontend_error(source, &crate::ModuleSources::new());
        assert!(error.span().is_some(), "{source}: {error}");
    }
}

#[test]
#[ignore = "exhaustive: cancel range slicing at every instruction"]
fn exhaustive_range_slices_instruction_cancellation() {
    let mut vm = parity_vm_with_prelude(SOURCE, VmMode::Interpreter, false);
    for entry in ["pending", "pending_open"] {
        assert_cancellation(
            &mut vm,
            entry,
            20_000,
            CancellationSweep::EveryInstruction,
            || {
                READY.with(|ready| ready.set(true));
                EVENTS.with(|events| events.borrow_mut().clear());
            },
            || EVENTS.with(|events| events.borrow().clone()),
            || {
                LIVE.with(|live| assert_eq!(live.get(), 0));
            },
        );
    }
}

#[test]
fn slice_members_preserve_alias_rejection_and_nested_diagnostics() {
    for source in [
        "import joky/slice.{type Slice} impl<T> Slice<T> { fn extra(&self) -> Int32 { 0 } }",
        "import joky/slice.{type Slice} type View<T> = Slice<T> impl<T> View<T> { fn extra(&self) -> Int32 { 0 } }",
    ] {
        let error = frontend_error(source, &crate::ModuleSources::new());
        assert!(error.message().contains("aliases and partially generic"), "{error}");
    }
    for value in ["view.missing()", "keep(view.missing())"] {
        let source = format!("fn keep<T>(value: T) -> T {{ value }} fn main() {{ let values = [1, 2]; let view = &values[0..2]; println({value}) }}");
        let error = frontend_error(&source, &crate::ModuleSources::new());
        assert_eq!(error.code(), crate::ErrorCode::UnknownName);
        assert!(
            error.message().contains("has no method 'missing'"),
            "{error}"
        );
    }
    let error = frontend_error(
        "import joky/array.{type Array} fn main() { let values = Array.new() }",
        &crate::ModuleSources::new(),
    );
    assert!(error.message().contains("Cannot infer"), "{error}");
    assert!(!error.message().contains("joky/member/"), "{error}");
}

#[test]
fn borrowed_queries_reject_invalidation_storage_and_escape() {
    for body in [
        "fn bad() { var a = [\"a\"]; let found = a.get_ref(0); a.push(\"b\"); found.is_some(); }",
        "import joky/list.{type List} fn bad() { var a = List.from_array([\"a\"]); let found = a.get_ref(0); a.reverse(); found.is_some(); }",
        "import joky/fixed_array.{type FixedArray} fn bad() -> Option<&String> { let a: FixedArray<String, 1> = [\"a\"]; a.get_ref(0) }",
        "fn bad() { var a = [\"a\"]; let found = a.get_ref(0); a.release(); found.is_some(); }",
        "fn bad() { var a = [\"a\"]; let found = a.get_ref(0); a = [\"b\"]; found.is_some(); }",
        "fn bad() -> Option<&String> { let a = [\"a\"]; a.get_ref(0) }",
        "fn bad(a: Array<String>) -> Option<&String> { a.get_ref(0) }",
        "fn bad() { let found = { let a = [\"a\"]; a.get_ref(0) }; found.is_some(); }",
        "fn bad() { let a = [\"a\"]; alloc(a.get_ref(0)); }",
        "fn bad() { let a = [\"a\"]; let saved = [a.get_ref(0)]; }",
        "fn bad() { var a = [\"a\"]; let saved: Option<&mut String> = Some(a.borrow_mut_ref(0)); }",
        "struct Invalid { owner: String, view: &String } fn bad() {}",
        "fn bad(a: &Array<String>) @noescape -> Option<&String> { a.get_ref(0) }",
        "fn bad() { var m: Map<String, String> = Map.new(); m.insert(\"k\", \"v\"); let r = m.get_ref(\"k\"); m.clear(); r.is_some(); }",
    ] {
        let source = format!("import joky/array.{{type Array}} import joky/map.{{type Map}} {body}");
        let error = frontend_error(&source, &crate::ModuleSources::new());
        assert!(matches!(error.code(), crate::ErrorCode::InvalidAssignment | crate::ErrorCode::TypeMismatch | crate::ErrorCode::InvalidCall), "{body}: {error}");
    }
}

#[test]
#[ignore = "exhaustive: cancel every borrowed query instruction"]
fn exhaustive_borrowed_queries_cancel_at_every_instruction() {
    let mut vm = super::VmEngine::new_interpreter();
    let source = format!(
        "{} fn sweep() @suspends -> Int32 {{ compute() }}",
        include_str!("../../../../tests/fixtures/borrowed_queries.jk")
    );
    vm.load_source(&source).unwrap_or_else(|e| panic!("{e}"));
    assert_cancellation(
        &mut vm,
        "sweep",
        20_000,
        CancellationSweep::EveryInstruction,
        || {},
        || LIVE.with(|n| n.get()),
        || LIVE.with(|n| assert_eq!(n.get(), 0)),
    );
}
