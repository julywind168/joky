use super::*;
use crate::vm::{VmEngine, VmMode};

const SOURCE: &str = r#"
import joky/option.{type Option}
import joky/result.{type Result}
struct Flat { pub n:Int32, empty:Unit }
struct Inner { flag:Bool, rune:Rune }
enum Choice { Empty, Value(Inner), Other(Option<Int32>) }
enum Phantom<T> { None }
fn flat(value:Flat) @suspends ->Flat {value}
fn echo(value:Choice) @suspends ->Choice {value}
fn phantom(value:Phantom<Int32>) @suspends ->Phantom<Int32> {value}
"#;

#[test]
fn pure_enum_packets_validate_tags_active_payloads_and_inactive_lanes() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE).unwrap();
        let signature = vm.function_signature("echo").unwrap();
        let ty = &signature.ret;
        let contract = schema(ty).unwrap();
        // tag, Value.flag, Value.rune, Other.tag, Other.Some(Int32).
        for words in [
            [0, 0, 0, 0, 0],
            [1, 1, '中' as u64, 0, 0],
            [2, 0, 0, 0, 0],
            [2, 0, 0, 1, 42],
        ] {
            assert!(valid_words(ty, &words));
            let (mut task, layout) = vm
                .prepare_script_call("echo", &contract, &contract, &words)
                .unwrap();
            while task.flat_result(layout).is_none() {
                task.resume().unwrap();
            }
            assert_eq!(task.flat_result(layout).unwrap(), words);
        }
        for words in [
            [3, 0, 0, 0, 0],
            [u64::MAX, 0, 0, 0, 0],
            [1 << 32, 0, 0, 0, 0],
            [1, 2, 0, 0, 0],
            [1, 0, 0xd800, 0, 0],
            [0, 1, 0, 0, 0],
            [0, 0, 0, 1, 0],
            [2, 0, 0, 2, 0],
            [2, 0, 0, 0, 42],
        ] {
            assert!(!valid_words(ty, &words), "{words:?}");
            assert!(vm
                .prepare_script_call("echo", &contract, &contract, &words)
                .is_err());
        }
        assert!(!valid_words(ty, &[0; 4]));
        assert!(!valid_words(ty, &[0; 6]));
        assert_eq!(
            schema(&vm.function_signature("flat").unwrap().ret).unwrap(),
            "record:4:Flat:false;1:n:false:Int32;5:empty:true:Unit"
        );
    }
}

#[test]
fn pure_enum_contracts_include_nominal_arguments_and_all_variants() {
    let mut vm = VmEngine::new();
    vm.load_source(SOURCE).unwrap();
    let contract = schema(&vm.function_signature("echo").unwrap().ret).unwrap();
    let phantom = schema(&vm.function_signature("phantom").unwrap().ret).unwrap();
    for next in [
        SOURCE.replace("Empty, Value(Inner)", "Value(Inner), Empty"),
        SOURCE.replace("Empty,", "Nothing,"),
        SOURCE.replace("rune:Rune", "rune:UInt32"),
        SOURCE
            .replace("enum Choice", "enum OtherChoice")
            .replace(":Choice", ":OtherChoice")
            .replace("->Choice", "->OtherChoice"),
        SOURCE.replace("Empty, Value", "Empty(Unit), Value"),
        SOURCE.replace("Other(Option<Int32>)", "Other(Option<Int64>)"),
    ] {
        vm.load_source(&next).unwrap();
        assert_ne!(
            schema(&vm.function_signature("echo").unwrap().ret).unwrap(),
            contract
        );
    }
    vm.load_source(&SOURCE.replace("Phantom<Int32>", "Phantom<UInt32>"))
        .unwrap();
    assert_ne!(
        schema(&vm.function_signature("phantom").unwrap().ret).unwrap(),
        phantom
    );
    vm.load_source(SOURCE).unwrap();
    assert_eq!(
        schema(&vm.function_signature("echo").unwrap().ret).unwrap(),
        contract
    );
}
