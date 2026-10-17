//! Explicit arenas use relocatable helpers and ordinary native owner cleanup.
use super::*;

#[test]
#[cfg(unix)]
fn explicit_arena_executes_scope_cleanup_and_failure_in_native_objects() {
    let source = include_str!("../../../../tests/fixtures/explicit_arena.jk");
    let fixture = ScratchDir::new("explicit-arena");
    let object = fixture.path().join("arena.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("arena");
    let mut compiler = AotCompiler::new("explicit-arena").unwrap();
    let exports = compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(compiler.requires_runtime());
    assert!(!exports.iter().any(|name| name == "explicit_make"));
    assert!(!exports.iter().any(|name| name == "explicit_forward"));
    compiler.write_object(&object).unwrap();
    let mut harness = String::from("#include \"joky_runtime.h\"\n");
    let cases = [
        "explicit_basic",
        "explicit_helper",
        "explicit_values",
        "explicit_nested",
        "explicit_task_reference",
        "explicit_helper_storage",
        "explicit_arena_parameters",
        "explicit_move",
        "explicit_drop",
        "explicit_loop",
        "explicit_jumps",
        "explicit_return",
    ];
    for name in cases {
        harness.push_str(&format!("extern int32_t {name}(void);\n"));
    }
    harness.push_str("extern int32_t explicit_failure(int32_t), explicit_many(int32_t);\nint main(void) { for (int run = 0; run < 20; ++run) {\n");
    for (index, name) in cases.iter().enumerate() {
        harness.push_str(&format!(
            "if ({name}() != 42 || joky_runtime_error_code()) return {};\n",
            index + 1
        ));
    }
    harness.push_str(
        r#"
        (void)explicit_failure(0);
        if (!joky_runtime_error_code()) return 20;
        joky_runtime_error_clear();
        if (explicit_failure(1) != 42 || joky_runtime_error_code()) return 21;
        }
        if (explicit_many(1000) != 42 || joky_runtime_error_code()) return 22;
        return 0;
        }
        "#,
    );
    std::fs::write(&host, harness).unwrap();
    link(
        link_runtime(c_compiler().arg(&host).arg(&object)),
        &executable,
    );
    let output = Command::new(executable).output().unwrap();
    assert!(
        output.status.success(),
        "{:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}
