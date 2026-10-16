//! Shared typed-enum Script calls for backend and cancellation coverage.
pub fn source(create: &str) -> String {
    let types = include_str!("../fixtures/pure_enum_types.jk");
    let script = format!(
        "import joky/task import joky/option.{{type Option}} import joky/result.{{type Result}} {types} fn echo(value:Message) @suspends ->Message {{task.yield_now() value}}"
    );
    let quote = format!("{:?}", script.replace('{', "{{").replace('}', "}}"));
    include_str!("../fixtures/script_enum_values.jk")
        .replace("__TYPES__", types)
        .replace("__CREATE__", create)
        .replace("__SCRIPT__", &quote)
}
