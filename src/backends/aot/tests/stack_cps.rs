//! CPS stack refusal must leave the task, its owners and the host usable.
use super::{runtime::runtime_archive, AotCompiler, ScratchDir};
use crate as joky;
use crate::{
    backends::jit,
    core::resolver::BindingRegistry,
    runtime::{
        cps::CpsEntry,
        suspension::{ContinuationStatus, ModuleId},
    },
    vm::{VmEngine, VmMode},
    ErrorCode, FunctionSignature, Type, TypeRegistry,
};
use std::{collections::HashMap, process::Command};

#[path = "../../../../tests/fixtures/native_stack_cps_host.rs"]
mod host;

const SOURCE: &str = include_str!("../../../../tests/fixtures/native_stack_cps.jk");
const ROUTES: [&str; 4] = [
    "start_direct",
    "start_indirect",
    "start_mutual",
    "start_branch",
];

fn clean() {
    crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
}

fn vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    vm.set_prelude_enabled(false);
    vm.register_resource_type("StackResource").unwrap();
    unsafe {
        vm.register_resource_factory(
            "stack_owner",
            "StackResource",
            host::__joky_resource_factory_v1_stack_owner,
            None,
        )
        .unwrap();
        vm.register_borrowing_suspending_fn(
            "stack_wait",
            FunctionSignature::new(vec![Type::reference(Type::Int32, true)], Type::Int32),
            host::__joky_cps_borrow_v1_stack_wait,
            None,
        )
        .unwrap();
    }
    vm.load_source(SOURCE)
        .unwrap_or_else(|e| panic!("{e} {:?}", e.span().and_then(|s| s.slice(SOURCE))));
    vm
}

#[test]
fn native_stack_cps_limits_cleanup_and_recovery() {
    const CHILD: &str = "JOKY_CPS_STACK_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let name = std::thread::current().name().unwrap().to_owned();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &name, "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    // Both VM modes must recover and preserve the same owner/borrow contract.
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        for depth in [3, 200_000, 3] {
            host::reset(true);
            let result = vm.call_async("start_direct", &[depth]);
            if depth == 3 {
                let task = result.unwrap();
                assert_eq!(task.status(), ContinuationStatus::Completed);
                assert_eq!(task.cps_result(), Some(42));
            } else {
                let error = match result {
                    Err(error) => error,
                    Ok(_) => panic!("expected runtime failure"),
                };
                assert_eq!(error.code(), ErrorCode::ResourceLimit);
                assert!(error.span().is_some());
            }
            host::balanced();
            clean();
        }
        host::reset(false);
        let mut task = vm.call_async("start_direct", &[128]).unwrap();
        task.resume().unwrap();
        assert!(!task.is_finished());
        let limit = crate::runtime::arena::LIMIT.with(|limit| limit.replace(0));
        drop(task);
        crate::runtime::arena::LIMIT.with(|value| value.set(limit));
        host::balanced();
        clean();
    }

    let ast = crate::parser::parse(SOURCE).unwrap();
    let mut types = TypeRegistry::new().with_prelude(false);
    let resource = types.register_resource("StackResource").unwrap();
    types.resource_factories.insert(
        "stack_owner".into(),
        FunctionSignature::new(vec![Type::UInt64], resource),
    );
    let compiled = jit::compile_with_registry(
        &ast,
        &[
            (
                "__joky_resource_factory_v1_stack_owner",
                &host::__joky_resource_factory_v1_stack_owner as *const _ as *const u8,
            ),
            (
                "__joky_cps_borrow_v1_stack_wait",
                &host::__joky_cps_borrow_v1_stack_wait as *const _ as *const u8,
            ),
        ],
        &HashMap::new(),
        vec![],
        ModuleId(1),
        &BindingRegistry::default(),
        &types,
    )
    .unwrap_or_else(|e| panic!("{e} {:?}", e.span().and_then(|s| s.slice(SOURCE))));
    let entries: Vec<_> = ROUTES
        .iter()
        .chain([&"reject_owner"])
        .map(|name| {
            let f = &compiled.exports[*name];
            (
                unsafe { std::mem::transmute::<*const u8, CpsEntry>(f.entry) },
                f.info.frame_slots,
            )
        })
        .collect();
    std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(move || {
            for &(entry, slots) in &entries[..ROUTES.len()] {
                host::exercise(entry, slots, clean);
            }
            let (entry, slots) = entries[ROUTES.len()];
            host::reject_input(entry, slots, clean);
        })
        .unwrap()
        .join()
        .unwrap();

    let tmp = ScratchDir::new("cps-stack");
    let object = tmp.path().join("program.o");
    let program = tmp.path().join("host");
    let mut compiler = AotCompiler::new("cps-stack").unwrap();
    compiler.set_prelude_enabled(false).unwrap();
    compiler.register_resource_type("StackResource").unwrap();
    compiler
        .register_resource_factory("stack_owner", "StackResource")
        .unwrap();
    compiler.compile_module(&ast).unwrap();
    let mut source = String::from("extern crate joky;\nmod probe;\nunsafe extern \"C\" {\n");
    for name in ROUTES.into_iter().chain(["reject_owner"]) {
        if let Some(export) = compiler.text_exports_v2().iter().find(|f| f.name == name) {
            source.push_str(&format!("#[link_name = {:?}]\n", export.symbol));
        }
        source.push_str(&format!(
            "fn {name}(frame: *mut std::ffi::c_void) -> i32;\n"
        ));
    }
    source
        .push_str("}\nfn main() { std::thread::Builder::new().stack_size(512 * 1024).spawn(|| {\n");
    for name in ROUTES {
        source.push_str(&format!(
            "probe::exercise(unsafe {{ std::mem::transmute({name} as unsafe extern \"C\" fn(*mut std::ffi::c_void)->i32) }}, {}, || {{}});\n",
            compiler.cps_frame_slots(name).unwrap()
        ));
    }
    source.push_str(&format!(
        "probe::reject_input(unsafe {{ std::mem::transmute(reject_owner as unsafe extern \"C\" fn(*mut std::ffi::c_void)->i32) }}, {}, || {{}});\n",
        compiler.cps_frame_slots("reject_owner").unwrap()
    ));
    source.push_str("}).unwrap().join().unwrap(); }\n");
    compiler.write_object(&object).unwrap();
    std::fs::write(tmp.path().join("host.rs"), source).unwrap();
    std::fs::write(
        tmp.path().join("probe.rs"),
        include_str!("../../../../tests/fixtures/native_stack_cps_host.rs"),
    )
    .unwrap();
    let library = runtime_archive().with_extension("rlib");
    let output = Command::new("rustc")
        .arg("--edition=2021")
        .arg(tmp.path().join("host.rs"))
        .arg("--extern")
        .arg(format!("joky={}", library.display()))
        .arg("-L")
        .arg(format!(
            "dependency={}",
            library.parent().unwrap().display()
        ))
        .arg("-C")
        .arg(format!("link-arg={}", object.display()))
        .arg("-o")
        .arg(&program)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new(program).output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
