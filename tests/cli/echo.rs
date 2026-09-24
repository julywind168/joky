use super::parity::Package;

#[test]
fn string_interpolation_uses_show_and_preserves_literals() {
    let package = Package::new(
        "interpolation",
        &[(
            "main.jk",
            r#"struct Point {}
impl Show for Point { fn show(&self) -> String { "point" } }
fn main() {
    let name = "Joky"
    let count: Int32 = 3
    println("hello {name}, count = {count}, {{ok}}")
    println("custom = {Point()}")
    let dynamic = Dyn(Show)(Point())
    println("dynamic = {dynamic}")
}
"#,
        )],
    );
    package.check(
        "hello Joky, count = 3, {ok}\ncustom = point\ndynamic = point\n",
        None,
        &[],
    );
}

#[test]
fn default_debug_crosses_modules_generics_and_dynamic_calls() {
    let package = Package::new(
        "default-debug",
        &[
            (
                "util.jk",
                r#"struct Point { let x: Int32; let text: String }
enum Shape { Empty; Dot(point: Point) }
class Owned { let text: String }
impl Drop for Owned { fn drop(&self) { println("dropped") } }
pub fn point() -> Point { Point(x: 1, text: "p") }
pub fn shape() -> Shape { Shape.Dot(point: point()) }
pub fn owned() -> Owned { Owned(text: "kept") }
pub fn inspect(T: type + Debug, value: T) -> T { echo value }
pub fn Box(T: type) -> type { struct { let value: T } }
"#,
            ),
            (
                "main.jk",
                r#"import util
class Envelope { let value: Dyn(Debug) }
class Node { let next: Option(Node) }
fn chain(n: Int32) -> Node { Node(next: if n == 0 { None } else { Some(chain(n - 1)) }) }
fn box_debug(T: type + Debug, value: T) -> String { value.debug() }
fn main() {
    let point = util.point()
    echo point
    echo util.shape()
    let envelope = Envelope(value: Dyn(Debug)(point))
    echo envelope
    if envelope.debug() != "Envelope(value: Point(x: 1, text: \"p\"))" { panic("dynamic field") }
    let result = point |> echo |> util.inspect
    println(result.debug())
    let owned = util.owned()
    echo owned
    if owned.debug() != "Owned(text: \"kept\")" { panic("owned reuse") }
    println(box_debug(util.Box(Int32)(value: 42)))
    let deep = chain(70)
    println(deep.debug())
    if chain(0).debug() != "Node(next: None)" { panic("fresh context") }
}
"#,
            ),
        ],
    );
    let deep = format!(
        "{}Node(<max-depth>){}",
        "Node(next: Some(".repeat(64),
        "))".repeat(64)
    );
    package.check_with(
        &format!("Point(x: 1, text: \"p\")\nBox(value: 42)\n{deep}\ndropped\n"),
        None,
        &[],
        |mode, output| {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let values = stderr
                .lines()
                .filter(|line| line.starts_with("[src/"))
                .map(|line| line.split_once("] ").unwrap().1)
                .collect::<Vec<_>>();
            assert_eq!(
                values,
                [
                    "Point(x: 1, text: \"p\")",
                    "Shape.Dot(point: Point(x: 1, text: \"p\"))",
                    "Envelope(value: Point(x: 1, text: \"p\"))",
                    "Point(x: 1, text: \"p\")",
                    "Point(x: 1, text: \"p\")",
                    "Owned(text: \"kept\")",
                ],
                "{mode}: {stderr}"
            );
        },
    );
}

#[test]
fn native_debug_preserves_resource_identity_and_behavior() {
    let package = Package::new("native-debug", &[]);
    let file = package.root.join("data.txt");
    std::fs::write(&file, "unchanged").unwrap();
    let source = r#"import joky/socket/tcp
import joky/file
import joky/sqlite
class Server { let listener: TcpListener }
fn inspect(T: type + Debug, value: T) -> T { echo value }
fn main() effects { tcp, file, sqlite } {
    let listener = tcp.listen(host: "127.0.0.1", port: 0)!
    let identity = listener.debug()
    echo listener
    let observed = listener |> echo |> inspect
    if observed.debug() != identity { panic("resource identity changed") }
    let other = tcp.listen(host: "127.0.0.1", port: 0)!
    if other.debug() == identity { panic("resource identity reused") }
    let server = Server(listener: observed)
    echo server
    if server.debug() != "Server(listener: " + identity + ")" { panic("native field") }
    let handle = file.open("FILE_PATH", FileMode.Read)!
    let file_identity = handle.debug()
    echo handle
    let start: UInt64 = 0
    if handle.position()! != start { panic("Debug moved file position") }
    let _ = handle.read_chunk(max_bytes: 9)!
    let end: UInt64 = 9
    if handle.position()! != end { panic("Debug changed file") }
    if handle.debug() != file_identity { panic("file identity changed") }
    handle.close()!
    let db = sqlite.open(":memory:")!
    echo db
    let statement = db.prepare("SELECT 'ok'")!
    echo statement
    for result in statement.query()! {
        let row = result!
        match row.text(0)! {
            None => panic("no text")
            Some(value) => { if value != "ok" { panic("Debug changed statement") } }
        }
    }
    db.close()!
    println("native ok")
}
"#
    .replace("FILE_PATH", &file.to_string_lossy());
    std::fs::write(package.root.join("src/main.jk"), source).unwrap();
    package.check_with("native ok\n", None, &[], |mode, output| {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let values = stderr
            .lines()
            .filter(|line| line.starts_with("[src/"))
            .map(|line| line.split_once("] ").unwrap().1)
            .collect::<Vec<_>>();
        assert_eq!(values.len(), 7, "{mode}: {stderr}");
        assert_eq!(values[0], values[1], "{mode}: {stderr}");
        assert_eq!(values[0], values[2], "{mode}: {stderr}");
        assert_eq!(
            values[3],
            format!("Server(listener: {})", values[0]),
            "{mode}: {stderr}"
        );
        let mut identities = std::collections::HashSet::new();
        for (value, name) in [
            (values[0], "TcpListener"),
            (values[4], "File"),
            (values[5], "SqliteConnection"),
            (values[6], "SqliteStatement"),
        ] {
            let id = value
                .strip_prefix(&format!("{name}(id: "))
                .and_then(|s| s.strip_suffix(')'))
                .unwrap_or_else(|| panic!("{mode}: {value}"));
            let id = id.parse::<u64>().unwrap();
            assert!(id != 0 && identities.insert(id), "{mode}: {stderr}");
        }
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "unchanged");
    });
}

#[test]
fn echo_recursively_formats_aggregate_variants() {
    let package = Package::new(
        "echo-aggregates",
        &[(
            "main.jk",
            r#"fn main() {
    echo(1, "hello", true)
    echo(1,)
    echo Some((42, Some("x")))
    echo Option(String).None
    echo Result(Int32, String).Ok(42)
    echo Result(Int32, String).Err("bad\nvalue")
    echo Some(())
    let wide: Result(UInt64, String) = Ok(18446744073709551615)
    echo wide
    println((Some("kept"), wide).debug())
}
"#,
        )],
    );
    package.check_with(
        "(Some(\"kept\"), Ok(18446744073709551615))\n",
        None,
        &[],
        |mode, output| {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let records = stderr
                .lines()
                .filter(|line| line.starts_with("[src/"))
                .collect::<Vec<_>>();
            assert_eq!(
                records,
                [
                    "[src/main.jk:2:5] (1, \"hello\", true)",
                    "[src/main.jk:3:5] (1,)",
                    "[src/main.jk:4:5] Some((42, Some(\"x\")))",
                    "[src/main.jk:5:5] None",
                    "[src/main.jk:6:5] Ok(42)",
                    "[src/main.jk:7:5] Err(\"bad\\nvalue\")",
                    "[src/main.jk:8:5] Some(())",
                    "[src/main.jk:10:5] Ok(18446744073709551615)",
                ],
                "{mode}: {stderr}"
            );
        },
    );
}

#[test]
fn aggregate_echo_preserves_ownership_across_modules_and_generic_calls() {
    let package = Package::new(
        "echo-aggregate-ownership",
        &[
            (
                "util.jk",
                r#"class Value { let text: String }
impl Debug for Value { fn debug(&self) -> String { "Value(" + self.text.debug() + ")" } }
impl Drop for Value { fn drop(&self) { println("dropped " + self.text) } }
pub fn make() -> Result((Value, Option(String)), String) {
    println("evaluated")
    Ok((Value(text: "kept"), Some("nested")))
}
pub fn inspect(T: type + Debug, value: T) -> T {
    echo value
}
pub fn boxed() -> Dyn(Debug) { Dyn(Debug)(Value(text: "boxed")) }
"#,
            ),
            (
                "main.jk",
                r#"import util
class Unused {}
impl Debug for Unused { fn debug(&self) -> String { panic("inactive payload"); "unreachable" } }
fn main() {
    let value = echo(util.make())
    echo value
    let result = value |> echo |> util.inspect
    println(result.debug())
    let absent: Option(Unused) = None
    let success: Result(String, Unused) = Ok("ok")
    let failure: Result(Unused, String) = Err("err")
    println((absent, success, failure).debug())
    let dynamic = Some(util.boxed())
    echo dynamic
    println(dynamic.debug())
    let counter = Counter()
    while counter.value < 3 {
        let present = counter.value != 1
        counter.advance()
        let value: Option((String, Bool)) = if present { Some(("label", true)) } else { None }
        let expected = if present { "Some((\"label\", true))" } else { "None" }
        if value.debug() != expected { panic("loop branch") }
        if result.debug() != "Ok((Value(\"kept\"), Some(\"nested\")))" { panic("owned reuse") }
    }
    ()
}
class Counter {
    var value: Int32 = 0
    fn advance(&self) { self.value = self.value + 1 }
}
"#,
            ),
        ],
    );
    package.check_with(
        "evaluated\nOk((Value(\"kept\"), Some(\"nested\")))\n(None, Ok(\"ok\"), Err(\"err\"))\nSome(Value(\"boxed\"))\ndropped boxed\ndropped kept\n",
        None, &[], |mode, output| {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let records = stderr.lines().filter(|line| line.starts_with("[src/")).collect::<Vec<_>>();
            assert_eq!(records, [
                "[src/main.jk:5:17] Ok((Value(\"kept\"), Some(\"nested\")))",
                "[src/main.jk:6:5] Ok((Value(\"kept\"), Some(\"nested\")))",
                "[src/main.jk:7:27] Ok((Value(\"kept\"), Some(\"nested\")))",
                "[src/util.jk:9:5] Ok((Value(\"kept\"), Some(\"nested\")))",
                "[src/main.jk:14:5] Some(Value(\"boxed\"))",
            ], "{mode}: {stderr}");
        },
    );
}

#[test]
fn echo_preserves_values_locations_and_debug_formatting() {
    let package = Package::new(
        "echo-values",
        &[(
            "main.jk",
            r#"fn once() -> Int32 {
    println("evaluated")
    20
}
fn twice(value: Int32) -> Int32 { value * 2 }
fn main() {
    echo "hello\n\"world\""
    let n = echo(once())
        |> twice
        |> echo
        |> twice
    println(n)
    echo 1 + 2
    echo(true)
    echo ()
    let unsigned: UInt64 = 18446744073709551615
    echo unsigned
    let float: Float64 = 1.5
    echo float
    println("a\tb".debug())
}
"#,
        )],
    );
    let expected = [
        "[src/main.jk:7:5] \"hello\\n\\\"world\\\"\"",
        "[src/main.jk:8:13] 20",
        "[src/main.jk:10:12] 40",
        "[src/main.jk:13:5] 3",
        "[src/main.jk:14:5] true",
        "[src/main.jk:15:5] ()",
        "[src/main.jk:17:5] 18446744073709551615",
        "[src/main.jk:19:5] 1.5",
    ];
    package.check_with("evaluated\n80\n\"a\\tb\"\n", None, &[], |mode, output| {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let records = stderr
            .lines()
            .filter(|line| line.starts_with("[src/"))
            .collect::<Vec<_>>();
        assert_eq!(records, expected, "{mode}: {stderr}");
    });
}

#[test]
fn echo_borrows_custom_debug_and_preserves_generic_definition_locations() {
    let package = Package::new(
        "echo-custom",
        &[
            (
                "util.jk",
                r#"class Value { let text: String }
impl Debug for Value { fn debug(&self) -> String { "Value(" + self.text.debug() + ")" } }
impl Drop for Value { fn drop(&self) { println("dropped") } }
pub fn make() -> Value { Value(text: "kept") }
pub fn inspect(T: type + Debug, value: T) -> T {
    echo(value)
}
"#,
            ),
            (
                "main.jk",
                r#"import util
fn main() {
    let value = util.make()
    echo value
    echo(value)
    let result = value |> echo |> util.inspect
    echo result
    println(result.debug())
}
"#,
            ),
        ],
    );
    package.check_with("Value(\"kept\")\ndropped\n", None, &[], |mode, output| {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let records = stderr
            .lines()
            .filter(|line| line.starts_with("[src/"))
            .collect::<Vec<_>>();
        assert_eq!(
            records,
            [
                "[src/main.jk:4:5] Value(\"kept\")",
                "[src/main.jk:5:5] Value(\"kept\")",
                "[src/main.jk:6:27] Value(\"kept\")",
                "[src/util.jk:6:5] Value(\"kept\")",
                "[src/main.jk:7:5] Value(\"kept\")",
            ],
            "{mode}: {stderr}"
        );
    });
}

#[test]
fn builtin_trait_dispatch_crosses_modules_and_uses_exact_protocols() {
    let package = Package::new(
        "builtin-trait-dispatch",
        &[
            (
                "util.jk",
                include_str!("../fixtures/builtin_trait_dispatch.jk"),
            ),
            (
                "main.jk",
                r#"import util
fn concrete() {
    let value = util.make()
    println(Show.show(value))
    println(Debug.debug(value))
    println(util.Custom.show(value))
    println(util.Custom.debug(value))
    println(util.show(value))
    println(util.debug(value))
    println(value)
    println("interpolation: {value}")
    echo value
    util.Custom.drop(value)
}
fn dynamic() {
    let value = util.dynamic()
    println(Show.show(value))
    println(Debug.debug(value))
    println(value)
    println("dynamic: {value}")
    echo value
    ()
}
fn main() { concrete(); dynamic() }
"#,
            ),
        ],
    );
    package.check_with(
        "builtin-show\nbuiltin-debug\ncustom-show\ncustom-debug\nbuiltin-show\nbuiltin-debug\nbuiltin-show\ninterpolation: builtin-show\ncustom-drop\nbuiltin-drop\nbuiltin-show\nbuiltin-debug\nbuiltin-show\ndynamic: builtin-show\nbuiltin-drop\n",
        None,
        &[("show", 1), ("debug", 1)],
        |mode, output| {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let values = stderr.lines().filter(|line| line.starts_with("[src/"))
                .map(|line| line.split_once("] ").unwrap().1).collect::<Vec<_>>();
            assert_eq!(values, ["builtin-debug", "builtin-debug"], "{mode}: {stderr}");
        },
    );
}

#[test]
fn string_show_ownership_crosses_cache_and_aot() {
    Package::new(
        "string-show-ownership",
        &[(
            "main.jk",
            include_str!("../fixtures/string_show_ownership.jk"),
        )],
    )
    .check_cached("string-show-ok\n", None, &[]);
}

#[test]
fn debug_values_cross_modules_cache_and_aot() {
    Package::new("debug-values", &[
        ("values.jk", include_str!("../fixtures/debug_values.jk")),
        ("main.jk", r#"
            import values
            fn main() {
                values.verify()
                let items = values.items()
                if items.debug() != "List#{{Some(item:first), None, Some(item:last)}}" { panic("imported Debug") }
                if values.render(items) != Debug.debug(items) { panic("generic Debug") }
                if values.render(List#{1s, 2ms}) != "List#{{1000ms, 2ms}}" { panic("generic Duration") }
                if values.render(Bytes.from_string("a")) != "Bytes[0x61]" { panic("generic Bytes") }
            }
        "#),
    ])
    .check_cached("debug-values-ok\n", None, &[]);
}
