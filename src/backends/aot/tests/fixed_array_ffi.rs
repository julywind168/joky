//! Exact C array prototypes and independent layout checks execute in every backend.
use super::*;
use crate::core::types::{ConstArgument, FixedArrayType};
use crate::vm::{VmEngine, VmMode};
use std::{rc::Rc, sync::Arc};

const SOURCE: &str = include_str!("../../../../tests/fixtures/fixed_array_f4.jk");
const C_SOURCE: &str = include_str!("../../../../tests/fixtures/fixed_array_f4.c");
const CASES: &[&str] = &[
    "ffi_array_records",
    "ffi_array_pointers",
    "ffi_array_bytes",
    "ffi_array_floats",
    "ffi_array_signed",
    "ffi_array_unsigned",
];
fn array(element: Type, length: u64) -> Type {
    Type::FixedArray(Arc::new(FixedArrayType {
        element,
        length: ConstArgument::Value(length),
    }))
}
fn registrations() -> Vec<(&'static str, Vec<Type>)> {
    let r = Type::reference;
    let row = array(Type::Int32, 4);
    let bytes = array(Type::UInt8, 8);
    vec![
        (
            "cfa_packets",
            vec![r(array(Type::Named("Packet".into()), 2), true)],
        ),
        ("cfa_packet", vec![r(Type::Named("Packet".into()), true)]),
        (
            "cfa_matrix",
            vec![r(array(array(Type::Int32, 3), 2), false)],
        ),
        (
            "cfa_cells",
            vec![r(array(Type::Named("FloatCell".into()), 2), true)],
        ),
        (
            "cfa_read",
            vec![r(row.clone(), false), r(Type::UInt64, false)],
        ),
        (
            "cfa_write",
            vec![r(row.clone(), true), r(Type::UInt64, false)],
        ),
        ("cfa_address", vec![r(row, false), r(Type::Int32, false)]),
        (
            "cfa_bytes",
            vec![r(bytes.clone(), true), r(Type::UInt64, true)],
        ),
        (
            "cfa_byte_read",
            vec![r(bytes, false), r(Type::UInt64, false)],
        ),
        (
            "cfa_floats",
            vec![
                r(array(Type::Float32, 2), true),
                r(array(Type::Float64, 2), true),
            ],
        ),
        (
            "cfa_signed",
            [Type::Int8, Type::Int16, Type::Int32, Type::Int64]
                .into_iter()
                .map(|t| r(array(t, 2), true))
                .collect(),
        ),
        (
            "cfa_unsigned",
            [Type::UInt8, Type::UInt16, Type::UInt32, Type::UInt64]
                .into_iter()
                .map(|t| r(array(t, 2), true))
                .collect(),
        ),
    ]
}

#[test]
fn fixed_array_f4_c_layout_and_references_execute_in_all_backends() {
    let fixture = ScratchDir::new("fixed-array-ffi");
    let dir = fixture.path();
    let ast = crate::parser::parse(SOURCE).unwrap();
    let header = crate::ffi::c_header(&ast).unwrap();
    assert!(header.contains("int32_t matrix[2][3]"));
    assert!(header.contains("const int32_t (*arg0)[2][3]"));
    assert!(header.contains("FloatCell (*arg0)[2]"));
    std::fs::write(dir.join("bindings.h"), header).unwrap();
    std::fs::write(dir.join("fixture.c"), C_SOURCE).unwrap();
    let library_path = dir.join("fixture.so");
    let output = Command::new("cc")
        .args([
            "-std=c11",
            "-pedantic-errors",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-fPIC",
            "-shared",
        ])
        .arg(dir.join("fixture.c"))
        .arg("-o")
        .arg(&library_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let library = Rc::new(unsafe { libloading::Library::new(&library_path) }.unwrap());
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        for (name, params) in registrations() {
            unsafe {
                vm.register_c_fn(
                    name,
                    FunctionSignature::new(params, Type::Int32),
                    *library.get::<*const u8>(name.as_bytes()).unwrap(),
                    Some(library.clone()),
                )
            }
            .unwrap();
        }
        vm.load_source(SOURCE).unwrap_or_else(|e| {
            panic!(
                "{mode:?}: {e}: {:?}",
                e.span().and_then(|s| s.slice(SOURCE))
            )
        });
        for _ in 0..2 {
            for name in CASES {
                assert_eq!(vm.call_int32_0(name).unwrap(), 42, "{mode:?}: {name}");
                crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
            }
        }
        if mode == VmMode::Interpreter {
            vm.set_interpreter_limits(crate::vm::InterpreterLimits {
                poll_budget: Some(1),
                ..Default::default()
            })
            .unwrap();
            let mut task = vm.call_async("ffi_array_task", &[]).unwrap();
            let mut instructions = 1;
            while !task.is_finished() {
                assert!(instructions < 10_000);
                task.resume().unwrap();
                instructions += 1;
            }
            assert_eq!(task.cps_result(), Some(42));
            drop(task);
            crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
            for budget in [1, 8, 32, 128, instructions - 1] {
                vm.set_interpreter_limits(crate::vm::InterpreterLimits {
                    poll_budget: Some(budget),
                    ..Default::default()
                })
                .unwrap();
                drop(vm.call_async("ffi_array_task", &[]).unwrap());
                crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
            }
        }
    }
    let mut aot = AotCompiler::with_symbol_prefix("fixed-array-ffi", "test_").unwrap();
    aot.compile_module(&ast).unwrap();
    let object = dir.join("program.o");
    aot.write_object(&object).unwrap();
    let mut main = String::from("#include <stdint.h>\n");
    for name in CASES {
        main.push_str(&format!("extern int32_t test_{name}(void);\n"));
    }
    main.push_str("int main(void) {\n");
    for (index, name) in CASES.iter().enumerate() {
        main.push_str(&format!("if(test_{name}()!=42) return {};\n", index + 1));
    }
    main.push_str("return 0; }\n");
    std::fs::write(dir.join("main.c"), main).unwrap();
    let executable = dir.join("program");
    link(
        link_runtime(
            Command::new("cc")
                .args([
                    "-std=c11",
                    "-pedantic-errors",
                    "-Wall",
                    "-Wextra",
                    "-Werror",
                ])
                .arg(dir.join("main.c"))
                .arg(dir.join("fixture.c"))
                .arg(&object),
        ),
        &executable,
    );
    let output = Command::new(executable).output().unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn fixed_array_f4_rejects_incompatible_c_boundaries() {
    use crate::core::{prepared::PreparedModule, resolver::BindingRegistry};
    let prefix = "import joky/fixed_array.{type FixedArray} type Row=FixedArray<Int32,4> #repr(C) struct Packet{data:Row} ";
    assert!(crate::parser::parse(&format!(
        "{prefix} unsafe extern \"C\" fn bad(x:&Row) @suspends"
    ))
    .is_err());
    let mut bodies:Vec<String> = [
        "unsafe extern \"C\" fn bad(x:Row)",
        "unsafe extern \"C\" fn bad()->Row",
        "unsafe extern \"C\" fn bad(x:Packet)",
        "unsafe extern \"C\" fn bad()->Packet",
        "extern fn bad(x:&Row) @suspends",
        "extern fn bad(x:&Packet) @suspends",
        "extern fn bad(x:Row)",
        "unsafe extern \"C\" fn bad(x:&FixedArray<Int32,3>) fn f(){let a=fixed_array.repeat::<Int32,4>(1); unsafe{bad(&a)}}",
        "unsafe extern \"C\" fn bad(x:&FixedArray<FixedArray<Int32,2>,3>) fn f(){let a=fixed_array.repeat::<FixedArray<Int32,3>,2>(fixed_array.repeat::<Int32,3>(1)); unsafe{bad(&a)}}",
        "unsafe extern \"C\" fn bad(x:&mut Row) fn f(){let a=fixed_array.repeat::<Int32,4>(1); unsafe{bad(&a)}}",
        "unsafe extern \"C\" fn bad(x:&mut Row) fn f(){var a=fixed_array.repeat::<Int32,4>(1); let r=&a; unsafe{bad(&mut a)}; r.len();}",
        "unsafe extern \"C\" fn bad(x:&mut Row,y:&Int32) fn f(){var a=fixed_array.repeat::<Int32,4>(1); unsafe{bad(&mut a,a.borrow_ref(0))}}",
    ].into_iter().map(str::to_owned).collect();
    for element in [
        "Bool",
        "Rune",
        "Unit",
        "String",
        "Ptr",
        "&Int32",
        "(Int32,Int32)",
        "Option<Int32>",
        "FixedArray<Int32,0>",
    ] {
        bodies.push(format!(
            "unsafe extern \"C\" fn bad(x:&FixedArray<{element},2>)"
        ));
        bodies.push(format!(
            "#repr(C) struct Bad{{data:FixedArray<{element},2>}}"
        ));
    }
    bodies.extend([
        "unsafe extern \"C\" fn bad(x:&FixedArray<Int32,0>)".into(),
        "#repr(C) struct Bad{data:FixedArray<Int32,0>}".into(),
        "struct Plain{x:Int32} unsafe extern \"C\" fn bad(x:&FixedArray<Plain,2>)".into(),
        "#repr(C) struct Generic<T>{data:T}".into(),
    ]);
    for body in bodies {
        let source = format!("{prefix}{body}");
        let ast = crate::parser::parse(&source).unwrap_or_else(|e| panic!("{body}: {e}"));
        let result = PreparedModule::new(
            &ast,
            &crate::TypeRegistry::default(),
            &BindingRegistry::default(),
        )
        .and_then(|p| p.plan(&Default::default()));
        assert!(result.is_err(), "accepted {body}");
        assert!(
            AotCompiler::new("invalid-array-ffi")
                .unwrap()
                .compile_module(&ast)
                .is_err(),
            "AOT accepted {body}"
        );
    }
}

#[test]
fn fixed_array_f4_registration_checks_exact_resolved_shape_and_permissions() {
    // Mismatched registrations must fail before any call; this entry is never invoked.
    extern "C" fn unused(_pointer: *const u8) -> i32 {
        panic!("invalid registration was called")
    }
    let source="import joky/fixed_array.{type FixedArray} #repr(C) struct Cell{x:Float64} #repr(C) struct Other{x:Float64} unsafe extern \"C\" fn probe(x:&FixedArray<FixedArray<Cell,3>,2>)->Int32 fn f()->Int32{42}";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        for (element, outer, inner, mutable, accepted) in [
            ("Cell", 2, 3, false, true),
            ("Cell", 3, 2, false, false),
            ("Cell", 2, 4, false, false),
            ("Cell", 2, 3, true, false),
            ("Other", 2, 3, false, false),
            ("Missing", 2, 3, false, false),
        ] {
            let signature = FunctionSignature::new(
                vec![Type::reference(
                    array(array(Type::Named(element.into()), inner), outer),
                    mutable,
                )],
                Type::Int32,
            );
            unsafe { vm.register_c_fn("probe", signature, unused as *const u8, None) }.unwrap();
            let result = vm.load_source(source);
            assert_eq!(
                result.is_ok(),
                accepted,
                "{mode:?}: {element} {outer} {inner} {mutable}: {result:?}"
            );
            assert_eq!(vm.call_int32_0("f").unwrap(), 42);
        }
        for ty in [
            array(Type::Int32, 0),
            array(Type::Bool, 2),
            array(Type::Unit, 2),
            array(Type::Rune, 2),
            array(Type::Ptr, 2),
            array(Type::Int32, 65537),
        ] {
            assert!(unsafe {
                vm.register_c_fn(
                    "probe",
                    FunctionSignature::new(vec![Type::reference(ty, false)], Type::Int32),
                    unused as *const u8,
                    None,
                )
            }
            .is_err());
        }
        let signature = FunctionSignature::new(
            vec![Type::reference(array(Type::Int32, 4), false)],
            Type::Int32,
        );
        assert!(
            unsafe { vm.register_fn("legacy", signature.clone(), unused as *const u8) }.is_err()
        );
        assert!(crate::core::ffi::validate_borrowing_registration(&signature).is_err());
        // F4 does not incidentally expand the existing scalar result matrix.
        for ret in [Type::Float32, Type::Float64] {
            assert!(unsafe {
                vm.register_c_fn(
                    "float_result",
                    FunctionSignature::new(signature.params.clone(), ret),
                    unused as *const u8,
                    None,
                )
            }
            .is_err());
        }
    }
}
