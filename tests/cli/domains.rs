//! State-domain operations, publication and migration.
use super::*;

#[test]
#[cfg(unix)]
fn state_domain_probe_owned_snapshots_replacement_and_cancellation_run_on_all_backends() {
    let work = Workspace::new();
    work.write(
        "probe.jk",
        include_str!("../fixtures/state_domain_probe.jk"),
    );
    work.assert_all_backends("probe.jk", &[], b"42\n77\n99\n", 0);
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
fn state_domains_snapshots_reject_forgery_and_unchecked_projections() {
    let work = Workspace::new();
    let prefix = r#"
        import joky/domain
        struct State { count: Int32 }
        fn make(value: Int32) -> State { State { count: value } }
        fn read(state: &State) -> Int32 { state.count }
    "#;
    for case in [
        "fn main() @suspends { let owner=domain.create(make,1); let snapshot=domain.snapshot_from(owner.read_handle(),read); let raw=snapshot.storage }",
        "fn main() @suspends { let owner=domain.create(make,1); let snapshot=domain.snapshot_from(owner.read_handle(),read); let moved=snapshot; snapshot.value(); moved.value(); }",
        "import joky/array.{type Array} fn project(state: &State) -> Array<Ptr> { Array.new::<Ptr>() } fn main() @suspends { let owner=domain.create(make,1); domain.snapshot_from(owner.read_handle(),project); }",
        "fn project(state: &mut State) -> Int32 { state.count=99; state.count } fn main() @suspends { let owner=domain.create(make,1); domain.snapshot_from(owner.read_handle(),project); }",
        "fn main() @suspends { let owner=domain.create(make,1); let operation=read; domain.snapshot_from(owner.read_handle(),operation); }",
        "fn main() @suspends { let factory=domain.snapshot_from::<State,Int32> }",
    ] {
        work.write("rejected.jk", &format!("{prefix}\n{case}"));
        let output = work.cli(&["build", "rejected.jk", "-o", "rejected"]);
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "accepted {case}");
        assert!(!error.contains("panicked"), "{error}");
        assert!(!error.contains("[parse"), "{error}");
    }
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
            task.sleep_ms(1)
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
fn state_domains_candidates_reject_forgery_reuse_and_unproved_constructors() {
    let work = Workspace::new();
    let prefix = r#"
        import joky/domain
        struct State { count: Int32 }
        struct Other { count: Int32 }
        fn make(value: Int32) -> State { State { count: value } }
        fn read(state: &State) -> Int32 { state.count }
    "#;
    let start = "let owner = domain.create(make, 1); let handle = owner.handle(); let snapshot = domain.snapshot_from(handle.read_only(), read)";
    for (extra, body) in [
        ("", "let candidate = domain.candidate_from(snapshot, handle, make, 2); snapshot.value();"),
        ("", "let candidate = domain.candidate_from(snapshot, handle, make, 2); domain.commit_to(handle, candidate); domain.commit_to(handle, candidate);"),
        ("", "let candidate = domain.candidate_from(snapshot, handle, make, 2); let storage = candidate.storage"),
        ("", "let candidate = domain.candidate_from(snapshot, handle, make, 2); domain.commit_to(handle.read_only(), candidate);"),
        ("", "let constructor = make; domain.candidate_from(snapshot, handle, constructor, 2);"),
        ("fn other(value: Int32) -> Other { Other { count: value } }", "domain.candidate_from(snapshot, handle, other, 2);"),
        ("import joky/array.{type Array} fn owns(value: Array<Ptr>) -> State { State { count: 1 } }", "domain.candidate_from(snapshot, handle, owns, Array.new::<Ptr>());"),
        ("fn noisy(value: Int32) -> State { println(value); make(value) }", "domain.candidate_from(snapshot, handle, noisy, 2);"),
        ("fn pending(value: Int32) @suspends -> State { make(value) }", "domain.candidate_from(snapshot, handle, pending, 2);"),
        ("", "let factory = domain.candidate_from::<State, Int32, State, Int32>"),
        ("", "let candidate = domain.candidate_from(snapshot, handle, make, 2); let receipt = domain.commit_to(handle, candidate); let storage = receipt.storage"),
    ] {
        work.write("rejected.jk", &format!("{prefix}\n{extra}\nfn main() @suspends {{ {start}; {body} }}"));
        let output = work.cli(&["build", "rejected.jk", "-o", "rejected"]);
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "accepted {body}");
        assert!(!error.contains("panicked"), "{error}");
        assert!(!error.contains("[parse"), "{error}");
    }
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
fn state_domains_owned_roots_reject_hooks_and_unsafe_graphs() {
    let work = Workspace::new();
    for definitions in [
        "import joky/drop.{type Drop} struct State { title: String } impl Drop for State { fn drop(&mut self) {} } fn make(n: Int32) -> State { State { title: \"owned\" } }",
        "import joky/drop.{type Drop} struct Temp { title: String } impl Drop for Temp { fn drop(&mut self) {} } struct State { title: String } fn make(n: Int32) -> State { let temporary = Temp { title: \"temp\" }; State { title: \"owned\" } }",
        "import joky/array.{type Array} import joky/drop.{type Drop} struct Item { name: String } impl Drop for Item { fn drop(&mut self) {} } struct State { items: Array<Item> } fn make(n: Int32) -> State { State { items: Array.new::<Item>() } }",
        "import joky/map.{type Map} import joky/drop.{type Drop} struct Item { name: String } impl Drop for Item { fn drop(&mut self) {} } struct State { items: Map<Int32,Item> } fn make(n: Int32) -> State { State { items: Map.new::<Int32,Item>() } }",
        "import joky/array.{type Array} struct Item { pointer: Ptr } struct State { items: Array<Item> } fn make(n: Int32) -> State { State { items: Array.new::<Item>() } }",
        "import joky/array.{type Array} struct Node { children: Array<Node> } struct State { root: Node } fn make(n: Int32) -> State { State { root: Node { children: Array.new::<Node>() } } }",
    ] {
        work.write("rejected.jk", &format!("import joky/domain\n{definitions}\nfn main() @suspends {{ let owner = domain.create(make, 7) }}"));
        let output = work.cli(&["build", "rejected.jk", "-o", "rejected"]);
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "accepted {definitions}");
        assert!(!error.contains("panicked"), "{error}");
        assert!(!error.contains("[parse"), "{error}");
        assert!(error.contains("proved synchronous"), "{error}");
    }
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
fn state_domains_deadlines_reject_negative_durations_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "negative.jk",
        "import joky/domain fn main() @suspends { let deadline = domain.deadline_after(-1) }",
    );
    let rejected = |output: std::process::Output| {
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr)
            .contains("Commit deadline duration must be nonnegative"));
    };
    rejected(work.cli(&["run", "negative.jk"]));
    rejected(work.cli(&["run", "--interpreter", "negative.jk"]));
    success(&work.cli(&["build", "negative.jk"]));
    rejected(Command::new(work.path().join("negative")).output().unwrap());
}

#[test]
fn state_domains_deadlines_reject_forgery_before_build() {
    let work = Workspace::new();
    for source in [
        "import joky/domain fn main() @suspends { let d = domain.CommitDeadline { storage: 1 } }",
        "extern fn __domain_deadline_after(n: UInt64) @suspends -> domain.DeadlineOwner fn main() @suspends { let d = __domain_deadline_after(1) }",
        "import joky/domain struct State { n: Int32 } fn make(n: Int32) -> State { State { n:n } } fn read(s: &State) -> Int32 { s.n } fn main() @suspends { let o=domain.create(make,1); let c=domain.candidate_from(domain.snapshot_from(o.read_handle(),read), o.handle(), make, 2); let d=0; let r=domain.commit_before(o.handle(),c,&d) }",
    ] {
        work.write("forged.jk", source);
        let output = work.cli(&["build", "forged.jk"]);
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{source}");
        assert!(!error.contains("panicked"), "{error}");
        assert!(!error.contains("[parse"), "{error}");
    }
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
fn state_domains_pairs_reject_unproved_entries_and_capability_mismatches() {
    let work = Workspace::new();
    let prefix = "import joky/domain struct S { n:Int32 } fn make(n:Int32)->S { S { n:n } } fn read(a:&S,b:&S)->Int32 { a.n+b.n } fn write(a:&mut S,b:&mut S,n:Int32)->Int32 { a.n=n; b.n=n; n }";
    for (extra, body) in [
        ("", "domain.write_pair(a.read_handle(),b.handle(),write,1).unwrap();"),
        ("", "domain.read_pair(a.read_handle(),b.read_handle(),write).unwrap();"),
        ("", "let f=read; domain.read_pair(a.read_handle(),b.read_handle(),f).unwrap();"),
        ("", "let f=domain.read_pair::<S,S,Int32>"),
        ("fn noisy(a:&S,b:&S)->Int32 { println(a.n); b.n }", "domain.read_pair(a.read_handle(),b.read_handle(),noisy).unwrap();"),
        ("fn pending(a:&S,b:&S) @suspends -> Int32 { a.n+b.n }", "domain.read_pair(a.read_handle(),b.read_handle(),pending).unwrap();"),
        ("fn escape(a:&S,b:&S)->&S { a }", "domain.read_pair(a.read_handle(),b.read_handle(),escape).unwrap();"),
        ("import joky/drop.{type Drop} struct Hook { n:Int32 } impl Drop for Hook { fn drop(&mut self) {} } fn hook(a:&S,b:&S)->Int32 { let x=Hook{n:1}; a.n+b.n }", "domain.read_pair(a.read_handle(),b.read_handle(),hook).unwrap();"),
    ] {
        work.write("invalid.jk", &format!("{prefix} {extra} fn main() @suspends {{ let a=domain.create(make,1); let b=domain.create(make,2); {body} }}"));
        for args in [vec!["run","invalid.jk"],vec!["run","--interpreter","invalid.jk"],vec!["build","invalid.jk"]] {
            let output=work.cli(&args);
            let error=String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "{body}");
            assert!(!error.contains("panicked") && !error.contains("[parse"), "{error}");
        }
    }
}

#[test]
#[cfg(unix)]
fn state_domains_pair_aliases_fail_before_execution_in_all_backends() {
    let work = Workspace::new();
    for (operation, call) in [
        (
            "fn mutate(a:&mut S,b:&mut S,n:Int32)->Int32 { a.n=1; b.n=2; n }",
            "domain.write_pair(a.handle(),a.handle(),mutate,0)",
        ),
        (
            "fn mutate(a:&S,b:&mut S,n:Int32)->Int32 { b.n=a.n+n; n }",
            "domain.update_pair(a.read_handle(),a.handle(),mutate,0)",
        ),
    ] {
        work.write("alias.jk", &format!("import joky/domain import joky/check struct S {{ n:Int32 }} fn make(n:Int32)->S {{ S{{n:n}} }} fn read(s:&S)->Int32 {{s.n}} {operation} fn main() @suspends {{ let a=domain.create(make,3); let message=\"alias rejection\"; check.assert(match {call} {{ Err(domain.DomainAccessError.InvalidAccess)=>true,_=>false }},&message); check.assert(domain.read_from(a.read_handle(),read).unwrap()==3,&message) }}"));
        work.assert_all_backends("alias.jk", &[], b"", 0);
    }
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
fn state_domains_joint_snapshots_reject_dependency_extraction_and_invalid_projections() {
    let work = Workspace::new();
    let prefix = "import joky/domain struct S { n:Int32 } fn make(n:Int32)->S { S{n:n} } fn project(a:&S,b:&S)->Int32 { a.n+b.n }";
    for (extra, body) in [
        ("", "let f=project; let s=domain.snapshot_pair(a.read_handle(),b.read_handle(),f)"),
        ("", "let f=domain.snapshot_pair::<S,S,Int32>"),
        ("fn mutating(a:&S,b:&mut S)->Int32 { b.n=3; a.n }", "let s=domain.snapshot_pair(a.read_handle(),b.read_handle(),mutating)"),
        ("import joky/array.{type Array} fn owning(a:&S,b:&S)->Array<Ptr> { Array.new::<Ptr>() }", "let s=domain.snapshot_pair(a.read_handle(),b.read_handle(),owning)"),
        ("fn noisy(a:&S,b:&S)->Int32 { println(a.n); b.n }", "let s=domain.snapshot_pair(a.read_handle(),b.read_handle(),noisy)"),
        ("fn pending(a:&S,b:&S) @suspends ->Int32 { a.n+b.n }", "let s=domain.snapshot_pair(a.read_handle(),b.read_handle(),pending)"),
        ("", "let s=domain.snapshot_pair(a.read_handle(),b.read_handle(),project); let raw=s.storage"),
        ("", "let s=domain.snapshot_pair(a.read_handle(),b.read_handle(),project); let c=domain.candidate_from(s, a.handle(), make, 2); s.value();"),
        ("", "let s=domain.Snapshot::<S,Int32>{storage:1}"),
    ] {
        work.write("invalid.jk", &format!("{prefix} {extra} fn main() @suspends {{ let a=domain.create(make,1); let b=domain.create(make,2); {body} }}"));
        for args in [vec!["run","invalid.jk"],vec!["run","--interpreter","invalid.jk"],vec!["build","invalid.jk"]] {
            let output=work.cli(&args);
            let error=String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "{body}");
            assert!(!error.contains("panicked") && !error.contains("[parse"), "{error}");
        }
    }
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
fn state_domains_pair_commits_reject_forgery_reuse_and_unproved_constructors() {
    let work = Workspace::new();
    let prefix = "import joky/domain struct S{n:Int32} fn make(n:Int32)->S{S{n:n}} fn total(a:&S,b:&S)->Int32{a.n+b.n}";
    for (extra, body) in [
        ("", "let p=domain.CandidatePair::<S,S>{storage:1}"),
        ("", "let f=domain.candidate_pair_from::<S,S,Int32>"),
        ("", "let f=make; let p=domain.candidate_pair_from(c, b.handle(), f, 2)"),
        ("fn noisy(n:Int32)->S { println(n); make(n) }", "let p=domain.candidate_pair_from(c, b.handle(), noisy, 2)"),
        ("fn pending(n:Int32) @suspends ->S { make(n) }", "let p=domain.candidate_pair_from(c, b.handle(), pending, 2)"),
        ("", "let p=domain.candidate_pair_from(c, b.handle(), make, 2); let p2=domain.candidate_pair_from(c, b.handle(), make, 3)"),
        ("", "let p=domain.candidate_pair_from(c, b.handle(), make, 2); let r=domain.commit_pair_to(a.read_handle(),b.handle(),p)"),
        ("", "let p=domain.candidate_pair_from(c, b.handle(), make, 2); let r=domain.commit_pair_to(a.handle(),b.handle(),p); let r2=domain.commit_pair_to(a.handle(),b.handle(),p)"),
        ("", "let p=domain.candidate_pair_from(c, b.handle(), make, 2); let raw=p.storage"),
    ] {
        work.write("invalid.jk", &format!("{prefix} {extra} fn main() @suspends {{ let a=domain.create(make,1); let b=domain.create(make,2); let c=domain.candidate_from(domain.snapshot_pair(a.read_handle(),b.read_handle(),total), a.handle(), make, 3); {body} }}"));
        for args in [vec!["run","invalid.jk"],vec!["run","--interpreter","invalid.jk"],vec!["build","invalid.jk"]] {
            let output=work.cli(&args);
            let error=String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "{body}");
            assert!(!error.contains("panicked") && !error.contains("[parse"), "{error}");
        }
    }
}

#[test]
#[cfg(unix)]
fn state_domains_pair_commits_reject_single_or_duplicate_snapshot_dependencies() {
    let work = Workspace::new();
    for snapshot in [
        "domain.snapshot_from(a.read_handle(),read)",
        "domain.snapshot_pair(a.read_handle(),a.read_handle(),total)",
    ] {
        work.write("invalid.jk", &format!("import joky/domain struct S{{n:Int32}} fn make(n:Int32)->S{{S{{n:n}}}} fn read(a:&S)->Int32{{a.n}} fn total(a:&S,b:&S)->Int32{{a.n+b.n}} fn main() @suspends {{ let a=domain.create(make,1); let c=domain.candidate_from({snapshot}, a.handle(), make, 2); let p=domain.candidate_pair_from(c, a.handle(), make, 3) }}"));
        let rejected = |output: std::process::Output| {
            assert!(!output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("cannot repeat a writable domain")
            );
        };
        rejected(work.cli(&["run", "invalid.jk"]));
        rejected(work.cli(&["run", "--interpreter", "invalid.jk"]));
        success(&work.cli(&["build", "invalid.jk"]));
        rejected(Command::new(work.path().join("invalid")).output().unwrap());
    }
}

#[test]
#[cfg(unix)]
fn state_domains_many_transfer_and_readonly_fourth_conflict_run_in_standalone_aot() {
    let work = Workspace::new();
    work.write("many.jk", include_str!("../fixtures/state_domain_many.jk"));
    work.assert_native("many.jk", &[], b"multi-domain transfer ok\n", 0);
}

#[test]
fn state_domains_many_reject_forgery_reuse_and_unproved_entries() {
    let work = Workspace::new();
    let prefix = "import joky/domain struct S{n:Int32} fn make(n:Int32)->S{S{n:n}} fn read(s:&S)->Int32{s.n}";
    for (extra, body) in [
        ("", "let raw=s.storage"),
        ("", "let x=domain.SnapshotSet{storage:1}"),
        ("", "let x=domain.CandidateSet{storage:1}"),
        ("", "let f=domain.read_snapshot::<S,Int32>"),
        ("", "let f=read; domain.read_snapshot(&s,a.read_handle(),f);"),
        ("fn noisy(s:&S)->Int32{println(s.n); s.n}", "domain.read_snapshot(&s,a.read_handle(),noisy);"),
        ("fn mutating(s:&mut S)->Int32{s.n=3; s.n}", "domain.read_snapshot(&s,a.read_handle(),mutating);"),
        ("fn pending(s:&S) @suspends ->Int32{s.n}", "domain.read_snapshot(&s,a.read_handle(),pending);"),
        ("", "let c=domain.candidate_set_from(s,a.read_handle(),make,3)"),
        ("", "let f=make; let c=domain.candidate_set_from(s,a.handle(),f,3)"),
        ("fn noisy(n:Int32)->S{println(n); make(n)}", "let c=domain.candidate_set_from(s,a.handle(),noisy,3)"),
        ("", "let c=domain.candidate_set_from(s,a.handle(),make,3); s.check();"),
        ("", "let c=domain.candidate_set_from(s,a.handle(),make,3); let d=domain.candidate_set_add(c,b.handle(),make,4); let e=domain.commit_set(c)"),
        ("", "let c=domain.candidate_set_from(s,a.handle(),make,3); let r=domain.commit_set(c); let r2=domain.commit_set(c)"),
    ] {
        work.write("invalid.jk", &format!("{prefix} {extra} fn main() @suspends {{ let a=domain.create(make,1); let b=domain.create(make,2); var access=domain.access_set(); access.include(a.read_handle()); access.include(b.read_handle()); let s=domain.snapshot_set(access); {body} }}"));
        for args in [vec!["run","invalid.jk"],vec!["run","--interpreter","invalid.jk"],vec!["build","invalid.jk"]] {
            let output=work.cli(&args);
            let error=String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "{body}");
            assert!(!error.contains("panicked") && !error.contains("[parse"), "{error}");
        }
    }
}

#[test]
#[cfg(unix)]
fn state_domains_many_reject_missing_or_duplicate_members_in_all_backends() {
    let work = Workspace::new();
    for (body, diagnostic) in [
        ("let s=domain.snapshot_set(domain.access_set())", "Empty domain access set"),
        ("var access=domain.access_set(); access.include(a.read_handle()); let s=domain.snapshot_set(access); domain.read_snapshot(&s,b.read_handle(),read);", "Projection domain is absent"),
        ("var access=domain.access_set(); access.include(a.read_handle()); let s=domain.snapshot_set(access); let c=domain.candidate_set_from(s,b.handle(),make,3)", "Candidate target is absent"),
        ("var access=domain.access_set(); access.include(a.read_handle()); let s=domain.snapshot_set(access); let c=domain.candidate_set_from(s,a.handle(),make,3); let d=domain.candidate_set_add(c,a.handle(),make,4)", "cannot repeat a writable domain"),
    ] {
        work.write("invalid.jk", &format!("import joky/domain struct S{{n:Int32}} fn make(n:Int32)->S{{S{{n:n}}}} fn read(s:&S)->Int32{{s.n}} struct Owned{{s:String}} fn make_owned(n:Int32)->Owned{{Owned{{s:\"text\"}}}} fn main() @suspends {{ let a=domain.create(make,1); let b=domain.create(make,2); {body} }}"));
        let rejected = |output: std::process::Output| {
            assert!(!output.status.success(), "{body}");
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(error.contains(diagnostic), "{error}");
        };
        rejected(work.cli(&["run", "invalid.jk"]));
        rejected(work.cli(&["run", "--interpreter", "invalid.jk"]));
        success(&work.cli(&["build", "invalid.jk"]));
        rejected(Command::new(work.path().join("invalid")).output().unwrap());
    }
}

#[test]
#[cfg(unix)]
fn admission_fifo_close_and_cancellation_run_in_standalone_aot() {
    let work = Workspace::new();
    work.write("admission.jk", include_str!("../fixtures/admission.jk"));
    work.assert_native("admission.jk", &[], b"admission ok\n", 0);
}

#[test]
fn admission_rejects_forgery_copy_and_hidden_storage() {
    let work = Workspace::new();
    for body in [
        "let pool=admission.Pool{storage:1}",
        "let permit=admission.Permit{storage:1}",
        "let handle=admission.Handle{token:1}",
        "let pool=admission.create(1,1); let raw=pool.storage",
        "let pool=admission.create(1,1); let h=pool.handle(); let raw=h.token",
        "let pool=admission.create(1,1); let p=pool.handle().acquire().unwrap(); p.release(); p.release()",
        "let pool=admission.create(1,1); let another=pool; pool.close()",
        "let pool=admission.create(1,1); let p=pool.handle().acquire().unwrap(); let raw=p.storage",
    ] {
        work.write("invalid.jk",&format!("import joky/admission fn main() @suspends {{ {body} }}"));
        for args in [vec!["run","invalid.jk"],vec!["run","--interpreter","invalid.jk"],vec!["build","invalid.jk"]] {
            let output=work.cli(&args);
            let error=String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(),"{body}");
            assert!(!error.contains("panicked") && !error.contains("[parse"),"{error}");
        }
    }
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
        .replace("rooms.remove(2)", "rooms.remove_by_ref(&second)")
        .replace("rooms.borrow_ref(1)", "rooms.value_ref(&first)")
        .replace("rooms.borrow_ref(2)", "rooms.value_ref(&second)");
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
fn directory_lookup_rejects_unsupported_types_and_private_witnesses() {
    let work = Workspace::new();
    for (definitions, state) in [
        ("", "Int32"),
        ("struct Bad { pointer:Ptr }", "Bad"),
        ("struct Bad { reference:&Int32 }", "Bad"),
        ("struct Generic<T> { value:T }", "Generic<Int32>"),
        (
            "import joky/drop.{type Drop} struct Bad { text:String } impl Drop for Bad { fn drop(&mut self) {} }",
            "Bad",
        ),
        (
            "import joky/array.{type Array} struct Element { pointer:Ptr } struct Bad { values:Array<Element> }",
            "Bad",
        ),
    ] {
        for operation in ["lookup", "lookup_write"] {
            work.write("invalid.jk", &format!("import joky/domain {definitions} fn main() @suspends {{ let table=domain.directory(1); domain.{operation}::<{state}>(table.handle(),1); }}"));
            failure(
                &work.cli(&["build", "invalid.jk"]),
                "Directory lookup requires a supported non-generic state record",
            );
        }
    }
    for operation in ["lookup_type::<Room>()", "lookup_witness::<Room>"] {
        work.write("private.jk", &format!("import joky/domain struct Room {{ value:Int32 }} fn main() {{ domain.{operation}; }}"));
        let output = work.cli(&["build", "private.jk"]);
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            !error.contains("panicked") && !error.contains("[parse"),
            "{error}"
        );
    }
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
    check.assert(domain.after(timer,room.handle(),100000,tick,2).is_ok(),&message)
    while match timer.status() { Active=>true,_=>false } { task.yield_now() }
    check.assert(match timer.status() { Cancelled=>true,_=>false },&message)
    check.assert(domain.after(timer,room.handle(),0,tick,2).is_ok(),&message)
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
    join {{ | h.call::<domain.TimerHandle,Unit>("hold",timer) | control(&h,timer) }};
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
fn domain_timers_reject_unsafe_contracts_in_all_frontends() {
    let work = Workspace::new();
    for source in [
        "import joky/domain fn main() @suspends { let timer=domain.TimerHandle{identity:1} }".to_owned(),
        "extern fn __timer_create(n:Int32) @suspends -> domain.TimerOwner fn main() @suspends { let timer=__timer_create(1) }".to_owned(),
        "extern fn forge() -> domain.TimerOwner fn main() {let timer=forge()}".to_owned(),
        "import joky/domain import joky/array.{type Array} struct Room{value:Int32} fn make(value:Int32)->Room{Room{value:value}} fn tick(room:&mut Room,value:Array<Ptr>){} fn main() @suspends{let room=domain.create(make,0); let timer=domain.timer(); domain.after(timer.handle(),room.handle(),0,tick,Array.new::<Ptr>());}".to_owned(),
        "import joky/domain struct Room{value:Int32} fn make(value:Int32)->Room{Room{value:value}} extern fn external(value:Int32) fn tick(room:&mut Room,value:Int32){external(value)} fn main() @suspends{let room=domain.create(make,0); let timer=domain.timer(); domain.after(timer.handle(),room.handle(),0,tick,1);}".to_owned(),
    ] {
        work.write("bad_timer.jk", &source);
        for args in [vec!["run","bad_timer.jk"],vec!["run","--interpreter","bad_timer.jk"],vec!["build","bad_timer.jk"]] {
            let output=work.cli(&args);
            let error=String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(),"{source}");
            assert!(!error.contains("panicked"),"{error}");
            assert!(!error.contains("[parse"),"{error}");
        }
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
    join {{ | h.call::<domain.DirectoryHandle,Unit>("hold",directory) | control(&h,directory) }};
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
    domain.after(t,a.handle(),0,tick,{value}).unwrap()
    while match t.status() {{Active=>true,_=>false}} {{task.yield_now()}}
    check.assert(domain.read_from(a.read_handle(),count).unwrap()==1,&message)
    domain.every(t,a.handle(),100000,tick,{value}).unwrap(); t.cancel()
}}
"#));
        work.assert_all_backends("bound.jk", &[], b"", 0);
    }
}

#[test]
#[cfg(unix)]
fn domain_value_packets_reject_unsafe_inputs_and_results() {
    let work = Workspace::new();
    for (name, operation, invocation) in [
        (
            "input",
            "fn turn(s:&mut State,p:Packet)->Int32 {s.n}",
            "domain.write_to(owner.handle(),turn,Packet{text:Array.new::<Ptr>()}).unwrap()",
        ),
        (
            "result",
            "fn turn(s:&State)->Packet {Packet{text:Array.new::<Ptr>()}}",
            "domain.read_from(owner.read_handle(),turn).unwrap()",
        ),
    ] {
        let file = format!("{name}.jk");
        work.write(
            &file,
            &format!(
                r#"import joky/domain import joky/array.{{type Array}}
struct State {{n:Int32}} struct Packet {{text:Array<Ptr>}}
fn make(n:Int32)->State {{State{{n:n}}}}
{operation}
fn main() @suspends {{let owner=domain.create(make,1); {invocation};}}"#
            ),
        );
        failure(&work.cli(&["build", &file]), "bounded value signature");
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
fn domain_queries_reject_invalid_permissions_payloads_and_dynamic_operations() {
    let work = Workspace::new();
    for (name, operation, invocation) in [
        (
            "mutable",
            "fn query(s:&mut State,n:Int32)->Int32{s.n=n; n}",
            "domain.query_from(h,query,1)",
        ),
        (
            "owning",
            "fn query(s:&State,p:Packet)->Int32{s.n}",
            "domain.query_from(h,query,Packet{text:Array.new::<Ptr>()})",
        ),
        (
            "async",
            "fn query(s:&State,n:Int32) @suspends ->Int32{s.n+n}",
            "domain.query_from(h,query,1)",
        ),
        (
            "dynamic",
            "fn query(s:&State,n:Int32)->Int32{s.n+n}",
            "{let operation=query; domain.query_from(h,operation,1)}",
        ),
        (
            "intrinsic_value",
            "fn query(s:&State,n:Int32)->Int32{s.n+n}",
            "{let q=domain.query_from::<State,Int32,Int32>; q(h,query,1)}",
        ),
        (
            "pair_mutable",
            "fn query(a:&State,b:&mut State,n:Int32)->Int32{b.n=n; a.n}",
            "domain.query_pair(h,h,query,1)",
        ),
    ] {
        let file = format!("query_{name}.jk");
        work.write(
            &file,
            &format!(
                r#"import joky/domain import joky/array.{{type Array}}
struct State {{n:Int32}} struct Packet {{text:Array<Ptr>}}
fn make(n:Int32)->State {{State{{n:n}}}}
{operation}
fn main() @suspends {{let owner=domain.create(make,1); let h=owner.read_handle(); {invocation};}}"#
            ),
        );
        let output = work.cli(&["build", &file]);
        assert!(!output.status.success(), "accepted {name}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("error:"),
            "{output:?}"
        );
    }
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
fn domain_create_values_reject_unsafe_payloads_and_unproved_constructors() {
    let work = Workspace::new();
    for (name, definitions, input, constructor, expected) in [
        (
            "owning",
            "struct Init {text:Array<Ptr>}",
            "Init{text:Array.new::<Ptr>()}",
            "fn make(p:Init)->State{State{n:1}}",
            "bounded value signature",
        ),
        (
            "tuple",
            "",
            "(1,Ptr.null())",
            "fn make(p:(Int32,Ptr))->State{State{n:1}}",
            "bounded value signature",
        ),
        (
            "generic",
            "struct Init<T>{n:T}",
            "Init::<Ptr>{n:Ptr.null()}",
            "fn make(p:Init<Ptr>)->State{State{n:1}}",
            "bounded value signature",
        ),
        (
            "async",
            "struct Init{n:Int32}",
            "Init{n:1}",
            "fn make(p:Init) @suspends ->State{State{n:p.n}}",
            "type mismatch",
        ),
        (
            "host",
            "struct Init{n:Int32}",
            "Init{n:1}",
            "fn make(p:Init)->State{println(\"host\"); State{n:p.n}}",
            "proved synchronous",
        ),
    ] {
        let file = format!("create_{name}.jk");
        work.write(&file, &format!("import joky/domain import joky/array.{{type Array}}\nstruct State {{n:Int32}}\n{definitions}\n{constructor}\nfn main() @suspends {{let owner=domain.create(make,{input})}}"));
        failure(&work.cli(&["build", &file]), expected);
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
fn domain_candidate_values_reject_unproved_and_non_value_inputs() {
    let work = Workspace::new();
    for (definition, input, constructor, expected) in [
        (
            "struct Init{text:Array<Ptr>}",
            "Init{text:Array.new::<Ptr>()}",
            "fn make(p:Init)->State{State{n:1}}",
            "bounded value signature",
        ),
        (
            "",
            "(1,Ptr.null())",
            "fn make(p:(Int32,Ptr))->State{State{n:1}}",
            "bounded value signature",
        ),
        (
            "struct Init<T>{n:T}",
            "Init::<Ptr>{n:Ptr.null()}",
            "fn make(p:Init<Ptr>)->State{State{n:1}}",
            "bounded value signature",
        ),
        (
            "struct Init{n:Int32}",
            "Init{n:1}",
            "fn make(p:Init) @suspends ->State{State{n:p.n}}",
            "type mismatch",
        ),
        (
            "struct Init{n:Int32}",
            "Init{n:1}",
            "fn make(p:Init)->State{println(p.n); State{n:p.n}}",
            "proved synchronous",
        ),
    ] {
        for body in [
            "let next=domain.candidate_from(domain.snapshot_from(a.read_handle(),read), a.handle(), make, INPUT)",
            "let first=domain.candidate_from(domain.snapshot_pair(a.read_handle(),b.read_handle(),total), a.handle(), seed, 0); let next=domain.candidate_pair_from(first, b.handle(), make, INPUT)",
            "var access=domain.access_set(); access.include(a.read_handle()); let next=domain.candidate_set_from(domain.snapshot_set(access),a.handle(),make,INPUT)",
            "var access=domain.access_set(); access.include(a.read_handle()); access.include(b.read_handle()); let first=domain.candidate_set_from(domain.snapshot_set(access),a.handle(),seed,0); let next=domain.candidate_set_add(first,b.handle(),make,INPUT)",
        ] {
            let body=body.replace("INPUT",input);
            work.write("candidate_invalid.jk", &format!("import joky/domain import joky/array.{{type Array}} struct State{{n:Int32}} {definition} {constructor} fn seed(n:Int32)->State{{State{{n:n}}}} fn read(s:&State)->Int32{{s.n}} fn total(a:&State,b:&State)->Int32{{a.n+b.n}} fn main() @suspends {{let a=domain.create(seed,0); let b=domain.create(seed,0); {body}}}"));
            failure(&work.cli(&["build","candidate_invalid.jk"]),expected);
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
    join {{ | h.call::<domain.DirectoryHandle,Unit>("hold",directory) | control(&h,directory) }};
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
fn domain_snapshot_values_reject_invalid_results_and_projections() {
    let work = Workspace::new();
    for (definitions, ty, value) in [
        (
            "struct Payload{text:Array<Ptr>}",
            "Payload",
            "Payload{text:Array.new::<Ptr>()}",
        ),
        ("", "(Int32,Ptr)", "(1,Ptr.null())"),
        (
            "struct Payload<T>{n:T}",
            "Payload<Ptr>",
            "Payload::<Ptr>{n:Ptr.null()}",
        ),
    ] {
        for (params,body) in [
            ("a:&State","let s=domain.snapshot_from(a.read_handle(),project)"),
            ("a:&State,b:&State","let s=domain.snapshot_pair(a.read_handle(),b.read_handle(),project)"),
            ("a:&State","var access=domain.access_set(); access.include(a.read_handle()); let s=domain.snapshot_set(access); domain.read_snapshot(&s,a.read_handle(),project);"),
        ] {
            work.write("invalid_snapshot.jk",&format!("import joky/domain import joky/array.{{type Array}} {definitions} struct State{{n:Int32}} fn make(n:Int32)->State{{State{{n:n}}}} fn project({params})->{ty}{{{value}}} fn main() @suspends {{let a=domain.create(make,1); let b=domain.create(make,2); {body}}}"));
            failure(&work.cli(&["build","invalid_snapshot.jk"]),"bounded value signature");
        }
    }
    for (modifier, receiver, extra, expected) in [
        ("", "&mut State", "a.n=5", "type mismatch"),
        ("@suspends", "&State", "", "type mismatch"),
        ("", "&State", "println(a.n)", "proved synchronous"),
    ] {
        for (params,body) in [
            (format!("a:{receiver}"),"let s=domain.snapshot_from(a.read_handle(),project)"),
            (format!("a:&State,b:{receiver}"),"let s=domain.snapshot_pair(a.read_handle(),b.read_handle(),project)"),
            (format!("a:{receiver}"),"var access=domain.access_set(); access.include(a.read_handle()); let s=domain.snapshot_set(access); domain.read_snapshot(&s,a.read_handle(),project);"),
        ] {
            let extra=if params.contains("b:&mut") {"b.n=5"} else {extra};
            work.write("invalid_snapshot.jk",&format!("import joky/domain import joky/array.{{type Array}} struct Payload{{n:Int32}} struct State{{n:Int32}} fn make(n:Int32)->State{{State{{n:n}}}} fn project({params}) {modifier} ->Payload{{{extra}; Payload{{n:a.n}}}} fn main() @suspends {{let a=domain.create(make,1); let b=domain.create(make,2); {body}}}"));
            failure(&work.cli(&["build","invalid_snapshot.jk"]),expected);
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
    join {{ | h.call::<domain.DirectoryHandle,Unit>("hold",directory) | control(&h,directory) }};
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
fn domain_request_values_reject_invalid_contracts() {
    let work = Workspace::new();
    for (definition, ty, value) in [
        (
            "struct P {text:Array<Ptr>}",
            "P",
            "P{text:Array.new::<Ptr>()}",
        ),
        ("struct P<T> {n:T}", "P<Ptr>", "P{n:Ptr.null()}"),
        ("enum P {A(Array<Ptr>)}", "P", "P.A(Array.new::<Ptr>())"),
        ("", "(Int32,Ptr)", "(1,Ptr.null())"),
    ] {
        for result in [false, true] {
            let (input, output, argument, body) = if result {
                ("Int32", ty, "0", value)
            } else {
                (ty, "Int32", value, "0")
            };
            work.write("rejected_request.jk", &format!(r#"import joky/domain import joky/array.{{type Array}}
{definition}
struct State {{n:Int32}}
fn make(n:Int32)->State {{State{{n:n}}}}
fn work(input:{input}) @suspends -> {output} {{{body}}}
fn main() @suspends {{let owner=domain.create(make,0); domain.request(owner.handle(),work,{argument});}}
"#));
            let output = work.cli(&["build", "rejected_request.jk"]);
            assert!(!output.status.success());
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(
                !error.contains("panicked") && !error.contains("[parse"),
                "{error}"
            );
            assert!(
                error.contains("bounded value") || error.contains("Copy type arguments"),
                "{error}"
            );
        }
    }
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
    join {{ | h.call::<domain.DirectoryHandle,Unit>("hold",directory) | control(&h,directory) }};
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
}

#[test]
fn domain_timer_values_reject_invalid_inputs_and_effects() {
    let work = Workspace::new();
    for (decl, ty, value, body, effect) in [
        (
            "struct P{text:Array<Ptr>}",
            "P",
            "P{text:Array.new::<Ptr>()}",
            "",
            "",
        ),
        ("struct P<T>{n:T}", "P<Ptr>", "P{n:Ptr.null()}", "", ""),
        (
            "enum P{Some(Array<Ptr>)}",
            "P",
            "P.Some(Array.new::<Ptr>())",
            "",
            "",
        ),
        ("", "(Int32,Ptr)", "(1,Ptr.null())", "", ""),
        (
            "struct P{n:Int32}",
            "P",
            "P{n:1}",
            "task.yield_now()",
            "@suspends",
        ),
        (
            "struct P{n:Int32}",
            "P",
            "P{n:1}",
            "let text=\"timer\"; io.print(&text)",
            "",
        ),
    ] {
        work.write("rejected_timer.jk",&format!(r#"import joky/domain import joky/array.{{type Array}} import joky/task import joky/io
{decl}
struct State {{n:Int32}}
fn make(n:Int32)->State {{State{{n:n}}}}
fn tick(s:&mut State,p:{ty}) {effect} {{{body}}}
fn main() @suspends {{let o=domain.create(make,0); let t=domain.timer(); domain.after(t.handle(),o.handle(),0,tick,{value});}}"#));
        let output = work.cli(&["build", "rejected_timer.jk"]);
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            !error.contains("panicked") && !error.contains("[parse"),
            "{error}"
        );
    }
    work.write("generic_timer_effect.jk",r#"import joky/domain import joky/array.{type Array} import joky/io
struct State {n:Int32} struct P {n:Int32}
fn make(n:Int32)->State {State{n:n}}
fn tick<T>(s:&mut State,p:T) {let text="timer"; io.print(&text)}
fn arm<T>(t:domain.TimerHandle,h:domain.WriteHandle<State>,p:T) @suspends {domain.after(t,h,0,tick::<T>,p);}
fn main() @suspends {let o=domain.create(make,0); let t=domain.timer(); arm(t.handle(),o.handle(),P{n:1});}
"#);
    failure(
        &work.cli(&["build", "generic_timer_effect.jk"]),
        "Domain operation must be a proved",
    );
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
    assert(domain.after(t,h,100000,tick,p).is_ok())
    while match t.status(){Active=>true,_=>false} {task.yield_now()}
    assert(match t.status(){Cancelled=>true,_=>false})
    let result=domain.after(t,h,0,tick,p)
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
    join {{ | h.call::<domain.TimerHandle,Unit>("hold",t) | control(&h,t) }};
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
fn state_domains_submission_targets_reject_missing_members_before_construction() {
    for pair in [false, true] {
        let work = Workspace::new();
        let prepare = if pair {
            "let first=domain.candidate_from(snapshot,a.handle(),make,2); domain.candidate_pair_from(first,c.handle(),broken,0);"
        } else {
            "domain.candidate_from(snapshot,c.handle(),broken,0);"
        };
        work.write(
            "missing.jk",
            &format!(
                r#"import joky/domain
struct S {{n:Int32}}
fn make(n:Int32)->S {{S{{n:n}}}}
fn broken(n:Int32)->S {{make(1/n)}}
fn sum(a:&S,b:&S)->Int32 {{a.n+b.n}}
fn main() @suspends {{
    let a=domain.create(make,1); let b=domain.create(make,2); let c=domain.create(make,3)
    let snapshot=domain.snapshot_pair(a.read_handle(),b.read_handle(),sum)
    {prepare}
}}"#
            ),
        );
        for args in [
            vec!["run", "missing.jk"],
            vec!["run", "--interpreter", "missing.jk"],
        ] {
            failure(
                &work.cli(&args),
                "Candidate target is absent from the complete read set",
            );
        }
        success(&work.cli(&["build", "missing.jk", "-o", "missing"]));
        failure(
            &Command::new(work.path().join("missing")).output().unwrap(),
            "Candidate target is absent from the complete read set",
        );
    }
}

#[test]
fn state_domains_submission_targets_require_matching_write_capabilities() {
    let work = Workspace::new();
    for (target, pair) in [
        ("a.read_handle()", false),
        ("b.handle()", false),
        ("a.read_handle()", true),
        ("b.handle()", true),
    ] {
        let candidate = if pair {
            format!("let first=domain.candidate_from(snapshot,b.handle(),rules,3); domain.candidate_pair_from(first,{target},make,4);")
        } else {
            format!("domain.candidate_from(snapshot,{target},make,3);")
        };
        work.write(
            "permission.jk",
            &format!(
                r#"import joky/domain
struct S {{n:Int32}} struct T {{n:Int32}}
fn make(n:Int32)->S {{S{{n:n}}}} fn rules(n:Int32)->T {{T{{n:n}}}}
fn total(a:&S,b:&T)->Int32 {{a.n+b.n}}
fn main() @suspends {{let a=domain.create(make,1); let b=domain.create(rules,2)
let snapshot=domain.snapshot_pair(a.read_handle(),b.read_handle(),total); {candidate}}}"#
            ),
        );
        let output = work.cli(&["build", "permission.jk"]);
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            !error.contains("panicked") && !error.contains("[parse"),
            "{error}"
        );
        assert!(error.contains("type_mismatch"), "{error}");
    }
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
fn state_domains_storage_editors_reject_invalid_contracts() {
    let work = Workspace::new();
    let prefix = "import joky/domain struct S{n:Int32} fn make(n:Int32)->S{S{n:n}} fn edit(s:&mut S,n:Int32){s.n=n}";
    for (extra, body) in [
        (
            "",
            "domain.edit_snapshot(snapshot,owner.read_handle(),edit,2);",
        ),
        (
            "fn readonly(s:&S,n:Int32){}",
            "domain.edit_snapshot(snapshot,owner.handle(),readonly,2);",
        ),
        (
            "fn result(s:&mut S,n:Int32)->Int32{n}",
            "domain.edit_snapshot(snapshot,owner.handle(),result,2);",
        ),
        (
            "fn pending(s:&mut S,n:Int32) @suspends {}",
            "domain.edit_snapshot(snapshot,owner.handle(),pending,2);",
        ),
        (
            "fn noisy(s:&mut S,n:Int32){println(n)}",
            "domain.edit_snapshot(snapshot,owner.handle(),noisy,2);",
        ),
        (
            "import joky/array.{type Array} fn owned(s:&mut S,n:Array<Ptr>){}",
            "domain.edit_snapshot(snapshot,owner.handle(),owned,Array.new::<Ptr>());",
        ),
        (
            "",
            "let f=edit; domain.edit_snapshot(snapshot,owner.handle(),f,2);",
        ),
        (
            "",
            "let c=domain.edit_snapshot(snapshot,owner.handle(),edit,2); snapshot.check();",
        ),
    ] {
        work.write("invalid.jk", &format!("{prefix} {extra} fn main() @suspends {{let owner=domain.create(make,1); var set=domain.access_set(); set.include(owner.read_handle()); let snapshot=domain.snapshot_set(set); {body}}}"));
        let output = work.cli(&["build", "invalid.jk"]);
        assert!(!output.status.success(), "{body}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            !error.contains("panicked") && !error.contains("[parse"),
            "{error}"
        );
    }
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
        ("fn key()->UInt64{1}", "let d=domain.directory(2); let routes=d.handle(); when (h,s:S=routes[key()]) {};", "UInt64 keys"),
        ("", "let d=domain.directory(2); let routes=d.handle(); when (h,s:S=routes[h.n]) {};", "UInt64 keys"),
        ("", "let d=domain.directory(2); let routes=d.handle(); when (h,s?:S=routes[1]) {s.n} else {s.n};", "Undefined"),
        ("", "let d=domain.directory(2); let routes=d.handle(); let text=\"owned\"; when (h,s:S=routes[1]) {let owned:String=text; owned};", "String"),

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
    join {{ | h.call::<domain.DirectoryHandle,Unit>("hold",directory) | control(&h,directory) }};
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
            .replace("#every(100000)", "#every(1)")
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
        work.write("invalid.jk", &format!("import joky/io #state struct Room{{value:Int32}} fn helper(s:&mut Room){{let n=s.value; io.println(&n)}} impl Room{{#every(1) {method}}} fn main(){{}}"));
        failure(&work.cli(&["build", "invalid.jk"]), "#every requires");
    }
    for source in [
        "struct Room{value:Int32} impl Room{#every(1) fn tick(&mut self){}}",
        "#state struct Room{value:Int32} #every(1) fn tick(s:&mut Room){}",
        "struct Room<T>{value:T} impl<T> Room<T>{#every(1) fn tick(&mut self){}}",
        "#state struct Room{value:Int32} trait Tick{fn tick(&mut self);} impl Tick for Room{#every(1) fn tick(&mut self){}}",
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
impl Room{#every(1) fn tick(&mut self){increment(self)}}
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
impl Room{#every(100000) fn tick(&mut self){self.value=10}}
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
            base.replace("#every(100000)", "")
        } else {
            base.into()
        };
        let next = if action == "remove" {
            base.replace("#every(100000)", "")
        } else {
            base.replace("#every(100000)", "#every(1)")
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
 race{{| h.call::<domain.DirectoryHandle,Unit>("hold",d) | control(&h,d)}};
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
impl Room{#every(100000) fn tick(&mut self){self.value+=1}}
fn make(n:Int32)->Room{Room{value:n}}
fn read(room:&Room)->Int32{room.value}
fn hold(d:domain.DirectoryHandle) @suspends {let room=domain.create(make,10); d.insert(0,room.handle()).unwrap(); while true{task.yield_now()}}
fn probe(d:domain.DirectoryHandle) @suspends ->Int32 {match domain.lookup::<Room>(d,0){Some(h)=>domain.read_from(h,read).unwrap(),None=>-1}}"#;
    let next = r#"import joky/domain
#state struct Room{value:Int64}
impl Room{#every(1) fn tick(&mut self){self.value=42}}
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
 race{{| h.call::<domain.DirectoryHandle,Unit>("hold",d) | control(&h,d)}};
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
fn owned_array_domain_contracts_reject_unsafe_elements_and_reuse() {
    let work = Workspace::new();
    let prefix = r#"
import joky/domain
import joky/array.{type Array}
import joky/map.{type Map}
import joky/drop.{type Drop}
struct S{n:Int32}
fn make(n:Int32)->S{S{n:n}}
"#;
    for (definitions, ty, value) in [
        ("", "Array<Ptr>", "Array.new::<Ptr>()"),
        ("", "Array<Array<Ptr>>", "Array.new::<Array<Ptr>>()"),
        (
            "struct D{text:String} impl Drop for D{fn drop(&mut self){}}",
            "Array<D>",
            "Array.new::<D>()",
        ),
        (
            "",
            "Array<Map<Int32,Int32>>",
            "Array.new::<Map<Int32,Int32>>()",
        ),
        ("", "Map<Int32,Int32>", "Map.new::<Int32,Int32>()"),
    ] {
        work.write("unsafe.jk", &format!("{prefix} {definitions} fn read(s:&S)->{ty}{{{value}}} fn main() @suspends {{let o=domain.create(make,1); domain.read_from(o.read_handle(),read);}}"));
        for args in [
            vec!["run", "unsafe.jk"],
            vec!["run", "--interpreter", "unsafe.jk"],
            vec!["build", "unsafe.jk"],
        ] {
            let result = work.cli(&args);
            let error = String::from_utf8_lossy(&result.stderr);
            assert!(
                !result.status.success()
                    && !error.contains("[parse")
                    && !error.contains("panicked"),
                "{ty}: {error}"
            );
        }
    }
    work.write("moved.jk", &format!("{prefix} fn echo(s:&S,a:Array<String>)->Array<String>{{a}} fn main() @suspends {{let o=domain.create(make,1); let a=Array.new::<String>(); domain.query_from(o.read_handle(),echo,a); a.len();}}"));
    failure(&work.cli(&["build", "moved.jk"]), "moved");
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
