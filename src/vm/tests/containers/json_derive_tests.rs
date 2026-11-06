use super::{support::CancellationSweep, VmEngine, VmMode};
use crate::ErrorCode;

fn vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    vm.load_source(include_str!(
        "../../../../tests/fixtures/json_derive_lifecycle.jk"
    ))
    .unwrap_or_else(|error| panic!("{error} at {:?}", error.span()));
    vm
}

fn clean() {
    crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
}

#[test]
fn json_derive_rejections_point_to_attributes_and_fields() {
    for (source, diagnostic, field) in [
        ("#derive struct Bad {}", "#derive requires", None),
        ("#derive() struct Bad {}", "at least one trait", None),
        ("#derive(Unknown) struct Bad {}", "Only #derive", None),
        (
            "#derive(ToJson, ToJson) struct Bad {}",
            "Duplicate ToJson",
            None,
        ),
        (
            "#derive(ToJson) #derive(ToJson) struct Bad {}",
            "Duplicate ToJson",
            None,
        ),
        ("#derive(ToJson(1)) struct Bad {}", "Only #derive", None),
        ("#derive(ToJson) enum Bad { A }", "require a struct", None),
        ("#derive(ToJson) fn bad() {}", "require a struct", None),
        (
            "#derive(ToJson) struct Bad { callback: fn() -> Int32 }",
            "Cannot derive",
            Some("callback: fn() -> Int32"),
        ),
        (
            "#derive(FromJson) struct Bad { pair: (Int32, Bool) }",
            "Cannot derive",
            Some("pair: (Int32, Bool)"),
        ),
        (
            "struct Unknown {} #derive(ToJson) struct Bad { value: Unknown }",
            "Cannot derive",
            Some("value: Unknown"),
        ),
        (
            "struct Unknown {} #derive(FromJson) struct Bad { value: Unknown }",
            "Cannot derive",
            Some("value: Unknown"),
        ),
        ("import joky/json/typed impl<T, U> typed.ToJson for T {} #derive(ToJson) struct Bad { value: Int32 }", "impl requires", None),
    ] {
        let source = format!("// 🙂\n{source}");
        let error = super::support::frontend_error(&source, &crate::ModuleSources::new());
        assert!(error.message().contains(diagnostic), "{source}: {error}");
        if let Some(field) = field {
            assert_eq!(error.span().unwrap().slice(&source), Some(field));
        }
    }
}

#[test]
fn json_derive_normalization_is_idempotent_and_manual_impls_conflict() {
    let source =
        "#derive(ToJson, FromJson) struct Value<T> { value: T } fn probe() -> Int32 { 42 }";
    let registry = crate::TypeRegistry::new().with_prelude(false);
    let parsed = crate::parser::parse(source).unwrap();
    let first = registry.resolve_aliases(&parsed).unwrap();
    let second = registry.resolve_aliases(&first).unwrap();
    assert_eq!(first.implementations.len(), second.implementations.len());
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_module(&second).unwrap();
        assert_eq!(vm.call_int32_0("probe").unwrap(), 42);
    }
    crate::backends::aot::tests::assert_sync_int32(&second, "probe", 42);
    let source = r#"
        import joky/json/typed
        import joky/json
        import joky/result
        #derive(ToJson)
        struct Value {}
        impl typed.ToJson for Value {
            fn to_json(&self, context: &mut typed.Context) @noescape -> result.Result<json.Value, typed.MappingError> {
                result.Ok(json.Value.Null)
            }
        }
    "#;
    let error = super::support::frontend_error(source, &crate::ModuleSources::new());
    assert_eq!(error.code(), ErrorCode::DuplicateDefinition);
}

struct RestoreBudget(usize);
impl Drop for RestoreBudget {
    fn drop(&mut self) {
        crate::runtime::arena::LIMIT.with(|limit| limit.set(self.0));
    }
}

#[test]
fn json_derive_partial_mapping_and_error_paths_release_owners() {
    let restore = RestoreBudget(crate::runtime::arena::LIMIT.with(|limit| limit.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        for entry in ["json_derive_probe", "json_derive_failure"] {
            let mut failed = false;
            let mut completed = false;
            for budget in (0..65536).step_by(128) {
                crate::runtime::arena::LIMIT.with(|limit| limit.set(budget));
                let result = vm.call_int32_0(entry);
                crate::runtime::arena::LIMIT.with(|limit| limit.set(restore.0));
                match result {
                    Ok(value) => {
                        assert_eq!(value, 42);
                        completed = true;
                    }
                    Err(error) => {
                        assert_eq!(error.code(), ErrorCode::ResourceLimit, "{mode:?} {entry}");
                        failed = true;
                    }
                }
                clean();
                if completed {
                    break;
                }
            }
            assert!(failed && completed, "{mode:?} {entry}");
        }
    }
}

fn cancellation(sweep: CancellationSweep) {
    let mut vm = VmEngine::with_mode(VmMode::Interpreter);
    vm.load_source(include_str!(
        "../../../../tests/fixtures/json_derive_cancel.jk"
    ))
    .unwrap_or_else(|error| panic!("{error} at {:?}", error.span()));
    super::support::assert_cancellation(
        &mut vm,
        "json_derive_cancel_work",
        100000,
        sweep,
        || {},
        || crate::runtime::arena::LIVE.with(|live| live.get()),
        clean,
    );
}

#[test]
fn json_derive_cancellation_releases_partial_structs_and_paths() {
    cancellation(CancellationSweep::ResourceBoundaries);
}

#[test]
#[ignore = "exhaustive: cancel derived JSON mapping at every interpreter instruction"]
fn exhaustive_json_derive_cancellation_releases_partial_structs_and_paths() {
    cancellation(CancellationSweep::EveryInstruction);
}

#[test]
fn json_derive_pending_values_survive_reload_and_vm_destruction() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for finish in [false, true] {
            let mut vm = vm(mode);
            let mut task = vm.call_async("json_derive_work", &[]).unwrap();
            assert!(!task.is_finished());
            crate::runtime::arena::LIVE.with(|live| assert!(live.get() > 0));
            vm.load_source("fn replacement() -> Int32 { 7 }").unwrap();
            drop(vm);
            if finish {
                while !task.is_finished() {
                    task.resume().unwrap();
                }
                assert_eq!(task.cps_result(), Some(42));
            }
            drop(task);
            clean();
        }
    }
}
