//! Embedded VM, Script bridges and hot reload.
use super::*;

#[test]
#[cfg(unix)]
fn float_hosts_and_vm_float_calls_match_all_backends() {
    let work = Workspace::new();
    // Exact binary fractions keep Rust's shortest spelling and C's %.17g
    // identical across the JIT, interpreter and built artifacts.
    work.write(
        "floats.jk",
        "import joky/io.{println} fn report_float64(value: Float64) { println(&value) }\nfn main() { report_float64(2.5); report_float64(-0.75) }",
    );
    work.assert_all_backends("floats.jk", &[], b"2.5\n-0.75\n", 0);
    // vm.call_float64_1 reaches an interpreter VM from every host backend.
    work.write(
        "vmfloat.jk",
        r#"
        import joky/vm as host
        fn main() -> Int32 {
            let h = host.Vm.interpreter()
            { let vm_text = "fn twice(v: Float64) -> Float64 {{ v * 2.0 }}"; host.load_source(h, &vm_text) }
            let result = { let vm_text = "twice"; host.call_float64_1(h, &vm_text, 2.5) }
            host.close(h)
            if result != 5.0 { 1 } else { 0 }
        }
    "#,
    );
    work.assert_all_backends("vmfloat.jk", &[], b"", 0);
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn vm_standard_library_runs_in_jit_and_standalone_executable() {
    let work = Workspace::new();
    work.write("vm.jk", include_str!("../../examples/vm.jk"));
    let jit = work.cli(&["run", "vm.jk"]);
    success(&jit);
    assert_eq!(jit.stdout, b"42\n100\n");
    let built = work
        .command()
        .args(["build", "vm.jk"])
        .env("JOKY_RUNTIME", runtime_archive())
        .output()
        .unwrap();
    success(&built);
    std::fs::remove_file(work.path().join("vm.jk")).unwrap();
    let native = Command::new(work.path().join("vm"))
        .env("PATH", "")
        .env_remove("JOKY_RUNTIME")
        .output()
        .unwrap();
    success(&native);
    assert_eq!(native.stdout, jit.stdout);
    assert!(native.stderr.is_empty());
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn standard_library_reports_failures_and_keeps_the_previous_module() {
    let work = Workspace::new();
    work.write("errors.jk", r#"
        import joky/vm
        import joky/io.{println} fn report_int32(value: Int32) { println(&value) }
        fn source() -> String { "fn value() -> Int32 {{ 42 }}" }
        fn identity(text: String) -> String { text }
        fn main() -> Int32 {
            let engine = vm.Vm.new()
            report_int32({ let vm_text = identity(source()); vm.load_source(engine, &vm_text) })
            report_int32({ let vm_text = "value"; vm.call_int32_0(engine, &vm_text) })
            report_int32({ let vm_text = "fn value() -> Bool {{ 42 }}"; vm.reload_source(engine, &vm_text) })
            report_int32(vm.error_code())
            vm.print_error()
            report_int32({ let vm_text = "value"; vm.call_int32_0(engine, &vm_text) })
            report_int32({ let vm_text = "missing"; vm.call_int32_0(engine, &vm_text) })
            report_int32(vm.error_code())
            vm.print_error()
            report_int32({ let vm_text = "import joky/vm fn nested() -> Int32 {{ let child = vm.Vm.new(); vm.close(child) }}"; vm.load_source(engine, &vm_text) })
            report_int32({ let vm_text = "nested"; vm.call_int32_0(engine, &vm_text) })
            report_int32(vm.close(engine))
            vm.error_code()
        }
    "#);
    let jit = work.cli(&["run", "errors.jk"]);
    assert_eq!(jit.status.code(), Some(0));
    assert_eq!(jit.stdout, b"0\n42\n1\n1\n42\n0\n1\n0\n0\n0\n");
    let stderr = String::from_utf8_lossy(&jit.stderr);
    assert!(
        stderr.contains("Return type mismatch") && stderr.contains("not found"),
        "{stderr}"
    );
    success(
        &work
            .command()
            .args(["build", "errors.jk"])
            .env("JOKY_RUNTIME", runtime_archive())
            .output()
            .unwrap(),
    );
    let native = Command::new(work.path().join("errors")).output().unwrap();
    assert_eq!(native.status.code(), jit.status.code());
    assert_eq!(native.stdout, jit.stdout);
    assert_eq!(native.stderr, jit.stderr);
}

#[test]
fn imports_and_standard_library_calls_are_checked_before_execution() {
    let work = Workspace::new();
    for (source, message) in [
        (
            "import joky/missing fn main() {}",
            "Unknown built-in module",
        ),
        (
            "import joky/vm import joky/vm fn main() {}",
            "Duplicate import",
        ),
        ("fn main() { let h = vm.Vm.new() }", "Undeclared function"),
        (
            "import joky/vm fn main() { vm.unknown() }",
            "Undeclared function",
        ),
        (
            "import joky/vm fn main() { let h = vm.Vm.new(); vm.load_source(h, 42) }",
            "Argument type mismatch",
        ),
        (
            "import joky/vm fn main() { vm.close(1) }",
            "Argument type mismatch",
        ),
        (
            "import joky/vm fn main() { vm.call_int32_0() }",
            "Argument count mismatch",
        ),
        (
            "import joky/vm fn main() { let h = vm.Vm.new(); { let vm_text = \"missing\"; vm.register_int32_0(h, &vm_text, missing) } }",
            "not defined in this module",
        ),
        (
            "import joky/vm fn host(value: String) -> Int32 { 1 } fn main() { let h = vm.Vm.new(); { let vm_text = \"host\"; vm.register_int32_1(h, &vm_text, host) } }",
            "must be a synchronous Int32 function",
        ),
        (
            "fn joky_vm_create() {} fn main() {}",
            "Reserved runtime function",
        ),
    ] {
        work.write("bad.jk", source);
        for command in ["run", "build"] {
            failure(&work.cli(&[command, "bad.jk"]), message);
        }
    }
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn host_executor_drives_standalone_aot_with_the_same_runtime() {
    let work = Workspace::new();
    let ast = joky::parser::parse(include_str!("../fixtures/host_executor.jk")).unwrap();
    let mut compiler = joky::aot::AotCompiler::new("host_executor").unwrap();
    compiler.compile_module(&ast).unwrap();
    work.write(
        "driver.rs",
        &format!(
            "const WORK_SLOTS: usize = {};\nconst BRANCH_SLOTS: usize = {};\n{}",
            compiler.cps_frame_slots("work").unwrap(),
            compiler.cps_frame_slots("branches").unwrap(),
            include_str!("../fixtures/host_executor.rs")
        ),
    );
    compiler.write_object(&work.path().join("tasks.o")).unwrap();
    // Link generated AOT code into a Rust embedding host, with exactly one copy
    // of the runtime (including its clock/waker TLS), just like production.
    let library = runtime_archive().with_extension("rlib");
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
            work.path().join("tasks.o").display()
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
    success(&Command::new(work.path().join("driver")).output().unwrap());
}

#[test]
#[cfg(unix)]
fn text_state_hosts_execute_in_standalone_aot() {
    let work = Workspace::new();
    let ast = joky::parser::parse(include_str!("../fixtures/text_state_hosts.jk")).unwrap();
    let mut compiler = joky::aot::AotCompiler::new("text_state_hosts").unwrap();
    compiler.compile_module(&ast).unwrap();
    work.write(
        "driver.rs",
        &format!(
            "const ASYNC_SLOTS: usize = {};\n{}",
            compiler.cps_frame_slots("check_text_hosts_async").unwrap(),
            include_str!("../fixtures/text_state_hosts.rs")
        ),
    );
    compiler.write_object(&work.path().join("text.o")).unwrap();
    let library = runtime_archive().with_extension("rlib");
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
        .arg(format!("link-arg={}", work.path().join("text.o").display()))
        .arg("-o")
        .arg(work.path().join("driver"))
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    success(&Command::new(work.path().join("driver")).output().unwrap());
}

#[test]
#[cfg(unix)]
fn scalar_wait_requests_execute_in_standalone_aot() {
    let work = Workspace::new();
    let ast = joky::parser::parse(include_str!("../fixtures/scalar_wait_requests.jk")).unwrap();
    let mut compiler = joky::aot::AotCompiler::new("scalar_wait_requests").unwrap();
    compiler.compile_module(&ast).unwrap();
    work.write(
        "driver.rs",
        &format!(
            "const WAIT_SLOTS: usize = {};\n{}",
            compiler.cps_frame_slots("wait_loop").unwrap(),
            include_str!("../fixtures/scalar_wait_requests.rs")
        ),
    );
    compiler.write_object(&work.path().join("waits.o")).unwrap();
    let library = runtime_archive().with_extension("rlib");
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
            work.path().join("waits.o").display()
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
    success(&Command::new(work.path().join("driver")).output().unwrap());
}

#[test]
#[cfg(unix)]
fn checked_runtime_bindings_compose_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "bindings.jk",
        r#"
        import joky/vm
        import joky/domain
        import joky/dns
        import joky/tcp.{type Address, type TcpListener}
        import joky/check.{assert}
        import joky/io.{println}
        struct State { value: Int32 }
        fn make(value: Int32) -> State { State { value: value } }
        fn read(state: &State) -> Int32 { state.value }
        fn main() @suspends -> Int32 {
            let message = "Checked runtime contracts compose"
            let child = vm.Vm.interpreter()
            let source = "fn next(value: Int32) @suspends -> Int32 {{ value + 1 }}"
            assert(vm.load_source(child, &source) == 0, &message)
            let first = child.call::<Int32, Int32>("next", 40)
            let second = child.call::<Int32, Int32>("next", first)
            let state = domain.create(make, second)
            assert(domain.read_from(state.read_handle(), read).unwrap() == 42, &message)
            var listener = match TcpListener.bind(Address.ipv4(0x7f000001, 0)) {
                Ok(listener) => listener, Err(_) => { return 1 }
            }
            let address = match listener.local_addr() {
                Ok(address) => address, Err(_) => { return 2 }
            }
            let addresses = match dns.lookup("127.0.0.1", address.port()) {
                Ok(addresses) => addresses, Err(_) => { return 3 }
            }
            assert(addresses.len() == 1, &message)
            assert(addresses.get(0).unwrap_or(Address.ipv4(0, 0)).port() == address.port(), &message)
            assert(listener.close().is_ok(), &message)
            state.close()
            assert(child.close() == 0, &message)
            println(&second)
            0
        }
        "#,
    );
    work.assert_all_backends("bindings.jk", &[], b"42\n", 0);
}

#[test]
#[cfg(unix)]
fn script_scalar_bridge_runs_in_all_host_and_script_backends() {
    for create in ["vm.Vm.interpreter()", "vm.Vm.new()"] {
        let work = Workspace::new();
        work.write(
            "script.jk",
            &include_str!("../fixtures/aot_script_scalar.jk")
                .replace("vm.Vm.interpreter()", create),
        );
        work.assert_all_backends("script.jk", &[], b"AOT Script scalar probe ok\n", 0);
        std::fs::remove_file(work.path().join("script.jk")).unwrap();
        let standalone = Command::new(work.path().join("script"))
            .env("PATH", "")
            .env_remove("JOKY_RUNTIME")
            .output()
            .unwrap();
        success(&standalone);
        assert_eq!(standalone.stdout, b"AOT Script scalar probe ok\n");
        assert!(standalone.stderr.is_empty());
    }
}

#[test]
#[cfg(unix)]
fn script_flat_bridge_runs_in_all_host_and_script_backends() {
    let work = Workspace::new();
    for source in [
        include_str!("../fixtures/aot_script_flat.jk").to_owned(),
        include_str!("../fixtures/aot_script_flat.jk")
            .replace("vm.Vm.interpreter()", "vm.Vm.new()"),
    ] {
        work.write("flat.jk", &source);
        work.assert_all_backends("flat.jk", &[], b"Script flat contracts ok\n", 0);
        std::fs::remove_file(work.path().join("flat.jk")).unwrap();
        let standalone = Command::new(work.path().join("flat"))
            .env("PATH", "")
            .env_remove("JOKY_RUNTIME")
            .output()
            .unwrap();
        success(&standalone);
        assert_eq!(standalone.stdout, b"Script flat contracts ok\n");
        assert!(standalone.stderr.is_empty());
    }
}

#[test]
#[cfg(unix)]
fn migration_script_staging_and_atomic_publication_run_in_all_backends() {
    assert_script_migration(
        include_str!("../fixtures/migration_old.jk"),
        include_str!("../fixtures/migration_new.jk"),
    );
}

#[test]
#[cfg(unix)]
fn migration_owned_graph_runs_in_all_host_and_script_backends() {
    assert_script_migration(
        include_str!("../fixtures/migration_owned_old.jk"),
        include_str!("../fixtures/migration_owned_new.jk"),
    );
}

#[test]
fn script_incremental_migration_yields_conflicts_cancels_and_publishes() {
    let source = include_str!("../fixtures/migration_incremental_old.jk")
        .replace('{', "{{")
        .replace('}', "}}");
    let next = include_str!("../fixtures/migration_incremental_new.jk")
        .replace('{', "{{")
        .replace('}', "}}");
    for create in ["new", "interpreter"] {
        let work = Workspace::new();
        work.write(
            "incremental.jk",
            &format!(
                r#"import joky/domain import joky/vm import joky/check import joky/task
fn assert(ok:Bool) {{let message="incremental migration"; check.assert(ok,&message)}}
fn control(h:&vm.Vm,directory:domain.DirectoryHandle) @suspends {{
    while h.call::<domain.DirectoryHandle,Int32>("probe",directory)==-1 {{task.yield_now()}}
    let schema=vm.domain_schema(h)
    let next={next:?}; let start="start"; let advance="advance"; let version="version"
    assert(vm.begin_migration(h,0,&next,&start,&advance)==1)
    assert(vm.begin_migration(h,schema,&next,&start,&advance)==0)
    assert(vm.begin_migration(h,schema,&next,&start,&advance)==1)
    assert(vm.step_migration(h,2)==1)
    assert(vm.error_code()==0)
    assert(vm.publish_migration(h)==1)
    assert(vm.step_migration(h,2)==-1)
    assert(vm.begin_migration(h,schema,&next,&start,&advance)==0)
    assert(vm.step_migration(h,2)==1)
    task.yield_now()
    assert(vm.cancel_migration(h)==0)
    assert(vm.step_migration(h,2)==-1)
    assert(vm.begin_migration(h,schema,&next,&start,&advance)==0)
    assert(vm.step_migration(h,0)==-1)
    assert(vm.publish_migration(h)==1)
    assert(vm.begin_migration(h,schema,&next,&start,&advance)==0)
    assert(vm.step_migration(h,2)==1)
    assert(h.call::<domain.DirectoryHandle,Int32>("bump",directory)==45)
    assert(vm.step_migration(h,2)==-1)
    assert(vm.publish_migration(h)==1)
    assert(vm.begin_migration(h,schema,&next,&start,&advance)==0)
    var batch:Int32=0
    while batch<6 {{
        let status=vm.step_migration(h,2)
        assert(status==(if batch==5 {{0}} else {{1}}))
        assert(vm.error_code()==0)
        task.yield_now()
        assert(vm.call_int32_0(h,&version)==1)
        assert(vm.domain_schema(h)==schema)
        assert(h.call::<domain.DirectoryHandle,Int32>("probe",directory)==45)
        batch=batch+1
    }}
    assert(vm.step_migration(h,2)==0)
    assert(vm.publish_migration(h)==0)
    assert(vm.domain_schema(h)!=schema)
    assert(vm.call_int32_0(h,&version)==2)
    assert(h.call::<domain.DirectoryHandle,Int32>("probe",directory)==46)
}}
fn main() @suspends {{
    let owner=domain.directory(1); let directory=owner.handle()
    let h=vm.Vm.{create}(); let source={source:?}
    assert(vm.load_source(h,&source)==0)
    race {{ h.call::<domain.DirectoryHandle,Unit>("hold",directory), control(&h,directory) }};
    assert(h.call::<domain.DirectoryHandle,Int32>("probe",directory)==-1)
    assert(vm.close(h)==0)
}}
"#
            ),
        );
        work.assert_all_backends("incremental.jk", &[], b"", 0);
    }
}

#[test]
fn script_migration_batch_is_atomic_across_types_and_read_dependencies() {
    let old = include_str!("../fixtures/migration_batch_old.jk")
        .replace('{', "{{")
        .replace('}', "}}");
    let next = include_str!("../fixtures/migration_batch_new.jk")
        .replace('{', "{{")
        .replace('}', "}}");
    for create in ["new", "interpreter"] {
        let work = Workspace::new();
        work.write("batch.jk",&format!(r#"import joky/domain import joky/vm import joky/task import joky/check
fn assert(ok:Bool) {{let message="migration batch"; check.assert(ok,&message)}}
fn control(h:&vm.Vm,directory:domain.DirectoryHandle) @suspends {{
    while h.call::<domain.DirectoryHandle,Int32>("probe",directory)==-1 {{task.yield_now()}}
    let next={next:?}; let start="start"; let advance="advance"; let ledger="upgrade_ledger"; let empty=""; let version="version"
    assert(vm.domain_schema(h)==0)
    assert(vm.begin_migration_batch(h,&next)==0)
    assert(vm.begin_migration_batch(h,&next)==1)
    assert(vm.stage_migration(h,&start,&advance)==0)
    assert(vm.publish_migration(h)==1)
    assert(vm.step_migration(h,2)==-1)
    assert(vm.begin_migration_batch(h,&next)==0)
    assert(vm.stage_migration(h,&start,&advance)==0)
    assert(vm.stage_migration(h,&start,&advance)==1)
    assert(vm.publish_migration(h)==1)
    assert(vm.begin_migration_batch(h,&next)==0)
    assert(vm.stage_migration(h,&start,&advance)==0)
    assert(vm.stage_migration(h,&ledger,&empty)==0)
    assert(vm.step_migration(h,2)==1)
    task.yield_now()
    assert(vm.cancel_migration(h)==0)
    assert(h.call::<domain.DirectoryHandle,Int32>("probe",directory)==191)
    assert(vm.begin_migration_batch(h,&next)==0)
    assert(vm.stage_migration(h,&start,&advance)==0)
    assert(vm.stage_migration(h,&ledger,&empty)==0)
    assert(vm.step_migration(h,2)==1)
    assert(h.call::<domain.DirectoryHandle,Int32>("bump",directory)==8)
    assert(vm.step_migration(h,2)==-1)
    assert(vm.publish_migration(h)==1)
    assert(h.call::<domain.DirectoryHandle,Int32>("probe",directory)==192)
    assert(vm.begin_migration_batch(h,&next)==0)
    assert(vm.stage_migration(h,&start,&advance)==0)
    assert(vm.stage_migration(h,&ledger,&empty)==0)
    var batch:Int32=0
    while batch<12 {{
        assert(vm.step_migration(h,2)==(if batch==11 {{0}} else {{1}}))
        assert(vm.error_code()==0)
        task.yield_now()
        assert(vm.call_int32_0(h,&version)==1)
        assert(h.call::<domain.DirectoryHandle,Int32>("probe",directory)==192)
        batch=batch+1
    }}
    assert(vm.publish_migration(h)==0)
    assert(vm.call_int32_0(h,&version)==2)
    assert(h.call::<domain.DirectoryHandle,Int32>("probe",directory)==195)
}}
fn main() @suspends {{
    let owner=domain.directory(4); let directory=owner.handle()
    let h=vm.Vm.{create}(); let old={old:?}
    assert(vm.load_source(h,&old)==0)
    race {{ h.call::<domain.DirectoryHandle,Unit>("hold",directory), control(&h,directory) }};
    assert(h.call::<domain.DirectoryHandle,Int32>("probe",directory)==-1)
    assert(vm.close(h)==0)
}}
"#));
        work.assert_all_backends("batch.jk", &[], b"", 0);
    }
}

#[test]
#[cfg(unix)]
fn server_reload_keeps_tcp_tick_and_atomic_replies_in_all_backends() {
    for create in ["new", "interpreter"] {
        let work = Workspace::new();
        work.write("server.jk", &server_reload::source(create));
        work.assert_all_backends("server.jk", &[], b"", 0);
    }
}

#[test]
#[cfg(unix)]
fn script_nested_bridge_preserves_lanes_and_rejects_changed_inner_contracts() {
    let definitions = "struct Empty {} struct Bits { high:UInt64, low:Int8, flag:Bool, real:Float64 } struct Packet { bits:Bits, empty:Empty, unit:Unit }";
    let target = format!(
        "import joky/task {definitions} fn run(p:Packet) @suspends ->Packet {{task.yield_now(); p}}"
    );
    let input = "Packet{bits:Bits{high:18446744073709551615,low:-128,flag:true,real:Float64.from_bits(9221120237041090626)},empty:Empty{},unit:{}}";
    for create in ["new", "interpreter"] {
        let work = Workspace::new();
        let quote = |s: &str| format!("{:?}", s.replace('{', "{{").replace('}', "}}"));
        let source = format!(
            r#"import joky/vm import joky/check
{definitions}
fn main() @suspends {{
    let h=vm.Vm.{create}(); let source={source}
    let message="nested Script lanes"
    check.assert(vm.load_source(h,&source)==0,&message)
    let reply=h.call::<Packet,Packet>("run",{input})
    check.assert(reply.bits.high==18446744073709551615 && reply.bits.low == -128 && reply.bits.flag && reply.bits.real.to_bits()==9221120237041090626,&message)
    vm.close(h);
}}"#,
            source = quote(&target)
        );
        work.write("nested.jk", &source);
        work.assert_all_backends("nested.jk", &[], b"", 0);
        for changed in [
            target.replace("low:Int8", "low:UInt8"),
            target.replace("Bits", "Other"),
            target.replace("high:UInt64, low:Int8", "low:Int8, high:UInt64"),
        ] {
            work.write(
                "mismatch.jk",
                &source.replace(&quote(&target), &quote(&changed)),
            );
            for args in [
                vec!["run", "mismatch.jk"],
                vec!["run", "--interpreter", "mismatch.jk"],
            ] {
                failure(&work.cli(&args), "Script entry contract mismatch");
            }
            success(&work.cli(&["build", "mismatch.jk"]));
            failure(
                &Command::new(work.path().join("mismatch")).output().unwrap(),
                "Script entry contract mismatch",
            );
        }
        let result_target = format!("{} fn result(u:Unit) @suspends ->Packet {{ Packet{{bits:Bits{{high:1,low:0,flag:true,real:0.5}},empty:Empty{{}},unit:{{}}}} }}", definitions.replace("low:Int8", "low:UInt8"));
        let result_source = source
            .replace(&quote(&target), &quote(&result_target))
            .replace(
                &format!("h.call::<Packet,Packet>(\"run\",{input})"),
                "h.call::<Unit,Packet>(\"result\",{})",
            );
        work.write("result.jk", &result_source);
        for args in [
            vec!["run", "result.jk"],
            vec!["run", "--interpreter", "result.jk"],
        ] {
            failure(&work.cli(&args), "Script entry contract mismatch");
        }
        success(&work.cli(&["build", "result.jk"]));
        failure(
            &Command::new(work.path().join("result")).output().unwrap(),
            "Script entry contract mismatch",
        );
    }
}

#[test]
#[cfg(unix)]
fn pure_enum_script_values_run_in_all_backend_combinations() {
    for create in ["new", "interpreter"] {
        let work = Workspace::new();
        work.write("enums.jk", &enum_values::source(create));
        work.assert_all_backends("enums.jk", &[], b"", 42);
    }
}

#[test]
#[cfg(unix)]
fn pure_enum_pending_script_results_keep_the_starting_contract() {
    assert_pending_script_result_contract(
        "enum Reply{Empty,Done(Int32)}",
        "enum Reply{Done(Int32),Empty}",
        "Reply",
        "Reply.Done(42)",
        "match result {Done(n)=>assert(n==42),_=>assert(false)}",
    );
}

#[test]
#[cfg(unix)]
fn pure_composite_pending_script_results_keep_the_starting_contract() {
    assert_pending_script_result_contract(
        "struct Reply<T>{value:T,flag:Bool}",
        "struct Reply<T>{flag:Bool,value:T}",
        "(Reply<(Int32,Bool)>,Unit)",
        "(Reply{value:(42,true),flag:false},{})",
        "assert(result.0.value.0==42 && result.0.value.1 && !result.0.flag)",
    );
}

#[test]
#[cfg(unix)]
fn pure_composite_script_values_run_in_all_backend_combinations() {
    for create in ["new", "interpreter"] {
        let work = Workspace::new();
        work.write("composites.jk", &composite_values::source(create));
        work.assert_all_backends("composites.jk", &[], b"", 42);
    }
}

#[test]
#[cfg(unix)]
fn owned_script_values_run_in_all_backend_combinations() {
    for create in ["new", "interpreter"] {
        let work = Workspace::new();
        work.write("owned.jk", &owned_values::source(create));
        work.assert_all_backends("owned.jk", &[], b"", 42);
    }
}

#[test]
#[cfg(unix)]
fn owned_script_pending_results_keep_the_starting_contract() {
    assert_pending_script_result_contract(
        "enum Reply{Empty,Done(String)}",
        "enum Reply{Done(String),Empty}",
        "Reply",
        "Reply.Done(\"old text\")",
        "match result {Done(text)=>assert(text == \"old text\"),_=>assert(false)}",
    );
}

#[test]
#[cfg(unix)]
fn owned_script_rejects_mismatched_contracts() {
    let imports = "import joky/array.{type Array} import joky/option.{type Option} ";
    for create in ["new", "interpreter"] {
        for (types, script, input, output, value) in [
            ("", "fn exchange(v:Array<String>) @suspends ->String {let bad=1/0; \"\"}", "String", "String", "\"input\""),
            ("", "fn exchange(v:String) @suspends ->Option<String> {let bad=1/0; Option.None}", "String", "String", "\"input\""),
            ("struct Item{text:String}", "struct Item{renamed:String} fn exchange(v:Array<Item>) @suspends ->Unit {let bad=1/0}", "Array<Item>", "Unit", "Array.new::<Item>()"),
        ] {
            let script=format!("{imports} {script}");
            let quoted=format!("{:?}",script.replace('{',"{{").replace('}',"}}"));
            let work=Workspace::new();
            work.write("reject.jk",&format!("import joky/vm {imports} {types} fn main() @suspends {{let h=vm.Vm.{create}(); h.load_source({quoted})!; h.call::<{input},{output}>(\"exchange\",{value});}}"));
            for args in [vec!["run","reject.jk"],vec!["run","--interpreter","reject.jk"]] {
                let output=work.cli(&args);
                assert!(!output.status.success());
                let error=String::from_utf8_lossy(&output.stderr);
                assert!(error.contains("Script entry contract mismatch") || error.contains("Function `exchange` not found"),"{error}");
            }
            success(&work.cli(&["build","reject.jk"]));
            let output=Command::new(work.path().join("reject")).output().unwrap();
            assert!(!output.status.success());
            let error=String::from_utf8_lossy(&output.stderr);
            assert!(error.contains("Script entry contract mismatch") || error.contains("Function `exchange` not found"),"{error}");
        }
    }
}

#[cfg(unix)]
fn assert_script_migration(old: &str, next: &str) {
    let source = old.replace('{', "{{").replace('}', "}}");
    let next = next.replace('{', "{{").replace('}', "}}");
    for create in ["new", "interpreter"] {
        let work = Workspace::new();
        work.write(
            "migrate.jk",
            &format!(
                r#"import joky/domain import joky/vm import joky/check import joky/task
fn assert(ok:Bool) {{let message="Script migration"; check.assert(ok,&message)}}
fn control(h:&vm.Vm,directory:domain.DirectoryHandle) @suspends {{
    var attempts:Int32=0
    while h.call::<domain.DirectoryHandle,Int32>("probe",directory)==-1 {{
        attempts=attempts+1; assert(attempts<1000); task.yield_now()
    }}
    let schema=vm.domain_schema(h)
    assert(schema!=0)
    let next={next:?}
    let method="upgrade"
    let version="version"
    assert(vm.prepare_migration(h,0,&next,&method)==1)
    assert(vm.prepare_migration(h,schema,&next,&method)==0)
    assert(vm.prepare_migration(h,schema,&next,&method)==1)
    assert(vm.call_int32_0(h,&version)==1)
    assert(h.call::<domain.DirectoryHandle,Int32>("probe",directory)==42)
    assert(vm.cancel_migration(h)==0)
    assert(vm.publish_migration(h)==1)
    assert(vm.prepare_migration(h,schema,&next,&method)==0)
    assert(h.call::<domain.DirectoryHandle,Int32>("bump",directory)==45)
    assert(vm.publish_migration(h)==1)
    assert(vm.domain_schema(h)==schema)
    assert(vm.call_int32_0(h,&version)==1)
    assert(h.call::<domain.DirectoryHandle,Int32>("probe",directory)==45)
    assert(vm.prepare_migration(h,schema,&next,&method)==0)
    task.yield_now()
    assert(vm.publish_migration(h)==0)
    assert(vm.domain_schema(h)!=schema)
    assert(vm.call_int32_0(h,&version)==2)
    assert(h.call::<domain.DirectoryHandle,Int32>("probe",directory)==46)
}}
fn main() @suspends {{
    let owner=domain.directory(1)
    let directory=owner.handle()
    let h=vm.Vm.{create}()
    let source={source:?}
    assert(vm.load_source(h,&source)==0)
    race {{ h.call::<domain.DirectoryHandle,Unit>("hold",directory), control(&h,directory) }};
    assert(h.call::<domain.DirectoryHandle,Int32>("probe",directory)==-1)
    assert(vm.close(h)==0)
}}
"#
            ),
        );
        work.assert_all_backends("migrate.jk", &[], b"", 0);
    }
}

#[cfg(unix)]
#[path = "../support/server_reload.rs"]
mod server_reload;

#[cfg(unix)]
#[path = "../support/enum_values.rs"]
mod enum_values;

#[cfg(unix)]
fn assert_pending_script_result_contract(
    types: &str,
    next_types: &str,
    result_type: &str,
    value: &str,
    assertion: &str,
) {
    let old = format!(
        r#"import joky/domain import joky/task
struct State{{n:Int32}}
{types}
fn make(n:Int32)->State{{State{{n:n}}}}
fn pending(directory:domain.DirectoryHandle) @suspends ->{result_type} {{
    let owner=domain.create(make,0)
    directory.insert(1,owner.handle());
    while domain.lookup::<State>(directory,1).is_some() {{task.yield_now()}}
    {value}
}}"#
    );
    let next = old.replace(types, next_types);
    let quote = |s: &str| format!("{:?}", s.replace('{', "{{").replace('}', "}}"));
    for create in ["new", "interpreter"] {
        let work = Workspace::new();
        work.write("pending.jk",&format!(r#"import joky/vm import joky/domain import joky/task import joky/check
struct State{{n:Int32}}
{types}
fn assert(ok:Bool){{check.assert(ok,"old Script result")}}
fn call(h:&vm.Vm,d:domain.DirectoryHandle) @suspends {{
    let result=h.call::<domain.DirectoryHandle,{result_type}>("pending",d)
    {assertion}
}}
fn reload(h:&vm.Vm,d:domain.DirectoryHandle) @suspends {{
    while domain.lookup::<State>(d,1).is_none() {{task.yield_now()}}
    let next={next}
    assert(vm.load_source(h,&next)==0)
    assert(vm.module_count(h)==2)
    match domain.lookup::<State>(d,1) {{Some(handle)=>assert(d.remove(1,handle)),None=>assert(false)}}
}}
fn main() @suspends {{
    let h=vm.Vm.{create}(); let old={old}
    assert(vm.load_source(h,&old)==0)
    let directory=domain.directory(1); let d=directory.handle()
    join {{ call(&h,d), reload(&h,d) }}
    assert(vm.module_count(h)==1)
    assert(vm.close(h)==0)
}}
"#,old=quote(&old),next=quote(&next)));
        work.assert_all_backends("pending.jk", &[], b"", 0);
    }
}

#[cfg(unix)]
#[path = "../support/composite_values.rs"]
mod composite_values;

#[cfg(unix)]
#[path = "../support/owned_values.rs"]
mod owned_values;

#[test]
#[cfg(unix)]
fn required_script_contracts_preflight_all_loads_on_all_backends() {
    let work = Workspace::new();
    let source = include_str!("../fixtures/script_required_contracts.jk");
    for create in ["new", "interpreter"] {
        let source = source.replace("vm.Vm.new()", &format!("vm.Vm.{create}()"));
        work.write("required.jk", &source);
        success(&work.cli(&["run", "required.jk"]));
        success(&work.cli(&["run", "--interpreter", "required.jk"]));
        work.assert_native("required.jk", &[], b"", 0);
    }
}

#[test]
#[cfg(unix)]
fn script_try_call_isolates_child_failures_on_all_backends() {
    let work = Workspace::new();
    let source = include_str!("../fixtures/script_try_call.jk");
    for create in ["new", "interpreter"] {
        work.write(
            "try_call.jk",
            &source.replace("vm.Vm.new()", &format!("vm.Vm.{create}()")),
        );
        work.assert_all_backends("try_call.jk", &[], b"", 0);
    }
}
