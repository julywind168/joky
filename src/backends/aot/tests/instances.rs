//! Exercise the private instance trampoline with independently linked native code.
use super::*;

#[cfg(unix)]
#[test]
fn persistent_instances_aot_bridges_use_real_mixed_scalar_abi_and_root_layout() {
    let fixture = ScratchDir::new("persistent-instances");
    let object = fixture.path().join("instances.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("instances");
    let mut compiler = AotCompiler::with_symbol_prefix("instances", "probe_").unwrap();
    compiler
        .compile_module(
            &crate::parser::parse(include_str!(
                "../../../../tests/fixtures/persistent_instances.jk"
            ))
            .unwrap(),
        )
        .unwrap();
    let ty = &compiler.exports["make"].signature.ret;
    let size = ty.memory_layout().unwrap().size;
    let mut initialization = String::new();
    for (index, (offset, scalar)) in ty.memory_scalars().unwrap().iter().enumerate() {
        let width = scalar.memory_layout().unwrap().size * 8;
        initialization.push_str(&format!("{{ uint{width}_t value = (uint{width}_t)lanes[{index}]; memcpy(root + {offset}, &value, sizeof(value)); }}\n"));
    }
    compiler.write_object(&object).unwrap();
    let mut harness = r#"
#include <stdint.h>
#include <stddef.h>
#include <string.h>
#include "joky_runtime.h"
extern void __joky_instance_entry_probe_make(const uint64_t *, uint64_t *, void *);
extern void __joky_instance_entry_probe_advance(const uint64_t *, uint64_t *, void *);
extern void __joky_instance_entry_probe_total(const uint64_t *, uint64_t *, void *);
extern void __joky_instance_entry_probe_small(const uint64_t *, uint64_t *, void *);
extern void __joky_instance_entry_probe_fail(const uint64_t *, uint64_t *, void *);
extern void __joky_instance_entry_probe_empty(const uint64_t *, uint64_t *, void *);
extern void __joky_instance_entry_probe_empty_value(const uint64_t *, uint64_t *, void *);
extern void *__joky_arena_new(void);
extern void __joky_arena_drop(void *);
#define CALL(name, input, output) do { void *arena = __joky_arena_new(); __joky_instance_entry_probe_##name(input, output, arena); __joky_arena_drop(arena); } while (0)
static uint64_t bits(double value) { uint64_t result; memcpy(&result, &value, 8); return result; }
int main(void) {
    union { max_align_t alignment; unsigned char root[ROOT_SIZE]; } storage;
    unsigned char *root = storage.root;
    uint64_t input[] = {10, bits(0.5)}, lanes[4] = {0}, result = 0;
    CALL(make, input, lanes);
    if (joky_runtime_error_code()) return 1;
    INITIALIZE
    uint64_t args[] = {(uint64_t)(uintptr_t)root, 1, bits(0.25)};
    for (int i = 1; i <= 100; ++i) {
        CALL(advance, args, &result);
        if ((int32_t)result != 10 + i || joky_runtime_error_code()) return 2;
    }
    CALL(total, args, &result);
    if (result != bits(25.5)) return 3;
    CALL(small, args, &result);
    if ((int16_t)result != -7) return 4;
    args[1] = 0;
    CALL(fail, args, &result);
    if (!joky_runtime_error_code()) return 5;
    joky_runtime_error_clear();
    CALL(empty, input, lanes);
    args[2] = UINT64_C(0x80000000);
    CALL(empty_value, args, &result);
    if (result != UINT64_C(0x80000000) || joky_runtime_error_code()) return 6;
    return 0;
}
"#
    .replace("ROOT_SIZE", &size.to_string());
    harness = harness.replace("INITIALIZE", &initialization);
    std::fs::write(&host, harness).unwrap();
    link(
        link_runtime(c_compiler().arg(&host).arg(&object)),
        &executable,
    );
    let status = Command::new(executable).status().unwrap();
    assert!(status.success(), "instance bridges: {status}");
}

/// Build ABI-correct forwarding bodies from the compiler's helper signatures.
/// The loaded object shares ownership accounting and errors with this process.
#[cfg(unix)]
pub(super) fn shared_runtime_object(
    module: &cranelift_object::ObjectModule,
    symbols: &std::collections::HashMap<String, *const u8>,
) -> Vec<u8> {
    use cranelift_codegen::ir::InstBuilder;
    use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
    use cranelift_module::{Linkage, Module};
    let mut forwarding = AotCompiler::new("instance_runtime").unwrap().module;
    for (_, declaration) in module.declarations().get_functions() {
        if declaration.linkage != Linkage::Import {
            continue;
        }
        let name = declaration.name.as_ref().unwrap();
        let Some(&address) = symbols.get(name) else {
            continue;
        };
        let signature = declaration.signature.clone();
        let id = forwarding
            .declare_function(name, Linkage::Export, &signature)
            .unwrap();
        let mut context = forwarding.make_context();
        context.func.signature = signature.clone();
        let mut frontend = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut context.func, &mut frontend);
        let block = builder.create_block();
        builder.append_block_params_for_function_params(block);
        builder.switch_to_block(block);
        builder.seal_block(block);
        let args = builder.block_params(block).to_vec();
        let target = builder.ins().iconst(
            forwarding.target_config().pointer_type(),
            address as usize as i64,
        );
        let signature = builder.import_signature(signature);
        let call = builder.ins().call_indirect(signature, target, &args);
        let results = builder.inst_results(call).to_vec();
        builder.ins().return_(&results);
        builder.finalize(forwarding.target_config());
        forwarding.define_function(id, &mut context).unwrap();
    }
    forwarding.finish().emit().unwrap()
}

#[cfg(unix)]
#[test]
fn persistent_instances_aot_owning_roots_use_vm_transfer_and_drop_protocol() {
    owning_roots(false);
}

#[cfg(unix)]
#[test]
fn persistent_instances_aot_containers_use_vm_transfer_and_drop_protocol() {
    owning_roots(true);
}

#[cfg(unix)]
fn owning_roots(containers: bool) {
    use crate::{
        backends::jit::{CompiledFunction, CompiledModule},
        runtime::{arena::LIVE, cleanup::Callback},
        vm::{InstanceStatus, InstanceValue as V, VmEngine},
    };
    use cranelift_codegen::ir::{AbiParam, InstBuilder, MemFlagsData};
    use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
    use cranelift_module::{Linkage, Module};
    use std::{cell::RefCell, collections::HashMap, rc::Rc};
    thread_local! { static EVENTS: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) }; }
    extern "C" fn record_drop(value: i32) {
        EVENTS.with(|events| events.borrow_mut().push(value));
    }
    let fixture = ScratchDir::new("persistent-aot-owners");
    let object = fixture.path().join("owners.o");
    let bridge = fixture.path().join("runtime.o");
    let library_path = fixture.path().join(if cfg!(target_os = "macos") {
        "owners.dylib"
    } else {
        "owners.so"
    });
    let mut compiler = AotCompiler::with_symbol_prefix("owners", "probe_").unwrap();
    let source = if containers {
        crate::vm::tests::instance_container_tests::SOURCE
    } else {
        include_str!("../../../../tests/fixtures/persistent_owners.jk")
    };
    let ast = crate::parser::parse(source).unwrap();
    let prepared = crate::core::prepared::PreparedModule::new(
        &ast,
        &compiler.types,
        compiler.registry.metadata(),
    )
    .unwrap();
    let imports = compiler
        .registry
        .import_layouts(prepared.ast(), &HashMap::new())
        .unwrap();
    let plan = prepared.plan(&imports).unwrap();
    let definitions = crate::backends::native::compile(
        &mut compiler.module,
        &plan,
        "probe_",
        prepared.bindings(),
    )
    .unwrap();
    // Test-only relocation accessors expose compiler-authored Drop addresses.
    let mut accessors = HashMap::new();
    for (index, (name, hooks)) in definitions.instance_hooks.iter().enumerate() {
        let symbol = format!("test_hooks_{index}");
        let ptr = compiler.module.target_config().pointer_type();
        let mut context = compiler.module.make_context();
        context.func.signature.params.push(AbiParam::new(ptr));
        let id = compiler
            .module
            .declare_function(&symbol, Linkage::Export, &context.func.signature)
            .unwrap();
        let mut frontend = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut context.func, &mut frontend);
        let block = builder.create_block();
        builder.append_block_params_for_function_params(block);
        builder.switch_to_block(block);
        builder.seal_block(block);
        let output = builder.block_params(block)[0];
        for (lane, (_, function)) in hooks.iter().enumerate() {
            let function = compiler
                .module
                .declare_func_in_func(*function, builder.func);
            let address = builder.ins().func_addr(ptr, function);
            builder.ins().store(
                MemFlagsData::trusted(),
                address,
                output,
                (lane * std::mem::size_of::<usize>()) as i32,
            );
        }
        builder.ins().return_(&[]);
        builder.finalize(compiler.module.target_config());
        compiler.module.define_function(id, &mut context).unwrap();
        accessors.insert(name.clone(), symbol);
    }
    let mut symbols: HashMap<_, _> = crate::runtime::buffer::symbols()
        .into_iter()
        .chain(crate::runtime::arena::symbols())
        .chain(crate::runtime::cleanup::symbols())
        .chain(crate::runtime::error::symbols())
        .chain(crate::runtime::owned_text::symbols())
        .chain(crate::runtime::cps::symbols())
        .chain(crate::runtime::float_env::symbols())
        .map(|(name, address)| (name.to_owned(), address))
        .collect();
    symbols.insert("probe_record_drop".into(), record_drop as *const u8);
    std::fs::write(&bridge, shared_runtime_object(&compiler.module, &symbols)).unwrap();
    std::fs::write(&object, compiler.module.finish().emit().unwrap()).unwrap();
    let output = Command::new("cc")
        .arg(if cfg!(target_os = "macos") {
            "-dynamiclib"
        } else {
            "-shared"
        })
        .arg(&object)
        .arg(&bridge)
        .arg("-o")
        .arg(&library_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let library = Rc::new(unsafe { libloading::Library::new(&library_path).unwrap() });
    let entries = definitions
        .instance_entries
        .into_iter()
        .map(|(name, (_, info))| {
            let entry = unsafe {
                *library
                    .get::<*const u8>(crate::core::instance::symbol("probe_", &name).as_bytes())
                    .unwrap()
            };
            (
                name,
                CompiledFunction {
                    info,
                    entry,
                    abi: crate::EntryAbi::Scalar,
                },
            )
        })
        .collect();
    let hooks = definitions
        .instance_hooks
        .into_iter()
        .map(|(name, hooks)| {
            let mut addresses = vec![0usize; hooks.len()];
            unsafe {
                library
                    .get::<unsafe extern "C" fn(*mut usize)>(accessors[&name].as_bytes())
                    .unwrap()(addresses.as_mut_ptr());
            }
            (
                name,
                hooks
                    .into_iter()
                    .zip(addresses)
                    .map(|((hook, _), address)| {
                        (hook, unsafe {
                            std::mem::transmute::<usize, Callback>(address)
                        })
                    })
                    .collect(),
            )
        })
        .collect();
    let weak = Rc::downgrade(&library);
    let compiled = CompiledModule::instance_test_module(
        prepared.ast().sources.clone(),
        entries,
        hooks,
        library.clone(),
    );
    let mut vm = VmEngine::with_instance_test_module(compiled);
    drop(library);
    EVENTS.with(|events| events.borrow_mut().clear());
    if containers {
        let check_events = |expected: &[i32]| {
            EVENTS.with(|events| {
                let mut actual = std::mem::take(&mut *events.borrow_mut());
                let mut expected = expected.to_vec();
                actual.sort();
                expected.sort();
                assert_eq!(actual, expected);
            })
        };
        crate::vm::tests::instance_container_tests::exercise(&vm, check_events);
        crate::vm::tests::instance_container_tests::transfer_failures(&vm, check_events);
        let old = vm
            .create_instance("make_populated_containers", &[])
            .unwrap();
        vm.load_source("fn unrelated() -> Int32 { 42 }").unwrap();
        assert!(weak.upgrade().is_some());
        assert_eq!(
            vm.call_instance(old, "container_count", &[]).unwrap(),
            V::UInt64(3)
        );
        vm.close_instance(old).unwrap();
        check_events(&[0, 1, 2]);
        assert!(weak.upgrade().is_none());
        clean_aot(&vm);
        return;
    }
    assert!(vm.create_instance("failed_construct", &[]).is_err());
    assert_eq!(vm.instance_bytes(), 0);
    assert_eq!(vm.instance_peak_bytes(), 0);
    EVENTS.with(|events| {
        assert_eq!(*events.borrow(), [-1, 3, 4, 5, 1, 2]);
        events.borrow_mut().clear();
    });
    let id = vm.create_instance("make_owner", &[]).unwrap();
    let bytes = vm.instance_bytes();
    let allocations = LIVE.with(std::cell::Cell::get);
    for value in 100..200 {
        assert_eq!(
            vm.call_instance(id, "advance_owner", &[V::Int32(value)])
                .unwrap(),
            V::Int32(value - 99)
        );
        assert_eq!(vm.instance_bytes(), bytes);
        LIVE.with(|n| assert_eq!(n.get(), allocations));
    }
    vm.close_instance(id).unwrap();
    EVENTS.with(|events| {
        let values = events.borrow();
        assert_eq!(&values[..3], &[-1, 1, -2]);
        assert_eq!(&values[values.len() - 2..], &[199, 2]);
    });
    assert_eq!(vm.instance_bytes(), 0);
    let id = vm.create_instance("make_owner", &[]).unwrap();
    assert!(vm.call_instance(id, "owner_fail", &[]).is_err());
    assert_eq!(vm.instance_status(id).unwrap(), InstanceStatus::Failed);
    vm.close_instance(id).unwrap();
    let broken = vm.create_instance("make_broken", &[]).unwrap();
    let generic = vm.create_instance("make_generic", &[]).unwrap();
    vm.load_source("fn unrelated() -> Int32 { 42 }").unwrap();
    assert!(vm.close_instance(broken).is_err());
    assert!(weak.upgrade().is_some());
    assert_eq!(
        vm.call_instance(generic, "generic_touch", &[]).unwrap(),
        V::UInt64(1)
    );
    drop(vm);
    assert!(weak.upgrade().is_none());
    LIVE.with(|n| assert_eq!(n.get(), 0));
}

#[cfg(unix)]
fn clean_aot(vm: &crate::vm::VmEngine) {
    assert_eq!(vm.instance_bytes(), 0);
    assert_eq!(vm.instance_peak_bytes(), 0);
    crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
}

#[cfg(unix)]
#[test]
fn state_domains_aot_execute_checked_fifo_turns_and_isolate_failures() {
    use crate::{
        backends::jit::{CompiledFunction, CompiledModule},
        vm::{tests::state_domain_tests, VmEngine},
    };
    use std::rc::Rc;
    let fixture = ScratchDir::new("state-domains-aot");
    let object = fixture.path().join("domains.o");
    let bridge = fixture.path().join("runtime.o");
    let path = fixture.path().join(if cfg!(target_os = "macos") {
        "domains.dylib"
    } else {
        "domains.so"
    });
    let mut compiler = AotCompiler::with_symbol_prefix("domains", "probe_").unwrap();
    let ast = crate::parser::parse(state_domain_tests::SOURCE).unwrap();
    let prepared = crate::core::prepared::PreparedModule::new(
        &ast,
        &compiler.types,
        compiler.registry.metadata(),
    )
    .unwrap();
    let imports = compiler
        .registry
        .import_layouts(prepared.ast(), &HashMap::new())
        .unwrap();
    let plan = prepared.plan(&imports).unwrap();
    let definitions = crate::backends::native::compile(
        &mut compiler.module,
        &plan,
        "probe_",
        prepared.bindings(),
    )
    .unwrap();
    let symbols = crate::runtime::buffer::symbols()
        .into_iter()
        .chain(crate::runtime::arena::symbols())
        .chain(crate::runtime::cleanup::symbols())
        .chain(crate::runtime::error::symbols())
        .chain(crate::runtime::owned_text::symbols())
        .chain(crate::runtime::cps::symbols())
        .chain(crate::runtime::float_env::symbols())
        .chain([
            (
                "__joky_domain_poll_v3",
                crate::runtime::domain::poll_native as *const u8,
            ),
            (
                "__joky_domain_read_i32",
                crate::stdlib::domain::read as *const u8,
            ),
            (
                "__joky_domain_write_i32",
                crate::stdlib::domain::write as *const u8,
            ),
            (
                "joky_task_sleep_ms",
                crate::stdlib::task::sleep_ms as *const u8,
            ),
            (
                "joky_task_yield_now",
                crate::stdlib::task::yield_now as *const u8,
            ),
        ])
        .map(|(name, address)| (name.to_owned(), address))
        .collect();
    std::fs::write(&bridge, shared_runtime_object(&compiler.module, &symbols)).unwrap();
    std::fs::write(&object, compiler.module.finish().emit().unwrap()).unwrap();
    let output = Command::new("cc")
        .arg(if cfg!(target_os = "macos") {
            "-dynamiclib"
        } else {
            "-shared"
        })
        .arg(&object)
        .arg(&bridge)
        .arg("-o")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let library = Rc::new(unsafe { libloading::Library::new(&path).unwrap() });
    let entries = definitions
        .instance_entries
        .into_iter()
        .map(|(name, (_, info))| {
            let entry = unsafe {
                *library
                    .get::<*const u8>(crate::core::instance::symbol("probe_", &name).as_bytes())
                    .unwrap()
            };
            (
                name,
                CompiledFunction {
                    info,
                    entry,
                    abi: crate::EntryAbi::Scalar,
                },
            )
        })
        .collect();
    let exports = definitions
        .exports
        .into_iter()
        .filter(|(_, (_, info))| info.suspends)
        .map(|(name, (_, info))| {
            let entry = unsafe {
                *library
                    .get::<*const u8>(format!("probe_{name}").as_bytes())
                    .unwrap()
            };
            (
                name,
                CompiledFunction {
                    info,
                    entry,
                    abi: crate::EntryAbi::Scalar,
                },
            )
        })
        .collect();
    let mut module = CompiledModule::instance_test_module(
        prepared.ast().sources.clone(),
        entries,
        HashMap::new(),
        library,
    );
    module.exports = exports;
    let mut vm = VmEngine::with_instance_test_module(module);
    state_domain_tests::exercise(&mut vm);
    state_domain_tests::exercise_tasks(&mut vm);
}
