use crate::{parser::ast::Item, ErrorCode, FunctionSignature};

pub const SOURCE: &str = include_str!("../fixtures/text_ffi.jk");
pub const C_SOURCE: &str = include_str!("../fixtures/text_ffi.c");

pub fn signatures() -> Vec<(String, FunctionSignature)> {
    crate::parser::parse(SOURCE)
        .unwrap()
        .items
        .into_iter()
        .filter_map(|item| {
            let Item::ExternFn(f) = item else { return None };
            Some((
                f.name,
                FunctionSignature::new(f.params.into_iter().map(|p| p.ty).collect(), f.ret_ty),
            ))
        })
        .collect()
}

pub const TEXTS: &[(&str, &str, i32)] = &[
    ("span_text", "中🙂\0tail", 0),
    ("cstring_text", "中🙂", 1),
    ("cstring_empty", "", 1),
    ("writer_text", "中🙂\0tail", 0),
    ("writer_empty", "", 0),
    ("indirect_text", "中🙂", 0),
    ("generic_text", "中🙂", 0),
    ("alias_temporary", "中🙂", 0),
    ("empty_text", "", 0),
    ("owned_empty_text", "", 1),
];

pub const FAILURES: &[(&str, ErrorCode, i32, i32)] = &[
    ("scratch_failure", ErrorCode::ResourceLimit, 0, 0),
    ("channel_error", ErrorCode::RuntimeError, 1, 1),
    ("cstring_nul", ErrorCode::InteriorNul, 0, 0),
    ("narrow_length", ErrorCode::ResourceLimit, 0, 0),
    ("bad_utf8", ErrorCode::InvalidUtf8, 1, 1),
    ("too_long", ErrorCode::ResourceLimit, 1, 1),
    ("host_failure", ErrorCode::HostError, 1, 1),
    ("writer_overflow", ErrorCode::ResourceLimit, 1, 0),
    ("no_terminator", ErrorCode::AbiError, 1, 1),
    ("allocation_failure", ErrorCode::ResourceLimit, 1, 1),
    ("null_text", ErrorCode::AbiError, 1, 0),
    ("writer_utf8", ErrorCode::InvalidUtf8, 1, 0),
];
