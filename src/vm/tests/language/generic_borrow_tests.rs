use super::{VmEngine, VmMode};
use crate::ErrorCode;

const SOURCE: &str = include_str!("../../../../tests/fixtures/generic_borrows.jk");

#[test]
fn generic_borrows_share_call_value_and_alias_rules() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE).unwrap();
        for name in [
            "borrow_shared",
            "borrow_mutable",
            "borrow_slices",
            "discard_explicit",
        ] {
            assert_eq!(vm.call_int32_0(name).unwrap(), 42, "{mode:?}: {name}");
        }
        let mut task = vm.call_async("borrow_pending", &[]).unwrap();
        assert!(!task.is_finished());
        while !task.is_finished() {
            task.resume().unwrap();
        }
        assert_eq!(task.cps_result(), Some(42));
        drop(task);
        drop(vm.call_async("borrow_pending", &[]).unwrap());
        crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

#[test]
fn generic_borrows_preserve_copy_storage_and_lifetime_restrictions() {
    let prelude = "fn identity<T>(value: T) -> T { value } ";
    for (source, code) in [
        ("fn copy<T: Copy>(x: T) -> T { x } fn bad() { var x = 1; let _ = copy(&mut x) }", ErrorCode::TypeMismatch),
        ("fn bad() -> &Int32 { let x = 1; identity(&x) }", ErrorCode::InvalidAssignment),
        ("fn bad() -> Int32 { var x = 1; let p = identity(&mut x); x = 2; *p }", ErrorCode::InvalidAssignment),
        ("fn bad() { let x = 1; let p = &x; let _ = identity::<&&Int32>(&p) }", ErrorCode::SyntaxError),
        ("struct Holder<T> { value: T } fn bad() { var x = 1; let _ = Holder { value: &mut x } }", ErrorCode::TypeMismatch),
        ("enum Holder<T> { Value(T) } fn bad() { var x = 1; let _ = Holder.Value(&mut x) }", ErrorCode::TypeMismatch),
        ("import joky/array.{type Array} fn bad() { let _ = Array::<&mut Int32>.new() }", ErrorCode::TypeMismatch),
        ("fn take<F: Fn(Int32) -> Int32>(f: F) -> Int32 { 42 } fn bad() { let _ = take::<fn(Bool) -> Bool> }", ErrorCode::TypeMismatch),
        ("trait Source { type Item } impl Source for Int32 { type Item = Bool } fn take<T: Source<Item = Int32>>(x: T) -> Int32 { 42 } fn bad() { let _ = take::<Int32> }", ErrorCode::TypeMismatch),
    ] {
        let source = if source.starts_with("import ") {
            source.to_owned()
        } else { format!("{prelude}{source}") };
        let error = VmEngine::new().load_source(&source).unwrap_err();
        assert_eq!(error.code(), code, "{source}: {error}");
    }
}

#[test]
fn discarded_results_report_the_expression_and_preserve_loaded_code() {
    let prefix = "fn attempt() -> Result<Int32, Int32> { Err(1) } ";
    let mut vm = VmEngine::new();
    vm.load_source("fn old() -> Int32 { 42 }").unwrap();
    for (body, marked) in [
        ("attempt(); 42", "attempt()"),
        ("attempt()\n42", "attempt()"),
        ("let value = attempt(); value; 42", "value"),
        ("while false { attempt() }; 42", "attempt()"),
        ("for _ in [1] { attempt() }; 42", "attempt()"),
        ("while false { (attempt(), 1) }; 42", "(attempt(), 1)"),
        ("if true { attempt(); }; 42", "attempt()"),
        ("{ attempt() }; 42", "attempt()"),
    ] {
        let source = format!("{prefix}fn bad() -> Int32 {{ {body} }}");
        let error = vm.load_source(&source).unwrap_err();
        assert_eq!(error.code(), ErrorCode::UnusedResult, "{source}: {error}");
        assert_eq!(error.span().unwrap().slice(&source), Some(marked));
        assert!(error.message().contains("let _ ="));
        assert_eq!(vm.call_int32_0("old").unwrap(), 42);
    }
}

#[test]
fn discarded_results_follow_standard_identity_and_generic_substitution() {
    for source in [
        "import joky/result.{type Result as Attempt} type Alias<T> = Attempt<T, Int32> fn produce() -> Alias<Int32> { Attempt.Err(1) } fn bad() { produce(); }",
        "import joky/result.{type Result} fn bad<T>(value: Result<T, Int32>) { value; }",
        "import joky/result.{type Result} fn discard<T>(value: T) { value; } fn bad() { discard(Result.Err::<Int32, Int32>(1)); }",
        "import joky/result.{type Result} fn work() @suspends -> Result<Int32, Int32> { Result.Err(1) } fn bad() @suspends { join { work(), work() }; }",
    ] {
        let mut vm = VmEngine::new();
        vm.set_prelude_enabled(false);
        let error = vm.load_source(source).unwrap_err();
        assert_eq!(error.code(), ErrorCode::UnusedResult, "{source}: {error}");
    }
    let mut vm = VmEngine::new();
    vm.set_prelude_enabled(false);
    vm.load_source(
        "enum Result { Ok(Int32), Err(Int32) } fn main() -> Int32 { Result.Err(1); 42 }",
    )
    .unwrap();
    assert_eq!(vm.call_int32_0("main").unwrap(), 42);
}
