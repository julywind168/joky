//! Source-defined Option and Result methods and their module boundaries.
use super::support::{with_suspends, Suspends};
use super::{VmEngine, VmMode};
use crate::{parser, ErrorCode, TypeRegistry};

#[test]
fn option_result_methods_preserve_values_and_eager_arguments() {
    for prelude in [false, true] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            for suspends in [false, true] {
                let source = with_suspends(
                    include_str!("../../tests/fixtures/option_result_methods.jk"),
                    suspends,
                    Suspends::Typed,
                );
                let mut vm = VmEngine::with_mode(mode);
                vm.set_prelude_enabled(prelude);
                vm.load_source(&source).unwrap();
                let result = if suspends {
                    vm.call_async("check", &[]).unwrap().cps_result().unwrap() as i32
                } else {
                    vm.call_int32_0("check").unwrap()
                };
                assert_eq!(
                    result, 42,
                    "{mode:?}, prelude={prelude}, suspends={suspends}"
                );
                let wide = if suspends {
                    vm.call_async_uint64("wide", &[])
                        .unwrap()
                        .cps_result()
                        .unwrap()
                } else {
                    vm.call_uint64_0("wide").unwrap()
                };
                assert_eq!(wide, u64::MAX);
            }
        }
    }
}

#[test]
fn option_result_defaults_resume_once_after_reload_failure_and_cancel() {
    use super::support::{parity_vm, EVENTS, READY};
    use crate::runtime::{scheduler::Scheduler, suspension::ResumeError};
    for (expression, expected) in [
        ("Option.Some(42).unwrap_or(VALUE)", 42),
        ("Option.Some(42).or(Option.Some(VALUE)).unwrap_or(0)", 42),
        (
            "Option.None::<Bool>.and(Option.Some(VALUE)).unwrap_or(42)",
            42,
        ),
        ("Result.Ok::<Int32, Bool>(42).unwrap_or(VALUE)", 42),
        (
            "Result.Ok::<Int32, Bool>(42).or::<Unit>(Result.Ok(VALUE)).unwrap_or(0)",
            42,
        ),
        (
            "Result.Err::<Int32, Bool>(false).or::<Unit>(Result.Ok(VALUE)).unwrap_or(0)",
            8,
        ),
    ] {
        let pending = expression.replace("VALUE", "delayed_value(record(7))");
        let failing = expression.replace("VALUE", "42 / delayed_value(record(-1))");
        let source = format!("extern fn delayed_value(n: Int32) @suspends -> Int32 extern fn record(n: Int32) -> Int32 fn pending() @suspends -> Int32 {{ {pending} }} fn failing() @suspends -> Int32 {{ let value = {failing} record(99); value }}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = parity_vm("", mode);
            vm.load_source_with_modules(&source, &crate::ModuleSources::new())
                .unwrap();
            let mut pending = vm.call_async("pending", &[]).unwrap();
            let mut failing = vm.call_async("failing", &[]).unwrap();
            let mut scheduler = Scheduler::new();
            let cancelled = scheduler.suspend(vm.call_async("pending", &[]).unwrap());
            for _ in 0..3 {
                pending.resume().unwrap();
                failing.resume().unwrap();
                scheduler.resume(cancelled).unwrap();
                assert_eq!(pending.cps_result(), None);
                assert_eq!(failing.cps_result(), None);
            }
            EVENTS.with(|events| assert_eq!(*events.borrow(), [7, 7, -1, -1, 7, 7]));
            vm.load_source("fn pending() -> Int32 { 0 }").unwrap();
            assert_eq!(vm.call_int32_0("pending").unwrap(), 0);
            scheduler.cancel(cancelled).unwrap();
            drop(vm);
            READY.with(|ready| ready.set(true));
            pending.resume().unwrap();
            assert_eq!(pending.cps_result(), Some(expected));
            let ResumeError::Failed(error) = failing.resume().unwrap_err() else {
                panic!()
            };
            assert_eq!(error.code(), ErrorCode::RuntimeError);
            let span = error.span().unwrap();
            assert_eq!(
                span.slice(error.sources().get(span.source_id()).unwrap().text()),
                Some("42 / delayed_value(record(-1))")
            );
            assert_eq!(failing.resume(), Err(ResumeError::AlreadyFinished));
            assert_eq!(scheduler.resume_all().unwrap(), 0);
            EVENTS.with(|events| assert_eq!(*events.borrow(), [7, 7, -1, -1, 7, 7]));
        }
    }
}

#[test]
fn result_option_dependency_keeps_identity_without_exporting_names() {
    for imports in [
        "import joky/result as res.{type Result as Outcome}",
        "import joky/result as res.{type Result as Outcome} import joky/option as opt.{type Option as Maybe}",
        "import joky/option as opt.{type Option as Maybe} import joky/result as res.{type Result as Outcome}",
    ] {
        let source = format!("{imports} struct Option {{ wrong: Bool }} fn Some(n: Int32) -> Bool {{ false }} fn check() -> Int32 {{ res.Result.Ok::<Int32, Bool>(42).ok().unwrap_or(0) }}");
        let types = TypeRegistry::new().with_prelude(false);
        let first = types.resolve_aliases(&parser::parse(&source).unwrap()).unwrap();
        let second = types.resolve_aliases(&first).unwrap();
        assert_eq!(first.enums.len(), 2);
        assert_eq!(second.implementations.len(), 2);
        assert_eq!(first.enums.iter().map(|d| d.id).collect::<Vec<_>>(), second.enums.iter().map(|d| d.id).collect::<Vec<_>>());
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.set_prelude_enabled(false);
            vm.load_module(&second).unwrap();
            assert_eq!(vm.call_int32_0("check").unwrap(), 42);
        }
    }
    for source in [
        "import joky/result as res fn bad() { res.Option.Some(1) }",
        "import joky/result as res fn bad() { res.Some(1) }",
        "import joky/result as res fn bad() { Option.Some(1) }",
        "import joky/result.{type Option} fn bad() {}",
        "import joky/option as opt fn bad() { opt.Result.Ok::<Int32, Bool>(1) }",
    ] {
        let mut vm = VmEngine::new_interpreter();
        vm.set_prelude_enabled(false);
        assert!(vm.load_source(source).is_err(), "{source}");
    }
}

#[test]
fn option_result_methods_reject_incompatible_defaults_and_alternatives() {
    for source in [
        "fn bad() { Some(1).unwrap_or(true) }",
        "fn bad() { Some(1).or(Some(true)) }",
        "fn bad() { Some(1).and::<String>(Some(\"text\")) }",
        "fn bad() { Result.Ok::<Int32, Bool>(1).unwrap_or(false) }",
        "fn bad() { Result.Ok::<Int32, Bool>(1).or(Result.Ok::<Bool, Unit>(true)) }",
        "fn bad() { Result.Ok::<Int32, Bool>(1).or::<String>(Result.Err(\"text\")) }",
        "fn bad() { Result.Ok::<Int32, Bool>(1).and(Result.Ok::<Int64, Unit>(1)) }",
        "fn bad() { let value: Option<Bool> = Result.Ok::<Int32, Bool>(1).ok() }",
    ] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source("fn current() -> Int32 { 42 }").unwrap();
            let version = vm.module_version();
            assert_eq!(
                vm.load_source(source).unwrap_err().code(),
                ErrorCode::TypeMismatch,
                "{source}"
            );
            assert_eq!(vm.module_version(), version);
            assert_eq!(vm.call_int32_0("current").unwrap(), 42);
        }
        let mut compiler = crate::aot::AotCompiler::new("invalid_stdlib").unwrap();
        assert_eq!(
            compiler
                .compile_module(&parser::parse(source).unwrap())
                .unwrap_err()
                .code(),
            ErrorCode::TypeMismatch,
            "{source}"
        );
    }
}
