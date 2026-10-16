use joky::{
    runtime::driver::block_on,
    vm::{VmEngine, VmMode},
};
#[path = "fixtures/fs_convenience_host.rs"]
mod host;
#[cfg(unix)]
#[path = "support/runtime.rs"]
mod runtime;
#[cfg(unix)]
#[path = "support/scratch.rs"]
mod scratch;

#[test]
fn file_convenience_short_io_limits_utf8_and_progress() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        host::suite(|service| {
            let mut vm = VmEngine::with_mode(mode);
            vm.set_file_service(service).unwrap();
            vm.load_source(include_str!("fixtures/fs_convenience.jk"))
                .unwrap_or_else(|error| panic!("{mode:?}: {error}"));
            block_on(vm.prepare_async("run", &[]).unwrap())
                .unwrap_or_else(|error| panic!("{mode:?}: {error}"))
        });
    }
}

#[test]
#[cfg(unix)]
fn file_convenience_standalone_aot() {
    use std::process::Command;
    let work = scratch::ScratchDir::new("fs-convenience");
    let mut compiler = joky::aot::AotCompiler::new("fs-convenience").unwrap();
    compiler
        .compile_module(&joky::parser::parse(include_str!("fixtures/fs_convenience.jk")).unwrap())
        .unwrap_or_else(|error| panic!("{error}"));
    let slots = compiler.cps_frame_slots("run").unwrap();
    compiler.write_object(&work.path().join("fs.o")).unwrap();
    let driver = format!("{}\nuse joky::runtime::{{cps::CpsFrame,suspension::Continuation,driver::block_on}}; use std::rc::Rc;\nextern \"C\" {{fn run(frame:*mut CpsFrame)->i32;}}\nfn main(){{suite(|service|{{let task=unsafe{{Continuation::from_native(run,CpsFrame::new({slots}),Rc::new(()))}}.with_file_service(service).unwrap();block_on(task).unwrap()}});}}", include_str!("fixtures/fs_convenience_host.rs"));
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
        .arg(format!("link-arg={}", work.path().join("fs.o").display()))
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
