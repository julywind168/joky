use super::{VmEngine, VmMode};
use crate::{runtime::text_ffi::*, text_ffi_model as model, ErrorCode, FunctionSignature, Type};
use std::rc::Rc;

use crate::backends::aot::tests::ScratchDir;

struct Fixture {
    library: Rc<libloading::Library>,
    _dir: ScratchDir,
}
impl Fixture {
    fn new() -> Self {
        let dir = ScratchDir::new("text-ffi");
        let c = dir.path().join("host.c");
        let library = dir.path().join("host.so");
        std::fs::write(&c, model::C_SOURCE).unwrap();
        let result = std::process::Command::new("cc")
            .args([
                "-std=c11",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-shared",
                "-DTEXT_FFI_VM_TEST",
                "-fPIC",
                "-I",
            ])
            .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include"))
            .arg(c)
            .arg("-o")
            .arg(&library)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let library = Rc::new(unsafe { libloading::Library::new(library).unwrap() });
        unsafe {
            library
                .get::<unsafe extern "C" fn(extern "C" fn(usize, usize))>(b"text_ffi_bind_error")
                .unwrap()(crate::runtime::error::division_by_zero);
        }
        Self { library, _dir: dir }
    }
    fn host(&self, index: usize) -> TextFfiHostV1 {
        unsafe {
            *self
                .library
                .get::<unsafe extern "C" fn(usize) -> *const TextFfiHostV1>(b"text_ffi_host")
                .unwrap()(index)
        }
    }
    fn reset(&self) {
        unsafe {
            self.library
                .get::<unsafe extern "C" fn()>(b"text_ffi_reset")
                .unwrap()()
        }
    }
    fn counts(&self) -> (i32, i32) {
        unsafe {
            (
                self.library
                    .get::<unsafe extern "C" fn() -> i32>(b"text_ffi_calls")
                    .unwrap()(),
                self.library
                    .get::<unsafe extern "C" fn() -> i32>(b"text_ffi_frees")
                    .unwrap()(),
            )
        }
    }
    fn register(&self, vm: &mut VmEngine) {
        for (index, (name, signature)) in model::signatures().into_iter().enumerate() {
            unsafe {
                vm.register_text_ffi_fn(
                    &name,
                    signature,
                    self.host(index),
                    Some(self.library.clone()),
                )
                .unwrap();
            }
        }
    }
}

#[test]
fn text_ffi_c_fixture_copies_and_releases_across_vm_backends() {
    let fixture = Fixture::new();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        fixture.register(&mut vm);
        vm.load_source(model::SOURCE).unwrap();
        for &(name, expected, frees) in model::TEXTS {
            fixture.reset();
            assert_eq!(
                vm.call_string_0(name).unwrap(),
                expected,
                "{mode:?}: {name}"
            );
            assert_eq!(fixture.counts(), (1, frees), "{name}");
            crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
        }
        for &(name, code, calls, frees) in model::FAILURES {
            fixture.reset();
            assert_eq!(
                vm.call_string_0(name).unwrap_err().code(),
                code,
                "{mode:?}: {name}"
            );
            assert_eq!(fixture.counts(), (calls, frees), "{name}");
            crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
        }
        fixture.reset();
        assert_eq!(
            vm.call_int32_0("partial_input_failure").unwrap_err().code(),
            ErrorCode::InteriorNul
        );
        assert_eq!(fixture.counts(), (0, 0));
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
        assert_eq!(vm.call_int32_0("length_text").unwrap(), 12);
        assert_eq!(vm.call_int32_0("scalar_and_unit").unwrap(), 42);
        assert_eq!(vm.call_int32_0("four_texts").unwrap(), 15);
    }
}

#[test]
fn text_ffi_rejects_unsupported_descriptors_and_declarations() {
    let fixture = Fixture::new();
    let signature = model::signatures()[0].1.clone();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        let good = fixture.host(0);
        for bad in [
            TextFfiHostV1 {
                abi_version: 2,
                ..good
            },
            TextFfiHostV1 { flags: 1, ..good },
            TextFfiHostV1 { call: None, ..good },
            TextFfiHostV1 {
                input_count: 5,
                ..good
            },
            TextFfiHostV1 {
                output_limit: u64::MAX,
                ..good
            },
            TextFfiHostV1 {
                input_kinds: [2, 0, 0, 0],
                ..good
            },
            TextFfiHostV1 {
                output_kind: TEXT_SCALAR,
                output_limit: 0,
                ..good
            },
        ] {
            assert_eq!(
                unsafe { vm.register_text_ffi_fn("span", signature.clone(), bad, None) }
                    .unwrap_err()
                    .code(),
                ErrorCode::AbiError
            );
        }
        assert!(unsafe {
            vm.register_text_ffi_fn(
                "span",
                FunctionSignature::new(vec![Type::Int32], Type::String),
                good,
                None,
            )
        }
        .is_err());
        fixture.register(&mut vm);
        for source in [
            "extern fn span(s: String) @suspends -> String fn run() @suspends -> String { span(\"x\") }",
            "extern fn span(s: &String) -> Int32 fn run() -> Int32 { let s = \"x\"; span(&s) }",
            "unsafe extern \"C\" fn span(s: &String) -> String fn run() -> String { let s = \"x\"; unsafe { span(&s) } }",
        ] { let ast = crate::parser::parse(source).unwrap(); assert!(vm.load_module(&ast).is_err(), "{source}"); }
        vm.load_source(model::SOURCE).unwrap();
        assert_eq!(vm.call_string_0("span_text").unwrap(), "中🙂\0tail");
    }
}

#[test]
fn text_ffi_temporary_budget_is_refunded_and_replacement_keeps_old_code_alive() {
    let fixture = Fixture::new();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        fixture.register(&mut vm);
        let source = format!("{} fn repeat() -> Int32 {{ let s = \"abc\"; var i = 0; while i < 1000 {{ let t = cstring(&s); t.release(); i = i + 1; }}; i }}", model::SOURCE);
        vm.load_source(&source).unwrap();
        struct Restore(usize);
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::runtime::arena::LIMIT.with(|n| n.set(self.0));
            }
        }
        {
            let _restore = Restore(crate::runtime::arena::LIMIT.with(|n| n.replace(4096)));
            assert_eq!(vm.call_int32_0("repeat").unwrap(), 1000);
        }
        // Replacement must not mutate the descriptor referenced by compiled code.
        let mut replacement = fixture.host(0);
        replacement.input_limits[0] = 0;
        unsafe {
            vm.register_text_ffi_fn(
                "span",
                model::signatures()[0].1.clone(),
                replacement,
                Some(fixture.library.clone()),
            )
            .unwrap();
        }
        assert_eq!(vm.call_string_0("span_text").unwrap(), "中🙂\0tail");
        vm.load_source(model::SOURCE).unwrap();
        assert_eq!(
            vm.call_string_0("span_text").unwrap_err().code(),
            ErrorCode::ResourceLimit
        );
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}
