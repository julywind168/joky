//! Generated programs exercise a controlled host file service and worker lifecycle.
mod host {
    include!("fixtures/host_file_io_host.rs");
}
#[cfg(unix)]
#[path = "support/runtime.rs"]
mod runtime;
#[cfg(unix)]
#[path = "support/scratch.rs"]
mod scratch;
use joky::{
    vm::{VmEngine, VmMode},
    FunctionSignature, Type,
};
const SOURCE: &str = include_str!("fixtures/host_file_io.jk");

#[test]
fn host_file_io_worker_lifecycle_in_jit_and_interpreter() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.register_resource_type("ProbeFile").unwrap();
        vm.register_resource_type("ProbeRequest").unwrap();
        // SAFETY: Static test callbacks have exact scalar/factory signatures;
        // request resource destruction revokes delivery before task cleanup.
        unsafe {
            vm.register_resource_factory(
                "host_file_io_file",
                "ProbeFile",
                host::FILE_FACTORY,
                None,
            )
            .unwrap();
            vm.register_resource_factory(
                "host_file_io_request",
                "ProbeRequest",
                host::REQUEST_FACTORY,
                None,
            )
            .unwrap();
            vm.register_suspending_fn(
                "host_file_io_wait",
                FunctionSignature::new(vec![Type::UInt64], Type::Int32),
                host::host_file_io_wait,
            )
            .unwrap();
            vm.register_fn(
                "host_file_io_closed",
                FunctionSignature::new(vec![Type::Int32], Type::Int32),
                host::host_file_io_closed as *const u8,
            )
            .unwrap();
        }
        vm.load_source(SOURCE)
            .unwrap_or_else(|error| panic!("{mode:?}: {error}"));
        host::check_suite(|entry| vm.prepare_async(entry, &[]).unwrap());
    }
}

#[test]
#[cfg(unix)]
fn host_file_io_worker_lifecycle_in_standalone_aot() {
    use std::process::Command;
    let work = scratch::ScratchDir::new("host_file_io");
    let mut compiler = joky::aot::AotCompiler::new("host_file_io").unwrap();
    compiler.register_resource_type("ProbeFile").unwrap();
    compiler.register_resource_type("ProbeRequest").unwrap();
    compiler
        .register_resource_factory("host_file_io_file", "ProbeFile")
        .unwrap();
    compiler
        .register_resource_factory("host_file_io_request", "ProbeRequest")
        .unwrap();
    compiler
        .compile_module(&joky::parser::parse(SOURCE).unwrap())
        .unwrap_or_else(|error| panic!("{error}"));
    let mut driver = include_str!("fixtures/host_file_io_host.rs").to_owned();
    driver.push_str("\nextern \"C\" {\n");
    for entry in host::ENTRIES {
        driver.push_str(&format!(
            "#[link_name=\"{entry}\"] fn entry_{entry}(frame:*mut CpsFrame)->i32;\n"
        ));
    }
    driver.push_str("}\nfn main() { check_suite(|name| { let (entry, slots): (joky::runtime::cps::CpsEntry,usize) = match name {\n");
    for entry in host::ENTRIES {
        driver.push_str(&format!(
            "\"{entry}\" => (entry_{entry}, {}),\n",
            compiler.cps_frame_slots(entry).unwrap()
        ));
    }
    driver.push_str("_ => unreachable!() }; unsafe { Continuation::from_native(entry,CpsFrame::new(slots),Rc::new(())) } }); }\n");
    std::fs::write(work.path().join("driver.rs"), driver).unwrap();
    compiler
        .write_object(&work.path().join("host_file_io.o"))
        .unwrap();
    let library = runtime::runtime_archive().with_extension("rlib");
    let built = Command::new("rustc")
        .arg("--edition=2021")
        .arg(work.path().join("driver.rs"))
        .arg("--extern")
        .arg(format!("joky={}", library.display()))
        .arg("-L")
        .arg(format!(
            "dependency={}",
            library.parent().unwrap().display()
        ))
        .arg("-C")
        .arg(format!(
            "link-arg={}",
            work.path().join("host_file_io.o").display()
        ))
        .arg("-o")
        .arg(work.path().join("driver"))
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let output = Command::new(work.path().join("driver")).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
