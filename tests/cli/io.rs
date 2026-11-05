//! Host resources, network protocols and native interoperability.
use super::*;

#[test]
#[cfg(unix)]
fn standard_error_and_flush_keep_output_streams_separate() {
    let work = Workspace::new();
    work.write(
        "streams.jk",
        r#"
        import joky/io
        fn report(text: &String) @suspends { eprint(text) }
        fn main() @suspends {
            print("data")
            eprintln("error")
            let message = "prompt> "
            scope { branch { report(&message) } }
            io.flush_stdout()
            io.flush_stderr()
        }
    "#,
    );
    success(&work.cli(&["build", "streams.jk", "--parallel"]));
    for backend in ["jit", "interpreter", "aot"] {
        let output = if backend == "aot" {
            Command::new(work.path().join("streams")).output().unwrap()
        } else {
            let mut command = work.command();
            command.args(["run", "streams.jk", "--parallel"]);
            if backend == "interpreter" {
                command.arg("--interpreter");
            }
            command.output().unwrap()
        };
        success(&output);
        assert_eq!(output.stdout, b"data", "{backend}");
        assert_eq!(output.stderr, b"error\nprompt> ", "{backend}");
    }
}

#[test]
#[cfg(unix)]
fn stdout_without_newline_is_preserved_on_every_entry_route() {
    let work = Workspace::new();
    for (annotation, flags) in [
        ("", vec![]),
        (" @suspends", vec![]),
        (" @suspends", vec!["--parallel"]),
    ] {
        work.write(
            "prompt.jk",
            &format!("fn main(){annotation} -> Int32 {{ print(\"prompt> \" ); 7 }}"),
        );
        work.assert_all_backends("prompt.jk", &flags, b"prompt> ", 7);
    }
}

#[test]
#[cfg(unix)]
fn owning_slices_run_and_build_with_resource_replacement() {
    let work = Workspace::new();
    work.write(
        "owning_slices.jk",
        include_str!("../../examples/owning_slices.jk"),
    );
    work.assert_all_backends("owning_slices.jk", &[], b"21\n42\n3\n", 0);
}

#[test]
#[cfg(unix)]
fn borrowed_host_io_loopback_probe_executes_in_standalone_aot() {
    let work = Workspace::new();
    let ast = joky::parser::parse(include_str!("../fixtures/borrowed_host_io.jk")).unwrap();
    let mut compiler = joky::aot::AotCompiler::new("borrowed_host_io").unwrap();
    compiler.set_prelude_enabled(false).unwrap();
    compiler.compile_module(&ast).unwrap();
    let mut driver = include_str!("../fixtures/borrowed_host_io_host.rs").to_owned();
    let entries = [
        "data",
        "eof",
        "domain_error",
        "startup_failure",
        "pending_failure",
        "timeout",
        "sibling_failure",
        "unstarted",
    ];
    driver.push_str("\nextern \"C\" {\n");
    for entry in entries {
        driver.push_str(&format!(
            "#[link_name = \"{entry}\"] fn entry_{entry}(frame: *mut CpsFrame) -> i32;\n"
        ));
    }
    driver.push_str("}\nfn main() { check_suite(|name| { let (entry, slots): (joky::runtime::cps::CpsEntry,usize) = match name {\n");
    for entry in entries {
        driver.push_str(&format!(
            "\"{entry}\" => (entry_{entry}, {}),\n",
            compiler.cps_frame_slots(entry).unwrap()
        ));
    }
    driver.push_str("_ => unreachable!() }; unsafe { Continuation::from_native(entry,CpsFrame::new(slots),Rc::new(())) } }); }\n");
    work.write("driver.rs", &driver);
    compiler
        .write_object(&work.path().join("borrowed_host_io.o"))
        .unwrap();
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
            work.path().join("borrowed_host_io.o").display()
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
fn resource_owners_execute_in_standalone_aot() {
    resource_program_in_standalone_aot(
        include_str!("../fixtures/resource_owners.jk"),
        &[
            "result",
            "pair",
            "array_task",
            "overwrite",
            "partial",
            "unstarted",
            "dynamic",
            "borrow",
        ],
    );
}

#[test]
#[cfg(unix)]
fn resource_handoff_execute_in_standalone_aot() {
    resource_program_in_standalone_aot(
        include_str!("../fixtures/resource_handoff.jk"),
        &[
            "success",
            "startup_failure",
            "output_failure",
            "invalid_pending",
            "empty_success",
            "runtime_failure",
            "race_cancel",
            "race_unstarted",
        ],
    );
}

#[test]
#[cfg(unix)]
fn resource_bytes_execute_in_standalone_aot() {
    use joky::runtime::byte_request::ByteIoKind;
    let work = Workspace::new();
    let ast = joky::parser::parse(include_str!("../fixtures/resource_bytes.jk")).unwrap();
    let mut compiler =
        joky::aot::AotCompiler::with_symbol_prefix("resource_bytes", "net1a_bytes_").unwrap();
    compiler.register_resource_type("Socket").unwrap();
    compiler
        .register_resource_factory("byte_make", "Socket")
        .unwrap();
    compiler
        .register_resource_io("read", "Socket", ByteIoKind::Read)
        .unwrap();
    compiler
        .register_resource_io("write", "Socket", ByteIoKind::Write)
        .unwrap();
    compiler.compile_module(&ast).unwrap();
    let entries = [
        "receive",
        "eof",
        "send",
        "empty_write",
        "zero_read",
        "oversized",
        "bad_count",
        "zero_progress",
        "cancel_reuse",
        "unstarted",
        "field",
        "indirect",
        "independent",
        "dynamic",
    ];
    let mut driver = include_str!("../fixtures/resource_bytes_host.rs")
        .replace(
            "__joky_resource_factory_v1_",
            "__joky_resource_factory_v1_net1a_bytes_",
        )
        .replace(
            "__joky_resource_io_v1_",
            "__joky_resource_io_v1_net1a_bytes_",
        );
    driver.push_str(
        "\nuse joky::runtime::suspension::Continuation; use std::rc::Rc;\nextern \"C\" {\n",
    );
    for entry in entries {
        driver.push_str(&format!(
            "#[link_name=\"net1a_bytes_{entry}\"] fn entry_{entry}(frame:*mut CpsFrame)->i32;\n"
        ));
    }
    driver.push_str("}\nfn main(){\n");
    for entry in entries {
        let fails = ["zero_read", "oversized", "bad_count", "zero_progress"].contains(&entry);
        driver.push_str(&format!("for cancel in [false,true] {{ reset(); let mut task=unsafe {{Continuation::from_native(entry_{entry},CpsFrame::new({}),Rc::new(()))}}; let mut failed=false; for _ in 0..50 {{if task.is_finished() {{break;}} if task.resume().is_err() {{failed=true;break;}} if cancel {{break;}} }} if !cancel {{assert_eq!(failed,{fails},\"{entry}\");if !failed {{assert_eq!(task.cps_result(),Some(42),\"{entry}\");}}}} drop(task);check(); if !cancel && \"{entry}\"==\"send\" {{assert_eq!(sent(),vec![0,255]);}} }}\n",compiler.cps_frame_slots(entry).unwrap()));
    }
    driver.push_str("}\n");
    work.write("driver.rs", &driver);
    compiler
        .write_object(&work.path().join("resource_bytes.o"))
        .unwrap();
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
            work.path().join("resource_bytes.o").display()
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
fn resource_bytes_c_header_matches_descriptor_layouts() {
    use joky::runtime::{
        byte_request::ResourceIoV1,
        resource::{ResourceFactoryCpsV1, ResourceFactoryV1},
    };
    let work = Workspace::new();
    let mut compiler = joky::aot::AotCompiler::with_symbol_prefix("headers", "net_").unwrap();
    compiler.register_resource_type("Socket").unwrap();
    compiler
        .register_resource_factory("make", "Socket")
        .unwrap();
    compiler
        .register_resource_factory_async("acquire", "Socket")
        .unwrap();
    compiler
        .register_resource_io(
            "read",
            "Socket",
            joky::runtime::byte_request::ByteIoKind::Read,
        )
        .unwrap();
    let header = compiler.resource_header_v1();
    assert!(header.contains("JokyResourceFactoryV1 __joky_resource_factory_v1_net_make"));
    assert!(header.contains("JokyResourceFactoryCpsV1 __joky_resource_factory_cps_v1_net_acquire"));
    assert!(header.contains("JokyResourceIoV1 __joky_resource_io_v1_net_read"));
    work.write("bindings.h", &header);
    work.write("layout.c",&format!("#include \"bindings.h\"\n_Static_assert(sizeof(JokyResourceFactoryV1)=={},\"factory size\");\n_Static_assert(offsetof(JokyResourceFactoryV1,release)=={},\"release offset\");\n_Static_assert(sizeof(JokyResourceFactoryCpsV1)=={},\"CPS size\");\n_Static_assert(offsetof(JokyResourceFactoryCpsV1,stop)=={},\"stop offset\");\n_Static_assert(sizeof(JokyResourceIoV1)=={},\"I/O size\");\n_Static_assert(offsetof(JokyResourceIoV1,poll)=={},\"poll offset\");\nint main(void) {{ return JOKY_BYTE_REQUEST_MAX_V1==1048576 ? 0 : 1; }}\n",std::mem::size_of::<ResourceFactoryV1>(),std::mem::offset_of!(ResourceFactoryV1,release),std::mem::size_of::<ResourceFactoryCpsV1>(),std::mem::offset_of!(ResourceFactoryCpsV1,stop),std::mem::size_of::<ResourceIoV1>(),std::mem::offset_of!(ResourceIoV1,poll)));
    let output = Command::new("cc")
        .arg("-std=c11")
        .arg("-Werror")
        .arg("-I")
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include"))
        .arg(work.path().join("layout.c"))
        .arg("-o")
        .arg(work.path().join("layout"))
        .output()
        .unwrap();
    success(&output);
    success(&Command::new(work.path().join("layout")).output().unwrap());
}

#[test]
#[cfg(unix)]
fn reactor_executes_in_standalone_aot() {
    use joky::runtime::byte_request::ByteIoKind;
    let work = Workspace::new();
    let ast = joky::parser::parse(include_str!("../fixtures/reactor_io.jk")).unwrap();
    let mut compiler =
        joky::aot::AotCompiler::with_symbol_prefix("reactor_io", "reactor_io_").unwrap();
    compiler.register_resource_type("Socket").unwrap();
    compiler
        .register_resource_factory("reactor_io_make", "Socket")
        .unwrap();
    compiler
        .register_resource_io("reactor_io_read", "Socket", ByteIoKind::Read)
        .unwrap();
    compiler.compile_module(&ast).unwrap();
    let mut driver = include_str!("../fixtures/reactor_io_host.rs")
        .replace(
            "__joky_resource_factory_v1_reactor_io_make",
            "__joky_resource_factory_v1_reactor_io_reactor_io_make",
        )
        .replace(
            "__joky_resource_io_v1_reactor_io_read",
            "__joky_resource_io_v1_reactor_io_reactor_io_read",
        );
    driver.push_str("\nextern \"C\" {\n");
    let entries = [
        "receive",
        "eof",
        "timeout",
        "sibling_failure",
        "reuse",
        "unstarted",
        "dynamic",
    ];
    for entry in entries {
        driver.push_str(&format!(
            "#[link_name=\"reactor_io_{entry}\"] fn entry_{entry}(frame:*mut CpsFrame)->i32;\n"
        ));
    }
    driver.push_str("}\nfn main(){suite(|name| { let (entry,slots)=match name {\n");
    for entry in entries {
        driver.push_str(&format!(
            "\"{entry}\" => (entry_{entry} as joky::runtime::cps::CpsEntry,{}),\n",
            compiler.cps_frame_slots(entry).unwrap()
        ));
    }
    driver.push_str("_=>unreachable!()}; unsafe {Continuation::from_native(entry,CpsFrame::new(slots),Rc::new(()))}}); blocking_suite(|name| { let (entry,slots)=match name {\n");
    for entry in entries {
        driver.push_str(&format!(
            "\"{entry}\" => (entry_{entry} as joky::runtime::cps::CpsEntry,{}),\n",
            compiler.cps_frame_slots(entry).unwrap()
        ));
    }
    driver.push_str("_=>unreachable!()}; let mut frame=CpsFrame::new(slots); joky::runtime::error::joky_runtime_error_clear(); let status=unsafe {joky::runtime::driver::joky_cps_block_on(entry,&mut frame)}; assert_eq!(status,if name==\"sibling_failure\" {3} else {0}); if status==0 {assert_eq!(joky::runtime::error::joky_runtime_error_code(),0); Ok(frame.result)} else {assert_ne!(joky::runtime::error::joky_runtime_error_code(),0); joky::runtime::error::joky_runtime_error_clear(); Err(joky::Error::new(joky::ErrorPhase::Runtime,\"Native driver failure\"))}});}\n");
    work.write("driver.rs", &driver);
    compiler
        .write_object(&work.path().join("reactor_io.o"))
        .unwrap();
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
            work.path().join("reactor_io.o").display()
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
fn tcp_public_module_runs_in_all_backends() {
    let work = Workspace::new();
    work.write("tcp.jk", include_str!("../fixtures/tcp.jk"));
    work.assert_all_backends("tcp.jk", &[], b"", 0);
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn tcp_connection_refused_matches_all_backends() {
    mod endpoint {
        include!("../support/tcp.rs");
    }
    let (_reserved, port) = endpoint::refused_endpoint();
    let work = Workspace::new();
    work.write(
        "refused.jk",
        &include_str!("../fixtures/tcp_refused.jk").replace("PORT", &port.to_string()),
    );
    work.assert_all_backends("refused.jk", &[], b"", 0);
}

#[test]
fn tcp_composition_runs_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "tcp_composition.jk",
        include_str!("../fixtures/tcp_composition.jk"),
    );
    work.assert_all_backends("tcp_composition.jk", &[], b"", 0);
}

#[test]
fn async_owner_tcp_buffers_execute_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "async_owner_tcp.jk",
        include_str!("../fixtures/async_owner_tcp.jk"),
    );
    work.assert_all_backends("async_owner_tcp.jk", &[], b"", 0);
}

#[test]
fn buffered_tcp_runs_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "buffered_tcp.jk",
        include_str!("../fixtures/buffered_tcp.jk"),
    );
    work.assert_all_backends("buffered_tcp.jk", &[], b"", 0);
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn buffered_tcp_reset_runs_in_all_backends() {
    mod endpoint {
        include!("../support/buffered_tcp.rs");
    }
    let (port, peer) = endpoint::reset_endpoint(3);
    let work = Workspace::new();
    work.write(
        "buffered_tcp_reset.jk",
        &include_str!("../fixtures/buffered_tcp_reset.jk").replace("PORT", &port.to_string()),
    );
    work.assert_all_backends("buffered_tcp_reset.jk", &[], b"", 0);
    peer.join().unwrap();
}

#[test]
fn buffered_tcp_frames_run_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "buffered_tcp_frames.jk",
        include_str!("../fixtures/buffered_tcp_frames.jk"),
    );
    work.assert_all_backends("buffered_tcp_frames.jk", &[], b"", 0);
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn buffered_tcp_frame_resets_run_in_all_backends() {
    mod endpoint {
        include!("../support/buffered_tcp.rs");
    }
    for method in ["read_exact(4)", "read_until(10, 4)", "read_line(4)"] {
        let (port, peer) = endpoint::reset_endpoint(3);
        let work = Workspace::new();
        let source = include_str!("../fixtures/buffered_tcp_reset.jk")
            .replace("PORT", &port.to_string())
            .replace(".read(4)", &format!(".{method}"));
        work.write("frame_reset.jk", &source);
        work.assert_all_backends("frame_reset.jk", &[], b"", 0);
        peer.join().unwrap();
    }
}

#[test]
fn buffered_tcp_messages_run_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "buffered_tcp_messages.jk",
        include_str!("../fixtures/buffered_tcp_messages.jk"),
    );
    // VM wire/limit tests already run this exact main with owner ledgers.
    work.assert_native("buffered_tcp_messages.jk", &[], b"", 0);
}

#[test]
#[ignore = "exhaustive: transfer maximum-size TCP messages in standalone AOT"]
fn exhaustive_buffered_tcp_maximum_messages_in_standalone_aot() {
    let work = Workspace::new();
    for fixture in [
        include_str!("../fixtures/buffered_tcp_messages.jk"),
        include_str!("../fixtures/buffered_tcp_split.jk"),
    ] {
        let source = fixture.replace("fn main()", "fn ordinary_messages()");
        work.write(
            "maximum_messages.jk",
            &format!("{source}\nfn main() @suspends -> Int32 {{ ensure(maximum() == 42); 0 }}"),
        );
        work.assert_native("maximum_messages.jk", &[], b"", 0);
    }
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn buffered_tcp_messages_reset_runs_in_all_backends() {
    mod endpoint {
        include!("../support/buffered_tcp.rs");
    }
    let (port, peer) = endpoint::reset_endpoint(3);
    let work = Workspace::new();
    let source = include_str!("../fixtures/buffered_tcp_reset.jk")
        .replace("PORT", &port.to_string())
        .replace(".read(4)", ".read_message(4)");
    work.write("message_reset.jk", &source);
    work.assert_all_backends("message_reset.jk", &[], b"", 0);
    peer.join().unwrap();
}

#[test]
fn duplex_example_runs_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "tcp_duplex.jk",
        include_str!("../../examples/tcp_duplex.jk"),
    );
    work.assert_all_backends("tcp_duplex.jk", &[], b"1\n2\n3\n", 0);
}

#[test]
fn buffered_tcp_echo_example_runs_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "tcp_messages.jk",
        include_str!("../../examples/tcp_messages.jk"),
    );
    work.assert_all_backends("tcp_messages.jk", &[], "hello\n你好\n\n".as_bytes(), 0);
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn buffered_tcp_native_peer_and_cancelled_prefix_in_standalone_aot() {
    message_native_peer(false, &[2]);
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn buffered_writer_native_peer_and_cancelled_prefix_in_standalone_aot() {
    message_native_peer(true, &[2]);
}

#[test]
fn detached_owners_execute_in_all_backends() {
    let work = Workspace::new();
    for (name, source) in [
        ("owners.jk", include_str!("../fixtures/detached_owners.jk")),
        ("tcp.jk", include_str!("../fixtures/detached_tcp_owners.jk")),
    ] {
        work.write(name, source);
        work.assert_all_backends(name, &[], b"", 0);
    }
}

#[test]
fn split_tcp_runs_in_all_backends() {
    let work = Workspace::new();
    work.write("tcp_split.jk", include_str!("../fixtures/tcp_split.jk"));
    work.assert_all_backends("tcp_split.jk", &[], b"", 0);
}

#[test]
fn buffered_split_native_execution_and_cli_resets() {
    mod matrix {
        include!("../support/buffered_tcp_split.rs");
    }
    let work = Workspace::new();
    work.write(
        "buffered_split.jk",
        &include_str!("../fixtures/buffered_tcp_split.jk")
            .replace("    ensure(maximum() == 42)\n", ""),
    );
    // buffered_split_duplex_cache_framing_and_cancel covers both VM modes.
    work.assert_native("buffered_split.jk", &[], b"", 0);
    work.write("split_frames.jk", &matrix::frames_source());
    work.assert_native("split_frames.jk", &[], b"", 0);
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        mod endpoint {
            include!("../support/buffered_tcp.rs");
        }
        for method in [
            "read(4)",
            "read_exact(4)",
            "read_line(4)",
            "read_message(4)",
        ] {
            let (port, peer) = endpoint::reset_endpoint(3);
            work.write("split_reset.jk", &matrix::reset_source(port, method));
            work.assert_all_backends("split_reset.jk", &[], b"", 0);
            peer.join().unwrap();
        }
    }
}

#[test]
#[cfg(unix)]
fn dns_public_module_runs_in_all_backends() {
    let work = Workspace::new();
    work.write("dns.jk", include_str!("../fixtures/dns.jk"));
    work.assert_all_backends("dns.jk", &[], b"", 0);
    work.write(
        "dns_example.jk",
        include_str!("../../examples/dns_lookup.jk"),
    );
    work.assert_all_backends("dns_example.jk", &[], b"localhost resolved\n", 0);
}

#[test]
#[cfg(unix)]
fn tcp_host_endpoints_native_local_and_parallel() {
    let work = Workspace::new();
    work.write("endpoints.jk", include_str!("../fixtures/tcp_endpoints.jk"));
    // VM behavior lives in runtime DNS tests; this completes standalone coverage.
    work.assert_native("endpoints.jk", &[], b"", 0);
    work.assert_native("endpoints.jk", &["--task-workers", "2"], b"", 0);
}

#[test]
#[ignore = "exhaustive: buffered TCP native-peer wire parity through both CLI VM modes"]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn exhaustive_buffered_tcp_native_peer_and_cancelled_prefix_cli_vm_modes() {
    message_native_peer(false, &[0, 1]);
}

#[test]
#[ignore = "exhaustive: split TCP writer native-peer wire parity through both CLI VM modes"]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn exhaustive_buffered_writer_native_peer_and_cancelled_prefix_cli_vm_modes() {
    message_native_peer(true, &[0, 1]);
}

#[cfg(unix)]
fn resource_program_in_standalone_aot(source: &str, entries: &[&str]) {
    let work = Workspace::new();
    let ast = joky::parser::parse(source).unwrap();
    let mut compiler = joky::aot::AotCompiler::new("resource_owners").unwrap();
    compiler.register_resource_type("Socket").unwrap();
    compiler
        .register_resource_factory("make", "Socket")
        .unwrap();
    compiler
        .register_resource_factory_async("acquire", "Socket")
        .unwrap();
    compiler.compile_module(&ast).unwrap();
    let mut driver = include_str!("../fixtures/resource_factory_host.rs").to_owned();
    driver.push_str(
        "\nuse joky::runtime::suspension::Continuation; use std::rc::Rc;\nextern \"C\" {\n",
    );
    for entry in entries {
        driver.push_str(&format!(
            "#[link_name = \"{entry}\"] fn entry_{entry}(frame: *mut CpsFrame) -> i32;\n"
        ));
    }
    driver.push_str("}\nfn main() {\n");
    for entry in entries {
        driver.push_str(&format!("for cancel in [false,true] {{ reset(); reset_stops(); let mut task=unsafe {{ Continuation::from_native(entry_{entry}, CpsFrame::new({}), Rc::new(())) }}; let mut failed=false; for _ in 0..20 {{ if task.is_finished() {{ break; }} if task.resume().is_err() {{ failed=true; break; }} if cancel {{ break; }} }} if !cancel {{ assert_eq!(failed, {}); if !failed {{ assert_eq!(task.cps_result(),Some(42)); }} }} drop(task); check(); assert!(stops().len()<=1); }}\n",compiler.cps_frame_slots(entry).unwrap(),*entry=="partial" || entry.ends_with("failure") || *entry=="invalid_pending" || *entry=="empty_success"));
    }
    driver.push_str("}\n");
    work.write("driver.rs", &driver);
    compiler
        .write_object(&work.path().join("resources.o"))
        .unwrap();
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
            work.path().join("resources.o").display()
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

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn message_native_peer(split: bool, backends: &[usize]) {
    mod endpoint {
        include!("../support/tcp_messages.rs");
    }
    for cancel in [false, true] {
        for &backend in backends {
            let mut peer = endpoint::peer();
            let work = Workspace::new();
            let source = include_str!("../fixtures/buffered_tcp_pressure.jk")
                .replace("PORT", &peer.port.to_string())
                .replace("COUNT", "8")
                .replace("extern fn sent(count: Int32)", "fn sent(count: Int32) {}")
                .replace("fn main()", "fn send_all()")
                .replace("fn cancel()", "fn stop_prefix()");
            let source = if split {
                source
                    .replace("type BufferedTcp}", "type BufferedTcp, type TcpWriter}")
                    .replace("connection: &mut BufferedTcp", "connection: &mut TcpWriter")
                    .replace(
                        "var connection = BufferedTcp.new(stream)",
                        "var (reader, connection) = BufferedTcp.new(stream).split()",
                    )
            } else {
                source
            };
            let entry = if cancel { "stop_prefix" } else { "send_all" };
            work.write(
                "pressure.jk",
                &format!("{source}\nfn main() @suspends -> Int32 {{ {entry}() }}"),
            );
            peer.release();
            let output = if backend < 2 {
                let args = if backend == 0 {
                    vec!["run", "pressure.jk"]
                } else {
                    vec!["run", "pressure.jk", "--interpreter"]
                };
                work.cli(&args)
            } else {
                success(&work.cli(&["build", "pressure.jk"]));
                std::process::Command::new(work.path().join("pressure"))
                    .output()
                    .unwrap()
            };
            success(&output);
            assert!(output.stdout.is_empty() && output.stderr.is_empty());
            work.assert_no_build_files();
            assert!(peer.accepted.load(std::sync::atomic::Ordering::Acquire));
            let received = peer.finish();
            if cancel {
                assert_eq!(received.complete, 0);
                assert!(received.partial > 4 && received.partial < 65540);
            } else {
                assert_eq!(received.complete, 8);
                assert_eq!(received.partial, 0);
            }
        }
    }
}

#[test]
#[cfg(unix)]
fn connection_sessions_run_in_all_backends() {
    let work = Workspace::new();
    work.write("session.jk", include_str!("../fixtures/session.jk"));
    work.assert_all_backends("session.jk", &[], b"", 0);
}

#[test]
#[cfg(unix)]
fn session_drain_and_outbox_seal_run_in_aot() {
    let work = Workspace::new();
    work.write_fixture_batch(
        "drain.jk",
        &[
            ("sealed.jk", include_str!("../fixtures/outbox_seal.jk")),
            ("shutdown.jk", include_str!("../fixtures/shutdown.jk")),
            (
                "session_drain.jk",
                include_str!("../fixtures/session_drain.jk"),
            ),
        ],
    );
    work.assert_native("drain.jk", &[], b"", 0);
    work.write(
        "parallel_drain.jk",
        r#"
import shutdown
import joky/outbox
import joky/check
import joky/task
fn seal(output: outbox.Handle) @suspends { task.yield_now(); output.seal() }
fn main() @suspends {
    shutdown.run_fixture()
    var queue = outbox.create(1, 0)
    let output = queue.handle()
    let (message, _) = join { queue.receive(), seal(output) }
    check.assert(message.is_none(), "parallel seal wakes pending receive")
}
"#,
    );
    work.assert_native("parallel_drain.jk", &["--parallel"], b"", 0);
}

#[test]
fn structured_log_preserves_types_and_rejects_invalid_fields() {
    let work = Workspace::new();
    work.write("events.jk", r#"
import joky/log
import joky/check
import joky/uuid
import joky/http
fn main() {
    check.assert(log.emit(log.Level.Debug, "event\nname", [
        log.field("detail", "quote\"\\\nline"), log.field("count", 9),
        log.field("delta", -2), log.field("ready", true)
    ]), "valid")
    check.assert(log.emit(log.Level.Info, "job.finished", [log.text("request_id", "queue-42")]), "application field")
    check.assert(!log.emit(log.Level.Info, "invalid", [log.uint("n", 1), log.uint("n", 2)]), "duplicate")
    check.assert(match log.try_emit(log.Level.Info, "invalid", [log.text("timestamp", "override")]) {
        Err(DuplicateField) => true, _ => false }, "reserved")
    let logger = log.Logger.new(log.Level.Warn)
    check.assert(!logger.enabled(log.Level.Info) && logger.enabled(log.Level.Warn), "threshold boundary")
    check.assert(match logger.try_emit(log.Level.Info, "", []) { Ok(Filtered) => true, _ => false }, "skip before validation")
    check.assert(match logger.try_emit(log.Level.Warn, "alert", []) { Ok(Written) => true, _ => false }, "at threshold")
    check.assert(match log.try_emit(log.Level.Info, "", []) { Err(InvalidEvent) => true, _ => false }, "empty event")
    let id = uuid.Uuid.parse("00112233-4455-6677-8899-aabbccddeeff")!
    log.info("typed", [
        log.field("i8", Int8.MIN), log.field("i16", Int16.MIN),
        log.field("i64", Int64.MIN), log.field("u8", UInt8.MAX),
        log.field("u16", UInt16.MAX), log.field("u32", UInt32.MAX),
        log.field("u64", UInt64.MAX), log.field("id", id)
    ])
    log.debug("module.debug", [])
    log.warn("module.warn", [])
    log.error("module.error", [])
    logger.debug("filtered", [])
    logger.info("filtered", [])
    logger.warn("logger.warn", [])
    logger.error("logger.error", [])
    let context = http.RequestContext { request_id: id, logger }
    context.debug("filtered", [])
    context.info("filtered", [])
    context.warn("context.warn", [])
    context.error("context.error", [])
    context.warn("invalid", [log.field("request_id", "duplicate")])
}
"#);
    success(&work.cli(&["build", "events.jk"]));
    for backend in ["jit", "interpreter", "aot"] {
        let output = match backend {
            "aot" => Command::new(work.path().join("events")).output().unwrap(),
            "interpreter" => work.cli(&["run", "events.jk", "--interpreter"]),
            _ => work.cli(&["run", "events.jk"]),
        };
        success(&output);
        assert!(output.stdout.is_empty());
        let text = std::str::from_utf8(&output.stderr).unwrap();
        let mut records = text
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap());
        let event = records.next().unwrap();
        assert_eq!(event["level"], "debug");
        assert_eq!(event["event"], "event\nname");
        assert_eq!(event["detail"], "quote\"\\\nline");
        assert_eq!(event["count"], 9);
        assert_eq!(event["delta"], -2);
        assert_eq!(event["ready"], true);
        assert!(event.get("request_id").is_none());
        let job = records.next().unwrap();
        assert_eq!(job["event"], "job.finished");
        assert_eq!(job["request_id"], "queue-42");
        assert_eq!(records.next().unwrap()["event"], "alert");
        let typed = records.next().unwrap();
        assert_eq!(typed["level"], "info");
        assert_eq!(typed["event"], "typed");
        for (key, value) in [
            ("i8", i8::MIN as i64),
            ("i16", i16::MIN as i64),
            ("i64", i64::MIN),
        ] {
            assert_eq!(typed[key].as_i64(), Some(value));
        }
        for (key, value) in [
            ("u8", u8::MAX as u64),
            ("u16", u16::MAX as u64),
            ("u32", u32::MAX as u64),
            ("u64", u64::MAX),
        ] {
            assert_eq!(typed[key].as_u64(), Some(value));
        }
        assert_eq!(typed["id"], "00112233-4455-6677-8899-aabbccddeeff");
        for (name, level) in [
            ("module.debug", "debug"),
            ("module.warn", "warn"),
            ("module.error", "error"),
            ("logger.warn", "warn"),
            ("logger.error", "error"),
            ("context.warn", "warn"),
            ("context.error", "error"),
        ] {
            let record = records.next().unwrap();
            assert_eq!(record["event"], name);
            assert_eq!(record["level"], level);
            if name.starts_with("context.") {
                assert_eq!(record["request_id"], typed["id"]);
            }
        }
        assert!(records.next().is_none());
    }
}

#[test]
#[cfg(unix)]
fn structured_log_reports_closed_pipe_without_stopping_the_task() {
    let work = Workspace::new();
    work.write("pipe.jk", r#"import joky/log
        fn main() @suspends {
            assert(match log.try_emit(log.Level.Info, "event", []) { Err(WriteFailed) => true, _ => false }, "broken pipe")
            assert(!log.emit(log.Level.Info, "event", []), "compatibility call reports failure")
            log.error("ignored write failure", [])
            println("continued")
        }"#);
    success(&work.cli(&["build", "pipe.jk", "--parallel"]));
    for mode in ["jit", "interpreter", "aot"] {
        let mut cmd = if mode == "aot" {
            Command::new(work.path().join("pipe"))
        } else {
            let mut cmd = work.command();
            cmd.args(["run", "pipe.jk", "--parallel"]);
            if mode == "interpreter" {
                cmd.arg("--interpreter");
            }
            cmd
        };
        let (read, write) = std::io::pipe().unwrap();
        drop(read);
        let output = cmd.stderr(write).output().unwrap();
        success(&output);
        assert_eq!(output.stdout, b"continued\n", "{mode}");
    }
}
