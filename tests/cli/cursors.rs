use super::parity::assert_package_parity;

#[test]
fn ranges_cross_modules_and_resume_in_all_modes() {
    let package = super::parity::Package::new(
        "ranges",
        &[
            (
                "ranges.jk",
                r#"
            pub fn numbers() -> Range(Int32) { 1..=5 by 2 }
            pub fn collect(T: type + Cursor, source: T) -> List(T.Item) { for item in source { item } }
        "#,
            ),
            (
                "main.jk",
                r#"
            import ranges
            eff time { @suspends fn sleep(duration: Duration) -> Unit }
            fn wait() -> Int32 effects { time } { time.sleep(1ms); 3 }
            fn main() effects { time } {
                let r: Range(Int32) = ranges.numbers()
                let values = ranges.collect(r)
                if values != List(1, 3, 5) { panic("imported range") }
                if r.advance()!.1.advance()!.0 != 3 { panic("imported advance") }
                let squares = for n in r { time.sleep(1ms); n * n }
                if squares != List(1, 9, 25) { panic("resume") }
                let after_wait = 0..wait()
                var sum = 0
                for n in after_wait { sum = sum + n }
                println(sum)
            }
        "#,
            ),
        ],
    );
    package.check_cached("3\n", None, &[]);
}

#[test]
fn ranges_reject_dynamic_zero_even_without_iteration() {
    let package = super::parity::Package::new(
        "range-zero",
        &[(
            "main.jk",
            r#"
        fn zero() -> Int32 { 0 }
        fn main() { let r = 0..0 by zero(); println("unreachable") }
    "#,
        )],
    );
    package.check_modes(&[], |mode, output| {
        assert!(!output.status.success(), "{mode}");
        assert!(output.stdout.is_empty(), "{mode}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("range step must not be zero"),
            "{mode}"
        );
    });
}

#[test]
fn ranges_boundaries_match_across_modes() {
    let package = super::parity::Package::new(
        "range-boundaries",
        &[("main.jk", include_str!("../fixtures/ranges.jk"))],
    );
    package.check_cached("", None, &[]);
}

#[test]
fn moving_owned_fields_releases_shared_siblings() {
    assert_package_parity(
        "owned-field-shared-siblings",
        &[(
            "main.jk",
            r#"
        class Token { let id: Int32 }
        impl Drop for Token { fn drop(&self) { println(self.id) } }
        struct Mixed { let text: String; let token: Token; let values: List(String) }
        fn main() {
            let pair = (List("row"), Token(id: 1))
            let retained = pair.0
            let token = pair.1
            println(retained.head()!)
            let aggregate = Mixed(text: "kept", token: Token(id: 2), values: List("data"))
            let next = aggregate.token
        }
    "#,
        )],
        "row\n2\n1\n",
        &[],
    );
}

#[test]
fn cursor_can_finish_inline_after_a_suspending_advance() {
    assert_package_parity(
        "cursor-inline-finish",
        &[(
            "main.jk",
            r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        class Source { let done: Bool }
        impl Cursor for Source {
            type Item = Result(String, String)
            fn advance(self) -> Option((Result(String, String), Self)) effects { time } {
                if self.done { None } else {
                    time.sleep(1ms)
                    Some((Err("once"), Source(done: true)))
                }
            }
        }
        fn main() effects { time } {
            var count = 0
            for result in Source(done: false) {
                count = count + 1
                if count > 1 { panic("reused previous result") }
                match result { Err(message) => println(message); Ok(_) => panic("wrong item") }
                continue
            }
            println(count)
        }
    "#,
        )],
        "once\n1\n",
        &[],
    );
}

#[test]
fn map_and_set_cursors_iterate_and_release_handles_across_modes() {
    let package = super::parity::Package::new(
        "map-cursors",
        &[(
            "main.jk",
            r#"
        fn main() {
            let u1: UInt64 = 1
            let u3: UInt64 = 3
            var table: Map(String, String) = Map#{}
            table = table.insert("a", "alpha").insert("b", "beta").insert("c", "gamma")
            let keys = for k in table.keys() { k }
            if keys.sorted() != List("a", "b", "c") { panic("keys") }
            var lengths = 0
            for entry in table.entries() {
                lengths = lengths + 1
                continue
            }
            if lengths != 3 { panic("entries") }
            var early = 0
            for (index, k) in table.keys() {
                if index == u1 { break }
                early = early + 1
                continue
            }
            if early != 1 { panic("break") }
            var numbers: Map(Int32, Int32) = Map#{}
            numbers = numbers.insert(1, 10).insert(2, 20)
            let values = for v in numbers.values() { v }
            if values.sorted() != List(10, 20) { panic("values") }
            var members: Set(Int32) = Set#{}
            members = members.insert(7, true).insert(8, true)
            let set_items = for m in members.iter() { m }
            if set_items.sorted() != List(7, 8) { panic("set") }
            let empty: Map(String, String) = Map#{}
            let none = for k in empty.entries() { k }
            if !none.is_empty() { panic("empty") }
            if table.length() != u3 { panic("input alive") }
            println("done")
        }
    "#,
        )],
    );
    package.check_modes(&[], |mode, output| {
        let stdout = String::from_utf8_lossy(&output.stdout);
        super::parity::assert_result(output, mode, &stdout, None);
        assert_eq!(stdout, "done\n", "{mode}: {stdout}");
    });
}

#[test]
fn mut_container_cursors_iterate_snapshot_and_retain_across_modes() {
    let package = super::parity::Package::new(
        "mut-cursors",
        &[(
            "main.jk",
            r#"
        fn main() {
            let u1: UInt64 = 1
            let items = MutList#{1, 2, 3, 4}
            if items.to_list() != List(1, 2, 3, 4) { panic("snapshot") }
            items.retain(|value| value > 2)
            if items.to_list() != List(3, 4) { panic("retain") }
            var seen = 0
            for (index, value) in MutList#{10, 20, 30}.into_iter() {
                if index == u1 { break }
                seen = seen + value
                continue
            }
            if seen != 10 { panic("break") }
            let collected = for value in items.into_iter() { value }
            if collected != List(3, 4) { panic("into_iter") }
            let table = MutMap(Int32, Int32)()
            let _ = table.insert(1, 10)
            let _ = table.insert(2, 20)
            table.retain(|key, value| (key == 1) && (value == 10))
            if table.length() != u1 { panic("map retain") }
            var mapped = 0
            for entry in table.into_iter() {
                mapped = mapped + entry.1
                continue
            }
            if mapped != 10 { panic("map into_iter") }
            println("done")
        }
    "#,
        )],
    );
    package.check_modes(&[], |mode, output| {
        let stdout = String::from_utf8_lossy(&output.stdout);
        super::parity::assert_result(output, mode, &stdout, None);
        assert_eq!(stdout, "done\n", "{mode}: {stdout}");
    });
}

#[test]
fn bytes_cursor_iterates_releases_handles_and_keeps_input_alive() {
    let package = super::parity::Package::new(
        "bytes-cursor",
        &[(
            "main.jk",
            r#"
        fn main() {
            let u1: UInt64 = 1
            let u4: UInt64 = 4
            let one: UInt8 = 1
            let data = Bytes.from_string("hail")
            let expected: List(UInt8) = List(104, 97, 105, 108)
            if for byte in data.iter() { byte } != expected { panic("items") }
            var seen: UInt8 = 0
            for (index, byte) in data.iter() {
                if index == u1 { break }
                seen = seen + 1
            }
            if seen != one { panic("break") }
            let streamed: List(UInt8) = for byte in data.iter() { continue }
            if !streamed.is_empty() { panic("stream") }
            if data.length() != u4 { panic("input alive") }
            println("done")
        }
    "#,
        )],
    );
    package.check_modes(&[], |mode, output| {
        let stdout = String::from_utf8_lossy(&output.stdout);
        super::parity::assert_result(output, mode, &stdout, None);
        assert_eq!(stdout, "done\n", "{mode}: {stdout}");
    });
}

#[test]
fn cursor_return_handoff_cancellation_drops_every_constructed_payload() {
    let package = super::parity::Package::new(
        "cursor-handoff",
        &[(
            "main.jk",
            r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        class Gate { var started: Bool = false; fn mark() { self.started = true } }
        class Token { let id: Int32 }
        impl Drop for Token { fn drop(&self) { println("drop item {self.id}") } }
        class Source { let id: Int32; let notify: fn() -> Unit }
        impl Drop for Source { fn drop(&self) { println("drop state {self.id}") } }
        impl Cursor for Source {
            type Item = Token
            fn advance(self) -> Option((Token, Self)) effects { time } {
                time.sleep(0ms)
                let id = self.id
                if id == 0 { None } else {
                    let item = Token(id: id)
                    let next = Source(id: id - 1, notify: fn() -> Unit {})
                    println("new item {id}")
                    println("new state {id - 1}")
                    self.notify()
                    Some((item, next))
                }
            }
        }
        fn run_round(round: Int32) effects { time } {
            let gate = Cown.new(Gate())
            let notify = fn() -> Unit { when (gate) |state| { state.mark() } }
            let started = fn() -> Bool { when (gate) |state| { state.started } }
            println("round {round}")
            race {
                | { println("new state 100"); for item in Source(id: 100, notify: notify) { continue }; () }
                | { while !started() { time.sleep(1ms) }; () }
            }
        }
        fn main() effects { time } {
            run_round(1)
            run_round(2)
            run_round(3)
            run_round(4)
            run_round(5)
            run_round(6)
            run_round(7)
            run_round(8)
        }
    "#,
        )],
    );
    package.check_modes(&[], |mode, output| {
        let stdout = String::from_utf8_lossy(&output.stdout);
        super::parity::assert_result(output, mode, &stdout, None);
        let mut created = std::collections::BTreeMap::new();
        let mut dropped = std::collections::BTreeMap::new();
        let mut round = 0;
        for line in stdout.lines() {
            if let Some(number) = line.strip_prefix("round ") {
                round = number.parse::<usize>().unwrap();
            } else if let Some(resource) = line.strip_prefix("new ") {
                assert_eq!(
                    created.insert((round, resource.to_owned()), 1),
                    None,
                    "{mode}: {stdout}"
                );
            } else if let Some(resource) = line.strip_prefix("drop ") {
                assert_eq!(
                    dropped.insert((round, resource.to_owned()), 1),
                    None,
                    "{mode}: {stdout}"
                );
            } else {
                panic!("{mode}: unexpected event {line}");
            }
        }
        assert_eq!(round, 8, "{mode}: {stdout}");
        for round in 1..=8 {
            assert!(
                created.contains_key(&(round, "item 100".into())),
                "{mode}: {stdout}"
            );
        }
        assert_eq!(created, dropped, "{mode}: {stdout}");
    });
}

#[test]
fn cursors_cross_modules_and_resume_generic_task_waits() {
    assert_package_parity(
        "cursor-modules",
        &[
            (
                "util.jk",
                r#"
            pub fn collect(T: type + Cursor, source: T) -> List(T.Item) {
                for value in source { value }
            }
            pub fn forward(T: type + Cursor, source: T) -> List(T.Item) { collect(source) }
            pub fn step(T: type + Cursor, source: T) -> Option((T.Item, T)) { Cursor.advance(source) }
        "#,
            ),
            (
                "model.jk",
                r#"
            struct Counter {
                let count: Int32
                fn advance(self) -> Int32 { 99 }
            }
            impl Cursor for Counter {
                type Item = Int32
                fn advance(self) -> Option((Int32, Self)) {
                    if self.count == 0 { None } else { Some((self.count, Counter(count: self.count - 1))) }
                }
            }
            class Pending { let count: Int32; let text: String }
            impl Cursor for Pending {
                type Item = String
                fn advance(self) -> Option((String, Self)) {
                    let count = self.count
                    let next = parallel { | count - 1 }
                    if count == 0 { None } else {
                        Some((self.text, Pending(count: next.0, text: self.text)))
                    }
                }
            }
            pub fn counter() -> Counter { Counter(count: 3) }
            pub fn pending() -> Pending { Pending(count: 2, text: "ready") }
        "#,
            ),
            (
                "main.jk",
                r#"
            import util
            import model
            fn main() {
                let direct = for (i, item) in model.counter() { item }
                if direct != List(3, 2, 1) { panic("direct cursor") }
                if util.forward(model.counter()) != direct { panic("imported cursor") }
                if util.forward(List(3, 2, 1)) != direct { panic("List cursor") }
                let parallel_values = @parallel(limit: 2) for value in List(1, 2, 3) { value + 1 }
                if parallel_values != List(2, 3, 4) { panic("parallel List") }
                if util.forward(model.pending()) != List("ready", "ready") { panic("Pending cursor") }
                let stepped = util.step(List("head", "tail"))!
                if stepped.0 != "head" { panic("List head") }
                if stepped.1 != List("tail") { panic("List tail") }
                println("cursor modules ok")
            }
        "#,
            ),
        ],
        "cursor modules ok\n",
        &[("forward", 3), ("step", 1)],
    );
}

#[test]
fn cursor_exits_drop_each_state_and_item_once() {
    assert_package_parity("cursor-exits", &[("main.jk", r#"
        eff Stop { @aborts fn stop() -> Unit }
        class Token { let id: Int32 }
        impl Drop for Token { fn drop(&self) { println("item {self.id}") } }
        class Values { let count: Int32 }
        impl Drop for Values { fn drop(&self) { println("state {self.count}") } }
        impl Cursor for Values {
            type Item = Token
            fn advance(self) -> Option((Token, Self)) {
                if self.count == 0 { None } else { Some((Token(id: self.count), Values(count: self.count - 1))) }
            }
        }
        fn early() -> Result(Unit, String) {
            for item in Values(count: 2) {
                let failed: Result(Unit, String) = Err("done")
                failed?
                continue
            }
            Ok(())
        }
        fn consume(value: Token) {}
        fn main() {
            println("exhaust")
            for item in Values(count: 2) { consume(item); continue }
            println("break")
            for item in Values(count: 2) { break }
            println("question")
            if early().is_ok() { panic("question did not return") }
            println("abort")
            do { for item in Values(count: 2) { Stop.stop(); continue }; () }
                with { Stop.stop() => () }
            println("done")
        }
    "#)], "exhaust\nstate 2\nitem 2\nstate 1\nitem 1\nstate 0\nbreak\nstate 2\nitem 2\nstate 1\nquestion\nstate 2\nitem 2\nstate 1\nabort\nstate 2\nitem 2\nstate 1\ndone\n", &[]);
}

#[test]
fn cursor_cancellation_cleans_advance_and_body_frames() {
    assert_package_parity("cursor-cancel", &[
        ("model.jk", r#"
            eff time { @suspends fn sleep(duration: Duration) -> Unit }
            class Waiting { let notify: fn() -> Unit }
            impl Drop for Waiting { fn drop(&self) { println("advance dropped") } }
            impl Cursor for Waiting {
                type Item = Int32
                fn advance(self) -> Option((Int32, Self)) effects { time } {
                    self.notify()
                    time.sleep(60s)
                    None
                }
            }
            class Pure { let notify: fn() -> Unit }
            impl Drop for Pure { fn drop(&self) { println("pure advance dropped") } }
            impl Cursor for Pure {
                type Item = Int32
                fn advance(self) -> Option((Int32, Self)) {
                    let work = parallel { | { self.notify(); while true {}; 0 } }
                    None
                }
            }
            class Token {}
            impl Drop for Token { fn drop(&self) { println("body item dropped") } }
            class Body { let count: Int32 }
            impl Drop for Body { fn drop(&self) { println("body state {self.count} dropped") } }
            impl Cursor for Body {
                type Item = Token
                fn advance(self) -> Option((Token, Self)) {
                    if self.count == 0 { None } else { Some((Token(), Body(count: self.count - 1))) }
                }
            }
            pub fn waiting(notify: fn() -> Unit) -> Waiting { Waiting(notify: notify) }
            pub fn pure(notify: fn() -> Unit) -> Pure { Pure(notify: notify) }
            pub fn body() -> Body { Body(count: 1) }
            pub fn drain(T: type + Cursor, value: T) { for item in value { continue }; () }
        "#),
        ("main.jk", r#"
            import model
            class Gate { var started: Bool = false; fn mark() { self.started = true } }
            fn main() effects { model.time } {
                let advance_gate = Cown.new(Gate())
                let started = fn() -> Bool { when (advance_gate) |state| { state.started } }
                race {
                    | { for item in model.waiting(fn() -> Unit { when (advance_gate) |state| { state.mark() } }) { continue }; () }
                    | { while !started() { model.time.sleep(1ms) }; () }
                }
                let pure_gate = Cown.new(Gate())
                let started = fn() -> Bool { when (pure_gate) |state| { state.started } }
                race {
                    | model.drain(model.pure(fn() -> Unit { when (pure_gate) |state| { state.mark() } }))
                    | { while !started() { model.time.sleep(1ms) }; () }
                }
                let body_gate = Cown.new(Gate())
                let started = fn() -> Bool { when (body_gate) |state| { state.started } }
                let notify = fn() -> Unit { when (body_gate) |state| { state.mark() } }
                race {
                    | { for item in model.body() { notify(); model.time.sleep(60s); continue }; () }
                    | { while !started() { model.time.sleep(1ms) }; () }
                }
                println("cancelled")
            }
        "#),
    ], "advance dropped\npure advance dropped\nbody state 1 dropped\nbody item dropped\nbody state 0 dropped\ncancelled\n", &[("drain", 1)]);
}
