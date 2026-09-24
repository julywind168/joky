use super::parity::{assert_package_parity, Package};
use super::strip_ansi;
use std::fs;
use std::process::Command;

#[test]
fn source_option_result_apis_have_jit_cache_and_aot_parity() {
    Package::new(
        "source-option-result",
        &[(
            "main.jk",
            r#"
            import joky/option
            import joky/result
            class Token { let text: String }
            impl Drop for Token { fn drop(&self) { println(self.text) } }
            fn main() {
                let value: option.Option(Token) = Some(Token("selected"))
                println(option.is_some(value))
                println(value.is_none())
                let token = value.unwrap_or(Token("unused"))
                println(token.text)
                let error: result.Result(Token, Token) = Err(Token("error"))
                println(result.is_err(error))
                let token = result.unwrap_or(error, Token("fallback"))
                println(token.text)
                let missing: Option(Int32) = None
                println(missing.unwrap_or_else(fn () -> Int32 { 42 }))
                let failed: Result(Int32, String) = Err("bad")
                println(failed.unwrap_or_else(fn (error: String) -> Int32 { 7 }))
                let mixed: Result(String, Token) = Ok("mixed" + " shared")
                if mixed.is_err() || mixed.is_err() { panic("mixed query") }
                if mixed.unwrap_or("bad") != "mixed shared" { panic("mixed payload") }
            }
        "#,
        )],
    )
    .check_cached(
        "true\nfalse\nunused\nselected\ntrue\nerror\nfallback\n42\n7\nfallback\nselected\n",
        None,
        &[],
    );
}

#[test]
fn jit_and_aot_show_primitive_values() {
    assert_package_parity(
        "show-primitives",
        &[(
            "main.jk",
            r#"
            fn main() {
                let signed: Int64 = -42
                let unsigned: UInt64 = 18446744073709551615
                let float: Float64 = 1.5
                println(signed.show())
                println(unsigned.show())
                println(float.show())
                println(true.show())
                println(false.show())
            }
            "#,
        )],
        "-42\n18446744073709551615\n1.5\ntrue\nfalse\n",
        &[],
    );
}

#[test]
fn jit_and_aot_share_transitive_instances_and_static_associated_types() {
    assert_package_parity(
        "generic-ready",
        &[
            (
                "util.jk",
                r#"
                trait Source { type Item; fn next(&self) -> Option(Item) }
                struct Number { let value: Int64 }
                struct Text { let value: String }
                impl Source for Number {
                    type Item = Int64
                    fn next() -> Option(Self.Item) { Some(self.value) }
                }
                impl Source for Text {
                    type Item = String
                    fn next() -> Option(Self.Item) { Some(self.value) }
                }
                pub fn number() -> Number { Number(value: 42) }
                pub fn text() -> Text { Text(value: "text") }
                pub fn next(I: type + Source, iter: I) -> Option(I.Item) { iter.next() }
                pub fn copy(T: type, value: T) -> T { value }
                pub fn display(T: type + Show, value: T) { println(value) }
            "#,
            ),
            (
                "bridge.jk",
                r#"
                import util
                pub fn forward(T: type, value: T) -> T { util.copy(value) }
            "#,
            ),
            (
                "a.jk",
                "import bridge\npub fn answer() -> Int32 { bridge.forward(42) }",
            ),
            (
                "b.jk",
                r#"
                import bridge
                pub fn answer() -> Int32 {
                    let unrelated = (true, "different local type indices", 1)
                    bridge.forward(Int32, value: 7)
                }
            "#,
            ),
            (
                "main.jk",
                r#"
                import util
                import a
                import b
                struct Point { let value: Int32 }
                impl Show for Point { fn show() -> String { "point" } }
                fn main() {
                    println(a.answer())
                    println(b.answer())
                    println(util.next(util.number())!)
                    println(util.next(util.text())!)
                    util.display(Point(value: 1))
                }
            "#,
            ),
        ],
        "42\n7\n42\ntext\npoint\n",
        &[("forward", 1), ("copy", 1), ("next", 2), ("display", 1)],
    );
}

#[test]
fn jit_and_aot_resume_concrete_trait_impls_and_owned_results() {
    assert_package_parity(
        "generic-pending",
        &[
            (
                "util.jk",
                r#"
                eff time { @suspends fn sleep(duration: Duration) -> Unit }
                trait Work { fn finish(self) -> Option(String) effects { time } }
                class Ready { let text: String }
                class Delayed { let text: String }
                impl Work for Ready {
                    fn finish(self) -> Option(String) effects { time } { Some(self.text) }
                }
                impl Work for Delayed {
                    fn finish(self) -> Option(String) effects { time } {
                        let prefix = "resumed "
                        time.sleep(1ms)
                        time.sleep(1ms)
                        Some(prefix + self.text)
                    }
                }
                pub fn ready() -> Ready { Ready(text: "ready") }
                pub fn delayed(text: String) -> Delayed { Delayed(text: text) }
                pub fn finish(T: type + Work, value: T) -> Option(String) effects { time } {
                    value.finish()
                }
            "#,
            ),
            (
                "main.jk",
                r#"
                import util
                fn main() effects { util.time } {
                    println(util.finish(util.ready())!)
                    println(util.finish(util.delayed("first"))!)
                    println(util.finish(util.delayed("second"))!)
                    let results = parallel {
                        | util.finish(util.delayed("left"))
                        | util.finish(util.delayed("right"))
                    }
                    println(results.0!)
                    println(results.1!)
                }
            "#,
            ),
        ],
        "ready\nresumed first\nresumed second\nresumed left\nresumed right\n",
        &[("finish", 2)],
    );
}

#[test]
fn jit_and_aot_cancel_generic_trait_frames_and_drop_the_receiver_once() {
    assert_package_parity(
        "generic-cancel",
        &[
            (
                "util.jk",
                r#"
                eff time { @suspends fn sleep(duration: Duration) -> Unit }
                class Tracked { let text: String }
                impl Drop for Tracked { fn drop(&self) { println("dropped") } }
                trait Work { fn finish(self, notify: fn () -> Unit) -> String effects { time } }
                impl Work for Tracked {
                    fn finish(self, notify: fn () -> Unit) -> String effects { time } {
                        time.sleep(1ms)
                        notify()
                        time.sleep(60s)
                        self.text
                    }
                }
                pub fn tracked() -> Tracked { Tracked(text: "loser") }
                pub fn finish(T: type + Work, value: T, notify: fn () -> Unit) -> String effects { time } {
                    value.finish(notify)
                }
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
                    let is_started = fn () -> Bool { when (gate) |state| { state.started } }
                    let winner = race {
                        | util.finish(util.tracked(), fn () -> Unit { when (gate) |state| { state.mark() } })
                        | {
                            while !is_started() { util.time.sleep(1ms) }
                            "winner"
                        }
                    }
                    println(winner)
                }
            "#,
            ),
        ],
        "dropped\nwinner\n",
        &[("finish", 1)],
    );
}

#[test]
fn generic_trait_effect_bounds_distinguish_same_named_operations_across_modules() {
    let package = Package::new(
        "generic-effect-identity-rejected",
        &[
            (
                "contract.jk",
                r#"
                eff Signal { @resumable fn read() -> Int32 }
                trait Source { fn read(self) -> Int32 effects { Signal } }
                pub fn read(T: type + Source, value: T) -> Int32 effects { Signal } {
                    value.read()
                }
                "#,
            ),
            (
                "model.jk",
                r#"
                import contract
                eff Signal { @resumable fn read() -> Int32 }
                struct Number { let value: Int32 }
                impl contract.Source for Number {
                    fn read(self) -> Int32 effects { Signal } { self.value }
                }
                pub fn number() -> Number { Number(value: 42) }
                "#,
            ),
            (
                "main.jk",
                r#"
                import contract
                import model
                fn main() effects { contract.Signal } {
                    println(contract.read(model.number()))
                }
                "#,
            ),
        ],
    );
    for command in ["run", "build"] {
        let output = Command::new(env!("CARGO_BIN_EXE_joky"))
            .current_dir(&package.root)
            .arg(command)
            .output()
            .unwrap();
        let stderr = strip_ansi(&String::from_utf8_lossy(&output.stderr));
        assert!(!output.status.success(), "{command}: {stderr}");
        assert!(stderr.contains("Source"), "{command}: {stderr}");
        assert!(
            stderr.contains("method 'read' declares effects not allowed by the trait"),
            "{command}: {stderr}"
        );
        assert!(stderr.contains("Signal.read"), "{command}: {stderr}");
    }
}

#[test]
fn cached_generic_calls_recheck_changed_impl_effect_declarations() {
    let model = |effect_clause: &str| {
        format!(
            r#"
            import contract
            eff Signal {{ @resumable fn read() -> Int32 }}
            struct Number {{ let value: Int32 }}
            impl contract.Source for Number {{
                fn read(self) -> Int32{effect_clause} {{ self.value }}
            }}
            pub fn number() -> Number {{ Number(value: 42) }}
            "#
        )
    };
    let package = Package::new(
        "generic-effect-cache-invalidation",
        &[
            (
                "contract.jk",
                r#"
                trait Source { fn read(self) -> Int32 }
                pub fn read(T: type + Source, value: T) -> Int32 { value.read() }
                "#,
            ),
            ("model.jk", &model("")),
            (
                "main.jk",
                r#"
                import contract
                import model
                fn main() { println(contract.read(model.number())) }
                "#,
            ),
        ],
    );
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_joky"))
            .current_dir(&package.root)
            .args(["run", "--verbose"])
            .output()
            .unwrap()
    };
    for cached in [false, true] {
        let output = run();
        let stderr = strip_ansi(&String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{stderr}");
        assert_eq!(output.stdout, b"42\n", "{stderr}");
        assert!(
            stderr.contains(if cached {
                "hit generic read"
            } else {
                "compile generic read"
            }),
            "{stderr}"
        );
        if cached {
            assert!(!stderr.contains("[cache] compile"), "{stderr}");
        }
    }
    fs::write(
        package.root.join("src/model.jk"),
        model(" effects { Signal }"),
    )
    .unwrap();
    let output = run();
    let stderr = strip_ansi(&String::from_utf8_lossy(&output.stderr));
    assert!(!output.status.success(), "{stderr}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(stderr.contains("Source"), "{stderr}");
    assert!(
        stderr.contains("method 'read' declares effects not allowed by the trait"),
        "{stderr}"
    );
    assert!(stderr.contains("Signal.read"), "{stderr}");
}

#[test]
fn jit_and_aot_accept_qualified_aliases_of_generic_trait_effects() {
    assert_package_parity(
        "generic-effect-identity-accepted",
        &[
            (
                "contract.jk",
                r#"
                eff Signal { @resumable fn read() -> Int32 }
                trait Source { fn read(self) -> Int32 effects { Signal } }
                pub fn read(T: type + Source, value: T) -> Int32 effects { Signal } {
                    value.read()
                }
                "#,
            ),
            (
                "model.jk",
                r#"
                import contract
                struct Number { let value: Int32 }
                impl contract.Source for Number {
                    fn read(self) -> Int32 effects { contract.Signal } { self.value }
                }
                pub fn number() -> Number { Number(value: 42) }
                "#,
            ),
            (
                "main.jk",
                r#"
                import contract
                import model
                fn main() effects { contract.Signal } {
                    println(contract.read(model.number()))
                }
                "#,
            ),
        ],
        "42\n",
        &[("read", 1)],
    );
}

#[test]
fn jit_and_aot_resume_imported_pure_generic_trait_task_waits() {
    assert_package_parity(
        "generic-pure-pending",
        &[
            (
                "util.jk",
                r#"
                trait Work { fn finish(self) -> Int32 }
                struct Worker { let value: Int32 }
                impl Work for Worker {
                    fn finish(self) -> Int32 {
                        let values = parallel {
                            | self.value + 1
                            | self.value + 2
                        }
                        values.0 + values.1
                    }
                }
                pub fn worker(value: Int32) -> Worker { Worker(value: value) }
                pub fn finish(T: type + Work, value: T) -> Int32 { value.finish() }
                "#,
            ),
            (
                "main.jk",
                r#"
                import util
                fn main() {
                    println(util.finish(util.worker(20)))
                    let values = parallel {
                        | util.finish(util.worker(1))
                        | util.finish(util.worker(2))
                    }
                    println(values.0)
                    println(values.1)
                }
                "#,
            ),
        ],
        "43\n5\n7\n",
        &[("finish", 1)],
    );
}
