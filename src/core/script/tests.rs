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

const COMPOSITE: &str = r#"
import joky/option.{type Option}
struct Cell<T> { value:T }
struct Phantom<T> {}
struct Empty {}
fn echo(value:(Unit, Cell<Option<(Bool,Rune)>>, Empty, UInt64)) @suspends
    ->(Unit, Cell<Option<(Bool,Rune)>>, Empty, UInt64) {value}
fn phantom(value:Phantom<(Int32,Bool)>) @suspends ->Phantom<(Int32,Bool)> {value}
fn zero(value:(Unit,Empty,Phantom<Bool>)) @suspends ->(Unit,Empty,Phantom<Bool>) {value}
"#;

#[test]
fn pure_composite_packets_validate_nested_values_and_zero_lane_offsets() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(COMPOSITE).unwrap();
        let ty = vm.function_signature("echo").unwrap().ret;
        let contract = schema(&ty).unwrap();
        for words in [[0, 0, 0, u64::MAX], [1, 1, '中' as u64, u64::MAX]] {
            assert!(valid_words(&ty, &words));
            let (mut task, layout) = vm
                .prepare_script_call("echo", &contract, &contract, &words)
                .unwrap();
            while task.flat_result(layout).is_none() {
                task.resume().unwrap();
            }
            assert_eq!(task.flat_result(layout).unwrap(), words);
        }
        for words in [
            [2, 0, 0, 0],
            [1, 2, 0, 0],
            [1, 0, 0xd800, 0],
            [0, 1, 0, 0],
            [0, 0, 42, 0],
        ] {
            assert!(!valid_words(&ty, &words));
            assert!(vm
                .prepare_script_call("echo", &contract, &contract, &words)
                .is_err());
        }
        assert!(!valid_words(&ty, &[0; 3]));
        assert!(!valid_words(&ty, &[0; 5]));
        let zero = vm.function_signature("zero").unwrap().ret;
        let contract = schema(&zero).unwrap();
        assert!(valid_words(&zero, &[]));
        let (mut task, layout) = vm
            .prepare_script_call("zero", &contract, &contract, &[])
            .unwrap();
        while task.flat_result(layout).is_none() {
            task.resume().unwrap();
        }
        assert!(task.flat_result(layout).unwrap().is_empty());
    }
}

#[test]
fn pure_composite_contracts_preserve_order_nominality_and_phantom_arguments() {
    let mut vm = VmEngine::new();
    vm.load_source(COMPOSITE).unwrap();
    let contract = schema(&vm.function_signature("echo").unwrap().ret).unwrap();
    let phantom = schema(&vm.function_signature("phantom").unwrap().ret).unwrap();
    for next in [
        COMPOSITE.replace("Bool,Rune", "Rune,Bool"),
        COMPOSITE.replace("Bool,Rune", "Bool,UInt32"),
        COMPOSITE.replace("value:T", "renamed:T"),
        COMPOSITE.replace("value:T", "pub value:T"),
        COMPOSITE.replace("Cell", "Boxed"),
        COMPOSITE.replace("Unit, Cell", "Cell"),
        COMPOSITE.replace("value:T", "value:T, empty:Unit"),
    ] {
        vm.load_source(&next).unwrap();
        assert_ne!(
            schema(&vm.function_signature("echo").unwrap().ret).unwrap(),
            contract
        );
    }
    vm.load_source(&COMPOSITE.replace("Phantom<(Int32,Bool)>", "Phantom<(UInt32,Bool)>"))
        .unwrap();
    assert_ne!(
        schema(&vm.function_signature("phantom").unwrap().ret).unwrap(),
        phantom
    );
    vm.load_source(COMPOSITE).unwrap();
    assert_eq!(
        schema(&vm.function_signature("echo").unwrap().ret).unwrap(),
        contract
    );
    assert_eq!(
        schema(&vm.function_signature("phantom").unwrap().ret).unwrap(),
        phantom
    );
}

#[test]
fn pure_composite_contracts_count_repeated_and_phantom_zero_lane_members() {
    let mut vm = VmEngine::new();
    // Each tuple is counted twice: as a concrete argument and as a field.
    for count in [31, 32] {
        let tuple = format!("({})", vec!["Unit"; count].join(","));
        vm.load_source(&format!(
            "struct Cell<T>{{value:T}} fn echo(v:Cell<{tuple}>) @suspends ->Cell<{tuple}>{{v}}"
        ))
        .unwrap();
        assert_eq!(
            schema(&vm.function_signature("echo").unwrap().ret).is_some(),
            count == 31
        );
    }
    for count in [63, 64] {
        let tuple = format!("({})", vec!["Unit"; count].join(","));
        vm.load_source(&format!(
            "struct Marker<T>{{}} fn echo(v:Marker<{tuple}>) @suspends ->Marker<{tuple}>{{v}}"
        ))
        .unwrap();
        assert_eq!(
            schema(&vm.function_signature("echo").unwrap().ret).is_some(),
            count == 63
        );
    }
    vm.load_source(COMPOSITE).unwrap();
    let before = schema(&vm.function_signature("echo").unwrap().ret).unwrap();
    vm.load_source(
        &COMPOSITE
            .replace("<T>", "<U>")
            .replace("value:T", "value:U"),
    )
    .unwrap();
    assert_eq!(
        schema(&vm.function_signature("echo").unwrap().ret).unwrap(),
        before
    );
}
