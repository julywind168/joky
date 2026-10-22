//! Container and owned-value behavior in standalone programs.
use super::*;

#[test]
#[cfg(unix)]
fn string_collections_example_and_module_aliases_run_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "collections.jk",
        include_str!("../../examples/string_collections.jk"),
    );
    work.assert_all_backends("collections.jk", &[], b"5\n11\n6\n", 0);
    work.write(
        "parts/text.jk",
        r#"
        import joky/string as text.{type StringBuilder as Writer}
        import joky/array.{type Array}
        pub type Word = String
        pub type Texts = Array<Word>
        pub type Builder = Writer
        pub fn builder(input: &Word) -> Builder { Writer.from(input) }
        pub struct Envelope<T> { value: T }
        pub fn pass<T>(value: T) -> T { value }
        pub fn unpack<T>(value: Envelope<T>) -> T { value.value }
        pub fn pack(input: &Word) -> Envelope<Texts> {
            let separator = "|"
            let parts = input.split(&separator).unwrap_or(Array.new::<Word>())
            Envelope { value: pass(parts) }
        }
        pub fn render(parts: &Texts) -> Word {
            let separator = "|"
            text.join(parts, &separator)
        }
    "#,
    );
    work.write(
        "aliases.jk",
        r#"
        import parts/text as words.{type Texts as Parts, type Builder as Writer}
        import joky/io.{println} fn report_int32(value: Int32) { println(&value) }
        fn unpack(value: words.Envelope<Parts>) -> Parts { words.unpack(value) }
        fn main() {
            let source = "中|🙂"
            let pack = words.pack
            let wrapped = pack(&source)
            source.release()
            let unpack = unpack
            let parts = unpack(wrapped)
            let render = words.render
            let text = render(&parts)
            parts.release()
            var builder: Writer = words.builder(&text)
            text.release()
            builder.push('!')
            let rendered = builder.finish()
            report_int32(int32(rendered.byte_length()))
        }
    "#,
    );
    work.assert_all_backends("aliases.jk", &[], b"9\n", 0);
}

#[test]
#[cfg(unix)]
fn array_example_matches_jit_interpreter_and_native_execution() {
    let work = Workspace::new();
    work.write("arrays.jk", include_str!("../../examples/arrays.jk"));
    for args in [
        vec!["run", "arrays.jk"],
        vec!["run", "--interpreter", "arrays.jk"],
        vec!["run", "--no-prelude", "arrays.jk"],
        vec!["run", "--interpreter", "--no-prelude", "arrays.jk"],
    ] {
        let output = work.cli(&args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"42\n");
    }
    let built = work.cli(&["build", "--no-prelude", "arrays.jk", "-o", "arrays"]);
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let output = Command::new(work.path().join("arrays")).output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"42\n");
}

#[test]
#[cfg(unix)]
fn owning_array_methods_match_all_backends_and_private_module_boundaries() {
    let work = Workspace::new();
    work.write(
        "arrays.jk",
        &format!(
            "import joky/io.{{println}}\n{}  fn report_int32(value: Int32) {{ println(&value) }} fn main() {{ report_int32(methods_answer()) }}",
            include_str!("../fixtures/owned_arrays.jk")
        ),
    );
    for flags in [&[][..], &["--no-prelude"][..]] {
        work.assert_all_backends("arrays.jk", flags, b"42\n", 0);
    }
    work.write(
        "mixed.jk",
        &format!(
            "import joky/io.{{println}}\n{}  fn report_int32(value: Int32) {{ println(&value) }} fn main() {{ report_int32(mixed_all()) }}",
            include_str!("../fixtures/mixed_owners.jk")
        ),
    );
    for flags in [&[][..], &["--no-prelude"][..]] {
        work.assert_all_backends("mixed.jk", flags, b"42\n", 0);
    }
    work.write(
        "collections.jk",
        r#"
        import joky/array as array.{type Array}
        import joky/option.{type Option}
        pub struct Batch<T> { pub version: UInt64, values: Array<T>, pub sorted: Bool }
        pub struct Secret { value: Int32 }
        impl Secret {
            pub fn new(value: Int32) -> Self { Self { value: value } }
            pub fn get(&self) -> Int32 { self.value }
        }
        pub struct Node { next: Option<&Node> }
        pub fn empty_node() -> Node { Node { next: None } }
        impl<T> Batch<T> {
            pub fn new() -> Self { Self { version: 0, values: Array.new::<T>(), sorted: true } }
            pub fn push(&mut self, value: T) { self.values.push(value); self.version = self.version + 1; self.sorted = false }
            pub fn release(self) { self.values.release() }
        }
        impl<T: Copy> Batch<T> {
            pub fn get(&self, index: UInt64) -> Option<T> { self.values.get(index) }
        }
    "#,
    );
    work.write(
        "bridge.jk",
        r#"
        import collections.{type Batch}
        pub type BatchAlias<T> = Batch<T>
        pub fn make<T>(value: T) -> BatchAlias<T> {
            var b = Batch.new::<T>()
            b.push(value)
            b
        }
    "#,
    );
    work.write("main.jk", r#"
        import collections.{type Batch, type Secret}
        import bridge as bridge
        import joky/io.{println} fn report_int32(value: Int32) { println(&value) }
        struct Point { value: Int32 }
        fn main() {
            var b = bridge.make(Point { value: 20 })
            b.push(Point { value: 22 })
            let answer = b.get(0).unwrap_or(Point { value: 0 }).value + b.get(1).unwrap_or(Point { value: 0 }).value
            if b.version != 2 || b.sorted { report_int32(0); return }
            b.release()
            let secret = Secret.new(answer)
            report_int32(secret.get())
        }
    "#);
    work.assert_all_backends("main.jk", &[], b"42\n", 0);
    for body in [
        "fn main() { let b = Batch.new::<Int32>(); b.values; }",
        "fn main() { var b = Batch.new::<Int32>(); b.values.push(42) }",
        "fn main() { let b = Batch { values: array.new::<Int32>() } }",
        "fn hidden<T>(b: Batch<T>) { b.values; } fn main() {}",
        "fn hidden<T>(b: &Batch<T>) { let r = &b.values } fn main() {}",
        "fn hidden<T>(b: &mut Batch<T>) { let r = &mut b.values } fn main() {}",
        "fn main() { let s = Secret.new(42); s.value; }",
        "fn main() { var s = Secret.new(42); s.value = 0 }",
        "fn main() { var s = Secret.new(42); let r = &mut s.value }",
        "fn main() { let s = alloc(Secret.new(42)); s.value; }",
        "fn main() { let s = alloc(Secret.new(42)); s.value = 0 }",
        "fn main() { let s = Secret { value: 42 } }",
        "fn hidden<T>(s: Secret) -> Int32 { s.value } fn main() {}",
        "fn main() { graph { let a = graph.alloc(collections.empty_node()); graph.link(a.next, a); a }; }",
    ] {
        work.write(
            "invalid.jk",
            &format!("import collections.{{type Batch, type Secret}} import joky/array as array {body}"),
        );
        for command in ["run", "build"] {
            failure(&work.cli(&[command, "invalid.jk"]), "private");
        }
        failure(
            &work.cli(&["run", "--interpreter", "invalid.jk"]),
            "private",
        );
    }
}

#[test]
fn owning_arrays_run_and_build_with_exactly_once_cleanup() {
    let work = Workspace::new();
    work.write(
        "owning_arrays.jk",
        include_str!("../../examples/owning_arrays.jk"),
    );
    work.assert_all_backends("owning_arrays.jk", &[], b"42\n20\n22\n", 0);
    work.write(
        "owning_enums.jk",
        include_str!("../../examples/owning_enums.jk"),
    );
    work.assert_all_backends("owning_enums.jk", &[], b"42\n42\n", 0);
}

#[test]
#[cfg(unix)]
fn owning_tuples_run_and_build_with_drop_and_zip() {
    let work = Workspace::new();
    work.write(
        "owning_tuples.jk",
        include_str!("../../examples/owning_tuples.jk"),
    );
    work.assert_all_backends("owning_tuples.jk", &[], b"3\n2\n1\n4\n42\n5\n5\n", 0);
}

#[test]
#[cfg(unix)]
fn borrowed_matches_run_and_build_without_consuming_the_enum() {
    let work = Workspace::new();
    work.write(
        "borrowed_matches.jk",
        include_str!("../../examples/borrowed_matches.jk"),
    );
    work.assert_all_backends("borrowed_matches.jk", &[], b"42\n42\n22\n20\n", 0);
}

#[test]
#[cfg(unix)]
fn mutable_borrows_run_and_build_with_in_place_updates() {
    let work = Workspace::new();
    work.write(
        "mutable_borrows.jk",
        include_str!("../../examples/mutable_borrows.jk"),
    );
    work.assert_all_backends("mutable_borrows.jk", &[], b"42\n42\n42\n", 0);
}

#[test]
#[cfg(unix)]
fn memory_replace_run_and_build_transfer_old_values() {
    let work = Workspace::new();
    work.write(
        "memory_replace.jk",
        include_str!("../../examples/memory_replace.jk"),
    );
    work.assert_all_backends("memory_replace.jk", &[], b"42\n1\n42\n", 0);
}

#[test]
#[cfg(unix)]
fn swap_remove_runs_and_builds_with_owned_elements() {
    let work = Workspace::new();
    work.write(
        "swap_remove.jk",
        include_str!("../../examples/swap_remove.jk"),
    );
    work.assert_all_backends("swap_remove.jk", &[], b"20\n22\n2\n1\n", 0);
}

#[test]
#[cfg(unix)]
fn ordered_arrays_run_and_build_with_owned_elements() {
    let work = Workspace::new();
    work.write(
        "ordered_arrays.jk",
        include_str!("../../examples/ordered_arrays.jk"),
    );
    work.assert_all_backends("ordered_arrays.jk", &[], b"21\n22\n20\n0\n", 0);
}

#[test]
#[cfg(unix)]
fn array_management_runs_and_builds_with_owned_elements() {
    let work = Workspace::new();
    work.write(
        "array_management.jk",
        include_str!("../../examples/array_management.jk"),
    );
    work.assert_all_backends("array_management.jk", &[], b"22\n1\n4\n42\n0\n", 0);
}

#[test]
#[cfg(unix)]
fn array_replace_runs_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "array_replace.jk",
        &recording_cli_source(include_str!("../fixtures/array_replace.jk")),
    );
    work.assert_native(
        "array_replace.jk",
        &[],
        b"42\n42\n42\n-1\n-2\n-3\n-4\n3\n4\n1\n2\n42\n-1\n-2\n2\n1\n42\n",
        0,
    );
}

#[test]
#[cfg(unix)]
fn int_map_public_operations_match_all_backends() {
    let work = Workspace::new();
    // Internal index checks run in VM and native test registries.
    // CLI validates the same production algorithm through public exports.
    let source: String = recording_cli_source(include_str!("../fixtures/int_map.jk"))
        .lines()
        .filter(|line| !line.contains("maps.test_bucket_count"))
        .collect::<Vec<_>>()
        .join("\n")
        .replace("maps.test_invariant(&m)", "true")
        .replace("maps.test_deleted_count(&m)", "0");
    work.write("int_map.jk", &source);
    work.assert_all_backends("int_map.jk", &[], b"42\n42\n42\n", 0);
    {
        let name = "capacity_failure";
        work.write(
            "failure.jk",
            &(source.replace("fn main()", "fn successful_main()")
                + &format!("\nfn main() {{ {name}(); }}\n")),
        );
        let assert_failure = |output: std::process::Output| {
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            assert!(String::from_utf8_lossy(&output.stderr)
                .contains("Map capacity exceeds the supported limit"));
        };
        assert_failure(work.cli(&["run", "failure.jk"]));
        assert_failure(work.cli(&["run", "--interpreter", "failure.jk"]));
        success(&work.cli(&["build", "failure.jk"]));
        assert_failure(Command::new(work.path().join("failure")).output().unwrap());
    }
}

#[test]
#[cfg(unix)]
fn owned_map_values_and_failures_match_all_backends() {
    check_owned_map_cli_cases(false);
}

#[test]
#[cfg(unix)]
#[ignore = "exhaustive: additional CLI Map matrix; run by the stress check"]
fn exhaustive_owned_map_cli_value_matrix() {
    check_owned_map_cli_cases(true);
}

#[test]
#[cfg(unix)]
fn generic_map_public_keys_values_and_examples_match_all_backends() {
    check_generic_map_cli_cases(false);
}

#[test]
#[cfg(unix)]
#[ignore = "exhaustive: additional CLI Map matrix; run by the stress check"]
fn exhaustive_generic_map_cli_type_matrix() {
    check_generic_map_cli_cases(true);
}

#[test]
#[cfg(unix)]
fn map_borrowing_and_shared_values_match_all_backends() {
    check_map_borrowing_cli_cases(false);
}

#[test]
#[cfg(unix)]
#[ignore = "exhaustive: additional CLI Map matrix; run by the stress check"]
fn exhaustive_map_borrowing_cli_value_matrix() {
    check_map_borrowing_cli_cases(true);
}

#[test]
#[cfg(unix)]
fn map_borrowing_cross_module_aliases_match_all_backends() {
    let work = Workspace::new();
    work.write(
        "map_views_library.jk",
        include_str!("../fixtures/map_views_library.jk"),
    );
    let prefix = recording_cli_source(include_str!("../fixtures/map_views_modules.jk"))
        .split("fn main()")
        .next()
        .unwrap()
        .to_owned();
    for name in ["copies", "owners", "references", "iteration"] {
        work.write(
            "map_views_modules.jk",
            &format!("{prefix} fn main() {{ record_int({name}::<Unit>()) }}"),
        );
        work.assert_all_backends("map_views_modules.jk", &[], b"42\n", 0);
    }
}

#[test]
#[cfg(unix)]
fn map_iteration_and_batches_match_all_backends() {
    check_map_iteration_cli_cases(false);
}

#[test]
#[cfg(unix)]
#[ignore = "exhaustive: additional CLI Map matrix; run by the stress check"]
fn exhaustive_map_iteration_cli_value_matrix() {
    check_map_iteration_cli_cases(true);
}

#[test]
#[cfg(unix)]
fn map_stress_matches_the_owner_ledger_in_standalone_aot() {
    // One cycle covers growth, dense deletion, rehash, random operations and clear.
    check_map_stress_in_standalone_aot(256);
}

#[test]
#[cfg(unix)]
#[ignore = "exhaustive: long AOT Map model matrix; run by the stress check"]
fn exhaustive_map_stress_matches_the_owner_ledger_in_standalone_aot() {
    check_map_stress_in_standalone_aot(map_stress_oracle::STEPS);
}

#[test]
#[cfg(unix)]
fn show_map_and_explicit_io_work_without_prelude_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "map.jk",
        r#"
        import joky/io.{print, println}
        import joky/map.{type Map}
        fn main() {
            var values = Map.new::<Int32, String>()
            values.insert(42, "hello")
            let message = "map={values}"; println(message)
            println(values)
            values.remove(42)
            let empty = "{values}"; println(empty)
        }
    "#,
    );
    work.assert_all_backends(
        "map.jk",
        &["--no-prelude"],
        b"map={42: hello}\n{42: hello}\n{}\n",
        0,
    );
}

#[test]
#[cfg(unix)]
fn list_example_and_cross_module_aliases_run_in_all_backends() {
    let work = Workspace::new();
    work.write("lists.jk", include_str!("../../examples/lists.jk"));
    work.assert_all_backends(
        "lists.jk",
        &["--no-prelude"],
        b"last\n0\nlast\nbefore\nextra\ntail\n",
        0,
    );
    work.write("views.jk", include_str!("../fixtures/list_views.jk"));
    work.write(
        "aliased.jk",
        r#"
        import views as chains
        import joky/array.{type Array}
        import joky/element as element
        import joky/io.{println}
        fn main() {
            var input=Array.new::<Int32>(); input.push(20); input.push(22)
            var list=chains.import_array(input)
            element.write(chains.edit(&mut list,0),20)
            let sum=element.read(chains.view(&list,0))+chains.copied(&list,1).unwrap_or(0)
            list.reverse()
            let output=chains.export(list)
            println(&sum); println(output.get(0).unwrap_or(0))
        }
    "#,
    );
    work.assert_all_backends("aliased.jk", &["--no-prelude"], b"42\n22\n", 0);
}

#[test]
#[cfg(unix)]
fn set_example_and_cross_module_aliases_run_in_all_backends() {
    let work = Workspace::new();
    work.write("sets.jk", include_str!("../../examples/sets.jk"));
    work.assert_all_backends(
        "sets.jk",
        &["--no-prelude"],
        b"2\ntrue\n3\ntrue\ntrue\n3\n",
        0,
    );
    work.write("views.jk", include_str!("../fixtures/set_views.jk"));
    work.write(
        "aliased.jk",
        r#"
        import views as groups
        import joky/array.{type Array}
        import joky/io.{println}
        import joky/string
        fn main() {
            var input=Array.new::<String>(); input.push("apple"); input.push("apple")
            var set=groups.import_array(input)
            var query="apple"
            println(groups.contains(&set,query))
            println(groups.remove(&mut set,query))
            query="pear"
            groups.insert(&mut set,query)
            let output=groups.export(set)
            println(output.borrow_ref(0))
        }
    "#,
    );
    work.assert_all_backends("aliased.jk", &["--no-prelude"], b"true\ntrue\npear\n", 0);
}

#[test]
#[cfg(unix)]
fn existing_primitives_run_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "primitives.jk",
        include_str!("../fixtures/json_primitives.jk"),
    );
    work.assert_all_backends("primitives.jk", &[], b"42\n", 0);
}

#[test]
#[cfg(unix)]
fn recursive_owners_run_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "recursive.jk",
        include_str!("../fixtures/json_recursive_owners.jk"),
    );
    work.assert_all_backends("recursive.jk", &[], b"1\n2\n", 0);
    let source = include_str!("../fixtures/recursive_owner_cleanup.jk").replace(
        "extern fn record_drop(value: Int32)",
        "fn record_drop(value: Int32) { println(value) }",
    );
    work.write(
        "transfers.jk",
        &format!("{source}\nfn main() {{ probe(); }}"),
    );
    work.assert_all_backends(
        "transfers.jk",
        &[],
        b"-1\n-2\n-3\n-4\n3\n2\n-5\n5\n-100\n4\n1\n",
        0,
    );
}

#[test]
fn inline_recursion_is_rejected_before_build() {
    let work = Workspace::new();
    for source in [
        "enum Value { Next(Value) }",
        "import joky/option.{type Option} enum Value { Next(Option<Value>) }",
        "type Next = Value enum Value { More((Int32, Next)) }",
        "struct First { next: Second } struct Second { next: First }",
    ] {
        work.write("recursive.jk", &format!("{source}\nfn main() {{}}"));
        failure(&work.cli(&["build", "recursive.jk"]), "recursive_");
        assert!(!work.path().join("recursive").exists());
    }
    work.assert_no_build_files();
}

#[test]
fn expanding_types_and_live_owner_loans_remain_rejected() {
    let work = Workspace::new();
    for (source, code) in [
        ("import joky/array.{type Array} enum Bad<T> { Items(Array<Bad<(T,)>>) } fn main() {}", "resource_limit"),
        ("import joky/array.{type Array} enum Value { Nil, Items(Array<Value>) } fn main() { var values = Array.new::<Value>(); values.push(Value.Nil); let view = values.borrow_ref(0); values.push(Value.Nil); match view { Nil => {}, _ => {} } }", "invalid_assignment"),
        ("import joky/array.{type Array} import joky/mem enum Value { Nil, Items(Array<Value>) } fn main() { var value = Value.Items(Array.new::<Value>()); let view = &value; let old = mem.replace(&mut value, Value.Nil); match view { Nil => {}, _ => {} } }", "invalid_assignment"),
        ("import joky/array.{type Array} enum Value { Nil, Items(Array<Value>) } fn main() { let values = Array.new::<Value>(); values.get(0) }", "type_mismatch"),
        ("import joky/array.{type Array} struct Shared { reference: &Int32 } enum Value { Nil, Items(Array<Value>), Hidden(Array<Shared>) } fn main() {}", "type_mismatch"),
        ("struct First<T, U> { value: T } enum Bad { Pair((First<Int32, Bad>, First<Bad, Int32>)) } fn main() {}", "recursive_type"),
        ("import joky/array.{type Array} struct Outer { values: Array<Holder> } struct Holder { reference: &Int32 } fn bad(value: Outer) @suspends {} fn main() {}", "invalid_call"),
        ("import joky/array.{type Array} struct Borrowed { target: &Owner } struct Owner { values: Array<Int32> } fn bad(values: Array<Borrowed>) {} fn main() {}", "type_mismatch"),
    ] {
        work.write("invalid.jk", source);
        failure(&work.cli(&["build", "invalid.jk"]), code);
        assert!(!work.path().join("invalid").exists());
    }
    work.assert_no_build_files();
}

#[test]
fn values_and_numbers_run_in_all_backends() {
    let work = Workspace::new();
    work.write("values.jk", include_str!("../fixtures/json_values.jk"));
    work.assert_all_backends("values.jk", &[], b"", 0);
}

#[test]
fn number_invariants_and_unique_value_ownership_are_enforced() {
    let work = Workspace::new();
    for (source, diagnostic) in [
        ("import joky/json.{type Number} fn main() { let number = Number { text: \"NaN\" } }", "private"),
        ("import joky/json as json fn main() { let text = \"1\"; json.parse_float64(&text) }", "unknown_name"),
        ("import joky/json.{type Number} fn main() { let number = Number.from_i64(1); number.text }", "private"),
        ("import joky/json.{type Value} import joky/array.{type Array} fn main() { let values = Array.new::<Value>(); values.get(0) }", "type_mismatch"),
    ] {
        work.write("invalid.jk", source);
        failure(&work.cli(&["build", "invalid.jk"]), diagnostic);
        assert!(!work.path().join("invalid").exists());
    }
    work.assert_no_build_files();
}

#[test]
fn strict_decoding_and_limits_run_in_all_backends() {
    let work = Workspace::new();
    work.write("decode.jk", include_str!("../fixtures/json_decode.jk"));
    work.assert_all_backends("decode.jk", &[], b"", 0);
}

#[test]
fn roundtrips_match_independent_semantic_models_in_all_backends() {
    let work = Workspace::new();
    work.write("models.jk", include_str!("../fixtures/json_model.jk"));
    work.assert_all_backends("models.jk", &[], b"", 0);
}

#[test]
fn compact_encoding_limits_and_borrowed_inputs_run_in_all_backends() {
    let work = Workspace::new();
    work.write("encode.jk", include_str!("../fixtures/json_encode.jk"));
    work.assert_all_backends("encode.jk", &[], b"", 0);
}

#[test]
fn loopback_example_runs_in_all_backends() {
    let work = Workspace::new();
    work.write("json_tcp.jk", include_str!("../../examples/tcp_json.jk"));
    work.assert_all_backends("json_tcp.jk", &[], b"push\nerror\nreply\nerror\nreply\n", 0);
}

#[test]
fn native_peer_fragmentation_errors_and_terminal_frames_in_standalone_aot() {
    mod endpoint {
        include!("../support/json_tcp.rs");
    }
    // json_tcp_tests covers every peer case in both VM modes, with owner ledgers.
    for case in 0..=3 {
        let peer = endpoint::peer(case);
        let work = Workspace::new();
        let entry = if case == 3 {
            format!("pressure({})", peer.port)
        } else {
            format!("native({}, {})", peer.port, if case == 0 { 2 } else { -2 })
        };
        let source = endpoint::source().replace("extern fn sent()", "fn sent() {}");
        work.write(
            "session.jk",
            &format!("{source}\nfn main() @suspends -> Int32 {{ ensure({entry} == 42); 0 }}"),
        );
        // The VM test holds this gate until writes really stall; CLI checks wire parity.
        let mut peer = peer;
        peer.release();
        success(&work.cli(&["build", "session.jk"]));
        let output = Command::new(work.path().join("session")).output().unwrap();
        success(&output);
        assert!(
            output.stdout.is_empty() && output.stderr.is_empty(),
            "{output:?}"
        );
        assert_eq!(peer.finish(), if case == 3 { 512 } else { 0 });
        work.assert_no_build_files();
    }
}

#[test]
fn json_typed_structs_and_example_run_in_all_backends() {
    let work = Workspace::new();
    work.write("typed.jk", include_str!("../fixtures/json_typed.jk"));
    work.assert_all_backends("typed.jk", &[], b"", 0);
    work.write("struct.jk", include_str!("../../examples/json_struct.jk"));
    work.assert_all_backends("struct.jk", &[], b"Ada\n37\n/age\n", 0);
}

#[test]
fn json_typed_scalars_containers_and_limits_run_in_all_backends() {
    let work = Workspace::new();
    work.write_fixture_batch(
        "typed_matrix.jk",
        &[
            (
                "numbers.jk",
                include_str!("../fixtures/json_typed_numbers.jk"),
            ),
            (
                "unsigned.jk",
                include_str!("../fixtures/json_typed_unsigned.jk"),
            ),
            (
                "floats.jk",
                include_str!("../fixtures/json_typed_floats.jk"),
            ),
            (
                "containers.jk",
                include_str!("../fixtures/json_typed_containers.jk"),
            ),
            (
                "errors.jk",
                include_str!("../fixtures/json_typed_errors.jk"),
            ),
            (
                "map_encode.jk",
                include_str!("../fixtures/json_typed_map_encode.jk"),
            ),
            (
                "dispatch.jk",
                include_str!("../fixtures/json_typed_dispatch.jk"),
            ),
            (
                "limits.jk",
                include_str!("../fixtures/json_typed_limits.jk"),
            ),
            (
                "cancel.jk",
                include_str!("../fixtures/json_typed_cancel.jk"),
            ),
        ],
    );
    work.assert_all_backends("typed_matrix.jk", &[], b"", 0);
}

#[test]
fn json_typed_custom_keys_reject_collisions_in_all_backends() {
    let work = Workspace::new();
    work.write("keys.jk", include_str!("../fixtures/json_typed_keys.jk"));
    work.assert_all_backends("keys.jk", &[], b"", 0);
}

#[test]
fn json_typed_nested_map_roundtrip_runs_in_all_backends() {
    let work = Workspace::new();
    work.write("map.jk", include_str!("../fixtures/json_typed_map.jk"));
    work.assert_all_backends("map.jk", &[], b"", 0);
}

#[test]
fn json_derive_composition_and_example_run_in_all_backends() {
    let work = Workspace::new();
    work.write_fixture_batch(
        "derived_matrix.jk",
        &[
            ("derive.jk", include_str!("../fixtures/json_derive.jk")),
            (
                "derive_example.jk",
                include_str!("../../examples/json_derive.jk"),
            ),
            (
                "derive_lifecycle.jk",
                include_str!("../fixtures/json_derive_lifecycle.jk"),
            ),
            (
                "derive_cancel.jk",
                include_str!("../fixtures/json_derive_cancel.jk"),
            ),
        ],
    );
    work.assert_all_backends("derived_matrix.jk", &[], b"Ada\n37\n/age\n", 0);
}

#[test]
fn json_derive_dependencies_keep_private_fields_and_generic_definition_scope() {
    let work = Workspace::new();
    work.write(
        "model.jk",
        r#"
        #derive(ToJson, FromJson)
        pub struct Packet<T> { data: T, label: String }
        pub fn make() -> Packet<String> { Packet { data: "payload", label: "private" } }
        pub fn valid(value: &Packet<String>) -> Bool {
            let expected = "payload"
            let label = "private"
            value.data.equals(&expected) && value.label.equals(&label)
        }
    "#,
    );
    work.write(
        "entry.jk",
        r#"
        import model.{type Packet as Wire}
        import joky/json/typed as codec
        import joky/check.{assert}
        trait ToJson { fn unrelated(&self) }
        struct Context {}
        fn main() {
            let value = model.make()
            let message = "derived dependency"
            match codec.encode(&value) {
                Ok(text) => match codec.decode::<Wire<String>>(&text) {
                    Ok(result) => { assert(model.valid(&result), &message) }
                    Err(_) => { assert(false, &message) }
                }
                Err(_) => { assert(false, &message) }
            }
        }
    "#,
    );
    work.assert_all_backends("entry.jk", &[], b"", 0);
    work.write(
        "private.jk",
        "import model fn main() { let value = model.make(); value.data; }",
    );
    failure(&work.cli(&["build", "private.jk"]), "private");
}

#[test]
#[cfg(unix)]
fn array_literals_and_sequence_patterns_run_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "array_patterns.jk",
        include_str!("../fixtures/array_patterns.jk"),
    );
    work.assert_all_backends("array_patterns.jk", &[], b"array patterns ok\n", 0);
}

#[test]
#[cfg(unix)]
fn array_sequence_cleanup_runs_in_all_backends() {
    let work = Workspace::new();
    let source = include_str!("../fixtures/array_patterns_cleanup.jk").replace(
        "extern fn note(value: Int32)",
        "fn note(value: Int32) { println(value) }",
    );
    work.write("array_cleanup.jk", &source);
    work.assert_all_backends(
        "array_cleanup.jk",
        &[],
        b"1\n2\n3\n4\n-3\n-2\n-1\n-4\n42\n1\n3\n-3\n-1\n42\n",
        0,
    );
    let failed = format!(
        "{}\nfn main() {{ failed_init(); }}",
        source.split("fn main()").next().unwrap()
    );
    work.write("array_failed_init.jk", &failed);
    let mut outputs = vec![
        work.cli(&["run", "array_failed_init.jk"]),
        work.cli(&["run", "--interpreter", "array_failed_init.jk"]),
    ];
    success(&work.cli(&["build", "array_failed_init.jk"]));
    outputs.push(
        Command::new(work.path().join("array_failed_init"))
            .output()
            .unwrap(),
    );
    for output in outputs {
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert_eq!(output.stdout, b"1\n-1\n", "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("Division by zero"),
            "{output:?}"
        );
    }
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn array_spread_executes_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "array_spread.jk",
        include_str!("../fixtures/array_spread.jk"),
    );
    work.assert_all_backends("array_spread.jk", &[], b"array spread ok\n", 0);
}

#[test]
#[cfg(unix)]
fn array_spread_cleanup_runs_in_all_backends() {
    let work = Workspace::new();
    let source = include_str!("../fixtures/array_spread_cleanup.jk").replace(
        "extern fn note(value: Int32)",
        "fn note(value: Int32) { println(value) }",
    );
    work.write("spread_cleanup.jk", &source);
    work.assert_all_backends(
        "spread_cleanup.jk",
        &[],
        b"1\n2\n3\n4\n-3\n-2\n-1\n-4\n42\n1\n2\n3\n4\n-4\n-2\n-1\n-3\n42\n1\n2\n3\n-2\n-1\n-3\n42\n",
        0,
    );
    let declarations = source.split("fn main()").next().unwrap();
    for (entry, stdout) in [
        ("failed_source", "1\n2\n3\n4\n-4\n-3\n-2\n-1\n"),
        ("failed_after", "1\n2\n-2\n-1\n"),
        ("failed_drop", "2\n1\n3\n-3\n-2\n-1\n"),
    ] {
        work.write(
            "spread_failure.jk",
            &format!("{declarations}\nfn main() {{ {entry}(); }}"),
        );
        let mut outputs = vec![
            work.cli(&["run", "spread_failure.jk"]),
            work.cli(&["run", "--interpreter", "spread_failure.jk"]),
        ];
        success(&work.cli(&["build", "spread_failure.jk"]));
        outputs.push(
            Command::new(work.path().join("spread_failure"))
                .output()
                .unwrap(),
        );
        for output in outputs {
            assert_eq!(output.status.code(), Some(1), "{entry}: {output:?}");
            assert_eq!(output.stdout, stdout.as_bytes(), "{entry}: {output:?}");
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("Division by zero"),
                "{entry}: {output:?}"
            );
        }
    }
    work.assert_no_build_files();
}

// VM tests cover the complete matrix daily. Keep representative CLI cases and
// diagnostics here; additional CLI specializations run in the stress check.
#[cfg(unix)]
fn check_owned_map_cli_cases(additional_cases: bool) {
    let work = Workspace::new();
    let prefix = recording_cli_source(include_str!("../fixtures/owned_map.jk"))
        .split("fn main()")
        .next()
        .unwrap()
        .replace("maps.test_invariant(&m)", "true");
    // Share specializations within the same groups as the VM tests.
    let groups: &[(&[&str], &str)] = if additional_cases {
        &[
            (&["nested_values"], "-6\n-7\n7\n6\n42\n"),
            (&["arrays", "reuse_budget"], "42\n42\n"),
        ]
    } else {
        &[(
            &["transitions", "return_move"],
            "-1\n-2\n101\n102\n-3\n-4\n3\n4\n-5\n-90\n5\n2\n1\n42\n-8\n8\n42\n",
        )]
    };
    for (names, expected) in groups {
        let body: String = names
            .iter()
            .map(|name| format!("record_int({name}::<Unit>());"))
            .collect();
        work.write("owned_map.jk", &format!("{prefix} fn main() {{ {body} }}"));
        work.assert_all_backends("owned_map.jk", &[], expected.as_bytes(), 0);
    }
    if additional_cases {
        return;
    }
    for (name, expected, message) in [
        ("clear_failure", "3\n2\n1\n", "Map Drop failure"),
        ("replacement_drop_failure", "2\n4\n", "Map Drop failure"),
        ("runtime_failure", "-1\n1\n", "Map operation failure"),
    ] {
        work.write(
            "failure.jk",
            &format!("{prefix} fn main() {{ {name}::<Unit>(); }}"),
        );
        let assert_failure = |output: std::process::Output| {
            assert!(!output.status.success());
            assert_eq!(output.stdout, expected.as_bytes());
            assert!(String::from_utf8_lossy(&output.stderr).contains(message));
        };
        assert_failure(work.cli(&["run", "failure.jk"]));
        assert_failure(work.cli(&["run", "--interpreter", "failure.jk"]));
        success(&work.cli(&["build", "failure.jk"]));
        assert_failure(Command::new(work.path().join("failure")).output().unwrap());
    }
}

// VM tests cover the complete matrix daily. Keep representative CLI cases and
// diagnostics here; additional CLI specializations run in the stress check.
#[cfg(unix)]
fn check_generic_map_cli_cases(additional_cases: bool) {
    let work = Workspace::new();
    let prefix = &recording_cli_source(include_str!("../fixtures/generic_map.jk"))
        .split("fn main()")
        .next()
        .unwrap()
        .to_owned();
    let groups: &[(&[&str], &str)] = if additional_cases {
        &[
            (&["integer32"], "42\n"),
            (&["integer64"], "42\n"),
            (&["unsigned32"], "42\n"),
            (&["unsigned64"], "42\n"),
            (&["booleans"], "42\n"),
            (&["units"], "42\n"),
            (&["collisions"], "42\n"),
            (&["enums"], "42\n"),
            (&["lifecycle"], "42\n"),
            (&["array_of_maps"], "42\n"),
            (&["map_of_arrays"], "42\n"),
            (&["map_of_maps"], "42\n"),
        ]
    } else {
        // These cases share specializations, as in the VM test group.
        &[(
            &["retained_key", "cached_rehash"],
            "11\n11\n11\n42\n101\n102\n201\n900\n901\n101\n201\n102\n201\n202\n42\n",
        )]
    };
    for (names, expected) in groups {
        let body: String = names
            .iter()
            .map(|name| format!("record_int({name}::<Unit>());"))
            .collect();
        work.write(
            "generic_map.jk",
            &format!("{prefix} fn main() {{ {body} }}"),
        );
        work.assert_all_backends("generic_map.jk", &[], expected.as_bytes(), 0);
    }
    if additional_cases {
        return;
    }
    work.write("maps.jk", include_str!("../../examples/maps.jk"));
    work.assert_all_backends("maps.jk", &[], b"42\n42\n42\n42\n42\n", 0);
    work.write(
        "map_batches.jk",
        include_str!("../../examples/map_batches.jk"),
    );
    work.assert_all_backends("map_batches.jk", &[], b"42\n", 0);
    for (name, message) in [
        ("hash_failure", "Map Hash failure"),
        ("eq_failure", "Map Eq failure"),
    ] {
        work.write(
            "failure.jk",
            &format!("{prefix} fn main() {{ {name}::<Unit>(); }}"),
        );
        let assert_failure = |output: std::process::Output| {
            assert!(!output.status.success());
            assert_eq!(output.stdout, b"2\n1\n");
            assert!(String::from_utf8_lossy(&output.stderr).contains(message));
        };
        assert_failure(work.cli(&["run", "failure.jk"]));
        assert_failure(work.cli(&["run", "--interpreter", "failure.jk"]));
        success(&work.cli(&["build", "failure.jk"]));
        assert_failure(Command::new(work.path().join("failure")).output().unwrap());
    }
}

// VM tests cover the complete matrix daily. Keep representative CLI cases and
// diagnostics here; additional CLI specializations run in the stress check.
#[cfg(unix)]
fn check_map_borrowing_cli_cases(additional_cases: bool) {
    let work = Workspace::new();
    let prefix = recording_cli_source(include_str!("../fixtures/map_borrowing.jk"))
        .split("fn main()")
        .next()
        .unwrap()
        .to_owned();
    for (name, expected) in [
        ("copy_views", "42\n"),
        ("owned_views", "-1\n-2\n2\n1\n42\n"),
        ("nested_views", "42\n"),
        ("reference_values", "42\n"),
        ("shared_aggregate", "42\n"),
        ("copied_reference", "42\n"),
        ("returned_reference", "42\n"),
        ("aggregate_references", "42\n"),
    ] {
        if additional_cases == matches!(name, "owned_views" | "shared_aggregate") {
            continue;
        }
        work.write(
            "map_borrowing.jk",
            &format!("{prefix} fn main() {{ record_int({name}::<Unit>()) }}"),
        );
        work.assert_all_backends("map_borrowing.jk", &[], expected.as_bytes(), 0);
    }
    if additional_cases {
        return;
    }
    for (name, expression) in [
        ("missing_copy", "m.borrow(2)"),
        ("missing_mut", "m.borrow_mut(2)"),
        ("missing_owner", "m.borrow_ref(2)"),
        ("missing_owner_mut", "m.borrow_mut_ref(2)"),
    ] {
        let source = format!("{prefix} fn main() {{ {name}::<Unit>(); }}");
        let start = source.rfind(expression).unwrap();
        let end = start + expression.len();
        let line = source[..start].bytes().filter(|&b| b == b'\n').count() + 1;
        work.write("failure.jk", &source);
        let assert_failure = |output: std::process::Output, native: bool| {
            assert!(!output.status.success(), "{output:?}");
            assert_eq!(
                output.stdout,
                if name.contains("owner") {
                    b"-1\n1\n".as_slice()
                } else {
                    b""
                }
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("Map key not found"), "{stderr}");
            let call = if native {
                format!("(bytes {start}..{end})")
            } else {
                format!("failure.jk:{line}")
            };
            assert!(stderr.contains(&call), "missing caller {call}: {stderr}");
        };
        assert_failure(work.cli(&["run", "failure.jk"]), false);
        assert_failure(work.cli(&["run", "--interpreter", "failure.jk"]), false);
        success(&work.cli(&["build", "failure.jk"]));
        assert_failure(
            Command::new(work.path().join("failure")).output().unwrap(),
            true,
        );
    }
}

// VM tests cover the complete matrix daily. Keep representative CLI cases and
// diagnostics here; additional CLI specializations run in the stress check.
#[cfg(unix)]
fn check_map_iteration_cli_cases(additional_cases: bool) {
    let work = Workspace::new();
    let prefix = recording_cli_source(include_str!("../fixtures/map_iteration.jk"))
        .split("fn main()")
        .next()
        .unwrap()
        .to_owned();
    for (name, expected) in [
        ("copy_views", "42\n"),
        ("consuming", "42\n"),
        ("batches", "42\n"),
        (
            "owned_pipeline",
            "-1\n-2\n-3\n1\n-4\n2\n3\n900\n901\n4\n42\n",
        ),
        ("references", "42\n"),
        ("unbounded", "42\n"),
        ("protocol_free_export", "101\n102\n42\n"),
    ] {
        if additional_cases == matches!(name, "owned_pipeline" | "batches") {
            continue;
        }
        work.write(
            "map_iteration.jk",
            &format!("{prefix} fn main() {{ record_int({name}::<Unit>()) }}"),
        );
        work.assert_all_backends("map_iteration.jk", &[], expected.as_bytes(), 0);
    }
    if additional_cases {
        return;
    }
    work.write(
        "failure.jk",
        &format!("{prefix} fn main() {{ drop_failure::<Unit>(); }}"),
    );
    let assert_failure = |output: std::process::Output| {
        assert!(!output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("Batch drop failure"));
        let events: Vec<i32> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|s| s.parse().unwrap())
            .collect();
        assert_eq!(&events[..7], &[-1, 101, -2, -3, -4, 101, 1]);
        let mut remaining = events[7..].to_vec();
        remaining.sort();
        assert_eq!(remaining, vec![2, 3, 4]);
    };
    assert_failure(work.cli(&["run", "failure.jk"]));
    assert_failure(work.cli(&["run", "--interpreter", "failure.jk"]));
    success(&work.cli(&["build", "failure.jk"]));
    assert_failure(Command::new(work.path().join("failure")).output().unwrap());
}

#[cfg(unix)]
fn check_map_stress_in_standalone_aot(steps: i32) {
    use map_stress_oracle as oracle;
    let work = Workspace::new();
    let mut source = recording_cli_source(&oracle::source(oracle::SEEDS[0], 0, 0, false));
    source.push_str("\nfn main() {");
    for distribution in 0..3 {
        for seed in oracle::SEEDS {
            source.push_str(&format!(
                " stress({seed},{distribution},{steps}); record_int(-2147483647); "
            ));
        }
    }
    source.push('}');
    work.write("stress.jk", &source);
    success(&work.cli(&["build", "stress.jk"]));
    let output = Command::new(work.path().join("stress")).output().unwrap();
    success(&output);
    let events: Vec<i32> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|v| v.parse().unwrap())
        .collect();
    let chunks: Vec<_> = events.split(|&v| v == -2147483647).collect();
    assert_eq!(chunks.len(), 7);
    assert!(chunks[6].is_empty());
    for (index, chunk) in chunks[..6].iter().enumerate() {
        oracle::verify(chunk, oracle::SEEDS[index % 2], steps, true);
    }
}
