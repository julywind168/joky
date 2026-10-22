#[path = "support/scratch.rs"]
mod scratch;
use joky::{
    runtime::{
        driver::block_on,
        fs::{FileLimits, FileService},
    },
    vm::{VmEngine, VmMode},
};
#[test]
fn filesystem_default_is_unsupported() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(
            r#"import joky/fs
            fn run() @suspends -> Int32 {
                match fs.open("missing", fs.OpenOptions.new()) {
                    Ok(file) => { let ignored = file.close(); 0 }
                    Err(error) => match error.kind { fs.ErrorKind.Unsupported => 42, _ => 1 }
                }
            }"#,
        )
        .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
        assert_eq!(block_on(vm.prepare_async("run", &[]).unwrap()).unwrap(), 42);
    }
}
#[test]
fn filesystem_native_roundtrip() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let work = scratch::ScratchDir::new("fs");
        let service = FileService::native(work.path(), FileLimits::default()).unwrap();
        let mut vm = VmEngine::with_mode(mode);
        vm.set_file_service(service.clone()).unwrap();
        vm.load_source(include_str!("fixtures/fs.jk"))
            .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
        assert_eq!(block_on(vm.prepare_async("run", &[]).unwrap()).unwrap(), 42);
        assert_eq!(
            std::fs::read(work.path().join("bytes.bin")).unwrap(),
            [0, 255, 42]
        );
        drop(vm);
        service.shutdown();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !service.is_quiescent() {
            assert!(std::time::Instant::now() < until, "{:?}", service.usage());
            std::thread::yield_now();
        }
        assert_eq!(service.usage(), Some((0, 0, 0)));
    }
}

#[cfg(unix)]
#[path = "support/runtime.rs"]
mod runtime;
#[test]
#[cfg(unix)]
fn filesystem_cli_and_standalone_aot() {
    use std::process::Command;
    let work = scratch::ScratchDir::new("fs-cli");
    let source = include_str!("fixtures/fs.jk").replace("fn run()", "fn main()");
    let source = source.replace("fn main()", "fn roundtrip()");
    let source = format!("{source}\nfn main() @suspends -> Int32 {{ let n=roundtrip(); if n != 42 {{ return n }}; boundaries() }}");
    let path = work.path().join("main.jk");
    std::fs::write(&path, source).unwrap();
    for interpreter in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_joky"));
        command.current_dir(work.path()).arg("run").arg(&path);
        if interpreter {
            command.arg("--interpreter");
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(42), "{output:?}");
    }
    let bin = work.path().join("installation/bin");
    let lib = work.path().join("installation/lib/joky");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_joky"), bin.join("joky")).unwrap();
    std::fs::hard_link(runtime::runtime_archive(), lib.join("libjoky.a")).unwrap();
    let output = Command::new(bin.join("joky"))
        .current_dir(work.path())
        .arg("build")
        .arg(&path)
        .env_remove("JOKY_RUNTIME")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let output = Command::new(work.path().join("main"))
        .current_dir(work.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(42), "{output:?}");
}

#[test]
fn filesystem_options_and_snapshot_lifetime() {
    use joky::runtime::fs::valid_open_flags;
    let accepted: Vec<_> = (0..128).filter(|&f| valid_open_flags(f)).collect();
    assert_eq!(
        accepted,
        [1, 2, 3, 4, 5, 10, 11, 12, 13, 18, 19, 20, 21, 34, 35, 42, 43]
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let work = scratch::ScratchDir::new("fs-snapshot");
        let service = FileService::native(work.path(), Default::default()).unwrap();
        let mut vm = VmEngine::with_mode(mode);
        vm.set_file_service(service.clone()).unwrap();
        vm.load_source(include_str!("fixtures/fs.jk")).unwrap();
        let task = vm.prepare_async("run", &[]).unwrap();
        assert!(vm.set_file_service(service.clone()).is_err());
        vm.load_source("fn different() -> Int32 { 7 }").unwrap();
        drop(vm);
        assert_eq!(block_on(task).unwrap(), 42);
        service.shutdown();
        wait_until(|| service.is_quiescent());
    }
}
fn wait_until(mut ready: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !ready() {
        assert!(
            std::time::Instant::now() < deadline,
            "file service did not drain"
        );
        std::thread::yield_now();
    }
}
mod descriptor_host {
    include!("fixtures/fs_descriptor.rs");
}
#[test]
fn filesystem_host_descriptor_protocol_and_ownership() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        descriptor_host::suite(|service, entry| {
            let mut vm = VmEngine::with_mode(mode);
            vm.set_file_service(service).unwrap();
            vm.load_source(include_str!("fixtures/fs.jk")).unwrap();
            block_on(vm.prepare_async(entry, &[]).unwrap()).unwrap()
        });
    }
    let stats = std::sync::Arc::new(descriptor_host::Stats::default());
    for invalid in 0..4 {
        let mut d = descriptor_host::descriptor(&stats);
        match invalid {
            0 => d.abi_version = 2,
            1 => d.struct_size = 0,
            2 => d.reserved = 1,
            _ => d.poll = None,
        }
        assert!(unsafe { FileService::from_descriptor(d) }.is_err());
        assert_eq!(std::sync::Arc::strong_count(&stats), 1);
    }
}
#[test]
#[cfg(unix)]
fn filesystem_descriptor_standalone_aot() {
    use std::process::Command;
    let work = scratch::ScratchDir::new("fs-descriptor");
    let mut compiler = joky::aot::AotCompiler::new("fs-descriptor").unwrap();
    compiler
        .compile_module(&joky::parser::parse(include_str!("fixtures/fs.jk")).unwrap())
        .unwrap();
    let mut driver = include_str!("fixtures/fs_descriptor.rs").to_owned();
    driver.push_str("\nuse joky::runtime::{cps::CpsFrame,suspension::Continuation,driver::block_on}; use std::rc::Rc;\nextern \"C\" {\n");
    let entries = ["run", "malformed", "cancel_started", "cancel_queued"];
    for entry in entries {
        driver.push_str(&format!("fn {entry}(frame:*mut CpsFrame)->i32;\n"));
    }
    driver.push_str("}\nfn main(){suite(|service,name|{let(entry,slots):(joky::runtime::cps::CpsEntry,usize)=match name {\n");
    for entry in entries {
        driver.push_str(&format!(
            "\"{entry}\"=>({entry},{}),\n",
            compiler.cps_frame_slots(entry).unwrap()
        ));
    }
    driver.push_str("_=>unreachable!()};let task=unsafe{Continuation::from_native(entry,CpsFrame::new(slots),Rc::new(()))}.with_file_service(service).unwrap();block_on(task).unwrap()});}");
    std::fs::write(work.path().join("driver.rs"), driver).unwrap();
    compiler.write_object(&work.path().join("fs.o")).unwrap();
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

fn descriptor_cancellation(exhaustive: bool) {
    use joky::{
        runtime::executor::{HostExecutor, TaskGroup},
        vm::InterpreterLimits,
    };
    use std::sync::{atomic::Ordering, Arc};
    for (mode, entry) in [
        (0, "run"),
        (2, "malformed"),
        (0, "convenience"),
        (0, "source_loading"),
    ] {
        let stats = Arc::new(descriptor_host::Stats {
            mode,
            ..Default::default()
        });
        let service =
            unsafe { FileService::from_descriptor(descriptor_host::descriptor(&stats)) }.unwrap();
        let mut vm = VmEngine::with_mode(VmMode::Interpreter);
        vm.set_file_service(service).unwrap();
        vm.set_interpreter_limits(InterpreterLimits {
            poll_budget: Some(1),
            ..Default::default()
        })
        .unwrap();
        vm.load_source(&format!(
            "import joky/vm\n{}\n{}\n{}",
            include_str!("fixtures/fs.jk"),
            include_str!("fixtures/fs_convenience_cancel.jk"),
            include_str!("fixtures/vm_source_cancel.jk")
        ))
        .unwrap();
        let execute = |cut: usize| {
            let executor = HostExecutor::default();
            let group = TaskGroup::new();
            executor
                .spawn(&group, vm.prepare_async(entry, &[]).unwrap())
                .unwrap();
            let mut count = 0;
            for _ in 0..cut {
                let report = executor.poll(1).unwrap();
                count += 1;
                if !report.completed.is_empty() {
                    assert_eq!(report.completed[0].result.as_ref().unwrap(), &42);
                    break;
                }
            }
            executor.cancel_group(&group);
            drop(executor);
            assert_eq!(stats.files.load(Ordering::SeqCst), 0, "cut {cut}");
            assert_eq!(stats.results.load(Ordering::SeqCst), 0, "cut {cut}");
            assert_eq!(stats.operations.load(Ordering::SeqCst), 0, "cut {cut}");
            count
        };
        let steps = execute(10000);
        assert!(steps < 10000);
        for cut in (0..=steps).step_by(if exhaustive { 1 } else { (steps / 12).max(1) }) {
            execute(cut);
        }
    }
}
#[test]
fn filesystem_cancellation_releases_result_and_file_owners() {
    descriptor_cancellation(false);
}
#[test]
#[ignore = "exhaustive: file resources cancelled at each interpreter instruction"]
fn exhaustive_filesystem_cancel_every_instruction() {
    descriptor_cancellation(true);
}

#[test]
#[cfg(unix)]
fn filesystem_c_header_and_default_aot_embedding() {
    use joky::runtime::fs::abi::*;
    use std::process::Command;
    let work = scratch::ScratchDir::new("fs-header");
    let source = r#"import joky/fs
    fn missing() @suspends -> Int32 { match fs.open("absent",fs.OpenOptions.new()) {
        Ok(f) => { let ignored=f.close(); 1 }, Err(e) => match e.kind {fs.ErrorKind.Unsupported => 42,_ => 2}
    }}"#;
    let mut compiler = joky::aot::AotCompiler::new("fs-header").unwrap();
    compiler
        .compile_module(&joky::parser::parse(source).unwrap())
        .unwrap();
    let slots = compiler.cps_frame_slots("missing").unwrap();
    compiler.write_object(&work.path().join("fs.o")).unwrap();
    let mut c = String::from("#include <stddef.h>\n#include \"joky_fs.h\"\n");
    macro_rules! layout {($rust:ty,$name:literal,$($field:ident),*)=>{{c.push_str(&format!("_Static_assert(sizeof({})=={},\"size\");\n",$name,std::mem::size_of::<$rust>()));$(c.push_str(&format!("_Static_assert(offsetof({},{})=={},\"offset\");\n",$name,stringify!($field),std::mem::offset_of!($rust,$field)));)*}}}
    layout!(
        FileServiceV1,
        "JokyFileServiceV1",
        abi_version,
        struct_size,
        reserved,
        context,
        retain,
        release,
        submit,
        poll,
        stop,
        release_result,
        release_file,
        shutdown,
        is_quiescent
    );
    layout!(
        FileInputV1,
        "JokyFileInputV1",
        operation_code,
        file_token,
        path,
        path_len,
        flags,
        bytes,
        bytes_len,
        limit,
        origin,
        offset_bits
    );
    layout!(
        FileResultV1,
        "JokyFileResultV1",
        result_token,
        error_code,
        message,
        message_len,
        file_token,
        bytes,
        bytes_len,
        count
    );
    layout!(FileWakeV1, "JokyFileWakeV1", context, retain, release, wake);
    c.push_str(
        &include_str!("fixtures/fs_descriptor.c").replace("FRAME_SLOTS", &slots.to_string()),
    );
    let headers = work.path().join("installation/include/joky");
    std::fs::create_dir_all(&headers).unwrap();
    for entry in
        std::fs::read_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include")).unwrap()
    {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "h") {
            std::fs::copy(&path, headers.join(path.file_name().unwrap())).unwrap();
        }
    }
    std::fs::write(work.path().join("driver.c"), c).unwrap();
    let mut cc = Command::new("cc");
    cc.args(["-std=c11", "-Werror", "-I"])
        .arg(&headers)
        .arg(work.path().join("driver.c"))
        .arg(work.path().join("fs.o"))
        .arg(runtime::runtime_archive());
    if cfg!(target_os = "macos") {
        cc.args(["-liconv", "-lSystem"]);
    } else {
        cc.args(["-ldl", "-lpthread", "-lm"]);
    }
    let built = cc
        .arg("-o")
        .arg(work.path().join("driver"))
        .output()
        .unwrap();
    assert!(built.status.success(), "{built:?}");
    let run = Command::new(work.path().join("driver")).output().unwrap();
    assert!(run.status.success(), "{run:?}");
}
#[test]
fn filesystem_native_options_errors_and_text_bytes() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let work = scratch::ScratchDir::new("fs-options");
        let service = FileService::native(work.path(), Default::default()).unwrap();
        let mut vm = VmEngine::with_mode(mode);
        vm.set_file_service(service.clone()).unwrap();
        vm.load_source(include_str!("fixtures/fs.jk")).unwrap();
        for exists in [false, true] {
            for flags in 0..64 {
                let path = work.path().join("matrix.bin");
                let _ = std::fs::remove_file(&path);
                if exists {
                    std::fs::write(&path, b"abc").unwrap();
                }
                let expected = if !joky::runtime::fs::valid_open_flags(flags) {
                    5
                } else if exists && flags & 16 != 0 {
                    4
                } else if !exists && flags & 24 == 0 {
                    3
                } else {
                    0
                };
                let result = block_on(
                    vm.prepare_async("flags", &[joky::vm::InstanceValue::UInt64(flags)])
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(result, expected, "{mode:?} exists={exists} flags={flags}");
            }
        }
        assert_eq!(
            block_on(vm.prepare_async("boundaries", &[]).unwrap()).unwrap(),
            42
        );
        assert_eq!(
            std::fs::read(work.path().join("limits.bin")).unwrap(),
            "你好，Joky🙂!".as_bytes()
        );
        drop(vm);
        service.shutdown();
        wait_until(|| service.is_quiescent());
    }
}
#[test]
fn filesystem_internal_resources_reject_forgery_and_wrong_signatures() {
    let cases = [
        "extern fn __fs_open(path: String, flags: UInt64) -> fs.OutcomeOwner",
        "extern fn __fs_read(file: &mut fs.FileOwner, limit: Int32) @suspends -> fs.OutcomeOwner",
        "extern fn __fs_code(result: &mut fs.FileOwner) @suspends -> UInt64",
        "import joky/fs fn duplicate(file: fs.File) { let a=file; let b=file }",
        "import joky/fs fn forge() -> fs.File { fs.File {owner: 1} }",
    ];
    for source in cases {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            assert!(
                VmEngine::with_mode(mode).load_source(source).is_err(),
                "{mode:?}: {source}"
            );
        }
        let mut aot = joky::aot::AotCompiler::new("fs-invalid").unwrap();
        assert!(
            aot.compile_module(&joky::parser::parse(source).unwrap())
                .is_err(),
            "{source}"
        );
    }
    for name in ["fs.FileOwner", "fs.OutcomeOwner"] {
        assert!(VmEngine::new().register_resource_type(name).is_err());
    }
}
#[test]
fn filesystem_nested_vm_does_not_inherit_ambient_service() {
    unsafe extern "C" fn nested() -> i32 {
        let mut vm = VmEngine::with_mode(VmMode::Interpreter);
        vm.load_source("import joky/fs fn run() @suspends -> Int32 { match fs.open(\"nested\", fs.OpenOptions.new()) { Ok(_) => 0, Err(e) => match e.kind { fs.ErrorKind.Unsupported => 42, _ => 1 } } }").unwrap();
        block_on(vm.prepare_async("run", &[]).unwrap()).unwrap() as i32
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_file_service(FileService::native(".", Default::default()).unwrap())
            .unwrap();
        unsafe {
            vm.register_fn(
                "nested",
                joky::FunctionSignature::new(vec![], joky::Type::Int32),
                nested as *const u8,
            )
            .unwrap();
        }
        vm.load_source("extern fn nested() -> Int32 fn run() @suspends -> Int32 { nested() }")
            .unwrap();
        assert_eq!(block_on(vm.prepare_async("run", &[]).unwrap()).unwrap(), 42);
    }
}

#[test]
#[cfg(unix)]
fn filesystem_example_runs_on_all_backends() {
    use std::process::Command;
    let work = scratch::ScratchDir::new("fs-example");
    std::fs::write(
        work.path().join("main.jk"),
        include_str!("../examples/files.jk"),
    )
    .unwrap();
    for interpreter in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_joky"));
        command.current_dir(work.path()).args(["run", "main.jk"]);
        if interpreter {
            command.arg("--interpreter");
        }
        let out = command.output().unwrap();
        assert!(out.status.success(), "{out:?}");
        assert_eq!(out.stdout, "Hello, Joky 文件！\n\n".as_bytes());
    }
    let out = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(work.path())
        .args(["build", "main.jk"])
        .env("JOKY_RUNTIME", runtime::runtime_archive())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let out = Command::new(work.path().join("main"))
        .current_dir(work.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(out.stdout, "Hello, Joky 文件！\n\n".as_bytes());
}
#[test]
fn filesystem_shared_service_enforces_quota_across_vms() {
    use joky::runtime::executor::{HostExecutor, TaskGroup};
    let source = r#"import joky/fs import joky/task
        fn hold() @suspends -> Int32 {
            var options=fs.OpenOptions.new(); options.write=true; options.create=true
            let file=match fs.open("held",options) {Ok(file)=>file,Err(e)=>{return 1}}
            task.sleep_ms(60000)
            let ignored=file.close(); 42
        }
        fn busy() @suspends -> Int32 { match fs.open("held",fs.OpenOptions.new()) {
            Ok(file)=>{let ignored=file.close(); 0},Err(e)=>match e.kind{fs.ErrorKind.Busy=>42,_=>1}
        }}"#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let work = scratch::ScratchDir::new("fs-shared");
        let service = FileService::native(
            work.path(),
            FileLimits {
                files: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let mut first = VmEngine::with_mode(mode);
        let mut second = VmEngine::with_mode(mode);
        for vm in [&mut first, &mut second] {
            vm.set_file_service(service.clone()).unwrap();
            vm.load_source(source).unwrap();
        }
        let executor = HostExecutor::default();
        let group = TaskGroup::new();
        executor
            .spawn(&group, first.prepare_async("hold", &[]).unwrap())
            .unwrap();
        wait_until(|| {
            executor.poll(1).unwrap();
            service
                .usage()
                .is_some_and(|(files, requests, _)| files == 1 && requests == 0)
        });
        assert_eq!(
            block_on(second.prepare_async("busy", &[]).unwrap()).unwrap(),
            42
        );
        drop(first);
        executor.cancel_group(&group);
        drop(second);
        service.shutdown();
        wait_until(|| service.is_quiescent());
    }
}
#[test]
fn filesystem_continuation_configuration_requires_an_unstarted_task() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source("fn run() @suspends -> Int32 {42}").unwrap();
        let service = FileService::native(".", Default::default()).unwrap();
        let mut task = vm
            .prepare_async("run", &[])
            .unwrap()
            .with_file_service(service.clone())
            .unwrap();
        task.resume().unwrap();
        assert!(task.with_file_service(service.clone()).is_err());
        drop(vm);
        service.shutdown();
        wait_until(|| service.is_quiescent());
    }
}
