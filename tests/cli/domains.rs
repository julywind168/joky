//! State-domain operations, publication and migration.
use super::*;

#[test]
#[cfg(unix)]
fn direct_parallel_multi_domain_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "transfer.jk",
        include_str!("../fixtures/parallel_multi_domain.jk"),
    );
    work.assert_native("transfer.jk", &["--task-workers", "2"], b"", 0);
}

#[test]
#[cfg(unix)]
fn direct_parallel_single_domain_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "stock.jk",
        include_str!("../fixtures/parallel_single_domain.jk"),
    );
    work.assert_native("stock.jk", &["--task-workers", "2"], b"", 0);
}

#[test]
#[cfg(unix)]
fn state_domain_probe_owned_snapshots_replacement_and_cancellation_run_on_all_backends() {
    let work = Workspace::new();
    work.write(
        "probe.jk",
        include_str!("../fixtures/state_domain_probe.jk"),
    );
    work.assert_all_backends("probe.jk", &[], b"42\n99\n", 0);
}

#[cfg(unix)]
#[test]
fn state_domains_access_rejections_are_values_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "access.jk",
        include_str!("../fixtures/state_domain_access_errors.jk"),
    );
    work.assert_all_backends("access.jk", &[], b"", 0);
}

#[test]
#[cfg(unix)]
fn state_domains_unwrap_rejected_access_fails_the_calling_task() {
    let work = Workspace::new();
    work.write(
        "domain.jk",
        r#"
        import joky/domain
        struct State { count: Int32 }
        fn read(state: &State) -> Int32 { state.count }
        fn main() @suspends { domain.read(read).unwrap(); }
    "#,
    );
    let check = |output: std::process::Output| {
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("Called Result.unwrap on Err"), "{error}");
        assert!(!error.contains("panicked"), "{error}");
    };
    check(work.cli(&["run", "domain.jk"]));
    check(work.cli(&["run", "domain.jk", "--interpreter"]));
    success(&work.cli(&["build", "domain.jk"]));
    check(Command::new(work.path().join("domain")).output().unwrap());
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn state_domains_source_lifecycle_runs_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "domain_source.jk",
        include_str!("../fixtures/state_domain_source.jk"),
    );
    work.assert_all_backends("domain_source.jk", &[], b"source domains ok\n", 0);
}

#[test]
#[cfg(unix)]
fn state_domains_scalar_bits_and_unit_inputs_run_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "scalars.jk",
        include_str!("../fixtures/state_domain_scalars.jk"),
    );
    work.assert_all_backends("scalars.jk", &[], b"scalar domains ok\n", 0);
}

#[test]
#[cfg(unix)]
fn state_domains_snapshot_acceptance_runs_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "acceptance.jk",
        include_str!("../fixtures/state_domain_acceptance.jk"),
    );
    work.assert_native("acceptance.jk", &[], b"SC1 acceptance ok\n", 0);
}

#[test]
#[cfg(unix)]
fn state_domains_versioned_snapshots_run_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "snapshots.jk",
        include_str!("../fixtures/state_domain_snapshots.jk"),
    );
    work.assert_native("snapshots.jk", &[], b"snapshot domains ok\n", 0);
}

#[test]
fn state_domains_snapshot_checks_proofs_and_generic_request_values() {
    let work = Workspace::new();
    let prefix = r#"
        import joky/domain
        import joky/io
        struct State { count: Int32 }
        fn make(value: Int32) -> State { State { count: value } }
        fn identity(value: Int32) -> Int32 { value }
        fn echo<T>(value: T) @suspends -> T { value }
        fn relay<T>(handle: domain.WriteHandle<State>, value: T) @suspends -> Result<T, domain.RequestError> {
            domain.request::<State, T, T>(handle, echo::<T>, value)
        }
    "#;
    for (case, diagnostic) in [
        ("fn bad(s: &State) -> Int32 { let f = identity; f(s.count) } fn main() @suspends { let o = domain.create(make, 1); domain.read_from(o.read_handle(), bad).unwrap(); }", "Domain operation must be a proved"),
        ("fn helper(n: Int32) { io.println(&n) } fn bad(s: &State) -> Int32 { helper(s.count); s.count } fn main() @suspends { let o = domain.create(make, 1); domain.read_from(o.read_handle(), bad).unwrap(); }", "Domain operation must be a proved"),
        ("fn bad(s: &State) -> Int32 { let values = [s.count]; values.any(|value| { io.println(value); true }); s.count } fn main() @suspends { let o = domain.create(make, 1); domain.read_from(o.read_handle(), bad).unwrap(); }", "Domain operation must be a proved"),
        ("fn main() @suspends { let o = domain.create(make, 1); relay(o.handle(), (1, Ptr.null())); }", "bounded value"),
    ] {
        work.write("rejected.jk", &format!("{prefix}\n{case}"));
        let output = work.cli(&["build", "rejected.jk", "-o", "rejected"]);
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "accepted {case}");
        assert!(error.contains(diagnostic), "{case}: {error}");
        assert!(!error.contains("panicked"), "{error}");
        assert!(output.stdout.is_empty());
    }
    work.write(
        "owned_request.jk",
        &format!(
            r#"{prefix} fn main() @suspends {{
            let owner = domain.create(make, 1)
            let text = relay(owner.handle(), "owned").unwrap()
            println(text)
        }}"#
        ),
    );
    work.assert_all_backends("owned_request.jk", &[], b"owned\n", 0);
}

#[test]
#[cfg(unix)]
fn state_domains_imported_entries_and_language_example_run_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "room.jk",
        r#"
        import joky/domain
        import joky/task
        pub struct State { count: Int32 }
        pub fn make(count: Int32) -> State { State { count: count } }
        fn add(state: &mut State, delta: Int32) -> Int32 {
            state.count = state.count + delta
            state.count
        }
        pub fn read(state: &State) -> Int32 { state.count }
        pub fn work(delta: Int32) @suspends -> Int32 {
            task.sleep(1ms)
            domain.write(add, delta).unwrap()
        }
        "#,
    );
    work.write(
        "imported.jk",
        r#"
        import room
        import joky/domain
        import joky/io
        fn main() @suspends {
            let owner = domain.create(room.make, 10)
            let result = domain.request(owner.handle(), room.work, 2).unwrap_or(-1)
            io.println(result)
            let value = domain.read_from(owner.read_handle(), room.read).unwrap()
            io.println(value)
            owner.close()
        }
        "#,
    );
    work.assert_all_backends("imported.jk", &[], b"12\n12\n", 0);
    let reference = include_str!("../../docs/language.md");
    let section = reference
        .split("## 内置域访问 `joky/domain`")
        .nth(1)
        .unwrap();
    let example = section
        .split("```jk\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    work.write("documented.jk", example);
    work.assert_all_backends("documented.jk", &[], b"", 0);
}

#[test]
#[cfg(unix)]
fn state_domains_candidates_commit_atomically_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "commits.jk",
        include_str!("../fixtures/state_domain_commits.jk"),
    );
    work.assert_native("commits.jk", &[], b"domain commits ok\n", 0);
}

#[test]
#[cfg(unix)]
fn state_domains_owned_graphs_run_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "owned.jk",
        include_str!("../fixtures/state_domain_owned.jk"),
    );
    work.assert_native("owned.jk", &[], b"owned domains ok\n", 0);
}

#[test]
#[cfg(unix)]
fn state_domains_deadlines_run_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "deadlines.jk",
        include_str!("../fixtures/state_domain_deadlines.jk"),
    );
    work.assert_native("deadlines.jk", &[], b"domain deadlines ok\n", 0);
}

#[test]
#[cfg(unix)]
fn state_domains_commit_acceptance_and_scalar_arrays_run_in_all_backends() {
    let work = Workspace::new();
    for (source, expected) in [
        (
            include_str!("../fixtures/state_domain_sc2_acceptance.jk"),
            b"SC2 acceptance ok\n".as_slice(),
        ),
        (
            include_str!("../fixtures/state_domain_sc2_arrays.jk"),
            b"SC2 scalar Arrays ok\n".as_slice(),
        ),
    ] {
        work.write("acceptance.jk", source);
        work.assert_all_backends("acceptance.jk", &[], expected, 0);
    }
}

#[test]
#[cfg(unix)]
fn state_domains_pairs_run_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "pairs.jk",
        include_str!("../fixtures/state_domain_pairs.jk"),
    );
    work.assert_native("pairs.jk", &[], b"domain pairs ok\n", 0);
}

#[test]
#[cfg(unix)]
fn state_domains_joint_snapshots_run_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "joint.jk",
        include_str!("../fixtures/state_domain_joint_snapshots.jk"),
    );
    work.assert_native("joint.jk", &[], b"joint snapshots ok\n", 0);
}

#[test]
#[cfg(unix)]
fn state_domains_pair_commits_run_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "pair_commits.jk",
        include_str!("../fixtures/state_domain_pair_commits.jk"),
    );
    work.assert_native("pair_commits.jk", &[], b"pair commits ok\n", 0);
}

#[test]
#[cfg(unix)]
fn state_domains_many_transfer_and_readonly_fourth_conflict_run_in_standalone_aot() {
    let work = Workspace::new();
    work.write("many.jk", include_str!("../fixtures/state_domain_many.jk"));
    work.assert_native("many.jk", &[], b"multi-domain transfer ok\n", 0);
}

#[test]
#[cfg(unix)]
fn admission_fifo_close_and_cancellation_run_in_standalone_aot() {
    let work = Workspace::new();
    work.write("admission.jk", include_str!("../fixtures/admission.jk"));
    work.assert_native("admission.jk", &[], b"admission ok\n", 0);
}

#[test]
#[cfg(unix)]
fn admission_invalid_capacity_and_queue_size_fail_in_all_backends() {
    let work = Workspace::new();
    for (capacity, waiters, message) in [
        ("0", "0", "capacity must be positive"),
        ("1", "18446744073709551615", "Admission queue size overflow"),
    ] {
        work.write("invalid.jk",&format!("import joky/admission fn main() @suspends {{ let pool=admission.create({capacity},{waiters}) }}"));
        let rejected = |output: std::process::Output| {
            assert!(!output.status.success());
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(error.contains(message), "{error}");
        };
        rejected(work.cli(&["run", "invalid.jk"]));
        rejected(work.cli(&["run", "--interpreter", "invalid.jk"]));
        success(&work.cli(&["build", "invalid.jk"]));
        rejected(Command::new(work.path().join("invalid")).output().unwrap());
    }
}

#[test]
#[cfg(unix)]
fn state_domains_nested_graphs_run_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "nested.jk",
        include_str!("../fixtures/state_domain_nested.jk"),
    );
    work.assert_native("nested.jk", &[], b"nested domains ok\n", 0);
}

#[test]
#[cfg(unix)]
fn state_domains_nested_string_keys_run_in_all_backends() {
    let work = Workspace::new();
    let source = include_str!("../fixtures/state_domain_nested.jk")
        .replace("Map<Int32, Player>", "Map<String, Player>")
        .replace("Map.new::<Int32, Player>()", "Map.new::<String, Player>()")
        .replace("rooms.insert(1,", "rooms.insert(\"first\",")
        .replace("rooms.insert(2,", "rooms.insert(\"second\",")
        .replace(
            "fn edit(state: &mut State, value: Int32) -> Int32 {",
            "fn edit(state: &mut State, value: Int32) -> Int32 { let second = \"second\"",
        )
        .replace(
            "fn inspect(state: &State) -> Bool {",
            "fn inspect(state: &State) -> Bool { let first = \"first\"; let second = \"second\"",
        )
        .replace("rooms.remove(2)", "rooms.remove(&second)")
        .replace("rooms.borrow_ref(1)", "rooms.borrow_ref(&first)")
        .replace("rooms.borrow_ref(2)", "rooms.borrow_ref(&second)");
    work.write("nested.jk", &source);
    work.assert_all_backends("nested.jk", &[], b"nested domains ok\n", 0);
}

#[test]
#[cfg(unix)]
fn outbox_atomic_publication_runs_in_all_backends() {
    let work = Workspace::new();
    work.write("outbox.jk", include_str!("../fixtures/outbox.jk"));
    work.assert_all_backends("outbox.jk", &[], b"outbox ok\n", 0);
}

#[test]
#[cfg(unix)]
fn outbox_overflow_policies_run_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "policies.jk",
        include_str!("../fixtures/outbox_policies.jk"),
    );
    work.assert_all_backends("policies.jk", &[], b"", 0);
}

#[test]
#[cfg(unix)]
fn outbox_tcp_writer_runs_in_all_backends() {
    let work = Workspace::new();
    work.write("outbox.jk", include_str!("../fixtures/outbox_tcp.jk"));
    work.assert_all_backends("outbox.jk", &[], b"outbox TCP ok\n", 0);
}

#[test]
#[cfg(unix)]
fn outbox_script_handles_survive_module_close_in_all_backends() {
    let target = r#"import joky/domain import joky/outbox import joky/array.{type Array}
struct State { n:Int32 }
fn make(n:Int32)->State { State{n:n} }
fn read(s:&State)->Int32{s.n}
fn send(target:outbox.Handle) @suspends ->Int32 {
    let state=domain.create(make,0)
    var bytes=Array.new::<UInt8>(); bytes.push(255)
    var batch=outbox.batch(1,1)
    outbox.stage(&mut batch,target,bytes);
    let candidate=domain.candidate_from(domain.snapshot_from(state.read_handle(),read), state.handle(), make, 1).with_outbox(batch)
    var receipt=domain.commit_to(state.handle(),candidate)
    match receipt.wait() { Ok(_) => 1, _ => 0 }
}"#.replace('{', "{{").replace('}', "}}");
    for create in ["new", "interpreter"] {
        let work = Workspace::new();
        work.write("script.jk", &format!(r#"import joky/vm import joky/outbox import joky/check
fn main() @suspends {{
    var queue=outbox.create(1,1)
    let h=vm.Vm.{create}()
    let source={target:?}
    let message="outbox Script"
    check.assert(vm.load_source(h,&source)==0,&message)
    check.assert(h.call::<outbox.Handle,Int32>("send",queue.handle())==1,&message)
    let next="fn newer()->Int32 {{{{ 2 }}}}"
    check.assert(vm.load_source(h,&next)==0,&message)
    check.assert(vm.close(h)==0,&message)
    check.assert(match queue.receive() {{ Some(bytes)=>bytes.len()==1 && bytes.get(0).unwrap_or(0)==255, None=>false }},&message)
    println("outbox Script ok")
}}
"#));
        work.assert_all_backends("script.jk", &[], b"outbox Script ok\n", 0);
    }
}

#[test]
fn directory_routes_run_in_all_backends() {
    let work = Workspace::new();
    work.write("directory.jk", include_str!("../fixtures/directory.jk"));
    work.assert_all_backends("directory.jk", &[], b"directory ok\n", 0);
}

#[test]
#[cfg(unix)]
fn directory_lookup_checks_imported_state_types_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "model.jk",
        "pub struct Room { pub value:Int32 } pub fn make(value:Int32)->Room { Room{value:value} }",
    );
    work.write(
        "imported.jk",
        r#"import joky/domain import model import joky/check
fn main() @suspends {
    let table=domain.directory(1)
    let owner=domain.create(model.make,42)
    table.handle().insert(1,owner.handle());
    let message="imported directory type"
    check.assert(domain.lookup::<model.Room>(table.handle(),1).is_some(),&message)
}"#,
    );
    work.assert_all_backends("imported.jk", &[], b"", 0);
    work.write(
        "mismatch.jk",
        r#"import joky/domain import model
struct Player { value:Int32 }
fn main() @suspends {
    let table=domain.directory(1)
    let owner=domain.create(model.make,42)
    table.handle().insert(1,owner.handle());
    domain.lookup::<Player>(table.handle(),1);
}"#,
    );
    success(&work.cli(&["build", "mismatch.jk"]));
    for output in [
        work.cli(&["run", "mismatch.jk"]),
        work.cli(&["run", "--interpreter", "mismatch.jk"]),
        Command::new(work.path().join("mismatch")).output().unwrap(),
    ] {
        failure(
            &output,
            "Directory target does not match the checked state type",
        );
    }
}

#[test]
#[cfg(unix)]
fn directory_script_routing_survives_code_publication_in_all_backends() {
    let source = r#"import joky/domain import joky/check
struct Room { value:Int32 }
fn make(value:Int32)->Room { Room{value:value} }
fn read(room:&Room)->Int32 { room.value }
fn install(directory:domain.DirectoryHandle) @suspends {
    let room=domain.create(make,42)
    let handle=room.read_handle()
    let message="directory register"
    check.assert(directory.insert_owned(7,room).is_ok(),&message)
    check.assert(directory.insert_read(8,handle).is_ok(),&message)
}
fn probe(directory:domain.DirectoryHandle) @suspends ->Int32 {
    match domain.lookup::<Room>(directory,8) { Some(room)=>domain.read_from(room,read).unwrap(),None=>-1 }
}
fn remove(directory:domain.DirectoryHandle) @suspends ->Bool {
    match domain.lookup::<Room>(directory,7) { Some(room)=>directory.remove(7,room),None=>false }
}"#;
    let next = source.replace("{ room.value }", "{ room.value + 100 }");
    let source = source.replace('{', "{{").replace('}', "}}");
    let next = next.replace('{', "{{").replace('}', "}}");
    for create in ["new", "interpreter"] {
        let work = Workspace::new();
        work.write(
            "directory_script.jk",
            &format!(
                r#"import joky/domain import joky/vm import joky/check
fn control(h:&vm.Vm,directory:domain.DirectoryHandle) @suspends {{
    let message="directory reload"
    check.assert(h.call::<domain.DirectoryHandle,Int32>("probe",directory)==42,&message)
    check.assert(vm.module_count(h)==1,&message)
    let next={next:?}
    check.assert(vm.load_source(h,&next)==0,&message)
    check.assert(vm.module_count(h)==2,&message)
    check.assert(vm.publish_domain_code(h)==1,&message)
    check.assert(vm.module_count(h)==1,&message)
    check.assert(h.call::<domain.DirectoryHandle,Int32>("probe",directory)==142,&message)
    check.assert(h.call::<domain.DirectoryHandle,Bool>("remove",directory),&message)
}}
fn main() @suspends {{
    let owner=domain.directory(2)
    let directory=owner.handle()
    let h=vm.Vm.{create}()
    let source={source:?}
    let message="directory Script"
    check.assert(vm.load_source(h,&source)==0,&message)
    h.call::<domain.DirectoryHandle,Unit>("install",directory)
    control(&h,directory)
    check.assert(h.call::<domain.DirectoryHandle,Int32>("probe",directory)==-1,&message)
    check.assert(vm.close(h)==0,&message)
    owner.close()
    println("directory Script ok")
}}
"#
            ),
        );
        work.assert_all_backends("directory_script.jk", &[], b"directory Script ok\n", 0);
    }
}

#[test]
#[cfg(unix)]
fn domain_timers_run_in_all_backends() {
    let work = Workspace::new();
    work.write("timers.jk", include_str!("../fixtures/domain_timers.jk"));
    work.assert_all_backends("timers.jk", &[], b"", 0);
    work.write(
        "precise.jk",
        &format!(
            "{}\nfn main() @suspends {{ assert(precision(250us.as_nanos()) == 42) }}",
            include_str!("../fixtures/domain_timers.jk").replace(
                "fn main() @suspends ->Int32",
                "fn lifecycle() @suspends ->Int32"
            )
        ),
    );
    work.assert_native("precise.jk", &[], b"", 0);
}

#[test]
#[cfg(unix)]
fn domain_timers_script_handles_survive_publication_in_all_backends() {
    let source = r#"import joky/domain import joky/task import joky/check
struct Room { value:Int32 }
fn make(value:Int32)->Room { Room{value:value} }
fn read(room:&Room)->Int32 { room.value }
fn tick(room:&mut Room,amount:Int32) { room.value=room.value+amount }
fn hold(timer:domain.TimerHandle) @suspends {
    let room=domain.create(make,10)
    let message="timer Script"
    check.assert(domain.after(timer,room.handle(),100000ms,tick,2).is_ok(),&message)
    while match timer.status() { Active=>true,_=>false } { task.yield_now() }
    check.assert(match timer.status() { Cancelled=>true,_=>false },&message)
    check.assert(domain.after(timer,room.handle(),0ms,tick,2).is_ok(),&message)
    while match timer.status() { Active=>true,_=>false } { task.yield_now() }
    check.assert(match timer.status() { Completed=>true,_=>false },&message)
    check.assert(domain.read_from(room.read_handle(),read).unwrap()==30,&message)
}"#;
    let next = source.replace("room.value+amount", "room.value+amount*10");
    let source = source.replace('{', "{{").replace('}', "}}");
    let next = next.replace('{', "{{").replace('}', "}}");
    for create in ["new", "interpreter"] {
        let work = Workspace::new();
        work.write(
            "timer_script.jk",
            &format!(
                r#"import joky/domain import joky/vm import joky/task import joky/check
fn control(h:&vm.Vm,timer:domain.TimerHandle) @suspends {{
    let message="timer publication"
    while match timer.status() {{ Idle=>true,_=>false }} {{ task.yield_now() }}
    let next={next:?}
    check.assert(vm.load_source(h,&next)==0,&message)
    check.assert(vm.publish_domain_code(h)==1,&message)
    timer.cancel()
}}
fn main() @suspends {{
    let owner=domain.timer()
    let timer=owner.handle()
    let h=vm.Vm.{create}()
    let source={source:?}
    let message="timer host"
    check.assert(vm.load_source(h,&source)==0,&message)
    join {{ h.call::<domain.TimerHandle,Unit>("hold",timer), control(&h,timer) }};
    check.assert(vm.close(h)==0,&message)
    owner.close()
    check.assert(match timer.status() {{ Closed=>true,_=>false }},&message)
}}
"#
            ),
        );
        work.assert_all_backends("timer_script.jk", &[], b"", 0);
    }
}

#[test]
#[cfg(unix)]
fn domain_value_packets_run_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "values.jk",
        include_str!("../fixtures/state_domain_values.jk"),
    );
    work.assert_native("values.jk", &[], b"", 0);
}

#[test]
#[cfg(unix)]
fn domain_value_packets_keep_old_flows_and_bind_new_script_operations_in_all_backends() {
    let old = include_str!("../fixtures/late_binding_values.jk");
    let new = old
        .replace("Room{n:input.amount}", "Room{n:input.amount+1000}")
        .replace("a.n-input.amount", "a.n-input.amount*10")
        .replace("b.n=b.n+input.amount", "b.n=b.n+input.amount*10")
        .replace("value:a.n+b.n,", "value:a.n+b.n+100,")
        .replace("{a.n+n}", "{a.n+n+100}")
        .replace("{a.n+b.n+n}", "{a.n+b.n+n+100}")
        .replace("value:a.n+input.amount,", "value:a.n+input.amount+100,")
        .replace(
            "value:a.n+b.n+input.amount,",
            "value:a.n+b.n+input.amount+100,",
        );
    let old = old.replace('{', "{{").replace('}', "}}");
    let new = new.replace('{', "{{").replace('}', "}}");
    for create in ["new", "interpreter"] {
        let work = Workspace::new();
        work.write(
            "values_reload.jk",
            &format!(
                r#"import joky/domain import joky/vm import joky/task import joky/check
fn assert(ok:Bool) {{let message="domain values reload"; check.assert(ok,&message)}}
fn control(h:&vm.Vm,directory:domain.DirectoryHandle) @suspends {{
    var attempts=0
    while h.call::<domain.DirectoryHandle,Int32>("probe",directory)==-1 {{
        attempts=attempts+1; assert(attempts<1000); task.yield_now()
    }}
    let next={new:?}
    assert(vm.load_source(h,&next)==0)
    assert(h.call::<Int32,Int32>("construct_probe",7)==1007)
    assert(h.call::<domain.DirectoryHandle,Int32>("probe",directory)==42)
    assert(vm.publish_domain_code(h)==2)
    assert(h.call::<domain.DirectoryHandle,Int32>("probe",directory)==142)
    assert(h.call::<domain.DirectoryHandle,Bool>("release",directory))
}}
fn main() @suspends {{
    let h=vm.Vm.{create}(); let source={old:?}
    assert(vm.load_source(h,&source)==0)
    let owner=domain.directory(2); let directory=owner.handle()
    join {{ h.call::<domain.DirectoryHandle,Unit>("hold",directory), control(&h,directory) }};
    assert(vm.module_count(h)==1)
    assert(vm.close(h)==0)
}}"#
            ),
        );
        work.assert_all_backends("values_reload.jk", &[], b"", 0);
    }
}

#[test]
#[cfg(unix)]
fn domain_value_packets_bound_fields_depth_and_zero_lane_records() {
    let mut cases = Vec::new();
    for fields in [64, 65] {
        for empty in [false, true] {
            let ty = if empty { "Empty" } else { "Int32" };
            let names = (0..fields)
                .map(|i| format!("n{i}:{ty}"))
                .collect::<Vec<_>>()
                .join(",");
            let values = (0..fields)
                .map(|i| {
                    format!(
                        "n{i}:{}",
                        if empty {
                            "Empty{}".to_owned()
                        } else {
                            i.to_string()
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            cases.push((
                format!("struct Empty {{}} struct Packet {{{names}}}"),
                "Packet".to_owned(),
                format!("Packet{{{values}}}"),
                if empty {
                    "true".to_owned()
                } else {
                    "p.n0==0 && p.n63==63".to_owned()
                },
                fields == 64,
            ));
        }
    }
    for depth in [8, 9] {
        let mut definitions = "struct N0 {n:Int32}".to_owned();
        let mut value = "N0{n:42}".to_owned();
        let mut access = "p".to_owned();
        for i in 1..depth {
            definitions.push_str(&format!(" struct N{i} {{inner:N{}}}", i - 1));
            value = format!("N{i}{{inner:{value}}}");
            access.push_str(".inner");
        }
        cases.push((
            definitions,
            format!("N{}", depth - 1),
            value,
            format!("{access}.n==42"),
            depth == 8,
        ));
    }
    // The same bounded-value contract serves every operation. Exercise all
    // accepted shapes in one program each; check input/output overflow once per
    // shape. Per-API invalid-contract tests below cover operation wiring.
    for (definitions, ty, value, assertion, accepted) in cases {
        let work = Workspace::new();
        let declarations = format!(
            r#"import joky/domain import joky/check import joky/task
{definitions}
struct State {{payload:{ty}, n:Int32}}
fn seed(n:Int32)->State {{State{{payload:{value},n:n}}}}
fn make(p:{ty})->State {{State{{payload:p,n:0}}}}
fn read(s:&State)->{ty} {{s.payload}}
fn pair(a:&State,b:&State)->{ty} {{a.payload}}
fn count(s:&State)->Int32 {{s.n}}
fn turn(s:&mut State,p:{ty})->{ty} {{s.n=s.n+1; p}}
fn query(s:&State,n:Int32)->{ty} {{s.payload}}
fn echo(p:{ty}) @suspends ->Int32 {{task.yield_now(); verify(p); 42}}
fn reply(n:Int32) @suspends ->{ty} {{task.yield_now(); {value}}}
fn tick(s:&mut State,p:{ty}) {{verify(p); s.n=s.n+1}}
fn verify(p:{ty}) {{let message="domain value bounds"; check.assert({assertion},&message)}}
"#
        );
        if !accepted {
            for invocation in [
                format!("domain.create(make,{value});"),
                "let owner=domain.create(seed,0); domain.query_from(owner.read_handle(),query,0);"
                    .to_owned(),
            ] {
                work.write(
                    "bound.jk",
                    &format!("{declarations} fn main() @suspends {{ {invocation} }}"),
                );
                failure(&work.cli(&["build", "bound.jk"]), "bounded value signature");
            }
            continue;
        }
        work.write("bound.jk", &format!(r#"{declarations}
fn main() @suspends {{
    let a=domain.create(make,{value}); let b=domain.create(seed,0)
    let message="domain value bounds"
    verify(domain.read_from(a.read_handle(),read).unwrap())
    verify(domain.write_to(a.handle(),turn,{value}).unwrap())
    verify(domain.query_from(a.read_handle(),query,0).unwrap())
    check.assert(domain.request(a.handle(),echo,{value}).unwrap()==42,&message)
    check.assert(domain.request_read(a.read_handle(),echo,{value}).unwrap()==42,&message)
    verify(domain.request(a.handle(),reply,0).unwrap())
    verify(domain.request_read(a.read_handle(),reply,0).unwrap())
    {{ let saved=domain.snapshot_from(a.read_handle(),read); verify(saved.value()) }}
    {{ let saved=domain.snapshot_pair(a.read_handle(),b.read_handle(),pair); verify(saved.value()) }}
    {{
        var access=domain.access_set(); access.include(a.read_handle())
        let saved=domain.snapshot_set(access)
        verify(domain.read_snapshot(&saved,a.read_handle(),read))
    }}
    {{
        let next=domain.candidate_from(domain.snapshot_from(a.read_handle(),read),a.handle(),make,{value})
        var receipt=domain.commit_to(a.handle(),next)
        check.assert(match receipt.wait(){{Ok(_)=>true,_=>false}},&message)
    }}
    {{
        let first=domain.candidate_from(domain.snapshot_pair(a.read_handle(),b.read_handle(),pair),a.handle(),seed,0)
        let next=domain.candidate_pair_from(first,b.handle(),make,{value})
        var receipt=domain.commit_pair_to(a.handle(),b.handle(),next)
        check.assert(match receipt.wait(){{Ok(_)=>true,_=>false}},&message)
    }}
    {{
        var access=domain.access_set(); access.include(a.read_handle()); access.include(b.read_handle())
        let first=domain.candidate_set_from(domain.snapshot_set(access),a.handle(),make,{value})
        let next=domain.candidate_set_add(first,b.handle(),make,{value})
        var receipt=domain.commit_set(next)
        check.assert(match receipt.wait(){{Ok(_)=>true,_=>false}},&message)
    }}
    verify(domain.read_from(a.read_handle(),read).unwrap())
    verify(domain.read_from(b.read_handle(),read).unwrap())
    let timer=domain.timer(); let t=timer.handle()
    domain.after(t,a.handle(),0ms,tick,{value}).unwrap()
    while match t.status() {{Active=>true,_=>false}} {{task.yield_now()}}
    check.assert(domain.read_from(a.read_handle(),count).unwrap()==1,&message)
    domain.every(t,a.handle(),100000ms,tick,{value}).unwrap(); t.cancel()
}}
"#));
        work.assert_all_backends("bound.jk", &[], b"", 0);
    }
}

#[test]
#[cfg(unix)]
fn domain_queries_run_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "queries.jk",
        include_str!("../fixtures/state_domain_queries.jk"),
    );
    work.assert_native("queries.jk", &[], b"", 0);
}

#[test]
#[cfg(unix)]
fn domain_create_values_run_and_fail_in_all_backends() {
    let source = include_str!("../fixtures/state_domain_create_values.jk");
    let work = Workspace::new();
    work.write("create_values.jk", source);
    work.assert_all_backends("create_values.jk", &[], b"", 0);
    work.write(
        "create_failure.jk",
        &source.replace("require(lifecycle()==24)", "failure();"),
    );
    success(&work.cli(&["build", "create_failure.jk"]));
    for output in [
        work.cli(&["run", "create_failure.jk"]),
        work.cli(&["run", "--interpreter", "create_failure.jk"]),
        Command::new(work.path().join("create_failure"))
            .output()
            .unwrap(),
    ] {
        failure(&output, "Division by zero");
        assert!(output.stdout.is_empty());
    }
}

#[test]
#[cfg(unix)]
fn domain_candidate_values_run_and_fail_in_all_backends() {
    let source = include_str!("../fixtures/state_domain_candidate_values.jk");
    let work = Workspace::new();
    work.write("candidate_values.jk", source);
    work.assert_all_backends("candidate_values.jk", &[], b"", 0);
    for (entry, from, to) in [
        (
            "construct_single",
            ", make, input(24)",
            ", fail_make, input(0)",
        ),
        (
            "construct_pair",
            ", make_scalar, input(24)",
            ", fail_scalar, input(0)",
        ),
        ("construct_set", ",make,input(24)", ",fail_make,input(0)"),
        (
            "construct_set",
            ",make_scalar,input(24)",
            ",fail_scalar,input(0)",
        ),
    ] {
        assert!(source.contains(from), "Missing failure injection: {from}");
        work.write(
            "candidate_failure.jk",
            &source
                .replace("require(lifecycle()==24)", &format!("{entry}();"))
                .replace(from, to),
        );
        success(&work.cli(&["build", "candidate_failure.jk"]));
        for output in [
            work.cli(&["run", "candidate_failure.jk"]),
            work.cli(&["run", "--interpreter", "candidate_failure.jk"]),
            Command::new(work.path().join("candidate_failure"))
                .output()
                .unwrap(),
        ] {
            failure(&output, "Division by zero");
            assert!(output.stdout.is_empty());
        }
    }
}

#[test]
#[cfg(unix)]
fn domain_candidate_values_pin_modules_and_conflict_after_publication() {
    let old = include_str!("../fixtures/late_binding_candidates.jk");
    let new = old.replace("State{n:p.n}", "State{n:p.n+1000}");
    let old = old.replace('{', "{{").replace('}', "}}");
    let new = new.replace('{', "{{").replace('}', "}}");
    for create in ["new", "interpreter"] {
        let work = Workspace::new();
        work.write(
            "candidate_reload.jk",
            &format!(
                r#"import joky/domain import joky/vm import joky/task import joky/check
fn assert(ok:Bool) {{let message="candidate versions"; check.assert(ok,&message)}}
fn control(h:&vm.Vm,directory:domain.DirectoryHandle) @suspends {{
    var attempts=0
    while !h.call::<domain.DirectoryHandle,Bool>("ready",directory) {{
        attempts=attempts+1; assert(attempts<1000); task.yield_now()
    }}
    let next={new:?}
    assert(vm.load_source(h,&next)==0)
    assert(h.call::<Int32,Int32>("probe",7)==1008)
    assert(vm.publish_domain_code(h)==3)
    assert(h.call::<domain.DirectoryHandle,Bool>("release",directory))
}}
fn main() @suspends {{
    let h=vm.Vm.{create}(); let source={old:?}
    assert(vm.load_source(h,&source)==0)
    let owner=domain.directory(1); let directory=owner.handle()
    join {{ h.call::<domain.DirectoryHandle,Unit>("hold",directory), control(&h,directory) }};
    assert(vm.module_count(h)==1)
    assert(vm.close(h)==0)
}}"#
            ),
        );
        work.assert_all_backends("candidate_reload.jk", &[], b"", 0);
    }
}

#[test]
#[cfg(unix)]
fn domain_snapshot_values_run_and_fail_in_all_backends() {
    let source = include_str!("../fixtures/state_domain_snapshot_values.jk");
    let work = Workspace::new();
    work.write("snapshots.jk", source);
    work.assert_all_backends("snapshots.jk", &[], b"", 0);
    for (entry, from, to) in [
        ("construct_single", ",inspect)", ",broken)"),
        ("construct_pair", ",pair)", ",broken_pair)"),
        ("construct_set", ",inspect)", ",broken)"),
    ] {
        work.write(
            "snapshot_failure.jk",
            &source
                .replace("assert(lifecycle()==24)", &format!("{entry}();"))
                .replace(from, to),
        );
        success(&work.cli(&["build", "snapshot_failure.jk"]));
        for output in [
            work.cli(&["run", "snapshot_failure.jk"]),
            work.cli(&["run", "--interpreter", "snapshot_failure.jk"]),
            Command::new(work.path().join("snapshot_failure"))
                .output()
                .unwrap(),
        ] {
            failure(&output, "Division by zero");
            assert!(output.stdout.is_empty());
        }
    }
}

#[test]
#[cfg(unix)]
fn domain_snapshot_values_keep_frozen_results_across_publication() {
    let old = include_str!("../fixtures/late_binding_snapshots.jk");
    let new = old
        .replace("n:s.n,", "n:s.n+100,")
        .replace("n:a.n+b.n,", "n:a.n+b.n+100,");
    let old = old.replace('{', "{{").replace('}', "}}");
    let new = new.replace('{', "{{").replace('}', "}}");
    for create in ["new", "interpreter"] {
        let work = Workspace::new();
        work.write(
            "snapshot_reload.jk",
            &format!(
                r#"import joky/domain import joky/vm import joky/task import joky/check
fn assert(ok:Bool) {{let message="snapshot versions"; check.assert(ok,&message)}}
fn control(h:&vm.Vm,directory:domain.DirectoryHandle) @suspends {{
    var attempts=0
    while !h.call::<domain.DirectoryHandle,Bool>("ready",directory) {{
        attempts=attempts+1; assert(attempts<1000); task.yield_now()
    }}
    let next={new:?}
    assert(vm.load_source(h,&next)==0)
    assert(vm.publish_domain_code(h)==2)
    assert(h.call::<domain.DirectoryHandle,Bool>("release",directory))
}}
fn main() @suspends {{
    let h=vm.Vm.{create}(); let source={old:?}
    assert(vm.load_source(h,&source)==0)
    let owner=domain.directory(1); let directory=owner.handle()
    join {{ h.call::<domain.DirectoryHandle,Unit>("hold",directory), control(&h,directory) }};
    assert(vm.module_count(h)==1)
    assert(vm.close(h)==0)
}}"#
            ),
        );
        work.assert_all_backends("snapshot_reload.jk", &[], b"", 0);
    }
}

#[test]
fn domain_request_values_run_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "request_values.jk",
        include_str!("../fixtures/state_domain_request_values.jk"),
    );
    work.assert_all_backends("request_values.jk", &[], b"", 0);
}

#[test]
fn domain_request_values_pin_started_tasks_and_check_new_contracts() {
    let original = include_str!("../fixtures/late_binding_request_values.jk");
    for incompatible in [false, true] {
        let old = if incompatible {
            original.replace(
                "assert(fresh.unwrap().n==9)",
                "assert(match fresh {Err(domain.RequestError.InvalidAccess)=>true,_=>false})",
            )
        } else {
            original.to_owned()
        };
        let new = old.replace("{1}", "{2}").replace("s.n+0", "s.n+100");
        let new = if incompatible {
            new.replace("pub n:Int32,tag", "n:Int32,tag")
        } else {
            new
        };
        let old = old.replace('{', "{{").replace('}', "}}");
        let new = new.replace('{', "{{").replace('}', "}}");
        for create in ["new", "interpreter"] {
            let work = Workspace::new();
            work.write("request_reload.jk",&format!(r#"import joky/domain import joky/vm import joky/task import joky/check
fn assert(ok:Bool) {{let message="request publication"; check.assert(ok,&message)}}
fn control(h:&vm.Vm,directory:domain.DirectoryHandle) @suspends {{
    var attempts=0
    while !h.call::<domain.DirectoryHandle,Bool>("ready",directory) {{attempts=attempts+1; assert(attempts<1000); task.yield_now()}}
    let next={new:?}
    assert(vm.load_source(h,&next)==0)
    assert(vm.publish_domain_code(h)==1)
    assert(h.call::<domain.DirectoryHandle,Bool>("release",directory))
}}
fn main() @suspends {{
    let h=vm.Vm.{create}(); let source={old:?}
    assert(vm.load_source(h,&source)==0)
    let owner=domain.directory(1); let directory=owner.handle()
    join {{ h.call::<domain.DirectoryHandle,Unit>("hold",directory), control(&h,directory) }};
    assert(vm.module_count(h)==1)
    assert(vm.close(h)==0)
}}"#));
            work.assert_all_backends("request_reload.jk", &[], b"", 0);
        }
    }
}

#[test]
fn domain_timer_values_run_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "timer_values.jk",
        include_str!("../fixtures/domain_timer_values.jk"),
    );
    work.assert_all_backends("timer_values.jk", &[], b"", 0);
    work.write(
        "precise_values.jk",
        &format!(
            "{}\nfn main() @suspends {{ assert(precision(250us.as_nanos()) == 42) }}",
            include_str!("../fixtures/domain_timer_values.jk").replace(
                "fn main() @suspends ->Int32",
                "fn lifecycle() @suspends ->Int32"
            )
        ),
    );
    work.assert_native("precise_values.jk", &[], b"", 0);
}

#[test]
fn domain_timer_values_script_calls_rebind_and_reject_changed_fields() {
    let template = r#"import joky/domain import joky/task import joky/check
struct Room {value:Int32}
struct Payload {pub step:Int32,tag:UInt64}
fn make(value:Int32)->Room {Room{value:value}}
fn read(room:&Room)->Int32 {room.value}
fn tick(room:&mut Room,p:Payload) {room.value=room.value+p.step}
fn assert(ok:Bool) {let message="timer record publication"; check.assert(ok,&message)}
fn hold(t:domain.TimerHandle) @suspends {
    let room=domain.create(make,10); let h=room.handle()
    var p=Payload{step:2,tag:18446744073709551615}
    assert(domain.after(t,h,100000ms,tick,p).is_ok())
    while match t.status(){Active=>true,_=>false} {task.yield_now()}
    assert(match t.status(){Cancelled=>true,_=>false})
    let result=domain.after(t,h,0ms,tick,p)
    p.step=99
    if INCOMPATIBLE {
        assert(match result {Err(InvalidAccess)=>true,_=>false})
        assert(domain.read_from(room.read_handle(),read).unwrap()==10)
    } else {
        assert(result.is_ok())
        while match t.status(){Active=>true,_=>false} {task.yield_now()}
        assert(match t.status(){Completed=>true,_=>false})
        assert(domain.read_from(room.read_handle(),read).unwrap()==30)
    }
}"#;
    for incompatible in [false, true] {
        let source = template.replace("INCOMPATIBLE", if incompatible { "true" } else { "false" });
        let next = source.replace("room.value+p.step", "room.value+p.step*10");
        let next = if incompatible {
            next.replace("pub step:Int32", "step:Int32")
        } else {
            next
        };
        let source = source.replace('{', "{{").replace('}', "}}");
        let next = next.replace('{', "{{").replace('}', "}}");
        for create in ["new", "interpreter"] {
            let work = Workspace::new();
            work.write("timer_reload.jk",&format!(r#"import joky/domain import joky/vm import joky/task import joky/check
fn assert(ok:Bool) {{let message="timer host"; check.assert(ok,&message)}}
fn control(h:&vm.Vm,t:domain.TimerHandle) @suspends {{
    while match t.status(){{Idle=>true,_=>false}} {{task.yield_now()}}
    let next={next:?}; assert(vm.load_source(h,&next)==0); assert(vm.publish_domain_code(h)==1); t.cancel()
}}
fn main() @suspends {{
    let timer=domain.timer(); let t=timer.handle()
    let h=vm.Vm.{create}(); let source={source:?}
    assert(vm.load_source(h,&source)==0)
    join {{ h.call::<domain.TimerHandle,Unit>("hold",t), control(&h,t) }};
    assert(vm.module_count(h)==1)
    assert(vm.close(h)==0)
}}"#));
            work.assert_all_backends("timer_reload.jk", &[], b"", 0);
        }
    }
}

#[test]
fn state_domains_submission_contract_runs_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "submission.jk",
        include_str!("../fixtures/state_domain_submission.jk"),
    );
    work.assert_all_backends("submission.jk", &[], b"", 0);
    let reference = include_str!("../../docs/language.md");
    let example = reference
        .split("## 提交式出站信箱")
        .nth(1)
        .unwrap()
        .split("```joky\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    work.write(
        "documented_submission.jk",
        &format!(
            r#"{example}
fn main() @suspends ->Int32 {{
    let owner=domain.create(make,0); let mailbox=outbox.create(1,4096)
    var receipt=update(owner.handle(),mailbox.handle()); receipt.wait().unwrap();
    domain.read_from(owner.read_handle(),read).unwrap()-42
}}"#
        ),
    );
    work.assert_all_backends("documented_submission.jk", &[], b"", 0);
}

#[test]
#[cfg(unix)]
fn state_domains_storage_owning_graphs_run_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "storage.jk",
        include_str!("../fixtures/state_domain_storage.jk"),
    );
    work.assert_native("storage.jk", &[], b"", 0);
}

#[test]
#[cfg(unix)]
fn state_domains_storage_editor_and_multi_failures_run_in_all_backends() {
    let work = Workspace::new();
    let source = include_str!("../fixtures/state_domain_storage.jk")
        .split("fn lifecycle()")
        .next()
        .unwrap();
    for body in [
        "domain.write_pair(a.handle(),b.handle(),fail_swap,0).unwrap();",
        "domain.update_pair(a.read_handle(),b.handle(),fail_mixed,0).unwrap();",
        "let candidate=domain.edit_snapshot(capture(a.read_handle(),b.read_handle(),b.read_handle()),a.handle(),broken,0)",
    ] {
        work.write("failure.jk", &format!("{source} fn broken(s:&mut State,n:Int32){{edit(s,1/n);}} fn main() @suspends {{let a=domain.create(make,1); let b=domain.create(make,2); {body}}}"));
        let rejected=|output:std::process::Output| {
            assert!(!output.status.success());
            let error=String::from_utf8_lossy(&output.stderr);
            assert!(error.contains("Division by zero") && !error.contains("panicked"),"{error}");
        };
        rejected(work.cli(&["run","failure.jk"]));
        rejected(work.cli(&["run","--interpreter","failure.jk"]));
        success(&work.cli(&["build","failure.jk"]));
        rejected(Command::new(work.path().join("failure")).output().unwrap());
    }
}

#[test]
#[cfg(unix)]
fn state_domains_when_syntax_and_methods_run_in_standalone_aot() {
    let work = Workspace::new();
    work.write("when.jk", include_str!("../fixtures/state_domain_when.jk"));
    work.assert_native("when.jk", &[], b"", 0);
}

#[test]
#[cfg(unix)]
fn state_domains_when_access_boundaries_run_in_all_backends() {
    let mut source = String::from("import joky/domain import joky/mem import joky/check import joky/array.{type Array} struct S{n:Int32,text:String} fn make(n:Int32)->S{S{n:n,text:\"owned\"}} fn assert(ok:Bool){check.assert(ok,\"when boundary\")} ");
    source.push_str("fn push(owners:&mut Array<domain.Owner<S>>,n:Int32) @suspends ->domain.WriteHandle<S>{let o=domain.create(make,n); let h=o.handle(); owners.push(o); h} ");
    for count in [1, 2, 3, 8, 64] {
        source.push_str(&format!(
            "fn run{count}() @suspends {{var owners=Array.new::<domain.Owner<S>>(); "
        ));
        for i in 0..count {
            source.push_str(&format!("let h{i}=push(&mut owners,{i}); "));
        }
        let accesses = (0..count)
            .map(|i| format!("mut h{i}"))
            .collect::<Vec<_>>()
            .join(",");
        source.push_str(&format!("let r=when ({accesses}) {{"));
        for i in 0..count {
            source.push_str(&format!("h{i}.n+=1; "));
            if i > 0 {
                source.push_str(&format!("mem.swap(&mut h0.text,&mut h{i}.text); "));
            }
        }
        source.push_str("h0.n}; assert(r.unwrap()==1) ");

        source.push_str("} ");
    }
    source.push_str("fn main() @suspends {run1(); run2(); run3(); run8(); run64()}");
    let work = Workspace::new();
    work.write("many_when.jk", &source);
    work.assert_all_backends("many_when.jk", &[], b"", 0);
}

#[test]
fn state_domains_when_reject_invalid_syntax_captures_and_effects() {
    let work = Workspace::new();
    let prefix =
        "import joky/domain import joky/outbox struct S{n:Int32} fn make(n:Int32)->S{S{n:n}}";
    for (extra, body, expected) in [
        ("", "when \"\" (h) {h.n};", "must not be empty"),
        ("", "when () {};", "1 to 64"),
        ("", "when (h,h) {};", "Duplicate binding"),
        ("", "let d=domain.directory(2); let routes=d.handle(); when (h, s?:S=routes[1]) {};", "else branch"),
        ("", "let d=domain.directory(2); let routes=d.handle(); when (s?:S=routes[1], h) {} else {};", "must be last"),
        ("fn key()->UInt64{1}", "let d=domain.directory(2); let routes=d.handle(); when (h,s:S=routes[key()]) {};", "UInt64/String keys"),
        ("", "let d=domain.directory(2); let routes=d.handle(); when (h,s:S=routes[h.n]) {};", "UInt64/String keys"),
        ("", "let d=domain.directory(2); let routes=d.handle(); when (h,s?:S=routes[1]) {s.n} else {s.n};", "Undefined"),
        ("", "let d=domain.directory(2); let routes=d.handle(); let text=\"owned\"; when (h,s:S=routes[1]) {let owned:String=text; owned};", "String"),

        ("", "let d=domain.directory(2); let routes=d.handle(); when (h,s?:S=routes.get_or_stage(1,make(1))) {} else {};", "cannot also be optional"),
        ("", "let d=domain.directory(2); let routes=d.handle(); when (s:S=routes.get_or_stage(1,make(1)), t:S=routes[s.n]) {};", "creation fallback"),
        ("fn noisy()->S{println(1);make(1)}", "let d=domain.directory(2); let routes=d.handle(); when (s:S=routes.get_or_stage(1,noisy())) {};", "proved synchronous"),
        ("fn noisy(s:&S)->Bool{println(s.n);true}", "let d=domain.directory(2); let name=\"room\"; domain.remove_named_if(d.handle(),&name,noisy);", "proved synchronous"),
        ("", "when (o.handle()) {};", "Expected"),
        (
            "",
            "when \"same\" (h) {}; when \"same\" (h) {};",
            "Duplicate when label",
        ),
        (
            "",
            "if true { when \"same\" (h) {}; } else { when \"same\" (h) {}; }",
            "Duplicate when label",
        ),
        ("", "when (h) {return 1};", "cannot contain return"),
        ("", "while true {when (h) {break};}", "break"),
        ("", "when (h) {when (h) {}};", "cannot be nested"),
        ("", "let ro=h.read_only(); when (mut ro) {};", "WriteHandle"),
        ("", "when (h) {h.n=2};", "shared"),
        ("", "when (mut h) {println(1)};", "proved synchronous"),
        (
            "fn emit(s:&mut S,target:outbox.Handle){let bytes=\"x\".into_bytes(); outbox.emit(target,&bytes)}",
            "let q=outbox.create(1,8); domain.write_to(h,emit,q.handle());",
            "proved",
        ),
        (
            "",
            "let text=\"owned\"; let view=&text; when (h) {view};",
            "value packet",
        ),
        (
            "",
            "let text=\"owned\"; let view=&text; when (h) {view.byte_length()};",
            "value packet",
        ),
        ("", "var n=1; let r=&n; when (h) {*r};", "value packet"),
        (
            "fn get(n:Int32)->Int32{n}",
            "let f=get; when (h) {f(h.n)};",
            "value packet",
        ),
        ("", "when (h) {&h.n};", "bounded value"),
        (
            "impl S {fn add(&mut self,n:Int32){self.n+=n}}",
            "let ro=h.read_only(); ro.add(2);",
            "WriteHandle",
        ),
        (
            "impl S {fn generic<T>(&self,n:T)->T{n}}",
            "h.generic(2);",
            "Generic domain handle methods",
        ),
        (
            "impl S {fn take(self)->Int32{self.n}}",
            "h.take();",
            "borrowed state receiver",
        ),
        (
            "impl S {fn noisy(&self){println(1)}}",
            "h.noisy();",
            "proved synchronous",
        ),
        ("#state struct Bad{f:fn()->Int32}", "", "#state requires"),
        ("#state struct Bad<T>{n:T}", "", "nongeneric"),
        ("#state(1) struct Bad{n:Int32}", "", "accepts no arguments"),
        ("#state #state struct Bad{n:Int32}", "", "Duplicate #state"),
        (
            "fn generic<T>(h:domain.ReadHandle<S>,v:T) @suspends {when(h){h.n};}",
            "",
            "nongeneric",
        ),
    ] {
        work.write("invalid_when.jk", &format!("{prefix} {extra} fn main() @suspends {{let o=domain.create(make,1); let h=o.handle(); {body}}}"));
        let output = work.cli(&["build", "invalid_when.jk"]);
        assert!(!output.status.success(), "{body} {extra}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            !error.contains("panicked") && error.contains(expected),
            "{body} {extra}: {error}"
        );
    }
    let bindings = (0..65)
        .map(|i| format!("let h{i}=o.handle(); "))
        .collect::<String>();
    let accesses = (0..65)
        .map(|i| format!("h{i}"))
        .collect::<Vec<_>>()
        .join(",");
    work.write("too_many_when.jk", &format!("{prefix} fn main() @suspends {{let o=domain.create(make,1); {bindings}; when({accesses}){{}};}}"));
    failure(&work.cli(&["build", "too_many_when.jk"]), "1 to 64");
}

#[test]
#[cfg(unix)]
fn state_domains_when_old_flows_bind_latest_script_blocks_in_all_backends() {
    let old = include_str!("../fixtures/late_binding_when.jk");
    let second = old
        .replace("x.n-=amount", "x.n-=amount*10")
        .replace("y.n+=amount", "y.n+=amount*10");
    let third = old
        .replace("x.n-=amount", "x.n-=amount*100")
        .replace("y.n+=amount", "y.n+=amount*100");
    let escape = |s: &str| s.replace('{', "{{").replace('}', "}}");
    let (old, second, third) = (escape(old), escape(&second), escape(&third));
    for create in ["new", "interpreter"] {
        let work = Workspace::new();
        work.write("when_reload.jk", &format!(r#"import joky/domain import joky/vm import joky/task import joky/check
fn assert(ok:Bool) {{check.assert(ok,"when Script")}}
fn control(h:&vm.Vm,directory:domain.DirectoryHandle) @suspends {{
    var attempts=0
    while h.call::<domain.DirectoryHandle,Int32>("probe",directory)==-1 {{
        attempts+=1; assert(attempts<1000); task.yield_now()
    }}
    let second={second:?}; assert(vm.load_source(h,&second)==0); assert(vm.publish_domain_code(h)==2)
    let third={third:?}; assert(vm.load_source(h,&third)==0); assert(vm.publish_domain_code(h)==2)
    assert(h.call::<domain.DirectoryHandle,Bool>("release",directory))
}}
fn main() @suspends {{
    let h=vm.Vm.{create}(); let source={old:?}; assert(vm.load_source(h,&source)==0)
    let owner=domain.directory(2); let directory=owner.handle()
    join {{ h.call::<domain.DirectoryHandle,Unit>("hold",directory), control(&h,directory) }};
    assert(vm.module_count(h)==1); assert(vm.close(h)==0)
}}"#));
        work.assert_all_backends("when_reload.jk", &[], b"", 0);
    }
}

#[test]
#[cfg(unix)]
fn state_domains_when_capture_limits_include_the_outer_packet() {
    let work = Workspace::new();
    let mut accepted = String::from("import joky/domain import joky/check struct S{n:Int32} fn make(n:Int32)->S{S{n:n}} fn assert(ok:Bool){check.assert(ok,\"capture limits\")} ");
    for (index, fields) in [63, 64].into_iter().enumerate() {
        let names = (0..fields)
            .map(|i| format!("f{i}:Int32"))
            .collect::<Vec<_>>()
            .join(",");
        let values = (0..fields)
            .map(|i| format!("f{i}:1"))
            .collect::<Vec<_>>()
            .join(",");
        let code = format!("struct P{index}{{{names}}} fn run{index}() @suspends {{let o=domain.create(make,1); let h=o.handle(); let p=P{index}{{{values}}}; let r=when(h){{p.f0+h.n}}; assert(r.unwrap()==2)}}");
        if fields == 63 {
            accepted.push_str(&code);
        } else {
            work.write("invalid_packet.jk",&format!("import joky/domain import joky/check struct S{{n:Int32}} fn make(n:Int32)->S{{S{{n:n}}}} fn assert(ok:Bool){{check.assert(ok,\"limit\")}} {code} fn main() @suspends {{run{index}()}}"));
            failure(
                &work.cli(&["build", "invalid_packet.jk"]),
                "64 contract members",
            );
        }
    }
    accepted.push_str("fn main() @suspends {run0()}");
    work.write("capture_limits.jk", &accepted);
    work.assert_all_backends("capture_limits.jk", &[], b"", 0);
}

#[test]
#[cfg(unix)]
fn state_domains_when_imported_blocks_and_body_failures_match_backends() {
    let work = Workspace::new();
    work.write("room.jk", r#"import joky/domain
#state pub struct S{pub n:Int32}
pub fn make(n:Int32)->S{S{n:n}}
impl S {pub fn value(&self)->Int32{self.n}}
pub fn probe(h:domain.ReadHandle<S>) @suspends ->Int32{h.value().unwrap()}
pub fn update(h:domain.WriteHandle<S>,n:Int32) @suspends ->Int32 {when "更新"(mut h){h.n+=n;h.n}.unwrap()}
"#);
    work.write("imports.jk", "import room import joky/domain fn main() @suspends ->Int32 {let o=domain.create(room.make,1); room.update(o.handle(),2); room.probe(o.read_handle())+o.read_handle().value().unwrap()}");
    work.assert_all_backends("imports.jk", &[], b"", 6);
    work.write("failure.jk", "import room import joky/domain fn main() @suspends {let a=domain.create(room.make,1); let b=domain.create(room.make,2); let x=a.handle(); let y=b.handle(); let zero=0; when(mut x,mut y){x.n=10; y.n=20; x.n/zero};}");
    for args in [
        ["run", "failure.jk"].as_slice(),
        ["run", "--interpreter", "failure.jk"].as_slice(),
    ] {
        failure(&work.cli(args), "Division by zero");
    }
    success(&work.cli(&["build", "failure.jk"]));
    failure(
        &Command::new(work.path().join("failure")).output().unwrap(),
        "Division by zero",
    );
}

#[test]
#[cfg(unix)]
fn pure_enum_domain_values_run_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "enum_values.jk",
        include_str!("../fixtures/state_domain_enum_values.jk"),
    );
    work.assert_native("enum_values.jk", &[], b"", 0);
}

#[test]
#[cfg(unix)]
fn pure_composite_domain_values_run_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "composites.jk",
        include_str!("../fixtures/state_domain_composite_values.jk"),
    );
    work.assert_native("composites.jk", &[], b"", 0);
}

#[test]
#[cfg(unix)]
fn every_declarations_run_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "every.jk",
        &include_str!("../fixtures/domain_every.jk")
            .replace("#every(100000ms)", "#every(250us)")
            .replace("{lifecycle()}", "{lifecycle(); live(); failure()}"),
    );
    work.assert_all_backends("every.jk", &[], b"", 0);
}

#[test]
#[cfg(unix)]
fn every_declarations_reject_invalid_entries() {
    let work = Workspace::new();
    for method in [
        "fn tick(){}",
        "fn tick(self){}",
        "fn tick(&self){}",
        "fn tick(&mut self) @suspends {}",
        "fn tick(&mut self)->Int32{1}",
        "fn tick<T>(&mut self){}",
        "fn tick(&mut self,dt:Int32){}",
        "fn tick(&mut self,p:String){}",
        "fn tick(&mut self,t:&mut Room){}",
        "fn tick(&mut self){let n=self.value; io.println(&n)}",
        "fn tick(&mut self){helper(self)}",
    ] {
        work.write("invalid.jk", &format!("import joky/io #state struct Room{{value:Int32}} fn helper(s:&mut Room){{let n=s.value; io.println(&n)}} impl Room{{#every(1ms) {method}}} fn main(){{}}"));
        failure(&work.cli(&["build", "invalid.jk"]), "#every requires");
    }
    for source in [
        "struct Room{value:Int32} impl Room{#every(1ms) fn tick(&mut self){}}",
        "#state struct Room{value:Int32} #every(1ms) fn tick(s:&mut Room){}",
        "struct Room<T>{value:T} impl<T> Room<T>{#every(1ms) fn tick(&mut self){}}",
        "#state struct Room{value:Int32} trait Tick{fn tick(&mut self);} impl Tick for Room{#every(1ms) fn tick(&mut self){}}",
    ] {
        work.write("invalid.jk",&format!("{source} fn main(){{}}"));
        failure(&work.cli(&["build","invalid.jk"]),"error:");
    }
}

#[test]
#[cfg(unix)]
fn every_declarations_import_aliases_preserve_automatic_methods() {
    let work = Workspace::new();
    work.write(
        "room.jk",
        r#"#state pub struct Room{pub n:Int32}
pub fn make(n:Int32)->Room{Room{n:n}}
fn increment(room:&mut Room){room.n+=1}
impl Room{#every(1ms) fn tick(&mut self){increment(self)}}
pub fn read(room:&Room)->Int32{room.n}"#,
    );
    work.write("imported.jk",r#"import room.{make as open} import joky/domain import joky/task import joky/check
fn main() @suspends {let o=domain.create(open,0)
while domain.read_from(o.read_handle(),room.read).unwrap()<1 {task.yield_now()}
check.assert(match domain.periodic_status(o.read_handle(),"tick"){Active=>true,_=>false},"imported every")}"#);
    work.assert_all_backends("imported.jk", &[], b"", 0);
}

#[test]
#[cfg(unix)]
fn every_declarations_script_publication_updates_automatic_methods() {
    let base = r#"import joky/domain import joky/task
#state struct Room{value:Int32}
impl Room{#every(100000ms) fn tick(&mut self){self.value=10}}
fn make(n:Int32)->Room{Room{value:n}}
fn read(room:&Room)->Int32{room.value}
fn hold(d:domain.DirectoryHandle) @suspends {
 let room=domain.create(make,10); d.insert(0,room.handle()).unwrap()
 while true{task.yield_now()}
}
fn probe(d:domain.DirectoryHandle) @suspends ->Int32 {
 match domain.lookup::<Room>(d,0){Some(h)=>domain.read_from(h,read).unwrap(),None=>-1}
}
fn status(d:domain.DirectoryHandle) @suspends ->Int32 {
 match domain.lookup::<Room>(d,0){Some(h)=>match domain.periodic_status(h,"tick"){Active=>1,InvalidAccess=>2,_=>0},None=>-1}
}"#;
    for action in ["add", "remove", "change"] {
        let old = if action == "add" {
            base.replace("#every(100000ms)", "")
        } else {
            base.into()
        };
        let next = if action == "remove" {
            base.replace("#every(100000ms)", "")
        } else {
            base.replace("#every(100000ms)", "#every(1ms)")
                .replace("self.value=10", "self.value=42")
        };
        let expected = if action == "remove" { 10 } else { 42 };
        let status = if action == "remove" { 2 } else { 1 };
        let quote = |s: &str| format!("{:?}", s.replace('{', "{{").replace('}', "}}"));
        for create in ["new", "interpreter"] {
            let work = Workspace::new();
            work.write(
                "reload.jk",
                &format!(
                    r#"import joky/domain import joky/vm import joky/task import joky/check
fn assert(ok:Bool){{check.assert(ok,"automatic periodic publication")}}
fn control(h:&vm.Vm,d:domain.DirectoryHandle) @suspends {{
 while h.call::<domain.DirectoryHandle,Int32>("probe",d)==-1{{task.yield_now()}}
 let next={next}; assert(vm.load_source(h,&next)==0)
 assert(vm.publish_domain_code(h)==1); assert(vm.module_count(h)==2)
 while h.call::<domain.DirectoryHandle,Int32>("probe",d)!={expected}{{task.yield_now()}}
 assert(h.call::<domain.DirectoryHandle,Int32>("status",d)=={status})
}}
fn main() @suspends {{
 let owner=domain.directory(1); let d=owner.handle(); let h=vm.Vm.{create}()
 let source={old}; assert(vm.load_source(h,&source)==0)
 race{{h.call::<domain.DirectoryHandle,Unit>("hold",d), control(&h,d)}};
 assert(vm.module_count(h)==1)
 assert(h.call::<domain.DirectoryHandle,Int32>("probe",d)==-1)
 assert(vm.close(h)==0)
}}"#,
                    next = quote(&next),
                    old = quote(&old)
                ),
            );
            work.assert_all_backends("reload.jk", &[], b"", 0);
        }
    }
}

#[test]
#[cfg(unix)]
fn every_declarations_migration_runs_in_all_host_and_script_backends() {
    let old = r#"import joky/domain import joky/task
#state struct Room{value:Int32}
impl Room{#every(100000ms) fn tick(&mut self){self.value+=1}}
fn make(n:Int32)->Room{Room{value:n}}
fn read(room:&Room)->Int32{room.value}
fn hold(d:domain.DirectoryHandle) @suspends {let room=domain.create(make,10); d.insert(0,room.handle()).unwrap(); while true{task.yield_now()}}
fn probe(d:domain.DirectoryHandle) @suspends ->Int32 {match domain.lookup::<Room>(d,0){Some(h)=>domain.read_from(h,read).unwrap(),None=>-1}}"#;
    let next = r#"import joky/domain
#state struct Room{value:Int64}
impl Room{#every(1ms) fn tick(&mut self){self.value=42}}
fn upgrade(old:&PreviousState)->Room{Room{value:int64(old.value)}}
fn read(room:&Room)->Int32{int32(room.value)}
fn probe(d:domain.DirectoryHandle) @suspends ->Int32 {match domain.lookup::<Room>(d,0){Some(h)=>domain.read_from(h,read).unwrap(),None=>-1}}"#;
    let quote = |s: &str| format!("{:?}", s.replace('{', "{{").replace('}', "}}"));
    for create in ["new", "interpreter"] {
        let work = Workspace::new();
        work.write(
            "migrate.jk",
            &format!(
                r#"import joky/domain import joky/vm import joky/task import joky/check
fn assert(ok:Bool){{check.assert(ok,"automatic migration")}}
fn control(h:&vm.Vm,d:domain.DirectoryHandle) @suspends {{
 while h.call::<domain.DirectoryHandle,Int32>("probe",d)==-1{{task.yield_now()}}
 let schema=vm.domain_schema(h); let next={next}
 assert(vm.prepare_migration(h,schema,&next,"upgrade")==0)
 assert(h.call::<domain.DirectoryHandle,Int32>("probe",d)==10)
 assert(vm.publish_migration(h)==0)
 while h.call::<domain.DirectoryHandle,Int32>("probe",d)!=42{{task.yield_now()}}
}}
fn main() @suspends {{
 let owner=domain.directory(1); let d=owner.handle(); let h=vm.Vm.{create}()
 let old={old}; assert(vm.load_source(h,&old)==0)
 race{{h.call::<domain.DirectoryHandle,Unit>("hold",d), control(&h,d)}};
 assert(vm.module_count(h)==1)
 assert(h.call::<domain.DirectoryHandle,Int32>("probe",d)==-1)
 assert(vm.close(h)==0)
}}"#,
                next = quote(next),
                old = quote(old)
            ),
        );
        work.assert_all_backends("migrate.jk", &[], b"", 0);
    }
}

#[test]
#[cfg(unix)]
fn owned_text_domain_values_run_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "text_values.jk",
        include_str!("../fixtures/state_domain_text_values.jk"),
    );
    work.assert_native("text_values.jk", &[], b"", 0);
}

#[test]
#[cfg(unix)]
fn owned_array_domain_values_run_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "array_values.jk",
        include_str!("../fixtures/state_domain_array_values.jk"),
    );
    work.assert_native("array_values.jk", &[], b"", 0);
}

#[test]
#[cfg(unix)]
fn state_domains_paths_run_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "paths.jk",
        include_str!("../fixtures/state_domain_paths.jk"),
    );
    work.assert_native("paths.jk", &[], b"", 0);
}

#[test]
#[cfg(unix)]
fn state_domains_creation_runs_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "creation.jk",
        include_str!("../fixtures/state_domain_creation.jk"),
    );
    work.assert_native("creation.jk", &[], b"", 0);
}

#[test]
#[cfg(unix)]
fn state_domains_optional_paths_run_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "optional.jk",
        include_str!("../fixtures/state_domain_optional.jk"),
    );
    work.assert_native("optional.jk", &[], b"", 0);
}

#[test]
#[cfg(unix)]
fn state_domains_outcome_runs_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "outcome.jk",
        include_str!("../fixtures/state_domain_outcome.jk"),
    );
    work.assert_native("outcome.jk", &[], b"", 0);
}
