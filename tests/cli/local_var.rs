use super::parity::assert_package_parity;

#[test]
fn local_var_replacement_drop_and_cross_module_generics_have_parity() {
    assert_package_parity("local-var-drop", &[
        ("util.jk", "pub fn replace(T: type, first: T, second: T) -> T { var value = first; value = second; value }"),
        ("main.jk", r#"
            import util
            class Token { let id: Int32 }
            impl Drop for Token { fn drop(&self) { println(self.id) } }
            fn run(flag: Bool) {
                var token = Token(id: 1)
                if flag { token = Token(id: 2) } else { token = Token(id: 3) }
                token = token
            }
            fn main() {
                run(true)
                run(false)
                let result = util.replace(Token, Token(id: 4), Token(id: 5))
            }
        "#),
    ], "1\n2\n1\n3\n4\n5\n", &[]);
}

#[test]
fn local_var_owned_cursor_and_scalar_resume_have_parity() {
    assert_package_parity(
        "local-var-cursor",
        &[(
            "main.jk",
            r#"
        import joky/time
        class Counter { let position: Int32 }
        impl Cursor for Counter {
            type Item = Int32
            fn advance(self) -> Option((Int32, Self)) {
                if self.position == 3 { None }
                else { Some((self.position, Counter(position: self.position + 1))) }
            }
        }
        fn main() effects { time } {
            var cursor = Counter(position: 0)
            var sum = 0
            var text = "a"
            loop {
                match cursor.advance() {
                    Some(pair) => {
                        let item = pair.0
                        cursor = pair.1
                        sum = sum + item
                        text = text + "b"
                        time.sleep(1ms)
                    }
                    None => break
                }
            }
            println(sum)
            println(text)
        }
    "#,
        )],
        "3\nabbb\n",
        &[],
    );
}

#[test]
fn local_var_rhs_early_return_and_cancellation_cleanup_have_parity() {
    assert_package_parity(
        "local-var-cancel",
        &[(
            "main.jk",
            r#"
        import joky/time
        class Token { let id: Int32 }
        impl Drop for Token { fn drop(&self) { println(self.id) } }
        class Signal { var ready: Bool; fn mark() { self.ready = true } }
        fn early() -> Option(Token) {
            var token = Token(id: 1)
            token = { let absent: Option(Token) = None; absent? }
            Some(token)
        }
        fn main() effects { time } {
            let ignored = early()
            let signal = Cown.new(Signal(ready: false))
            let result = race {
                | {
                    var token = Token(id: 10)
                    token = Token(id: 11)
                    var text = "a"
                    let next = move fn() -> String { text = text + "b"; text }
                    println(next())
                    when (signal) |state| { state.mark() }
                    time.sleep(60s)
                    println(next())
                    token.id
                }
                | {
                    var done = false
                    while !done {
                        done = when (signal) |state| { state.ready }
                        time.sleep(1ms)
                    }
                    42
                }
            }
            println(result)
        }
    "#,
        )],
        "1\n10\nab\n11\n42\n",
        &[],
    );
}

#[test]
fn local_var_move_capture_state_and_outer_reinitialization_have_parity() {
    assert_package_parity(
        "local-var-move-state",
        &[(
            "main.jk",
            r#"
        class Token { let value: Int32 }
        fn twice(read: fn() -> Int32) -> Int32 { read() + read() }
        fn main() {
            var n = 1
            let read = move fn() -> Int32 { n = n + 1; n }
            println(read())
            println(read())
            n = 100
            println(read())
            println(n)
            let alias = read
            println(twice(alias))

            var text = "a"
            let append = move fn() -> String { text = text + "b"; text }
            println(append())
            println(append())
            text = "outside"
            println(append())
            println(text)

            var token = Token(value: 1)
            let replace = move fn() -> Int32 {
                token = Token(value: token.value + 1)
                token.value
            }
            println(replace())
            println(replace())
            token = Token(value: 20)
            println(replace())
            println(token.value)

            var literal = 10
            println((move fn() -> Int32 { literal = literal + 1; literal })())
        }
    "#,
        )],
        "2\n3\n4\n100\n11\nab\nabb\nabbb\noutside\n2\n3\n4\n20\n11\n",
        &[],
    );
}

#[test]
fn local_var_move_capture_return_and_cross_module_calls_have_parity() {
    assert_package_parity(
        "local-var-move-module",
        &[
            (
                "counter.jk",
                r#"
                pub fn make(start: Int32) -> fn() -> Int32 {
                    var value = start
                    move fn() -> Int32 { value = value + 1; value }
                }
                pub fn twice(read: fn() -> Int32) -> Int32 { read() + read() }
                pub fn setter(T: type, initial: T) -> fn(T) -> Unit {
                    var value = initial
                    move fn(next: T) -> Unit { value = next }
                }
            "#,
            ),
            (
                "main.jk",
                r#"
                import counter
                fn make(start: Int32) -> fn() -> Int32 {
                    var value = start
                    move fn() -> Int32 { value = value + 1; value }
                }
                fn main() {
                    let local = make(10)
                    println(local())
                    println(local())
                    println(counter.twice(local))
                    let imported = counter.make(20)
                    println(imported())
                    println(imported())
                    let alias = imported
                    println(counter.twice(alias))
                    let integer = counter.setter(Int32, 1)
                    integer(2)
                    let text = counter.setter(String, "old")
                    text("new")
                }
            "#,
            ),
        ],
        "11\n12\n27\n21\n22\n47\n",
        &[],
    );
}

#[test]
fn local_var_nested_fresh_move_captures_and_shadowing_have_parity() {
    assert_package_parity(
        "local-var-move-nested",
        &[(
            "main.jk",
            r#"
        fn main() {
            var value = 10
            let make = move fn() -> fn() -> Int32 {
                {
                    let value = 900
                    if value != 900 { panic("immutable shadow") }
                }
                {
                    var value = 800
                    value = value + 1
                    if value != 801 { panic("mutable shadow") }
                }
                value = value + 1
                var fresh = value
                move fn() -> Int32 { fresh = fresh + 1; fresh }
            }
            let first = make()
            println(first())
            println(first())
            let second = make()
            println(second())
            println(first())
            println(second())

            var transferred = 30
            let run = move fn() -> Int32 { transferred = transferred + 1; transferred }
            let results = parallel {
                | run() + run()
                | 42
            }
            println(results.0)
            println(results.1)
        }
    "#,
        )],
        "12\n13\n13\n14\n14\n63\n42\n",
        &[],
    );
}
