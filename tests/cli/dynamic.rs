use super::parity::assert_package_parity;
use super::*;

#[test]
fn dynamic_trait_cancellation_drops_the_receiver_once() {
    assert_package_parity(
        "dynamic-cancel",
        &[
            (
                "util.jk",
                r#"
            eff time { @suspends fn sleep(duration: Duration) -> Unit }
            trait Work { fn finish(self, notify: fn () -> Unit) -> String effects { time } }
            class Tracked { let text: String }
            impl Drop for Tracked { fn drop(&self) { println("dropped") } }
            impl Show for Tracked { fn show(&self) -> String { self.text } }
            impl Work for Tracked {
                fn finish(self, notify: fn () -> Unit) -> String effects { time } {
                    time.sleep(1ms)
                    notify()
                    time.sleep(60s)
                    self.text
                }
            }
            pub fn make() -> Dyn(Work + Show) { Dyn(Show + Work)(Tracked(text: "loser")) }
        "#,
            ),
            (
                "main.jk",
                r#"
            import util
            class Gate {
                var started: Bool = false
                fn mark() { self.started = true }
            }
            fn main() effects { util.time } {
                let gate = Cown.new(Gate())
                let started = fn () -> Bool { when (gate) |state| { state.started } }
                let winner = race {
                    | Dyn(util.Work)(util.make()).finish(fn () -> Unit { when (gate) |state| { state.mark() } })
                    | { while !started() { util.time.sleep(1ms) }; "winner" }
                }
                println(winner)
            }
        "#,
            ),
        ],
        "dropped\nwinner\n",
        &[],
    );
}

#[test]
fn dynamic_trait_examples_and_associated_types() {
    for (name, source, expected) in [
        (
            "dynamic-basic",
            include_str!("../../examples/traits/dyn_traits.jk"),
            "point\ncounter\n7\n42\n",
        ),
        (
            "dynamic-associated",
            include_str!("../fixtures/dyn_associated_types.jk"),
            "9223372036854775807\nhello\nhello\n",
        ),
        (
            "dynamic-pending",
            include_str!("../fixtures/dyn_pending.jk"),
            "dropped\nresumed\ndropped\n",
        ),
    ] {
        assert_package_parity(name, &[("main.jk", source)], expected, &[]);
    }
    assert_package_parity(
        "dynamic-borrows",
        &[("main.jk", include_str!("../fixtures/dyn_borrows.jk"))],
        "42\n42\nlabel\n7\nlabel\n7\nshown\nshown\ndefault\n",
        &[],
    );
}

#[test]
fn dynamic_trait_composition_examples() {
    for (name, source, expected) in [
        (
            "dynamic-composition",
            include_str!("../../examples/traits/dyn_trait_composition.jk"),
            "memory file\nhello\nclosed\nmemory file\ndefault\n",
        ),
        (
            "dynamic-composition-pending",
            include_str!("../fixtures/dyn_composition_pending.jk"),
            "resumed\nresumed\ndropped\nresumed\ndropped\n",
        ),
    ] {
        assert_package_parity(name, &[("main.jk", source)], expected, &[]);
    }
}

#[test]
fn dynamic_trait_owned_upcasts() {
    for (name, source, expected) in [
        (
            "dynamic-upcast",
            include_str!("../../examples/traits/dyn_upcast.jk"),
            "hello\nmemory file\ndropped\nhello\ndropped\n",
        ),
        (
            "dynamic-upcast-pending",
            include_str!("../fixtures/dyn_upcast_pending.jk"),
            "read\nread\ndropped\nfinished\n",
        ),
    ] {
        assert_package_parity(name, &[("main.jk", source)], expected, &[]);
    }
}

#[test]
fn dynamic_trait_compositions_cross_module_boundaries() {
    assert_package_parity(
        "dynamic-composition-modules",
        &[
            (
                "reader.jk",
                "trait Read { type Item; fn read(&self) -> Item }",
            ),
            (
                "closer.jk",
                "trait Close { type Status; fn close(self) -> Status }",
            ),
            (
                "a.jk",
                r#"
                import reader
                import closer
                struct Number { let value: Int64 }
                impl reader.Read for Number { type Item = Int64; fn read(&self) -> Self.Item { self.value } }
                impl closer.Close for Number { type Status = String; fn close(self) -> Self.Status { "closed a" } }
                impl Show for Number { fn show(&self) -> String { "a" } }
                pub fn make() -> Dyn(reader.Read + Show + closer.Close, Item: Int64, Status: String) {
                    Dyn(closer.Close + reader.Read + Show, Status: String, Item: Int64)(Number(value: 42))
                }
            "#,
            ),
            (
                "b.jk",
                r#"
                import reader
                import closer
                class Number { let value: Int64 }
                impl reader.Read for Number { type Item = Int64; fn read(&self) -> Self.Item { self.value } }
                impl closer.Close for Number { type Status = String; fn close(self) -> Self.Status { "closed b" } }
                impl Show for Number { fn show(&self) -> String { "b" } }
                pub fn make() -> Dyn(closer.Close + Show + reader.Read, Status: String, Item: Int64) {
                    Dyn(Show + reader.Read + closer.Close, Item: Int64, Status: String)(Number(value: 7))
                }
            "#,
            ),
            (
                "bridge.jk",
                r#"
                import a
                import b
                import reader
                pub fn readable(first: Bool) -> Dyn(Show + reader.Read, Item: Int64) {
                    let value = if first { a.make() } else { b.make() }
                    Dyn(reader.Read + Show, Item: Int64)(value)
                }
                pub fn shown(first: Bool) -> Dyn(Show) {
                    Dyn(Show)(readable(first))
                }
            "#,
            ),
            (
                "main.jk",
                r#"
                import a
                import b
                import bridge
                import reader
                import closer
                fn choose(first: Bool) -> Dyn(Show + reader.Read + closer.Close, Item: Int64, Status: String) {
                    if first { a.make() } else { b.make() }
                }
                fn display(value: &Dyn(closer.Close + reader.Read + Show, Status: String, Item: Int64)) {
                    println(value)
                    println(value.read())
                }
                fn main() {
                    let first = choose(true)
                    let second = choose(false)
                    display(first)
                    display(second)
                    println(first.close())
                    println(second.close())
                    let readable = bridge.readable(true)
                    println(readable.read())
                    let shown = Dyn(Show)(bridge.shown(false))
                    println(shown)
                }
            "#,
            ),
        ],
        "a\n42\nb\n7\nclosed a\nclosed b\n42\nb\n",
        &[],
    );
}

#[test]
fn dynamic_traits_cross_module_boundaries() {
    assert_package_parity(
        "dynamic-modules",
        &[
            (
                "util.jk",
                r#"
            trait Read { fn read(&self) -> Int64 }
            struct Number { let value: Int64 }
            impl Read for Number { fn read(&self) -> Int64 { self.value } }
            pub fn make() -> Dyn(Read) { Dyn(Read)(Number(value: 42)) }
        "#,
            ),
            (
                "bridge.jk",
                "import util\npub fn make() -> Dyn(util.Read) { util.make() }",
            ),
            (
                "main.jk",
                "import bridge\nfn main() { let value = bridge.make(); println(value.read()) }",
            ),
        ],
        "42\n",
        &[],
    );
    assert_package_parity(
        "dynamic-implementations",
        &[
            (
                "contract.jk",
                "trait Read { type Item; fn read(&self) -> Item }",
            ),
            (
                "a.jk",
                r#"
            import contract
            struct Number { let value: Int64 }
            impl contract.Read for Number { type Item = Int64; fn read(&self) -> Self.Item { self.value } }
            pub fn make() -> Dyn(contract.Read, Item: Int64) { Dyn(contract.Read, Item: Int64)(Number(value: 42)) }
        "#,
            ),
            (
                "b.jk",
                r#"
            import contract
            class Number { let value: Int64 }
            impl contract.Read for Number { type Item = Int64; fn read(&self) -> Self.Item { self.value } }
            pub fn make() -> Dyn(contract.Read, Item: Int64) { Dyn(contract.Read, Item: Int64)(Number(value: 7)) }
        "#,
            ),
            (
                "main.jk",
                r#"
            import contract
            import a
            import b
            fn choose(first: Bool) -> Dyn(contract.Read, Item: Int64) { if first { a.make() } else { b.make() } }
            fn main() { let first = choose(true); let second = choose(false); println(first.read()); println(second.read()) }
        "#,
            ),
        ],
        "42\n7\n",
        &[],
    );
}

#[test]
fn dynamic_traits_reject_invalid_ownership_and_interfaces() {
    for (index, (source, message)) in [
        ("trait T { fn eq(&self, value: Self) -> Bool } fn main() { let D = Dyn(T) }", "not object safe"),
        ("trait T { type Item; fn read(&self) -> Item } fn main() { let D = Dyn(T) }", "requires all associated types"),
        ("trait T { fn read(&self) -> Int32 } struct S {} fn main() { let value = Dyn(T)(S()) }", "does not implement trait"),
        ("trait T { fn read(&self) -> Int32 } struct S {} impl T for S { fn read(&self) -> Int32 { 1 } } fn main() { let a = Dyn(T)(S()); let b = a; println(a.read()) }", "moved"),
        ("trait T { fn take(self) -> Int32 } class S {} impl T for S { fn take(self) -> Int32 { 1 } } fn main() { let a = Dyn(T)(S()); println(a.take()); println(a.take()) }", "moved"),
        ("trait T { type Item; fn read(&self) -> Item } struct S {} impl T for S { type Item = Int32; fn read(&self) -> Int32 { 1 } } fn main() { let value = Dyn(T, Item: String)(S()) }", "does not match"),
        ("trait T { type Item; fn read(&self) -> Item } fn main() { let D = Dyn(T, Item: Int32, Item: String) }", "duplicate"),
        ("trait T { fn take(self) -> Int32 } class S {} impl T for S { fn take(self) -> Int32 { 1 } } fn consume(value: &Dyn(T)) -> Int32 { value.take() } fn main() { let value = Dyn(T)(S()); println(consume(value)) }", "non-owned"),
        ("eff time { @suspends fn sleep(duration: Duration) -> Unit } trait T { fn take(self) -> Int32 } class S {} impl T for S { fn take(self) -> Int32 effects { time } { time.sleep(1ms); 1 } } fn main() { let value = Dyn(T)(S()) }", "must declare"),
        ("trait A { fn a(&self) -> Unit } trait B { fn b(&self) -> Unit } struct S {} impl A for S { fn a(&self) {} } fn main() { let value = Dyn(A + B)(S()) }", "does not implement trait"),
        ("trait A { fn a(&self) -> Unit } fn main() { let D = Dyn(A + A) }", "duplicate Dyn trait"),
        ("trait A { fn read(&self) -> Int32 } trait B { fn read(&self) -> Int32 } fn main() { let D = Dyn(A + B) }", "conflicting Dyn method 'read'"),
        ("trait A { type Item; fn a(&self) -> Item } trait B { type Item; fn b(&self) -> Item } fn main() { let D = Dyn(A + B, Item: Int32) }", "conflicting Dyn associated type 'Item'"),
        ("trait A { type Item; fn a(&self) -> Item } trait B { type Status; fn b(&self) -> Status } fn main() { let D = Dyn(A + B, Item: Int32) }", "requires all associated types"),
        ("trait A { fn a(&self) -> Unit } trait B { fn b(&self) -> Self } fn main() { let D = Dyn(A + B) }", "not object safe"),
        ("trait A { fn a(&self) -> Unit } fn main() { let D = Dyn(A + Drop) }", "Drop cannot"),
        ("trait A { type Item } trait B { type Status } fn main() { let D = Dyn(A + B, Item: Int32, Status: String) }", "at least one method"),
        ("trait A { fn a(&self) -> Unit } fn main() { let D = Dyn(A + 1) }", "requires trait names"),
        ("trait A { fn a(&self) -> Unit } trait B { fn b(self) -> Unit } class S {} impl A for S { fn a(&self) {} } impl B for S { fn b(self) {} } fn main() { let v = Dyn(A + B)(S()); v.b(); v.a() }", "moved"),
        ("trait A { fn a(&self) -> Unit } trait B { type Item; fn b(&self) -> Item } struct S {} impl A for S { fn a(&self) {} } impl B for S { type Item = Int32; fn b(&self) -> Self.Item { 1 } } fn main() { let v = Dyn(A + B, Item: String)(S()) }", "does not match"),
        ("trait A { fn a(&self) -> Unit } trait B { fn b(self) -> Unit } class S {} impl A for S { fn a(&self) {} } impl B for S { fn b(self) {} } fn consume(v: &Dyn(A + B)) { v.b() } fn main() { let v = Dyn(B + A)(S()); consume(v) }", "non-owned"),
        ("eff time { @suspends fn sleep(duration: Duration) -> Unit } trait A { fn a(self) -> Unit } trait B { fn b(&self) -> Unit effects { time } } class S {} impl A for S { fn a(self) effects { time } { time.sleep(1ms) } } impl B for S { fn b(&self) {} } fn main() { let v = Dyn(A + B)(S()) }", "must declare"),
        ("trait A { fn a(&self) -> Unit } trait B { fn b(&self) -> Unit } struct S {} impl A for S { fn a(&self) {} } fn main() { let a = Dyn(A)(S()); let ab = Dyn(A + B)(a) }", "source interface to include trait 'B'"),
        ("trait A { type Item; fn a(&self) -> Item } struct S {} impl A for S { type Item = Int32; fn a(&self) -> Self.Item { 1 } } fn main() { let a = Dyn(A, Item: Int32)(S()); let b = Dyn(A, Item: String)(a) }", "associated type 'Item' does not match"),
        ("trait A { fn a(&self) -> Unit } struct S {} impl A for S { fn a(&self) {} } fn main() { let a = Dyn(A)(S()); let b = Dyn(A)(a); a.a() }", "moved"),
        ("trait A { fn a(&self) -> Unit } trait B { fn b(&self) -> Unit } struct S {} impl A for S { fn a(&self) {} } impl B for S { fn b(&self) {} } fn narrow(v: &Dyn(A + B)) -> Dyn(A) { Dyn(A)(v) } fn main() { let v = Dyn(A + B)(S()); let a = narrow(v) }", "non-owned"),
    ].into_iter().enumerate() {
        let package = super::parity::Package::new(&format!("dynamic-invalid-{index}"), &[("main.jk", source)]);
        let output = Command::new(env!("CARGO_BIN_EXE_joky")).current_dir(&package.root).args(["run", "--no-cache"]).output().unwrap();
        let stderr = strip_ansi(&String::from_utf8_lossy(&output.stderr));
        assert!(!output.status.success(), "{source}");
        assert!(stderr.contains(message) || (message == "moved" && stderr.contains("after move")), "{source}\n{stderr}");
    }
}
