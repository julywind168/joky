//! Standard-module ↔ runtime layout contract tests.
//!
//! Production registration is name-driven and generic; these tests hold the
//! second source of truth deliberately. They re-validate the shipped
//! standard declarations against the layouts the runtime hooks expect, so
//! drift fails here instead of misreading arguments at runtime.

use super::socket::operation_metadata as socket_operation_metadata;
use crate::sema::check_program;
use crate::sema::{EffectId, EffectMode, Type, TypeTable};
use crate::syntax::parse_program;

const FILE_API: &str = include_str!("../../../std/joky/file.jk");
const ENV_API: &str = include_str!("../../../std/joky/env.jk");
const RANDOM_API: &str = include_str!("../../../std/joky/crypto/random.jk");
const SQLITE_API: &str = include_str!("../../../std/joky/sqlite.jk");
const TCP_API: &str = include_str!("../../../std/joky/socket/tcp.jk");
const UDP_API: &str = include_str!("../../../std/joky/socket/udp.jk");
#[cfg(unix)]
const UNIX_API: &str = include_str!("../../../std/joky/socket/unix.jk");

fn checked(source: &str) -> TypeTable {
    check_program(&parse_program(source).unwrap())
        .unwrap()
        .module
        .types
}

fn validated(
    types: &TypeTable,
    effect: EffectId,
    name: &str,
    parameters: &[Type],
    ok: Type,
) -> bool {
    let Some(id) = types.effects().operation_by_name(effect, name) else {
        return false;
    };
    let Some(info) = types.effects().operation_info(id) else {
        return false;
    };
    let Type::Result(result) = info.return_type else {
        return false;
    };
    info.suspends
        && info.mode == EffectMode::Normal
        && info.parameters == parameters
        && types.result_types(result) == (ok, Type::String)
}

fn enum_matches(types: &TypeTable, ty: Type, name: &str, variants: &[&str]) -> bool {
    let Type::Enum(id) = ty else {
        return false;
    };
    types.enum_name(id).rsplit('/').next() == Some(name)
        && types.enum_variants(id).len() == variants.len()
        && types
            .enum_variants(id)
            .iter()
            .zip(variants)
            .all(|(variant, name)| variant.name == *name && variant.fields.is_empty())
}

/// The file hook layouts the runtime expects, mirroring the name-keyed table
/// in `crates/joky-runtime/src/runtime/file.rs::operation_hooks`.
fn file_mask(types: &TypeTable) -> u64 {
    let Some(effect) = types.effects().by_name("file") else {
        return 0;
    };
    let mut mask = 0u64;
    for (slot, name, parameters, ok) in [
        (0u64, "read", vec![Type::String], Type::String),
        (1, "read_bytes", vec![Type::String], Type::Bytes),
        (2, "write", vec![Type::String, Type::String], Type::U64),
        (3, "write_bytes", vec![Type::String, Type::Bytes], Type::U64),
        (5, "read_chunk", vec![Type::file(), Type::U64], Type::Bytes),
        (6, "write_chunk", vec![Type::file(), Type::Bytes], Type::U64),
        (7, "position", vec![Type::file()], Type::U64),
        (9, "flush", vec![Type::file()], Type::Unit),
        (10, "sync", vec![Type::file()], Type::Unit),
        (11, "close", vec![Type::file()], Type::Unit),
    ] {
        if validated(types, effect, name, &parameters, ok) {
            mask |= 1 << slot;
        }
    }
    for (slot, name, prefix, ok) in [
        (4u64, "open", vec![Type::String], Type::file()),
        (8, "seek", vec![Type::file(), Type::I64], Type::U64),
    ] {
        let Some(id) = types.effects().operation_by_name(effect, name) else {
            continue;
        };
        let Some(info) = types.effects().operation_info(id) else {
            continue;
        };
        let Type::Result(result) = info.return_type else {
            continue;
        };
        let (arity, matches) = match name {
            "open" => (
                2,
                enum_matches(
                    types,
                    info.parameters[1],
                    "FileMode",
                    &["Read", "Write", "Append", "ReadWrite", "CreateNew"],
                ),
            ),
            _ => (
                3,
                enum_matches(
                    types,
                    info.parameters[2],
                    "SeekFrom",
                    &["Start", "Current", "End"],
                ),
            ),
        };
        if info.suspends
            && info.mode == EffectMode::Normal
            && info.parameters.len() == arity
            && info.parameters[..arity - 1] == prefix
            && matches
            && types.result_types(result) == (ok, Type::String)
        {
            mask |= 1 << slot;
        }
    }
    mask
}

/// The sqlite hook layouts the runtime expects, mirroring the name-keyed
/// table in `crates/joky-runtime/src/runtime/sqlite.rs::operation_hooks`.
pub(crate) fn sqlite_mask(types: &TypeTable) -> u64 {
    let Some(effect) = types.effects().by_name("sqlite") else {
        return 0;
    };
    let mut mask = 0u64;
    for (slot, name, parameters, ok, borrows_prepare) in [
        (
            0u64,
            "open",
            vec![Type::String],
            Type::sqlite_connection(),
            false,
        ),
        (
            1,
            "prepare",
            vec![Type::sqlite_connection(), Type::String],
            Type::sqlite_statement(),
            true,
        ),
        (
            2,
            "execute",
            vec![Type::sqlite_statement()],
            Type::U64,
            false,
        ),
        (
            4,
            "bind_text",
            vec![Type::sqlite_statement(), Type::U64, Type::String],
            Type::sqlite_statement(),
            false,
        ),
        (
            5,
            "bind_i64",
            vec![Type::sqlite_statement(), Type::U64, Type::I64],
            Type::sqlite_statement(),
            false,
        ),
        (
            6,
            "finalize",
            vec![Type::sqlite_statement()],
            Type::Unit,
            false,
        ),
        (
            8,
            "bind_null",
            vec![Type::sqlite_statement(), Type::U64],
            Type::sqlite_statement(),
            false,
        ),
        (
            9,
            "bind_f64",
            vec![Type::sqlite_statement(), Type::U64, Type::F64],
            Type::sqlite_statement(),
            false,
        ),
        (
            10,
            "bind_blob",
            vec![Type::sqlite_statement(), Type::U64, Type::Bytes],
            Type::sqlite_statement(),
            false,
        ),
    ] {
        let Some(id) = types.effects().operation_by_name(effect, name) else {
            continue;
        };
        let Some(info) = types.effects().operation_info(id) else {
            continue;
        };
        let Type::Result(result) = info.return_type else {
            continue;
        };
        let borrows = parameters
            .iter()
            .enumerate()
            .map(|(index, _)| borrows_prepare && index == 0)
            .collect::<Vec<_>>();
        if info.suspends
            && info.mode == EffectMode::Normal
            && info.parameters == parameters
            && info.parameter_borrows == borrows
            && types.result_types(result) == (ok, Type::String)
        {
            mask |= 1 << slot;
        }
    }
    // close is the one sqlite operation whose Ok type is bare Unit: the
    // deferred physical close cannot fail, so its signature has no error case.
    if let Some(id) = types.effects().operation_by_name(effect, "close") {
        if let Some(info) = types.effects().operation_info(id) {
            if info.suspends
                && info.mode == EffectMode::Normal
                && info.parameters == [Type::sqlite_connection()]
                && info.parameter_borrows == [false]
                && info.return_type == Type::Unit
            {
                mask |= 1 << 7;
            }
        }
    }
    // query/step return composite layouts that embed SqliteRow/SqliteRows.
    for (slot, name) in [(11u64, "query"), (12u64, "step")] {
        let Some(operation) = types.effects().operation_by_name(effect, name) else {
            continue;
        };
        let Some(info) = types.effects().operation_info(operation) else {
            continue;
        };
        let Type::Result(result) = info.return_type else {
            continue;
        };
        let (ok_type, error) = types.result_types(result);
        if !info.suspends
            || info.mode != EffectMode::Normal
            || info.parameters != [Type::sqlite_statement()]
            || info.parameter_borrows != [false]
            || error != Type::String
        {
            continue;
        }
        let valid = if name == "query" {
            matches!(ok_type, Type::Struct(rows) if sqlite_rows_layout(types, rows))
        } else {
            matches!(ok_type, Type::Option(option)
                if matches!(types.option_type(option), Type::Tuple(pair)
                    if types.tuple_elements(pair).len() == 2
                        && types.tuple_elements(pair)[1] == Type::sqlite_statement()
                        && sqlite_row_layout(types, types.tuple_elements(pair)[0])))
        };
        if valid {
            mask |= 1 << slot;
        }
    }
    mask
}

fn sqlite_rows_layout(types: &TypeTable, rows: usize) -> bool {
    let fields = types.struct_fields(rows);
    types.struct_name(rows).rsplit('/').next() == Some("SqliteRows")
        && fields.len() == 1
        && matches!(fields[0].1, Type::Option(id) if types.option_type(id) == Type::sqlite_statement())
}

fn sqlite_row_layout(types: &TypeTable, row: Type) -> bool {
    let Type::Struct(row) = row else {
        return false;
    };
    let fields = types.struct_fields(row);
    if types.struct_name(row).rsplit('/').next() != Some("SqliteRow") || fields.len() != 1 {
        return false;
    }
    let Type::List(list) = fields[0].1 else {
        return false;
    };
    let Type::Enum(value) = types.list_type(list) else {
        return false;
    };
    let variants = types.enum_variants(value);
    let expected = [
        ("Null", None),
        ("Integer", Some(Type::I64)),
        ("Real", Some(Type::F64)),
        ("Text", Some(Type::String)),
        ("Blob", Some(Type::Bytes)),
    ];
    types.enum_name(value).rsplit('/').next() == Some("SqliteValue")
        && variants.len() == expected.len()
        && variants
            .iter()
            .zip(expected)
            .all(|(variant, (name, field))| {
                variant.name == name && variant.fields.iter().map(|(_, ty)| *ty).eq(field)
            })
}

#[test]
fn runtime_effect_registry_groups_match_provider_dispatch() {
    // Order matters: AOT launcher registration failures report 2 + index
    // over these groups. Mirrors `host::dispatch_provider`.
    let groups = super::provider_effects();
    let names = groups
        .iter()
        .map(|(provider, effects)| (*provider, effects.to_vec()))
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            ("process", vec!["process"]),
            ("file", vec!["file"]),
            ("env", vec!["env"]),
            ("socket", vec!["tcp", "udp", "unix", "unix_dgram", "tls"]),
            ("sqlite", vec!["sqlite"]),
            ("random", vec!["random"]),
        ]
    );
    // Every provider-attributed registry row is a runtime effect and every
    // effect without a provider is still identity-canonicalized.
    assert!(crate::sema::effects::is_runtime_effect("time"));
    assert!(!crate::sema::effects::is_runtime_effect("custom"));
}

#[test]
fn std_random_declaration_matches_runtime_hook_layout() {
    let types = checked(&format!(
        "{RANDOM_API}\nfn main() effects {{ random }} {{ () }}"
    ));
    let effect = types.effects().by_name("random").expect("random effect");
    assert!(validated(
        &types,
        effect,
        "bytes",
        &[Type::U64],
        Type::Bytes
    ));
    let operation = types.effects().operation_by_name(effect, "bytes").unwrap();
    assert_eq!(
        types
            .effects()
            .operation_info(operation)
            .unwrap()
            .parameter_borrows,
        [false]
    );
}

#[test]
fn std_env_declarations_are_suspending_result_operations() {
    let types = checked(&format!("{ENV_API}\nfn main() effects {{ env }} {{ () }}"));
    let effect = types.effects().by_name("env").expect("env effect");
    for (name, parameters, ok) in [
        (
            "get",
            vec![Type::String],
            Type::Option(types.option_id(Type::String).unwrap()),
        ),
        (
            "vars",
            vec![],
            Type::Map(types.map_id(Type::String, Type::String).unwrap()),
        ),
        (
            "args",
            vec![],
            Type::List(types.list_id(Type::String).unwrap()),
        ),
        ("current_dir", vec![], Type::String),
        ("temp_dir", vec![], Type::String),
    ] {
        assert!(
            validated(&types, effect, name, &parameters, ok),
            "env.{name} layout changed"
        );
    }
}

#[test]
fn std_file_declarations_match_runtime_hook_layout() {
    let types = checked(&format!(
        "{FILE_API}\nfn main() effects {{ file }} {{ () }}"
    ));
    assert_eq!(file_mask(&types), 0xfff, "every file hook must validate");
}

#[test]
fn std_sqlite_declarations_match_runtime_hook_layout() {
    let types = checked(&format!(
        "{SQLITE_API}\nfn main() effects {{ sqlite }} {{ () }}"
    ));
    assert_eq!(
        sqlite_mask(&types),
        0x1ff7,
        "every sqlite hook must validate"
    );
}

#[test]
fn std_socket_declarations_match_runtime_hook_layout() {
    let source = format!("{TCP_API}\n{UDP_API}\nfn main() effects {{ tcp, udp }} {{ () }}");
    let types = checked(&source);
    let (_, mask) = socket_operation_metadata(&types);
    assert_eq!(mask, 0x1fff, "tcp and udp hooks must validate");
}

#[cfg(unix)]
#[test]
fn std_unix_socket_declarations_match_runtime_hook_layout() {
    let source = format!("{UNIX_API}\nfn main() effects {{ unix, unix_dgram }} {{ () }}");
    let types = checked(&source);
    let (_, mask) = socket_operation_metadata(&types);
    assert_eq!(
        mask >> 13,
        0x3ff,
        "all unix and unix_dgram hooks must validate (tcp/udp absent by design)"
    );
}

#[test]
fn incompatible_sqlite_contracts_stay_unregistered() {
    for declaration in [
        "fn open(path: String) -> Result(SqliteConnection, String)",
        "@suspends fn open(path: Bytes) -> Result(SqliteConnection, String)",
        "@suspends fn close(db: &SqliteConnection) -> Result(Unit, String)",
        "@suspends fn close(db: SqliteConnection) -> Result(Unit, String)",
        "@suspends fn open(path: String) -> Result(String, String)",
        "@suspends fn prepare(db: SqliteConnection, sql: String) -> Result(SqliteStatement, String)",
        "@suspends fn bind_i64(stmt: SqliteStatement, index: UInt64, value: Int64) -> SqliteStatement",
    ] {
        let source = format!("eff sqlite {{ {declaration} }} fn main() {{}}");
        let types = checked(&source);
        assert_eq!(sqlite_mask(&types), 0, "{declaration}");
    }
}

#[test]
fn incompatible_file_contracts_stay_unregistered() {
    for declaration in [
        "fn read(path: String) -> Result(String, String)",
        "@suspends fn read(path: Bytes) -> Result(String, String)",
        "@suspends fn read(path: String) -> Result(Bytes, String)",
        "@suspends fn write(path: String, data: String) -> Result(Unit, String)",
        "@suspends fn read_chunk(handle: File, max_bytes: Int64) -> Result(Bytes, String)",
    ] {
        let source = format!("eff file {{ {declaration} }} fn main() {{}}");
        let types = checked(&source);
        assert_eq!(file_mask(&types), 0, "{declaration}");
    }
}

#[test]
fn std_process_declarations_match_owned_provider_layouts() {
    let types = checked(&format!(
        "{}\nfn main() effects {{ process }} {{ () }}",
        include_str!("../../../std/joky/process.jk")
    ));
    let command = types.class_type("Command").unwrap();
    let Type::Class(class) = command else {
        unreachable!()
    };
    let option = Type::Option(types.option_id(Type::String).unwrap());
    let policy = types.enum_type("EnvPolicy").unwrap();
    assert_eq!(
        types.class_fields(class),
        &[
            ("program".into(), Type::String),
            (
                "args".into(),
                Type::List(types.list_id(Type::String).unwrap())
            ),
            ("cwd".into(), option),
            ("env".into(), policy),
        ]
    );
    let Type::Enum(policy) = policy else {
        unreachable!()
    };
    let variants = types.enum_variants(policy);
    assert_eq!(variants.len(), 2);
    assert_eq!(variants[0].name, "Inherit");
    assert_eq!(
        variants[0].fields,
        [(
            "overrides".into(),
            Type::Map(types.map_id(Type::String, option).unwrap())
        )]
    );
    assert_eq!(variants[1].name, "Replace");
    assert_eq!(
        variants[1].fields,
        [(
            "values".into(),
            Type::Map(types.map_id(Type::String, Type::String).unwrap())
        )]
    );
    let status = types.enum_type("ExitStatus").unwrap();
    let Type::Enum(id) = status else {
        unreachable!()
    };
    let variants = types.enum_variants(id);
    assert_eq!(variants.len(), 2);
    assert_eq!(variants[0].name, "Exited");
    assert_eq!(variants[0].fields, [("code".into(), Type::U32)]);
    assert_eq!(variants[1].name, "Signaled");
    assert_eq!(variants[1].fields, [("signal".into(), Type::I32)]);
    let output = types.struct_type("Output").unwrap();
    let Type::Struct(id) = output else {
        unreachable!()
    };
    assert_eq!(
        types.struct_fields(id),
        &[
            ("status".into(), status),
            ("stdout".into(), Type::Bytes),
            ("stderr".into(), Type::Bytes),
        ]
    );
    let stage = types.struct_type("Stage").unwrap();
    let Type::Struct(stage_id) = stage else {
        unreachable!()
    };
    assert_eq!(types.struct_fields(stage_id), types.class_fields(class));
    let pipeline = types.class_type("Pipeline").unwrap();
    let Type::Class(pipeline_id) = pipeline else {
        unreachable!()
    };
    assert_eq!(
        types.class_fields(pipeline_id),
        &[("stages".into(), Type::List(types.list_id(stage).unwrap()))]
    );
    let statuses = Type::List(types.list_id(status).unwrap());
    let pipeline_output = types.struct_type("PipelineOutput").unwrap();
    let Type::Struct(output_id) = pipeline_output else {
        unreachable!()
    };
    assert_eq!(
        types.struct_fields(output_id),
        &[
            ("statuses".into(), statuses),
            ("stdout".into(), Type::Bytes),
            (
                "stderrs".into(),
                Type::List(types.list_id(Type::Bytes).unwrap())
            ),
        ]
    );
    let effect = types.effects().by_name("process").unwrap();
    for (name, parameters, ok, borrows) in [
        ("pipeline_status", vec![pipeline], statuses, vec![true]),
        (
            "pipeline_output",
            vec![pipeline, Type::Bytes, Type::U64],
            pipeline_output,
            vec![true, false, false],
        ),
        ("status", vec![command], status, vec![true]),
        (
            "output",
            vec![command, Type::Bytes, Type::U64],
            output,
            vec![true, false, false],
        ),
    ] {
        assert!(
            validated(&types, effect, name, &parameters, ok),
            "process.{name}"
        );
        let operation = types.effects().operation_by_name(effect, name).unwrap();
        assert_eq!(
            types
                .effects()
                .operation_info(operation)
                .unwrap()
                .parameter_borrows,
            borrows
        );
    }
}

#[test]
fn std_tls_declarations_match_runtime_hook_layout() {
    let tls = include_str!("../../../std/joky/socket/tls.jk").replace("import joky/socket/tcp", "");
    let types = checked(&format!(
        "{TCP_API}\n{tls}\nfn main() effects {{ tcp, tls }} {{}}"
    ));
    let effect = types.effects().by_name("tls").unwrap();
    let stream = Type::Native(11);
    assert_eq!(Type::NATIVE_TYPES[11], "TlsStream");
    for (name, params, ok, borrows) in [
        (
            "upgrade",
            vec![Type::tcp_stream(), Type::String, Type::Bytes],
            stream,
            vec![false, false, false],
        ),
        (
            "read",
            vec![stream, Type::U64],
            Type::Bytes,
            vec![true, false],
        ),
        (
            "write",
            vec![stream, Type::Bytes],
            Type::U64,
            vec![true, false],
        ),
        ("close", vec![stream], Type::Unit, vec![false]),
    ] {
        assert!(validated(&types, effect, name, &params, ok), "tls.{name}");
        let id = types.effects().operation_by_name(effect, name).unwrap();
        assert_eq!(
            types
                .effects()
                .operation_info(id)
                .unwrap()
                .parameter_borrows,
            borrows
        );
    }
}
