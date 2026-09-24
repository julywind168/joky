//! End-to-end compiler tests grouped by the runtime feature they exercise.
//!
//! Keeping these tests outside `compiler.rs` makes the production entry point
//! easy to scan while preserving one test module and the existing test names.

use super::*;
use std::sync::{Arc, Barrier};
use std::thread;

mod env;
mod option_result;
mod path_process;

fn run_program(source: &str) {
    try_run_program(source).expect("test program should run");
}

fn try_run_program(source: &str) -> Result<(), Diagnostic> {
    Compiler::new()?.run_program(source)
}

#[test]
fn standalone_runtime_host_links_beside_the_legacy_jit_runtime() {
    let scope = joky_runtime::host::RuntimeScope::new();
    let _guard = scope.enter();
    scope.wait_for_idle();
}

#[test]
fn compile_object_program_emits_native_object() {
    let mut compiler = Compiler::new().expect("compiler");
    let object = compiler
        .compile_object_program("fn main() {}")
        .expect("object compilation");
    assert!(object.len() > 64);
}

/// AOT must relocate C callees. Baking the compiler process's `dlsym` address
/// is valid for JIT and for macOS's shared cache, but it segfaults under
/// Linux ASLR.
#[test]
#[cfg(unix)]
fn aot_object_imports_c_symbols_instead_of_baking_addresses() {
    let library = if cfg!(target_os = "macos") {
        "libSystem.B.dylib"
    } else {
        "libc.so.6"
    };
    let source = format!(
        "@extern(c, \"{library}\", \"abs\") fn absolute(value: Int32) -> Int32; fn main() {{ let _ = absolute(-1); }}"
    );
    let mut compiler = Compiler::new().expect("compiler");
    let object = compiler
        .compile_object_program(&source)
        .expect("object compilation");
    use object::{Object, ObjectSymbol};
    let file = object::File::parse(object.as_slice()).expect("parse object");
    // Mach-O decorates C symbols with a leading underscore; ELF does not.
    let expected = if cfg!(target_os = "macos") {
        "_abs"
    } else {
        "abs"
    };
    let imported = file
        .symbols()
        .any(|symbol| symbol.name().ok() == Some(expected) && symbol.is_undefined());
    assert!(
        imported,
        "AOT object should reference an undefined C symbol `abs`"
    );
}

#[test]
fn compile_object_program_reports_file_provider_metadata() {
    let source = format!(
        "{}\nfn main() effects {{ file }} {{ let _ = file.read(\"missing\")!; () }}",
        include_str!("../../std/joky/file.jk")
    );
    let mut compiler = Compiler::new().expect("compiler");
    let (_object, metadata) = compiler
        .compile_object_program_with_metadata(&source)
        .expect("object compilation");
    let provider = metadata
        .providers
        .iter()
        .find(|provider| provider.name == "file")
        .expect("file provider");
    for name in ["read", "read_bytes", "open"] {
        assert!(
            provider
                .operations
                .iter()
                .any(|operation| operation.effect == "file" && operation.name == name),
            "{name} operation must be registered"
        );
    }
}

#[test]
fn compile_object_program_reports_socket_provider_metadata() {
    let source = format!(
        "{}\n{}\nfn main() effects {{ tcp, udp }} {{ () }}",
        include_str!("../../std/joky/socket/tcp.jk"),
        include_str!("../../std/joky/socket/udp.jk")
    );
    let mut compiler = Compiler::new().expect("compiler");
    let (_object, metadata) = compiler
        .compile_object_program_with_metadata(&source)
        .expect("object compilation");
    let provider = metadata
        .providers
        .iter()
        .find(|provider| provider.name == "socket")
        .expect("socket provider");
    for (effect, name) in [
        ("tcp", "connect"),
        ("tcp", "close"),
        ("udp", "bind"),
        ("udp", "recv_from"),
    ] {
        assert!(
            provider
                .operations
                .iter()
                .any(|operation| operation.effect == effect && operation.name == name),
            "{effect}.{name} operation must be registered"
        );
    }
}

#[test]
fn compile_object_program_reports_sqlite_provider_metadata() {
    let source = format!(
        "{}\nfn main() effects {{ sqlite }} {{ () }}",
        include_str!("../../std/joky/sqlite.jk")
    );
    let mut compiler = Compiler::new().expect("compiler");
    let (_object, metadata) = compiler
        .compile_object_program_with_metadata(&source)
        .expect("object compilation");
    let provider = metadata
        .providers
        .iter()
        .find(|provider| provider.name == "sqlite")
        .expect("sqlite provider");
    assert_eq!(provider.operations.len(), 12);
    for name in [
        "open",
        "prepare",
        "execute",
        "bind_text",
        "bind_i64",
        "bind_null",
        "bind_f64",
        "bind_blob",
        "query",
        "step",
        "finalize",
        "close",
    ] {
        assert!(
            provider
                .operations
                .iter()
                .any(|operation| operation.name == name),
            "{name} operation must be registered"
        );
    }
}

#[test]
fn compile_object_program_reports_no_providers_without_effects() {
    let mut compiler = Compiler::new().expect("compiler");
    let (_object, metadata) = compiler
        .compile_object_program_with_metadata("fn main() {}")
        .expect("object compilation");
    assert!(metadata.providers.is_empty());
}

#[test]
fn sqlite_provider_rejects_operations_with_incompatible_contracts() {
    for declaration in [
        "fn open(path: String) -> Result(SqliteConnection, String)",
        "@suspends fn open(path: Bytes) -> Result(SqliteConnection, String)",
        "@suspends fn close(db: &SqliteConnection) -> Result(Unit, String)",
        "@suspends fn open(path: String) -> Result(String, String)",
        "@suspends fn prepare(db: SqliteConnection, sql: String) -> Result(SqliteStatement, String)",
        "@suspends fn bind_i64(stmt: SqliteStatement, index: UInt64, value: Int64) -> SqliteStatement",
    ] {
        let source = format!("eff sqlite {{ {declaration} }} fn main() {{}}");
        let program = crate::syntax::parse_program(&source).unwrap();
        let types = crate::sema::check_program(&program).unwrap();
        assert_eq!(providers::contract::sqlite_mask(&types), 0, "{declaration}");
    }
}

#[test]
fn sqlite_provider_rejects_incompatible_row_layouts() {
    let variants = "Null, Integer(value: Int64), Real(value: Float64), Text(value: String), Blob(value: Bytes)";
    for (declarations, slot) in [
        ("struct SqliteRows { let statement: SqliteStatement }".to_owned(), 11),
        (format!("enum SqliteValue {{ {variants} }} struct SqliteRow {{ let values: List(SqliteValue); let extra: Int32 }}"), 12),
        (format!("enum SqliteValue {{ {} }} struct SqliteRow {{ let values: List(SqliteValue) }}", variants.replace("Int64", "Int32")), 12),
        (format!("enum SqliteValue {{ {} }} struct SqliteRow {{ let values: List(SqliteValue) }}", variants.replace("Null, Integer(value: Int64)", "Integer(value: Int64), Null")), 12),
    ] {
        let operation = if slot == 11 {
            "@suspends fn query(stmt: SqliteStatement) -> Result(SqliteRows, String)"
        } else {
            "@suspends fn step(stmt: SqliteStatement) -> Result(Option((SqliteRow, SqliteStatement)), String)"
        };
        let source = format!("{declarations} eff sqlite {{ {operation} }} fn main() {{}}");
        let program = crate::syntax::parse_program(&source).unwrap();
        let types = crate::sema::check_program(&program).unwrap();
        let mask = providers::contract::sqlite_mask(&types);
        assert_eq!(mask & (1 << slot), 0, "{declarations}");
    }
}

#[test]
fn sqlite_cursor_rejects_parallel_and_hidden_generic_effects() {
    for (body, expected) in [
        ("fn consume(rows: SqliteRows) effects { sqlite } { @parallel(limit: 2) for row in rows { continue }; () } fn main() {}", "parallel for requires a List"),
        ("fn consume(T: type + Cursor, value: T) { for item in value { continue }; () } fn main() { consume(SqliteRows(statement: None)) }", "declares effects not allowed"),
    ] {
        let source = format!("{}\n{body}", include_str!("../../std/joky/sqlite.jk"));
        let program = crate::syntax::parse_program(&source).unwrap();
        let error = crate::sema::check_program(&program).unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
    }
}

mod benches;
mod collections;
mod debug;
mod equality;
mod equality_containers;
mod strings;

mod continuations;
mod cown;
mod drop;

#[test]
fn target_specific_extern_selection_covers_all_platforms() {
    let source = "@extern(c, \"libSystem.B.dylib\", \"f\", os = \"macos\") fn f() -> Unit; @extern(c, \"libc.so.6\", \"f\", os = \"linux\") fn f() -> Unit; @extern(c, \"ucrtbase.dll\", \"f\", os = \"windows\") fn f() -> Unit; fn main() {}";
    for os in ["macos", "linux", "windows"] {
        let mut program = crate::syntax::parse_program(source).unwrap();
        super::select_target_externs(&mut program, os).unwrap();
        assert_eq!(
            program
                .functions
                .iter()
                .filter(|f| f.foreign.is_some())
                .count(),
            1
        );
    }
}

#[test]
fn target_specific_extern_selection_rejects_duplicate_matches() {
    let source = "@extern(c, \"a\", \"f\", os = \"linux\") fn f() -> Unit; @extern(c, \"b\", \"f\", os = \"linux\") fn f() -> Unit; fn main() {}";
    let mut program = crate::syntax::parse_program(source).unwrap();
    assert!(super::select_target_externs(&mut program, "linux").is_err());
}
mod arithmetic;
mod cursors;
mod effects;
mod ffi;
mod files;
mod iteration;
mod local_var;
mod ownership;
mod ranges;
mod resources;
mod resumable;
mod robustness;
mod runtime;
mod tasks;

mod from_string;
mod intrinsic_methods;
mod ordering;
mod sorting;
