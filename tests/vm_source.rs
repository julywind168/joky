use joky::{
    runtime::driver::block_on,
    vm::{VmEngine, VmMode},
};
#[path = "fixtures/vm_source_host.rs"]
mod host;
#[cfg(unix)]
#[path = "support/runtime.rs"]
mod runtime;
#[cfg(unix)]
#[path = "support/scratch.rs"]
mod scratch;

#[test]
fn vm_source_files_and_atomic_publication() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for entry in ["run_jit", "run_interp"] {
            host::suite(|service| {
                let mut vm = VmEngine::with_mode(mode);
                vm.set_file_service(service).unwrap();
                vm.load_source(include_str!("fixtures/vm_source.jk"))
                    .unwrap_or_else(|error| panic!("{mode:?}: {error}"));
                block_on(vm.prepare_async(entry, &[]).unwrap())
                    .unwrap_or_else(|error| panic!("{mode:?}: {error}"));
            });
        }
    }
}

#[test]
#[cfg(unix)]
fn vm_source_files_and_atomic_publication_standalone_aot() {
    use std::process::Command;
    let work = scratch::ScratchDir::new("vm-source");
    let mut compiler = joky::aot::AotCompiler::new("vm-source").unwrap();
    compiler
        .compile_module(&joky::parser::parse(include_str!("fixtures/vm_source.jk")).unwrap())
        .unwrap_or_else(|error| panic!("{error}"));
    let entries =
        ["run_jit", "run_interp"].map(|entry| (entry, compiler.cps_frame_slots(entry).unwrap()));
    compiler
        .write_object(&work.path().join("source.o"))
        .unwrap();
    for (entry, slots) in entries {
        let host_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/vm_source_host.rs");
        let driver = format!(
            r#"#[path={host_path:?}] mod host;
use joky::runtime::{{cps::CpsFrame,suspension::Continuation,driver::block_on}}; use std::rc::Rc;
extern "C" {{fn {entry}(frame:*mut CpsFrame)->i32;}}
fn main(){{host::suite(|service|{{let task=unsafe{{Continuation::from_native({entry},CpsFrame::new({slots}),Rc::new(()))}}.with_file_service(service).unwrap();block_on(task).unwrap();}});}}"#
        );

        std::fs::write(work.path().join("driver.rs"), driver).unwrap();
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
                work.path().join("source.o").display()
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
        assert!(output.status.success(), "{output:?}");
    }
}

#[test]
fn vm_source_file_loading_requires_explicit_file_capability() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(r#"import joky/vm
import joky/fs
fn run() @suspends ->Bool {
    let engine=vm.Vm.interpreter()
    match engine.load_file("room.jk") {
        Err(error)=>match error.kind {vm.LoadErrorKind.Io(io)=>match io.kind{fs.ErrorKind.Unsupported=>true,_=>false},_=>false}
        Ok(_)=>false
    }
}"#).unwrap();
        assert_eq!(block_on(vm.prepare_async("run", &[]).unwrap()).unwrap(), 1);
    }
}

#[test]
fn vm_source_named_diagnostics_and_staged_file_capability() {
    use joky::runtime::fs::{FileLimits, FileService};
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_file_service(
            FileService::native(env!("CARGO_MANIFEST_DIR"), FileLimits::default()).unwrap(),
        )
        .unwrap();
        vm.load_source("fn answer()->Int32{7}").unwrap();
        let error = vm
            .load_named_source("scripts/broken.jk", "fn broken(")
            .unwrap_err();
        let span = error.span().unwrap();
        assert_eq!(
            error.sources().get(span.source_id()).unwrap().name(),
            "scripts/broken.jk"
        );
        assert_eq!(vm.call_int32_0("answer").unwrap(), 7);
        let source = r#"import joky/fs
fn run() @suspends ->Bool {
    match fs.open("Cargo.toml",fs.OpenOptions.default()) {
        Ok(file)=>{file.close().unwrap(); true}
        Err(_)=>false
    }
}"#;
        assert_eq!(vm.publish_source(source).unwrap(), 0);
        assert_eq!(block_on(vm.prepare_async("run", &[]).unwrap()).unwrap(), 1);
    }
}
