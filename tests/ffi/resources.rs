use super::*;

const MACOS_EXAMPLE: &str = include_str!("../../examples/ffi/tmpfile.jk");
const LINUX_EXAMPLE: &str = include_str!("../../examples/ffi/tmpfile.jk");

fn wrapper() -> &'static str {
    MACOS_EXAMPLE.split_once("\nfn main()").unwrap().0
}

fn instrumented_wrapper() -> String {
    // jk_temp_* lives in the shared fixture, not in the system C library.
    // Rewrite every platform declaration; os filters still pick one of them.
    wrapper()
        .replace("/usr/lib/libSystem.B.dylib", "@LIB@")
        .replace("libSystem.B.dylib", "@LIB@")
        .replace("libc.so.6", "@LIB@")
        .replace("ucrtbase.dll", "@LIB@")
        .replace("\"tmpfile\"", "\"jk_temp_open\"")
        .replace("\"fflush\"", "\"jk_temp_flush\"")
        .replace("\"fclose\"", "\"jk_temp_close\"")
}

const COUNTERS: &str = r#"
@extern(c, "@LIB@", "jk_temp_reset") fn reset(mode: Int32) -> Unit effects { stdio };
@extern(c, "@LIB@", "jk_temp_opens") fn opens() -> Int32 effects { stdio };
@extern(c, "@LIB@", "jk_temp_flushes") fn flushes() -> Int32 effects { stdio };
@extern(c, "@LIB@", "jk_temp_closes") fn closes() -> Int32 effects { stdio };
@extern(c, "@LIB@", "jk_temp_live") fn live() -> Int32 effects { stdio };
fn check_counts(open_count: Int32, flush_count: Int32, close_count: Int32) effects { stdio } {
    if (opens() != open_count) || (flushes() != flush_count) || (closes() != close_count) || (live() != 0) {
        panic("incorrect native resource lifetime")
    }
}
"#;

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn tmpfile_example_runs_against_system_stdio() {
    let fixture = Fixture::new();
    let source = if cfg!(target_os = "macos") {
        MACOS_EXAMPLE
    } else {
        LINUX_EXAMPLE
    };
    fixture.write("main.jk", source);
    for flags in [
        &["--verbose"][..],
        &["--verbose"],
        &["--no-cache"],
        &["--legacy"],
    ] {
        assert_eq!(success(&fixture.run(flags)), "temporary file closed\n");
    }
}

#[test]
fn tmpfile_wrapper_closes_once_on_success_and_all_error_paths() {
    let fixture = Fixture::new();
    let mut cases = String::new();
    for (mode, message) in [
        (0, None),
        (1, Some("tmpfile failed")),
        (2, Some("fflush failed")),
        (4, Some("fclose failed")),
        (6, Some("fflush failed; fclose failed")),
    ] {
        let check = match message {
            None => "if result.is_err() { panic(\"unexpected resource error\") }".to_owned(),
            Some(message) => format!(
                r#"match result {{
                Ok(_) => panic("missing resource error")
                Err(error) => if error != "{message}" {{ panic(error) }} else {{}}
            }}"#
            ),
        };
        let count = i32::from(mode != 1);
        cases.push_str(&format!(
            "reset({mode}); let result = exercise_temp_file(); {check}; check_counts(1, {count}, {count});\n"
        ));
    }
    // Null must not reach fflush (where it means flush all streams) or fclose.
    cases.push_str(
        r#"
        reset(0)
        let empty = TempFile(CMutPtr.null(Unit))
        if empty.flush().is_ok() { panic("null flush") }
        if empty.close().is_ok() { panic("null close") }
        check_counts(0, 0, 0)
    "#,
    );
    fixture.write("main.jk", &format!(
        "{}\n{COUNTERS}\nfn main() effects {{ stdio }} {{\n{cases}\nprintln(\"resource paths ok\")\n}}",
        instrumented_wrapper(),
    ));
    for flags in [
        &["--verbose"][..],
        &["--verbose"],
        &["--no-cache"],
        &["--legacy"],
    ] {
        assert_eq!(success(&fixture.run(flags)), "resource paths ok\n");
    }
}

#[test]
fn tmpfile_owner_and_effects_survive_module_cache() {
    let fixture = Fixture::new();
    fixture.write("api.jk", &instrumented_wrapper());
    let counters = COUNTERS.replace("effects { stdio }", "effects { api.stdio }");
    fixture.write(
        "main.jk",
        &format!(
            r#"
import api
{counters}
fn main() effects {{ api.stdio }} {{
    reset(0)
    match api.open_temp_file() {{
        Err(error) => panic(error)
        Ok(file) => {{
            let owner = file
            let flushed = owner.flush()
            let closed = owner.close()
            if flushed.is_err() || closed.is_err() {{ panic("resource methods") }} else {{}}
        }}
    }}
    check_counts(1, 1, 1)
    println("cached resource ok")
}}
"#
        ),
    );
    assert_eq!(
        success(&fixture.run(&["--verbose"])),
        "cached resource ok\n"
    );
    let warm = fixture.run(&["--verbose"]);
    assert_eq!(success(&warm), "cached resource ok\n");
    assert!(
        String::from_utf8_lossy(&warm.stderr)
            .matches("[cache] hit")
            .count()
            >= 2
    );
    assert_eq!(
        success(&fixture.run(&["--no-cache"])),
        "cached resource ok\n"
    );
    // The cached imported method retains its consuming receiver contract.
    let main_path = fixture.0.join("src/main.jk");
    let main_source = std::fs::read_to_string(&main_path).unwrap();
    fixture.write(
        "main.jk",
        &main_source.replace(
            "let closed = owner.close()",
            "let closed = owner.close(); let invalid = owner.flush()",
        ),
    );
    let invalid = fixture.run(&["--verbose"]);
    assert!(!invalid.status.success());
    let error = String::from_utf8_lossy(&invalid.stderr);
    assert!(
        error.contains("[cache] hit") && error.contains("after move"),
        "{error}"
    );
    fixture.write(
        "main.jk",
        &main_source.replace("fn main() effects { api.stdio }", "fn main()"),
    );
    let missing_effect = fixture.run(&["--verbose"]);
    assert!(!missing_effect.status.success());
    assert!(String::from_utf8_lossy(&missing_effect.stderr).contains("effect"));
}
