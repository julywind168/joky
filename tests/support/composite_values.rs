//! Shared composite Script calls for backend and cancellation coverage.
pub fn source(create: &str) -> String {
    let types = "struct Cell<T>{value:T} struct Empty<T>{} enum Fault{Denied,Code(Int32)}";
    let script = format!(
        "import joky/task import joky/option.{{type Option}} import joky/result.{{type Result}} {types}
        fn tuple(v:(Unit,Bool,Rune,UInt64,Float64,Float32)) @suspends ->(Unit,Bool,Rune,UInt64,Float64,Float32) {{task.yield_now(); v}}
        fn record(v:Cell<Option<(Int32,Bool)>>) @suspends ->Result<Cell<Option<(Int32,Bool)>>,Fault> {{task.yield_now(); Result.Ok(v)}}
        fn zero(v:(Empty<(Unit,Bool)>,Unit)) @suspends ->(Empty<(Unit,Bool)>,Unit) {{task.yield_now(); v}}"
    );
    let quote = format!("{:?}", script.replace('{', "{{").replace('}', "}}"));
    include_str!("../fixtures/script_composite_values.jk")
        .replace("__TYPES__", types)
        .replace("__CREATE__", create)
        .replace("__SCRIPT__", &quote)
}
