#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

#[path = "ffi/resources.rs"]
mod resources;

#[path = "ffi/callbacks.rs"]
mod callbacks;

#[path = "ffi/sqlite.rs"]
mod sqlite;

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "joky-ffi-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("src")).unwrap();
        let fixture = Self(root);
        let mut compiler = Command::new("cc");
        compiler.arg(if cfg!(target_os = "macos") {
            "-dynamiclib"
        } else {
            "-shared"
        });
        let output = compiler
            .args(["-fPIC", "-O2", "-std=c11", "-pthread"])
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ffi/scalars.c"))
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ffi/resources.c"))
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ffi/callbacks.c"))
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ffi/sqlite_callbacks.c"),
            )
            .arg("-lsqlite3")
            .arg("-o")
            .arg(fixture.library())
            .output()
            .expect("C compiler required for FFI ABI tests");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        fixture
    }

    fn library(&self) -> PathBuf {
        self.0.join(if cfg!(target_os = "macos") {
            "libfixture.dylib"
        } else {
            "libfixture.so"
        })
    }

    fn write(&self, name: &str, source: &str) {
        std::fs::write(
            self.0.join("src").join(name),
            source.replace("@LIB@", self.library().to_str().unwrap()),
        )
        .unwrap();
    }

    fn run(&self, flags: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_joky"))
            .arg("run")
            .arg(self.0.join("src/main.jk"))
            .args(flags)
            .current_dir(&self.0)
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn success(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).unwrap()
}

#[test]
fn c_scalars_link_across_modules_and_reload_from_disk() {
    let fixture = Fixture::new();
    let mut declarations = String::new();
    let mut calls = String::new();
    for (name, ty, literal) in [
        ("i8", "Int8", "-117"),
        ("u8", "UInt8", "250"),
        ("i16", "Int16", "-30000"),
        ("u16", "UInt16", "60000"),
        ("i32", "Int32", "-2000000000"),
        ("u32", "UInt32", "4000000000"),
        ("i64", "Int64", "-5000000000"),
        ("u64", "UInt64", "10000000000000000000"),
    ] {
        declarations.push_str(&format!(
            "@extern(c, \"@LIB@\", \"jk_{name}\") pub fn {name}(value: {ty}) -> {ty};\n"
        ));
        calls.push_str(&format!("let {name}: {ty} = {literal}; if api.{name}({name}) != {name} {{ panic(\"{name}\") }} else {{}};\n"));
    }
    declarations.push_str(r#"
@extern(c, "@LIB@", "jk_f32") pub fn f32(value: Float32) -> Float32;
@extern(c, "@LIB@", "jk_f64") pub fn f64(value: Float64) -> Float64;
@extern(c, "@LIB@", "jk_set") pub fn set(value: Int32) -> Unit;
@extern(c, "@LIB@", "jk_get") pub fn get() -> Int32;
@extern(c, "@LIB@", "jk_mixed") pub fn mixed(a: Int8, b: UInt8, c: Int16, d: UInt16, e: Int32, f: UInt32, g: Int64, h: UInt64, i: Float32, j: Float64, k: Int8, l: UInt8, m: Int16, n: UInt16, o: Int32, p: UInt32, q: Float32, r: Float64) -> Float64;
"#);
    fixture.write("api.jk", &declarations);
    fixture.write("main.jk", &format!(r#"import api
fn main() {{
    {calls}
    let single: Float32 = 1.25; let twice: Float32 = 2.5;
    if api.f32(single) != twice {{ panic("float32") }} else {{}}
    if api.f64(-1.25) != -2.5 {{ panic("float64") }} else {{}}
    api.set(42)
    if api.get() != 42 {{ panic("void") }} else {{}}
    let call = fn (x: Int32) -> Int32 {{ api.i32(x) }}
    if call(-7) != -7 {{ panic("indirect") }} else {{}}
    let result = api.mixed(-1, 2, -3, 4, -5, 6, -7, 8, 1.25, 2.5, -9, 10, -11, 12, -13, 14, 3.25, 4.5)
    if result != 18.5 {{ panic("mixed ABI") }} else {{}}
    branch {{ if api.i32(17) != 17 {{ panic("task call") }} else {{}} }}
    println("ffi ok")
}}"#));
    assert_eq!(success(&fixture.run(&["--verbose"])), "ffi ok\n");
    let warm = fixture.run(&["--verbose"]);
    assert_eq!(success(&warm), "ffi ok\n");
    assert!(
        String::from_utf8_lossy(&warm.stderr)
            .matches("[cache] hit")
            .count()
            >= 2
    );
    assert_eq!(success(&fixture.run(&["--legacy"])), "ffi ok\n");
    assert_eq!(success(&fixture.run(&["--no-cache"])), "ffi ok\n");
}

#[test]
fn c_binding_changes_invalidate_callers_and_conflicting_declarations_fail() {
    let fixture = Fixture::new();
    fixture.write(
        "api.jk",
        "@extern(c, \"@LIB@\", \"jk_negative\") pub fn value() -> Int32;",
    );
    fixture.write("main.jk", "import api\nfn main() { println(api.value()) }");
    assert_eq!(success(&fixture.run(&["--verbose"])), "-42\n");
    let warm = fixture.run(&["--verbose"]);
    assert_eq!(success(&warm), "-42\n");
    assert!(
        String::from_utf8_lossy(&warm.stderr)
            .matches("[cache] hit")
            .count()
            >= 2
    );
    fixture.write(
        "api.jk",
        "@extern(c, \"@LIB@\", \"jk_get\") pub fn value() -> Int32;",
    );
    let changed = fixture.run(&["--verbose"]);
    assert_eq!(success(&changed), "0\n");
    assert!(
        String::from_utf8_lossy(&changed.stderr)
            .matches("[cache] compile")
            .count()
            >= 2
    );
    fixture.write(
        "main.jk",
        "import api\n@extern(c, \"@LIB@\", \"jk_get\") fn conflicting() -> Float64; fn main() {}",
    );
    let invalid = fixture.run(&[]);
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("conflicting C declarations"));
}

#[test]
fn missing_c_libraries_and_symbols_report_errors_even_with_warm_mir() {
    let fixture = Fixture::new();
    fixture.write("main.jk", "@extern(c, \"@LIB@\", \"jk_negative\") fn jk_negative() -> Int32; fn main() { println(jk_negative()) }");
    assert_eq!(success(&fixture.run(&["--verbose"])), "-42\n");
    std::fs::remove_file(fixture.library()).unwrap();
    let missing = fixture.run(&["--verbose"]);
    assert!(!missing.status.success());
    let error = String::from_utf8_lossy(&missing.stderr);
    assert!(
        error.contains("[cache] hit") && error.contains("cannot load C library"),
        "{error}"
    );
    let fixture = Fixture::new();
    fixture.write("main.jk", "@extern(c, \"@LIB@\", \"not_a_real_symbol\") fn absent() -> Int32; fn main() { println(absent()) }");
    let missing = fixture.run(&[]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr)
        .contains("cannot resolve C symbol 'not_a_real_symbol'"));
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn calls_real_system_c_library() {
    let fixture = Fixture::new();
    let library = if cfg!(target_os = "macos") {
        "/usr/lib/libSystem.B.dylib"
    } else {
        "libc.so.6"
    };
    fixture.write("main.jk", &format!("@extern(c, \"{library}\", \"abs\") fn absolute(value: Int32) -> Int32; fn main() {{ println(absolute(-42)) }}"));
    assert_eq!(success(&fixture.run(&[])), "42\n");
}

#[test]
fn c_pointer_results_can_be_passed_back_to_c() {
    let fixture = Fixture::new();
    fixture.write(
        "main.jk",
        r#"
@repr(c) struct Point {
    let x: Int32
    let y: Int32
}
@extern(c, "@LIB@", "jk_point_ptr") fn point() -> CPtr(Point);
@extern(c, "@LIB@", "jk_point_mut_ptr") fn mut_point() -> CMutPtr(Point);
@extern(c, "@LIB@", "jk_point_sum") fn sum(value: CPtr(Point)) -> Int32;
@extern(c, "@LIB@", "jk_point_set") fn set(value: CMutPtr(Point), x: Int32, y: Int32) -> Unit;
fn main() {
    let value = point()
    if sum(value) != 18 { panic("pointer read") }
    set(mut_point(), 20, 22)
    if sum(value) != 42 { panic("pointer write") }
    println("pointer ok")
}
"#,
    );
    assert_eq!(success(&fixture.run(&[])), "pointer ok\n");
}

#[test]
fn c_strings_and_fixed_arrays_cross_the_pointer_abi() {
    let fixture = Fixture::new();
    fixture.write(
        "main.jk",
        r#"
@extern(c, "@LIB@", "jk_message") fn message() -> CStr;
@extern(c, "@LIB@", "jk_message_length") fn length(value: CStr) -> UInt64;
@extern(c, "@LIB@", "jk_fixed_values") fn values() -> CPtr(CArray(UInt8, 4));
@extern(c, "@LIB@", "jk_fixed_sum") fn sum(value: CPtr(CArray(UInt8, 4))) -> Int32;
fn main() {
    let expected: UInt64 = 8
    if length(message()) != expected { panic("cstring") }
    if sum(values()) != 26 { panic("carray") }
    println("cstring array ok")
}
"#,
    );
    assert_eq!(success(&fixture.run(&[])), "cstring array ok\n");
}

#[test]
fn string_as_cstr_lends_nul_terminated_payloads_without_copying() {
    let fixture = Fixture::new();
    fixture.write(
        "api.jk",
        r#"
@extern(c, "@LIB@", "jk_cstr_check") pub fn check(text: CStr, utf8_len: UInt64) -> Int32;
"#,
    );
    // The lending wrapper lives in an imported module so the intrinsic is
    // exercised through module-cache serialization and relinking.
    fixture.write(
        "lib.jk",
        r#"
import api
pub fn verify(text: String) -> Int32 { api.check(text.as_cstr(), text.byte_count()) }
"#,
    );
    fixture.write(
        "main.jk",
        r#"
import lib
import api
fn main() {
    let ok: Int32 = 1
    // Multibyte UTF-8 content survives the lend unchanged.
    let multibyte = "héllo wörld"
    if lib.verify(multibyte) != ok { panic("multibyte") }
    // The empty string lends a non-null pointer to a single NUL byte.
    let empty = ""
    if empty.as_cstr().is_null() { panic("empty lent null") }
    if lib.verify(empty) != ok { panic("empty") }
    // Anonymous temporaries stay alive across the C call.
    let joined = "hello"
    if api.check((joined + " world").as_cstr(), 11) != ok { panic("concat") }
    let padded = "  joky  "
    if api.check(padded.trim().as_cstr(), 4) != ok { panic("trim") }
    // Closures keep their captured and parameter strings alive for lends.
    let lend = fn (text: String) -> Int32 { api.check(text.as_cstr(), text.byte_count()) }
    if lend("from a closure") != ok { panic("closure") }
    println("as_cstr ok")
}
"#,
    );
    assert_eq!(success(&fixture.run(&[])), "as_cstr ok\n");
    let warm = fixture.run(&["--verbose"]);
    assert_eq!(success(&warm), "as_cstr ok\n");
    assert!(String::from_utf8_lossy(&warm.stderr).contains("[cache] hit"));
    assert_eq!(success(&fixture.run(&["--no-cache"])), "as_cstr ok\n");
    // The legacy flattener does not re-export externs through intermediate
    // modules, so rerun the checks without the wrapper import.
    fixture.write(
        "main.jk",
        r#"
import api
fn main() {
    let ok: Int32 = 1
    let multibyte = "héllo wörld"
    if api.check(multibyte.as_cstr(), multibyte.byte_count()) != ok { panic("multibyte") }
    let padded = "  joky  "
    if api.check(padded.trim().as_cstr(), 4) != ok { panic("trim") }
    println("as_cstr ok")
}
"#,
    );
    assert_eq!(success(&fixture.run(&["--legacy"])), "as_cstr ok\n");
}

#[test]
fn cstr_to_string_copies_foreign_strings_into_managed_ones() {
    let fixture = Fixture::new();
    fixture.write(
        "api.jk",
        r#"
@extern(c, "@LIB@", "jk_message") pub fn message() -> CStr;
@extern(c, "@LIB@", "jk_utf8_message") pub fn utf8() -> CStr;
@extern(c, "@LIB@", "jk_invalid_utf8_string") pub fn invalid() -> CStr;
@extern(c, "@LIB@", "jk_null_string") pub fn none() -> CStr;
@extern(c, "@LIB@", "jk_empty_string") pub fn empty() -> CStr;
@extern(c, "@LIB@", "jk_cstr_check") pub fn check(text: CStr, utf8_len: UInt64) -> Int32;
"#,
    );
    // The copying wrapper lives in an imported module so the intrinsic is
    // exercised through module-cache serialization and relinking.
    fixture.write(
        "lib.jk",
        r#"
import api
pub fn copy(text: CStr) -> Option(String) { text.to_string() }
"#,
    );
    fixture.write(
        "main.jk",
        r#"
import lib
import api
fn main() {
    // ASCII and multibyte payloads copy byte-for-byte into owned Strings.
    match api.message().to_string() {
        Some(text) => {
            let expected: UInt64 = 8
            if text.byte_count() != expected { panic("ascii length") }
            if text != "joky ffi" { panic("ascii content") }
        }
        None => panic("ascii message expected")
    }
    match api.utf8().to_string() {
        Some(text) => {
            if text != "héllo wörld" { panic("utf8 content") }
            // The copy is a fully owned, lendable String.
            if api.check(text.as_cstr(), text.byte_count()) != 1 { panic("relend") }
        }
        None => panic("utf8 message expected")
    }
    match lib.copy(api.utf8()) {
        Some(text) => { if text != "héllo wörld" { panic("cached copy") } }
        None => panic("cached message expected")
    }
    // The empty string stays distinct from null.
    match api.empty().to_string() {
        Some(text) => { if text != "" { panic("empty content") } }
        None => panic("empty string must not convert to none")
    }
    // A null pointer converts to none; is_null discriminates first.
    if api.none().is_null() {} else { panic("expected null") }
    match api.none().to_string() {
        Some(text) => panic("null must convert to none")
        None => {}
    }
    // Non-UTF-8 payloads are rejected rather than lossily copied.
    match api.invalid().to_string() {
        Some(text) => panic("invalid utf8 must convert to none")
        None => {}
    }
    println("to_string ok")
}
"#,
    );
    assert_eq!(success(&fixture.run(&[])), "to_string ok\n");
    let warm = fixture.run(&["--verbose"]);
    assert_eq!(success(&warm), "to_string ok\n");
    assert!(String::from_utf8_lossy(&warm.stderr).contains("[cache] hit"));
    assert_eq!(success(&fixture.run(&["--no-cache"])), "to_string ok\n");
    // The legacy flattener does not re-export externs through intermediate
    // modules, so rerun the checks without the wrapper import.
    fixture.write(
        "main.jk",
        r#"
import api
fn main() {
    match api.utf8().to_string() {
        Some(text) => { if text != "héllo wörld" { panic("utf8 content") } }
        None => panic("utf8 message expected")
    }
    match api.none().to_string() {
        Some(text) => panic("null must convert to none")
        None => {}
    }
    match api.invalid().to_string() {
        Some(text) => panic("invalid utf8 must convert to none")
        None => {}
    }
    println("to_string ok")
}
"#,
    );
    assert_eq!(success(&fixture.run(&["--legacy"])), "to_string ok\n");
}

#[test]
fn c_pointers_survive_module_cache_closures_and_managed_containers() {
    let fixture = Fixture::new();
    let api = r#"
@repr(c) struct Point { let x: Int32; let y: Int32 }
@repr(c) struct Packet { let tag: UInt8; let data: CArray(UInt32, 3); let next: CPtr(Packet) }
@extern(c, "@LIB@", "jk_point_ptr") pub fn point() -> CPtr(Point);
@extern(c, "@LIB@", "jk_point_sum") pub fn sum(p: CPtr(Point)) -> Int32;
@extern(c, "@LIB@", "jk_message") pub fn message() -> CStr;
@extern(c, "@LIB@", "jk_message_length") pub fn length(s: CStr) -> UInt64;
@extern(c, "@LIB@", "jk_fixed_values") pub fn values() -> CPtr(CArray(UInt8, 4));
@extern(c, "@LIB@", "jk_fixed_sum") pub fn array_sum(p: CPtr(CArray(UInt8, 4))) -> Int32;
pub fn identity(T: type, value: T) -> T { value }
pub fn relay(p: CPtr(Packet)) -> CPtr(Packet) { p }
"#;
    fixture.write("api.jk", api);
    fixture.write(
        "main.jk",
        r#"
import api
import joky/time
// Ensure local type indices differ from those in api.
fn local(p: CPtr(UInt64)) -> CPtr(UInt64) { p }
struct Labelled { let text: String; let pointer: CStr }
class Box { let pointer: CStr; fn length() -> UInt64 { api.length(self.pointer) } }
fn read(value: Labelled) -> UInt64 { api.length(value.pointer) }
fn wait() -> Int32 effects { time } {
    let point = api.identity(api.point())
    let string = api.identity(api.message())
    let array = api.identity(api.values())
    let labelled = Labelled(text: "managed label", pointer: string)
    let captured = fn () -> UInt64 { read(labelled) }
    let expected: UInt64 = 8
    time.sleep(1ms)
    if captured() != expected { panic("capture") }
    if read(labelled) != expected { panic("shared copy") }
    let boxed = Box(pointer: string)
    if boxed.length() != expected { panic("class pointer") }
    if api.sum(point) != 18 { panic("nominal pointer") }
    api.array_sum(array)
}
fn main() effects { time } {
    let result = parallel { | wait() }
    if result.0 != 26 { panic("array pointer") }
    let text = api.message()
    let deferred = fn () -> UInt64 { api.length(text) }
    let expected: UInt64 = 8
    let winner = race {
        | { time.sleep(1s); deferred() }
        | expected
    }
    if winner != expected { panic("cancelled pointer capture") }
    println("cached pointers ok")
}
"#,
    );
    assert_eq!(
        success(&fixture.run(&["--verbose"])),
        "cached pointers ok\n"
    );
    let warm = fixture.run(&["--verbose"]);
    assert_eq!(success(&warm), "cached pointers ok\n");
    assert!(
        String::from_utf8_lossy(&warm.stderr)
            .matches("[cache] hit")
            .count()
            >= 2
    );
    assert_eq!(
        success(&fixture.run(&["--no-cache"])),
        "cached pointers ok\n"
    );
    // A native layout change invalidates the exporter and its callers.
    fixture.write(
        "api.jk",
        &api.replace("CArray(UInt32, 3)", "CArray(UInt32, 5)"),
    );
    let changed = fixture.run(&["--verbose"]);
    assert_eq!(success(&changed), "cached pointers ok\n");
    assert!(
        String::from_utf8_lossy(&changed.stderr)
            .matches("[cache] compile")
            .count()
            >= 2
    );
}

#[test]
fn c_null_pointers_cross_native_calls_modules_and_disk_cache() {
    let fixture = Fixture::new();
    fixture.write(
        "api.jk",
        r#"
        @repr(c) struct Point { let x: Int32; let y: Int32 }
        @extern(c, "@LIB@", "jk_null_const") pub fn const_null() -> CPtr(Int32);
        @extern(c, "@LIB@", "jk_null_mut") pub fn mut_null() -> CMutPtr(UInt8);
        @extern(c, "@LIB@", "jk_null_string") pub fn string_null() -> CStr;
        @extern(c, "@LIB@", "jk_empty_string") pub fn empty_string() -> CStr;
        @extern(c, "@LIB@", "jk_null_void") pub fn void_null() -> CPtr(Unit);
        @extern(c, "@LIB@", "jk_point_ptr") pub fn point() -> CPtr(Point);
        @extern(c, "@LIB@", "jk_point_mut_ptr") pub fn mut_point() -> CMutPtr(Point);
        @extern(c, "@LIB@", "jk_accept_nulls")
        pub fn accept(a: CPtr(Int32), b: CMutPtr(UInt8), c: CStr, d: CPtr(Unit), e: CMutPtr(Unit), f: CPtr(Point), g: CPtr(CArray(UInt8, 4))) -> Int32;
        pub fn make() -> CPtr(Int32) { CPtr.null(Int32) }
        pub fn make_array() -> CPtr(CArray(UInt8, 4)) { CPtr.null(CArray(UInt8, 4)) }
        pub fn make_point() -> CPtr(Point) { CPtr.null(Point) }
        pub fn check(p: CPtr(Int32)) -> Bool { p.is_null() }
        pub fn identity(T: type, value: T) -> T { value }
    "#,
    );
    fixture.write("main.jk", r#"
        import api
        import joky/time
        struct Holder { let label: String; let pointer: CStr }
        class Box { let pointer: CPtr(Int32) }
        fn main() effects { time } {
            let perturb = CMutPtr.null(Float64)
            if !perturb.is_null() { panic("local null") }
            if api.accept(api.make(), CMutPtr.null(UInt8), CStr.null(), CPtr.null(Unit),
                          CMutPtr.null(Unit), api.make_point(), api.make_array()) != 1 {
                panic("C did not receive NULL")
            }
            if !api.const_null().is_null() || !api.mut_null().is_null() || !api.string_null().is_null() || !api.void_null().is_null() {
                panic("C NULL return")
            }
            if api.point().is_null() || api.mut_point().is_null() || api.empty_string().is_null() {
                panic("non-null C return")
            }
            let h = Holder("held", api.identity(CStr, CStr.null()))
            if !h.pointer.is_null() { panic("struct field") }
            let b = Box(api.make())
            if !api.check(b.pointer) { panic("class field") }
            let p = api.make()
            let captured = fn () -> Bool { p.is_null() }
            time.sleep(1ms)
            if !captured() || !p.is_null() { panic("suspended pointer") }
            println("null pointers ok")
        }
    "#);
    for flags in [
        &["--verbose"][..],
        &["--verbose"],
        &["--no-cache"],
        &["--legacy"],
    ] {
        let output = fixture.run(flags);
        assert_eq!(success(&output), "null pointers ok\n");
    }
    let warm = fixture.run(&["--verbose"]);
    assert_eq!(success(&warm), "null pointers ok\n");
    assert!(
        String::from_utf8_lossy(&warm.stderr)
            .matches("[cache] hit")
            .count()
            >= 2
    );
}

#[test]
fn c_cells_back_out_parameters_and_nested_pointers() {
    let fixture = Fixture::new();
    fixture.write(
        "api.jk",
        r#"
@extern(c, "@LIB@", "jk_cell_take") pub fn take(cell: CMutPtr(Int64)) -> Int32;
@extern(c, "@LIB@", "jk_cell_stored") pub fn stored() -> Int64;
@extern(c, "@LIB@", "jk_make_pointer") pub fn make_pointer(out: CMutPtr(CStr)) -> Int32;
@extern(c, "@LIB@", "jk_make_handle") pub fn make_handle(out: CMutPtr(CMutPtr(Unit))) -> Int32;
@extern(c, "@LIB@", "jk_handle_value") pub fn handle_value(handle: CMutPtr(Unit)) -> Int64;
"#,
    );
    // The cell wrapper lives in an imported module so the intrinsics are
    // exercised through module-cache serialization and relinking.
    fixture.write(
        "lib.jk",
        r#"
import api
pub fn round_trip(value: Int64) -> Int64 {
    let cell = CMutPtr.alloc(value)
    let ignored = api.take(cell)
    let stored = api.stored()
    cell.free()
    stored
}
"#,
    );
    fixture.write(
        "main.jk",
        r#"
import lib
import api
fn main() {
    // A scalar cell crosses into C and back with its typed width intact.
    let value: Int64 = 42
    if lib.round_trip(value) != value { panic("scalar cell") }

    // A CStr cell works as an out-parameter: C writes, Joky reads a CStr.
    let text_cell = CMutPtr.alloc(CStr.null())
    if text_cell.is_null() { panic("cell allocation failed") } else {
        let ignored = api.make_pointer(text_cell)
        match text_cell.read().to_string() {
            Some(text) => { if text != "out param" { panic("out param content") } }
            None => panic("out param expected a message")
        }
        text_cell.free()
    }

    // A pointer-to-pointer cell receives an opaque handle from C.
    let handle_cell = CMutPtr.alloc(CMutPtr.null(Unit))
    if handle_cell.is_null() { panic("handle cell allocation failed") } else {
        let ignored = api.make_handle(handle_cell)
        let handle = handle_cell.read()
        if handle.is_null() { panic("expected a handle") } else {
            let expected: Int64 = 777
            if api.handle_value(handle) != expected { panic("handle value") }
        }
        handle_cell.free()
    }

    // Local mutation through write keeps C and Joky in sync.
    let cell = CMutPtr.alloc(value)
    let updated: Int64 = 7
    cell.write(updated)
    let ignored = api.take(cell)
    cell.free()
    if api.stored() != updated { panic("write through cell") }
    println("c cells ok")
}
"#,
    );
    assert_eq!(success(&fixture.run(&[])), "c cells ok\n");
    let warm = fixture.run(&["--verbose"]);
    assert_eq!(success(&warm), "c cells ok\n");
    assert!(String::from_utf8_lossy(&warm.stderr).contains("[cache] hit"));
    assert_eq!(success(&fixture.run(&["--no-cache"])), "c cells ok\n");
    // The legacy flattener does not re-export externs through intermediate
    // modules, so rerun the checks without the wrapper import.
    fixture.write(
        "main.jk",
        r#"
import api
fn main() {
    let value: Int64 = 42
    let cell = CMutPtr.alloc(value)
    let ignored = api.take(cell)
    cell.free()
    if api.stored() != value { panic("scalar cell") }
    let handle_cell = CMutPtr.alloc(CMutPtr.null(Unit))
    let ignored = api.make_handle(handle_cell)
    let handle = handle_cell.read()
    let expected: Int64 = 777
    if api.handle_value(handle) != expected { panic("handle value") }
    handle_cell.free()
    println("c cells ok")
}
"#,
    );
    assert_eq!(success(&fixture.run(&["--legacy"])), "c cells ok\n");
}

#[test]
fn native_callbacks_run_joky_closures_from_c() {
    let fixture = Fixture::new();
    // Batch-1 callbacks are single-module: the extern declares the comparator
    // inline and no import boundary re-resolves the callback type.
    fixture.write(
        "main.jk",
        r#"
@extern(c, "@LIB@", "jk_sort3") fn sort3(a: Int32, b: Int32, c: Int32, compare: fn(CMutPtr(Int32), CMutPtr(Int32)) -> Int32) -> Int32;
fn main() {
    // The comparator dereferences the element pointers C hands over.
    let ascending = region {
        sort3(3, 1, 2, fn (x: CMutPtr(Int32), y: CMutPtr(Int32)) -> Int32 {
            x.read() - y.read()
        })
    }
    let descending = sort3(3, 1, 2, fn (x: CMutPtr(Int32), y: CMutPtr(Int32)) -> Int32 {
        y.read() - x.read()
    })
    let expected_ascending: Int32 = 123
    let expected_descending: Int32 = 321
    if ascending != expected_ascending { panic("ascending") }
    if descending != expected_descending { panic("descending") }
    // Repeated calls with different inputs stay correct.
    let again = sort3(9, 4, 6, fn (x: CMutPtr(Int32), y: CMutPtr(Int32)) -> Int32 {
        x.read() - y.read()
    })
    let expected_again: Int32 = 469
    if again != expected_again { panic("repeat") }
    println("native callback ok")
}
"#,
    );
    assert_eq!(success(&fixture.run(&[])), "native callback ok\n");
    let warm = fixture.run(&["--verbose"]);
    assert_eq!(success(&warm), "native callback ok\n");
    assert!(String::from_utf8_lossy(&warm.stderr).contains("[cache] hit"));
    assert_eq!(
        success(&fixture.run(&["--no-cache"])),
        "native callback ok\n"
    );
    assert_eq!(success(&fixture.run(&["--legacy"])), "native callback ok\n");
}
