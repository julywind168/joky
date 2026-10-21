//! Shared owning Script calls for backend, allocation and cancellation coverage.
pub fn source(create: &str) -> String {
    let types = "struct Item { text: Option<String>, number: Int16 }";
    let script = format!(
        r#"import joky/task
import joky/array.{{type Array}}
import joky/option.{{type Option}}
import joky/result.{{type Result}}
{types}
fn text(value:String) @suspends ->String {{task.yield_now() value}}
fn length(value:String) @suspends ->UInt64 {{task.yield_now() value.byte_length()}}
fn items(value:Array<Item>) @suspends ->Result<Array<Item>,String> {{task.yield_now() Result.Ok(value)}}
fn nested(value:Array<Array<String>>) @suspends ->Array<Array<String>> {{task.yield_now() value}}
fn empty(_:Unit) @suspends ->Array<String> {{Array.new::<String>()}}
fn absent(value:Option<String>) @suspends ->Option<String> {{task.yield_now() value}}
fn reply(value:Result<String,String>) @suspends ->Result<String,String> {{task.yield_now() value}}
"#
    );
    let quoted = format!("{:?}", script.replace('{', "{{").replace('}', "}}"));
    include_str!("../fixtures/script_owned_values.jk")
        .replace("__TEXT__", &format!("{:?}", "你好\0🙂".repeat(100)))
        .replace("__TYPES__", types)
        .replace("__CREATE__", create)
        .replace("__SCRIPT__", &quoted)
}
