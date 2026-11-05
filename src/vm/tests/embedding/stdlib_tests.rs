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
                    include_str!("../../../../tests/fixtures/option_result_methods.jk"),
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
        let source = format!("extern fn delayed_value(n: Int32) @suspends -> Int32 extern fn record(n: Int32) -> Int32 fn pending() @suspends -> Int32 {{ {pending} }} fn failing() @suspends -> Int32 {{ let value = {failing}; record(99); value }}");
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
        assert_eq!(second.implementations.len(), first.implementations.len());
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

#[test]
fn option_result_borrowed_queries_and_lazy_defaults_execute_in_all_backends() {
    let source = include_str!("../../../../tests/fixtures/option_result_queries.jk");
    for prelude in [false, true] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.set_prelude_enabled(prelude);
            vm.load_source(source).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(
                vm.call_int32_0("check").unwrap(),
                42,
                "{mode:?}, prelude={prelude}"
            );
            crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
    crate::backends::aot::tests::assert_sync_int32(&parser::parse(source).unwrap(), "check", 42);
}

#[test]
fn option_result_lazy_defaults_reject_wrong_callbacks_and_consumed_owners() {
    for source in [
        "fn bad() { Some(1).unwrap_or_else(|| { true }); }",
        "fn bad() { None::<Int32>.unwrap_or_else(|x: Int32| { x }); }",
        "fn bad() { Err::<Int32, Bool>(false).unwrap_or_else(|x: Int32| { x }); }",
        "fn bad() { let value = Some(\"owned\"); let taken = value.unwrap_or_else(|| { \"fallback\" }); value.is_some(); }",
        "fn bad() { let fallback = \"owned\"; let take = move || { fallback }; Some(\"kept\").unwrap_or_else(take); take(); }",
        "fn wait() @suspends -> Int32 { 42 } fn bad() { Some(1).unwrap_or_else(wait); }",
    ] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            assert!(VmEngine::with_mode(mode).load_source(source).is_err(), "{source}");
        }
        let mut compiler = crate::aot::AotCompiler::new("invalid-lazy-default").unwrap();
        assert!(compiler.compile_module(&parser::parse(source).unwrap()).is_err(), "{source}");
    }
}

#[test]
fn postfix_unwrap_and_propagation_preserve_values() {
    for prelude in [false, true] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            for suspends in [false, true] {
                let source = with_suspends(
                    include_str!("../../../../tests/fixtures/postfix_unwrap.jk"),
                    suspends,
                    Suspends::Typed,
                );
                let mut vm = VmEngine::with_mode(mode);
                vm.set_prelude_enabled(prelude);
                vm.load_source(&source)
                    .unwrap_or_else(|e| panic!("{mode:?}, suspends={suspends}: {e}"));
                let result = if suspends {
                    vm.call_async("check", &[]).unwrap().cps_result().unwrap() as i32
                } else {
                    vm.call_int32_0("check").unwrap()
                };
                assert_eq!(
                    result, 42,
                    "{mode:?}, prelude={prelude}, suspends={suspends}"
                );
                for name in ["fail_option", "fail_result"] {
                    let error = if suspends {
                        vm.call_async(name, &[]).err().unwrap()
                    } else {
                        vm.call_int32_0(name).unwrap_err()
                    };
                    assert!(error.to_string().contains("Postfix `!` on"), "{error}");
                    let expression = error.span().unwrap().slice(&source).unwrap();
                    assert!(expression.ends_with('!'), "{expression}");
                }
            }
        }
    }
}

#[test]
fn postfix_unwrap_normalized_modules_keep_standard_enum_identity() {
    let parsed =
        parser::parse(include_str!("../../../../tests/fixtures/postfix_unwrap.jk")).unwrap();
    let registry = crate::TypeRegistry::new().with_prelude(false);
    let normalized = registry.resolve_aliases(&parsed).unwrap();
    let normalized = registry.resolve_aliases(&normalized).unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_module(&normalized).unwrap();
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
        // A separate stdlib cache must not replace the module's identities.
        vm.load_source("fn unrelated() -> Int32 { Some(7)! }")
            .unwrap();
        assert_eq!(vm.call_int32_0("unrelated").unwrap(), 7);
        vm.load_module(&normalized).unwrap();
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
    }
    crate::backends::aot::tests::assert_sync_int32(&normalized, "check", 42);
}

#[test]
fn postfix_unwrap_rejects_wrong_types_and_consumed_owners() {
    for source in [
        "fn bad() -> Int32 { 1! }",
        "fn bad() -> Bool { true? }",
        "fn bad() -> Int32 { Some(1)? }",
        "fn bad() -> Result<Int32, Unit> { Ok(Some(1)?) }",
        "fn bad() -> Option<Int32> { Some(Result.Ok::<Int32, Unit>(1)?) }",
        "fn bad(x: Result<Int32, String>) -> Result<Int32, Bool> { Ok(x?) }",
        "fn bad() { let x = Some(\"owned\"); let y = x!; x!; }",
        "fn bad() -> Option<String> { let x = Some(\"owned\"); let y = x?; Some(x!) }",
        "fn bad(x: &Option<Int32>) -> Int32 { x! }",
        "fn bad() -> &Int32 { let n = 42; Some(&n)! }",
        "fn bad() { let f = || -> Int32 { Some(1)? } }",
        "import joky/domain fn bad(x:domain.Outcome<Int32,String>)->domain.Outcome<Int32,Bool>{domain.Outcome.Committed(x?)}",
        "import joky/domain fn bad(x:domain.Outcome<Int32,String>)->Result<Int32,String>{Ok(x?)}",
        "import joky/domain fn bad(x:Result<Int32,String>)->domain.Outcome<Int32,String>{domain.Outcome.Committed(x?)}",
        "import joky/domain fn bad(x:domain.Outcome<String,String>){let value=x!; x!;}",
    ] {
        let mut vm = VmEngine::new_interpreter();
        assert!(vm.load_source(source).is_err(), "{source}");
    }
    for name in ["Option", "Result", "Outcome"] {
        let source =
            format!("enum {name}<T> {{ None, Some(T) }} fn bad() -> Int32 {{ {name}.Some(42)! }}");
        let mut vm = VmEngine::new_interpreter();
        vm.set_prelude_enabled(false);
        let error = vm.load_source(&source).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("standard Option, Result or Outcome"),
            "{error}"
        );
    }
}

#[test]
fn postfix_unwrap_cleanup_and_cancellation() {
    use super::support::{parity_vm, EVENTS};
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(
                include_str!("../../../../tests/fixtures/postfix_cleanup.jk"),
                suspends,
                Suspends::Typed,
            );
            let vm = parity_vm(&source, mode);
            if suspends {
                assert_eq!(vm.call_async("check", &[]).unwrap().cps_result(), Some(42));
            } else {
                assert_eq!(vm.call_int32_0("check").unwrap(), 42);
            }
            EVENTS.with(|e| assert_eq!(*e.borrow(), [-1, -42, 42, 1, -1, -7, 1, 7, 99]));
            EVENTS.with(|e| e.borrow_mut().clear());
            let error = if suspends {
                vm.call_async("force_failure", &[]).err().unwrap()
            } else {
                vm.call_int32_0("force_failure").unwrap_err()
            };
            assert!(error.to_string().contains("Postfix `!` on Err"), "{error}");
            EVENTS.with(|e| assert_eq!(*e.borrow(), [-1, -7, 7, 1]));
            crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
    postfix_cancellation(super::support::CancellationSweep::ResourceBoundaries);
}

fn postfix_cancellation(sweep: super::support::CancellationSweep) {
    use super::support::{assert_cancellation, parity_vm, EVENTS};
    let source = with_suspends(
        include_str!("../../../../tests/fixtures/postfix_cleanup.jk"),
        true,
        Suspends::Typed,
    );
    let mut vm = parity_vm(&source, VmMode::Interpreter);
    assert_cancellation(
        &mut vm,
        "check",
        4000,
        sweep,
        || EVENTS.with(|e| e.borrow_mut().clear()),
        || EVENTS.with(|e| e.borrow().clone()),
        || {
            crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
            EVENTS.with(|e| {
                let events = e.borrow();
                for (id, maximum) in [(42, 1), (7, 1), (1, 2)] {
                    let created = events.iter().filter(|&&n| n == -id).count();
                    let dropped = events.iter().filter(|&&n| n == id).count();
                    assert!(dropped >= created && dropped <= maximum, "{events:?}");
                }
            });
        },
    );
}

#[test]
#[ignore = "exhaustive: cancel postfix extraction and propagation at every instruction"]
fn exhaustive_postfix_unwrap_cancellation() {
    postfix_cancellation(super::support::CancellationSweep::EveryInstruction);
}

#[test]
fn postfix_unwrap_resumes_once_and_joins_scopes_before_return() {
    use super::support::{parity_vm, EVENTS};
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm(
            include_str!("../../../../tests/fixtures/postfix_tasks.jk"),
            mode,
        );
        assert_eq!(vm.call_int32_0("closure_check").unwrap(), 42);
        let mut task = vm.call_async("check", &[]).unwrap();
        assert!(!task.is_finished());
        vm.load_source("fn check() -> Int32 { 0 }").unwrap();
        drop(vm);
        for _ in 0..100 {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        assert_eq!(task.cps_result(), Some(42));
        drop(task);
        EVENTS.with(|e| assert_eq!(*e.borrow(), [10, 20, 10, 30]));
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn option_unwrap_preserves_payloads_and_cleans_up_failed_tasks() {
    use crate::runtime::{arena::LIVE, suspension::ResumeError};
    let source = include_str!("../../../../tests/fixtures/option_unwrap.jk");
    let clean = || LIVE.with(|n| assert_eq!(n.get(), 0));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source(source).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(vm.call_int32_0("option_unwrap_values").unwrap(), 42);
        clean();
        for name in ["option_unwrap_none", "option_unwrap_unit_none"] {
            let error = vm.call_int32_0(name).unwrap_err();
            assert_eq!(error.code(), ErrorCode::RuntimeError);
            assert!(
                error.to_string().contains("Called Option.unwrap on None"),
                "{error}"
            );
            clean();
        }
        for name in ["option_unwrap_pending", "option_unwrap_pending_none"] {
            for finish in [false, true] {
                let mut task = vm.call_async(name, &[]).unwrap();
                assert!(!task.is_finished());
                if finish {
                    if name.ends_with("_none") {
                        let ResumeError::Failed(error) = task.resume().unwrap_err() else {
                            panic!()
                        };
                        assert_eq!(error.code(), ErrorCode::RuntimeError);
                        assert!(
                            error.to_string().contains("Called Option.unwrap on None"),
                            "{error}"
                        );
                    } else {
                        task.resume().unwrap();
                        assert_eq!(task.cps_result(), Some(42));
                    }
                }
                drop(task);
                clean();
            }
        }
        // A failed extraction does not poison later calls through the same VM.
        assert_eq!(vm.call_int32_0("option_unwrap_values").unwrap(), 42);
        clean();
    }
    #[cfg(unix)]
    crate::backends::aot::tests::assert_sync_results(
        &parser::parse(source).unwrap(),
        &[
            ("option_unwrap_values", Some(42)),
            ("option_unwrap_none", None),
            ("option_unwrap_unit_none", None),
            ("option_unwrap_values", Some(42)),
        ],
    );
    super::support::frontend_error(
        "fn bad() { let value = Some(\"owned\"); value.unwrap(); value.unwrap(); }",
        &crate::ModuleSources::new(),
    );
}
