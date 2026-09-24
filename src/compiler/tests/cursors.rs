use super::*;

const COUNTER: &str = r#"
struct Counter { let position: Int32; let end: Int32 }
impl Cursor for Counter {
    type Item = Int32
    fn advance(self) -> Option((Int32, Self)) {
        if self.position < self.end {
            Some((self.position, Counter(position: self.position + 1, end: self.end)))
        } else { None }
    }
}
"#;

#[test]
fn cursor_collects_with_index_skip_break_and_nested_loops() {
    run_program(&format!(
        r#"{COUNTER}
        fn main() {{
            let one: UInt64 = 1
            let result = for (i, value) in Counter(position: 0, end: 5) {{
                if i == one {{ continue }} else {{}}
                if value == 4 {{ break }} else {{ value + 10 }}
            }}
            if result != List(10, 12, 13) {{ panic("cursor result") }} else {{}}
            let empty = for x in Counter(position: 0, end: 0) {{ x }}
            if !empty.is_empty() {{ panic("empty cursor") }} else {{}}
            let indices = for (i, x) in Counter(position: 0, end: 3) {{ i }}
            let expected: List(UInt64) = List(0, 1, 2)
            if indices != expected {{ panic("cursor index") }} else {{}}
            let nested = for x in Counter(position: 1, end: 3) {{
                let values = for y in Counter(position: 10, end: 12) {{ x + y }}
                values.head()!
            }}
            if nested != List(11, 12) {{ panic("nested cursor") }} else {{}}
        }}
    "#
    ));
}

#[test]
fn bytes_cursor_iterates_streams_and_survives_the_input() {
    run_program(
        r#"
        fn drain(c: BytesCursor) -> List(UInt8) {
            for b in c { b }
        }
        fn collect(T: type + Cursor, source: T) -> List(T.Item) {
            for item in source { item }
        }
        fn main() {
            let u1: UInt64 = 1
            let u3: UInt64 = 3
            let one: UInt8 = 1
            let three: UInt8 = 3
            let data = Bytes.from_string("abc")
            let expected: List(UInt8) = List(97, 98, 99)
            if for byte in data.iter() { byte } != expected { panic("bytes items") }
            let empty = for byte in Bytes.from_string("").iter() { byte }
            if !empty.is_empty() { panic("empty bytes") }
            if drain(data.iter()) != expected { panic("annotated cursor") }
            if collect(data.iter()) != expected { panic("generic cursor") }
            let pairs = for (i, byte) in data.iter() { (i, byte) }
            if pairs.length() != u3 { panic("indexed bytes") }
            var seen: UInt8 = 0
            for (i, byte) in data.iter() {
                if i == u1 { break }
                seen = seen + 1
                continue
            }
            if seen != one { panic("bytes break") }
            var steps: UInt8 = 0
            var cursor = data.iter()
            loop {
                match cursor.advance() {
                    Some(pair) => { steps = steps + 1; cursor = pair.1 }
                    None => break
                }
            }
            if steps != three { panic("handwritten advance") }
            if data.length() != u3 { panic("input alive after iter") }
        }
    "#,
    );
}

#[test]
fn map_and_set_cursors_iterate_entries_keys_values_and_survive() {
    run_program(
        r#"
        fn collect_keys(m: Map(Int32, Int32)) -> List(Int32) {
            for k in m.keys() { k }
        }
        fn main() {
            let u1: UInt64 = 1
            let u3: UInt64 = 3
            var table: Map(Int32, Int32) = Map#{}
            table = table.insert(1, 10)
            table = table.insert(2, 20)
            table = table.insert(3, 30)

            let keys = for k in table.keys() { k }
            if keys.sorted() != List(1, 2, 3) { panic("keys") }
            if keys.length() != u3 { panic("key count") }
            if collect_keys(table).sorted() != List(1, 2, 3) { panic("annotated") }

            var total = 0
            for v in table.values() {
                total = total + v
                continue
            }
            if total != 60 { panic("values") }

            var seen = 0
            for (index, entry) in table.entries() {
                if index == u1 { break }
                seen = seen + 1
                continue
            }
            // Iteration order is hash order, so only the count is stable.
            if seen != 1 { panic("entries break leaves one value") }

            let entries = for entry in table.entries() { entry.0 }
            if entries.sorted() != List(1, 2, 3) { panic("entry keys") }

            var letters: Set(Int32) = Set#{}
            letters = letters.insert(7, true).insert(8, true)
            let members = for m in letters.iter() { m }
            if members.sorted() != List(7, 8) { panic("set iter") }

            let empty: Map(Int32, Int32) = Map#{}
            let none = for k in empty.keys() { k }
            if !none.is_empty() { panic("empty map") }

            var steps = 0
            var cursor = table.entries()
            loop {
                match cursor.advance() {
                    Some(pair) => { steps = steps + 1; cursor = pair.1 }
                    None => break
                }
            }
            if steps != 3 { panic("handwritten advance") }
            if table.length() != u3 { panic("input alive") }

            var texts: Map(String, String) = Map#{}
            texts = texts.insert("a", "alpha").insert("b", "beta")
            let joined = for entry in texts.entries() { entry.1 }
            var lengths = 0
            for text in joined {
                lengths = lengths + 1
                continue
            }
            if lengths != 2 { panic("string values") }
        }
    "#,
    );
}

#[test]
fn cursor_generic_for_and_explicit_advance_accept_lists() {
    run_program(&format!(
        r#"{COUNTER}
        fn collect(T: type + Cursor, source: T) -> List(T.Item) {{
            for item in source {{ item }}
        }}
        fn step(T: type + Cursor, source: T) -> Option((T.Item, T)) {{
            Cursor.advance(source)
        }}
        fn forward(T: type + Cursor, source: T) -> List(T.Item) {{ collect(source) }}
        fn main() {{
            if collect(Counter(position: 0, end: 3)) != List(0, 1, 2) {{ panic("generic cursor") }} else {{}}
            if collect(List("a", "b")) != List("a", "b") {{ panic("generic List") }} else {{}}
            if forward(List(1, 2)) != List(1, 2) {{ panic("forwarded List") }} else {{}}
            let pair = step(List("a", "b"))!
            if pair.0 != "a" {{ panic("List Item") }} else {{}}
            if pair.1 != List("b") {{ panic("List remainder") }} else {{}}
            let empty: List(Int32) = List#{{}}
            if empty.advance().is_some() {{ panic("empty advance") }} else {{}}
        }}
    "#
    ));
}

#[test]
fn cursor_shared_and_owned_states() {
    run_program(
        r#"
        struct Shared { let state: String; let count: Int32 }
        impl Cursor for Shared {
            type Item = String
            fn advance(self) -> Option((String, Self)) {
                if self.count == 0 { None } else {
                    Some((self.state, Shared(state: self.state, count: self.count - 1)))
                }
            }
        }
        class Owned { let count: Int32 }
        impl Cursor for Owned {
            type Item = String
            fn advance(self) -> Option((String, Self)) {
                if self.count == 0 { None } else {
                    Some(("owned", Owned(count: self.count - 1)))
                }
            }
        }
        fn main() {
            let shared = for value in Shared(state: "shared", count: 2) { value }
            if shared != List("shared", "shared") { panic("shared cursor") }
            let owned = for value in Owned(count: 2) { value }
            if owned != List("owned", "owned") { panic("owned cursor") }
        }
    "#,
    );
}

#[test]
fn cursor_rejects_parallel_wrong_receiver_and_hidden_effects() {
    for (source, expected) in [
        (format!("{COUNTER} fn main() {{ @parallel(limit: 2) for x in Counter(position: 0, end: 2) {{ x }} }}"), "parallel for requires a List"),
        (COUNTER.replace("advance(self)", "advance(&self)") + " fn main() {}", "Cursor"),
        (format!("{} fn consume(T: type + Cursor, value: T) {{ for x in value {{ continue }}; () }} fn main() {{ consume(Counter(position: 0, end: 2)) }}", COUNTER.replace("fn advance(self) -> Option((Int32, Self))", "fn advance(self) -> Option((Int32, Self)) effects { Signal }") + " eff Signal { fn read() -> Unit }"), "declares effects not allowed"),
    ] {
        let error = try_run_program(&source).expect_err(&source).to_string();
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn cursor_effects_propagate_and_can_be_handled() {
    let source = COUNTER
        .replace(
            "fn advance(self) -> Option((Int32, Self))",
            "fn advance(self) -> Option((Int32, Self)) effects { Pull }",
        )
        .replace("self.position + 1", "self.position + Pull.step()");
    for mode in ["", "@resumable "] {
        run_program(&format!(
            r#"{source}
        eff Pull {{ {mode}fn step() -> Int32 }}
        fn main() {{
            let values = do {{ for item in Counter(position: 0, end: 3) {{ item }} }}
                with {{ Pull.step() => 1 }}
            if values != List(0, 1, 2) {{ panic("handled cursor effect") }} else {{}}
        }}
    "#
        ));
    }
    let error = try_run_program(&format!("{source} eff Pull {{ fn step() -> Int32 }} fn main() {{ for item in Counter(position: 0, end: 3) {{ item }} }}")).unwrap_err().to_string();
    assert!(error.contains("Pull"), "{error}");
    let forward = format!(
        r#"{source}
        eff Pull {{ fn step() -> Int32 }}
        struct Caller {{ fn collect(&self) -> List(Int32) {{ for item in Counter(position: 0, end: 2) {{ item }} }} }}
        fn main() {{ Caller().collect() }}
    "#
    );
    let program = syntax::parse_program(&forward).unwrap();
    let error = sema::check_program(&program).unwrap_err().to_string();
    assert!(error.contains("Pull"), "{error}");

    run_program(
        r#"
        eff Pull { @resumable fn step() -> Unit }
        eff Stop { @aborts fn stop() -> Unit }
        class Source {}
        impl Cursor for Source {
            type Item = Int32
            fn advance(self) -> Option((Int32, Self)) effects { Pull, Stop } {
                Pull.step()
                Stop.stop()
                None
            }
        }
        fn main() {
            do {
                do { for item in Source() { continue }; () }
                    with { Pull.step() => () }
            } with { Stop.stop() => () }
        }
    "#,
    );
}

#[test]
fn cursor_resumes_effectful_and_pure_generic_advance() {
    run_program(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        class Delayed { let count: Int32 }
        impl Cursor for Delayed {
            type Item = String
            fn advance(self) -> Option((String, Self)) effects { time } {
                time.sleep(0ms)
                if self.count == 0 { None } else { Some(("ready", Delayed(count: self.count - 1))) }
            }
        }
        class Pure { let count: Int32 }
        impl Cursor for Pure {
            type Item = String
            fn advance(self) -> Option((String, Self)) {
                let count = self.count
                let work = parallel { | count - 1 }
                if count == 0 { None } else { Some(("pure", Pure(count: work.0))) }
            }
        }
        fn collect(T: type + Cursor, value: T) -> List(T.Item) { for item in value { item } }
        fn main() effects { time } {
            let values = for item in Delayed(count: 2) { time.sleep(0ms); item }
            if values != List("ready", "ready") { panic("delayed cursor") }
            if collect(Pure(count: 2)) != List("pure", "pure") { panic("pure Pending cursor") }
        }
    "#,
    );
}

#[test]
fn cursor_ownership_matrix_releases_uncollected_items() {
    for state in ["copy", "shared", "owned"] {
        for item in ["Int32", "String", "Token"] {
            let (kind, fields, initial, next) = match state {
                "copy" => ("struct", "", "", ""),
                "shared" => (
                    "struct",
                    "; let bytes: Bytes",
                    ", bytes: Bytes.from_string(\"state\")",
                    ", bytes: self.bytes",
                ),
                _ => ("class", "", "", ""),
            };
            let value = match item {
                "Int32" => "self.count",
                "String" => "\"item\"",
                _ => "Token(id: self.count)",
            };
            for body in [
                "continue",
                "break",
                "if value_guard { break } else { continue }",
            ] {
                let source = format!(
                    r#"
                    class Token {{ let id: Int32 }}
                    {kind} Values {{ let count: Int32 {fields} }}
                    impl Cursor for Values {{
                        type Item = {item}
                        fn advance(self) -> Option(({item}, Self)) {{
                            if self.count == 0 {{ None }} else {{
                                Some(({value}, Values(count: self.count - 1 {next})))
                            }}
                        }}
                    }}
                    fn main() {{
                        let value_guard = true
                        let result: List(Unit) = for value in Values(count: 3 {initial}) {{ {body} }}
                        if !result.is_empty() {{ panic("skipped items must not be collected") }} else {{}}
                    }}
                "#
                );
                try_run_program(&source)
                    .unwrap_or_else(|error| panic!("{state}/{item}/{body}: {error}\n{source}"));
            }
        }
    }
}

#[test]
fn cursor_drains_branches_before_continue_break_and_exhaustion() {
    run_program(&format!(
        r#"{COUNTER}
        eff time {{ @suspends fn sleep(duration: Duration) -> Unit }}
        class Total {{ var count: Int32 = 0; fn increment() {{ self.count = self.count + 1 }} }}
        fn main() effects {{ time }} {{
            let total = Cown.new(Total())
            for item in Counter(position: 0, end: 4) {{
                branch {{ time.sleep(0ms); when (total) |state| {{ state.increment() }} }}
                if item == 1 {{ continue }} else {{}}
                if item == 2 {{ break }} else {{ continue }}
            }}
            let count = when (total) |state| {{ state.count }}
            if count != 3 {{ panic("iteration branches were not drained") }} else {{}}
            for item in Counter(position: 0, end: 4) {{
                branch {{ time.sleep(0ms); when (total) |state| {{ state.increment() }} }}
                break
            }}
            for item in Counter(position: 0, end: 2) {{
                branch {{ time.sleep(0ms); when (total) |state| {{ state.increment() }} }}
                continue
            }}
            let count = when (total) |state| {{ state.count }}
            if count != 6 {{ panic("break or exhaustion did not drain branches") }} else {{}}
        }}
    "#
    ));
}

#[test]
fn cursor_consumption_and_collected_item_rules_are_checked() {
    let definitions = r#"
        class Token {}
        class Source {}
        impl Cursor for Source {
            type Item = Token
            fn advance(self) -> Option((Token, Self)) { None }
        }
        fn collect(T: type + Cursor, source: T) -> List(T.Item) { for item in source { item } }
    "#;
    for (body, expected) in [
        (
            "let source = Source(); for item in source { continue }; let _ = source.advance(); ()",
            "after move",
        ),
        ("for item in Source() { item }", "immutable value"),
        ("collect(Source())", "immutable value"),
    ] {
        let error = try_run_program(&format!("{definitions} fn main() {{ {body} }}"))
            .unwrap_err()
            .to_string();
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn cursor_tail_continue_emits_no_output_nodes() {
    let program = syntax::parse_program(&format!(
        "{COUNTER} fn main() {{ for value in Counter(position: 0, end: 100) {{ continue }} }}"
    ))
    .unwrap();
    let types = sema::check_program(&program).unwrap();
    let core = CoreProgram::lower(program, types).unwrap();
    let mir = MirProgram::lower(&core).unwrap();
    assert!(!mir
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.statements)
        .any(|statement| matches!(
            statement,
            crate::mir::MirStatement::RuntimeCall {
                intrinsic: crate::mir::RuntimeIntrinsic::ListCons(_)
                    | crate::mir::RuntimeIntrinsic::BatchPush(_),
                ..
            }
        )));
}

#[test]
fn mut_containers_into_iter_to_list_and_retain() {
    run_program(
        r#"
        fn main() {
            let u0: UInt64 = 0
            let u1: UInt64 = 1
            let u2: UInt64 = 2
            let u3: UInt64 = 3
            let u4: UInt64 = 4
            let items = MutList#{1, 2, 3, 4}
            if items.to_list() != List(1, 2, 3, 4) { panic("snapshot") }
            if items.length() != u4 { panic("to_list keeps container") }
            items.retain(|value| value > 2)
            if items.to_list() != List(3, 4) { panic("retain") }
            let collected = for value in items.into_iter() { value }
            if collected != List(3, 4) { panic("into_iter") }

            let empty = MutList(Int32)()
            if !empty.to_list().is_empty() { panic("empty snapshot") }
            empty.retain(|value| value == 0)
            if empty.length() != u0 { panic("empty retain") }
            if !for value in empty.into_iter() { value }.is_empty() { panic("empty into_iter") }

            let early = MutList#{10, 20, 30}
            var seen = 0
            for (index, value) in early.into_iter() {
                if index == u1 { break }
                seen = seen + value
            }
            if seen != 10 { panic("break drops cursor") }
        }
    "#,
    );
}

#[test]
fn mut_map_and_set_into_iter_to_list_and_retain() {
    run_program(
        r#"
        fn main() {
            let u2: UInt64 = 2
            let u3: UInt64 = 3
            let table = MutMap(Int32, String)()
            let _ = table.insert(1, "a")
            let _ = table.insert(2, "b")
            let _ = table.insert(3, "c")
            if table.to_list().length() != u3 { panic("map snapshot count") }
            if table.length() != u3 { panic("map to_list keeps container") }
            table.retain(|key, value| (key != 2) && (value != ""))
            if table.length() != u2 { panic("map retain") }
            if table.contains_key(2) { panic("map retain removed") }
            var mapped = 0
            for entry in table.into_iter() {
                mapped = mapped + entry.0
                continue
            }
            if mapped != 4 { panic("map into_iter") }

            let members = MutSet(Int32)()
            let _ = members.add(1)
            let _ = members.add(2)
            let _ = members.add(3)
            if members.to_list().length() != u3 { panic("set snapshot") }
            members.retain(|item| item != 1)
            if members.length() != u2 { panic("set retain") }
            if members.contains(1) { panic("set retain removed") }
            var total = 0
            for item in members.into_iter() {
                total = total + item
                continue
            }
            if total != 5 { panic("set into_iter") }
        }
    "#,
    );
}

#[test]
fn mut_container_into_iter_consumes_the_container() {
    let error = try_run_program(
        r#"
        fn main() {
            let items = MutList#{1, 2, 3}
            let values = for x in items.into_iter() { x }
            println(items.length())
            println(values.length())
        }
    "#,
    )
    .expect_err("into_iter must consume MutList");
    assert!(
        error.to_string().contains("moved") || error.to_string().contains("after move"),
        "{error}"
    );
}

#[test]
fn for_accepts_map_set_range_through_into_cursor() {
    run_program(
        r#"
        fn show_item(I: type + Cursor, cursor: I) -> String
        where I.Item: Show
        {
            match cursor.advance() {
                Some(pair) => pair.0.show()
                None => ""
            }
        }
        fn main() {
            var table: Map(Int32, Int32) = Map#{}
            table = table.insert(1, 10).insert(2, 20)
            let keys = for entry in table {
                match entry {
                    (key, _) => key
                }
            }
            if keys.sorted() != List(1, 2) { panic("map into cursor") }

            var letters: Set(Int32) = Set#{}
            letters = letters.insert(7, true).insert(8, true)
            let members = for entry in letters {
                match entry {
                    (key, _) => key
                }
            }
            if members.sorted() != List(7, 8) { panic("set into cursor") }

            if (for i in 0..3 { i }) != List(0, 1, 2) { panic("range into cursor") }

            let items = MutList#{1, 2, 3}
            let values = for x in items { x }
            if values != List(1, 2, 3) { panic("mut list into cursor") }

            if show_item(List(9)) != "9" { panic("associated where") }
        }
    "#,
    );
}
