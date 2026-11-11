use super::{c_compiler, link, link_runtime, AotCompiler, ScratchDir};
use crate::{
    backends::native,
    core::prepared::PreparedModule,
    runtime::error,
    vm::{VmEngine, VmMode},
    ErrorCode, TypeRegistry,
};
use std::{
    collections::HashMap,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

const SOURCE: &str = include_str!("../../../../tests/fixtures/native_stack_strict.jk");
static HOST_CALLS: AtomicUsize = AtomicUsize::new(0);
extern "C" fn unknown(_: i32) -> i32 {
    HOST_CALLS.fetch_add(1, Ordering::Relaxed);
    0
}

#[test]
fn strict_native_stack_checks_targets_before_owned_transfer() {
    const CHILD: &str = "JOKY_STRICT_STACK_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let name = std::thread::current().name().unwrap().to_owned();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &name, "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        return;
    }
    // Exercise ordinary public loading independently of the malformed-value probe.
    for mode in [VmMode::Interpreter, VmMode::Jit] {
        let mut vm = VmEngine::with_mode(mode);
        if mode == VmMode::Jit {
            vm.set_strict_native_stack(true).unwrap();
        } else {
            assert!(vm.set_strict_native_stack(true).is_err());
        }
        vm.load_source(SOURCE)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(vm.call_int32_0("indirect_run").unwrap(), 42);
        assert_eq!(vm.call_int32_0("dyn_run").unwrap(), 42);
        assert_eq!(vm.call_int32_1("owned_run", 3).unwrap(), 42);
    }
    let ast = crate::parser::parse(SOURCE).unwrap();
    let types = TypeRegistry::default().with_prelude(false);
    let prepared = PreparedModule::new(&ast, &types, &Default::default()).unwrap();
    let mut plan = prepared.plan(&HashMap::new()).unwrap();
    // Function values have no public host ABI. Inject a malformed value into a
    // checked IR slot to test the generated guard, without adding an unsafe
    // source cast or a test-only runtime admission bypass. Other functions keep
    // the original FunctionRef/DynTable evidence for the admitted target set.
    for name in ["scalar_probe", "owned_probe", "dyn_probe"] {
        let mut replaced = false;
        for op in &mut plan.bodies.get_mut(name).unwrap().ops {
            let dst = match op {
                crate::core::ir::Op::FunctionRef(dst, _)
                | crate::core::ir::Op::DynMethod { dst, .. } => Some(dst.clone()),
                _ => None,
            };
            if let Some(dst) = dst {
                *op = crate::core::ir::Op::Copy(
                    dst,
                    crate::core::ir::Slot {
                        index: 0,
                        ty: crate::Type::UInt64,
                    },
                );
                replaced = true;
            }
        }
        assert!(replaced);
    }
    let mut builder =
        cranelift_jit::JITBuilder::new(cranelift_module::default_libcall_names()).unwrap();
    for (name, address) in crate::runtime::buffer::symbols()
        .into_iter()
        .chain(crate::runtime::arena::symbols())
        .chain(crate::runtime::cleanup::symbols())
        .chain(crate::runtime::error::symbols())
        .chain(crate::runtime::float_env::symbols())
        .chain(crate::runtime::owned_text::symbols())
        .chain(crate::runtime::place::symbols())
    {
        builder.symbol(name, address);
    }
    let mut module = cranelift_jit::JITModule::new(builder);
    let definitions = native::compile(&mut module, &plan, "", prepared.bindings(), true).unwrap();
    module.finalize_definitions().unwrap();
    let entries: HashMap<_, _> = definitions
        .exports
        .iter()
        .map(|(name, (id, _))| (name.as_str(), module.get_finalized_function(*id)))
        .collect();
    type Invoke = unsafe extern "C" fn(usize) -> i32;
    for name in ["scalar_probe", "owned_probe", "dyn_probe"] {
        let invoke: Invoke = unsafe { std::mem::transmute(entries[name]) };
        for target in [unknown as *const () as usize, 1] {
            let failure = error::checked(|| unsafe { invoke(target) }).unwrap_err();
            assert_eq!(failure.code(), ErrorCode::InvalidCall);
            assert!(failure.message().contains("outside strict stack admission"));
            assert!(failure.span().unwrap().slice(SOURCE).is_some());
            crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
        }
    }
    // A public C adapter is not the internal language function-value ABI.
    let invoke: Invoke = unsafe { std::mem::transmute(entries["scalar_probe"]) };
    assert_eq!(
        error::checked(|| unsafe { invoke(entries["increment"] as usize) })
            .unwrap_err()
            .code(),
        ErrorCode::InvalidCall
    );
    let ordinary: unsafe extern "C" fn() -> i32 =
        unsafe { std::mem::transmute(entries["indirect_run"]) };
    assert_eq!(error::checked(|| unsafe { ordinary() }).unwrap(), 42);
    assert_eq!(HOST_CALLS.load(Ordering::Relaxed), 0);
    let failure: unsafe extern "C" fn(i32) -> i32 =
        unsafe { std::mem::transmute(entries["owned_failure"]) };
    assert_eq!(
        error::checked(|| unsafe { failure(0) }).unwrap_err().code(),
        ErrorCode::RuntimeError
    );
    crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
    assert_eq!(error::checked(|| unsafe { failure(1) }).unwrap(), 42);
    let owned: unsafe extern "C" fn(i32) -> i32 =
        unsafe { std::mem::transmute(entries["owned_run"]) };
    std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(move || {
            for depth in [3, 10_000_000, 3] {
                let result = error::checked(|| unsafe { owned(depth) });
                if depth == 3 {
                    assert_eq!(result.unwrap(), 42);
                } else {
                    assert_eq!(result.unwrap_err().code(), ErrorCode::ResourceLimit);
                }
                crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
            }
        })
        .unwrap()
        .join()
        .unwrap();

    let mut compiler = AotCompiler::new("strict-stack").unwrap();
    compiler.set_strict_native_stack(true).unwrap();
    compiler.set_prelude_enabled(false).unwrap();
    native::compile(&mut compiler.module, &plan, "", prepared.bindings(), true).unwrap();
    let tmp = ScratchDir::new("strict-stack");
    let object = tmp.path().join("strict.o");
    let host = tmp.path().join("host.c");
    let program = tmp.path().join("strict");
    std::fs::write(&object, compiler.module.finish().emit().unwrap()).unwrap();
    std::fs::write(&host, r#"
#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>
#include "joky_runtime.h"
extern int32_t scalar_probe(uint64_t), owned_probe(uint64_t), dyn_probe(uint64_t), increment(int32_t);
extern int32_t indirect_run(void), dyn_run(void), owned_run(int32_t), owned_failure(int32_t);
static int calls;
static int32_t unknown(int32_t n) { ++calls; return n; }
static void *run(void *unused) {
    (void)unused;
    uint64_t hostile_cr = (1u << 24) | (1u << 22), hostile_sr = 16;
    __asm__ volatile("msr fpcr, %0" :: "r"(hostile_cr));
    __asm__ volatile("msr fpsr, %0" :: "r"(hostile_sr));
    if (indirect_run() != 42 || dyn_run() != 42) abort();
    int32_t (*invocations[])(uint64_t) = {scalar_probe, owned_probe, dyn_probe};
    for (int route = 0; route < 3; ++route) {
        uintptr_t targets[] = {(uintptr_t)unknown, 1};
        for (int i = 0; i < 2; ++i) {
            joky_runtime_error_clear();
            (void)invocations[route](targets[i]);
            uint64_t after_cr, after_sr;
            __asm__ volatile("mrs %0, fpcr" : "=r"(after_cr));
            __asm__ volatile("mrs %0, fpsr" : "=r"(after_sr));
            if (after_cr != hostile_cr || after_sr != hostile_sr) abort();
            if (!joky_runtime_error_code() || joky_runtime_error_end() <= joky_runtime_error_start()) abort();
            joky_runtime_print_error();
        }
    }
    joky_runtime_error_clear();
    (void)scalar_probe((uintptr_t)increment);
    if (!joky_runtime_error_code()) abort();
    joky_runtime_error_clear();
    if (indirect_run() != 42 || joky_runtime_error_code()) abort();
    int depths[] = {3, 10000000, 3};
    for (int i = 0; i < 3; ++i) {
        joky_runtime_error_clear();
        int32_t value = owned_run(depths[i]);
        if (i == 1) { if (!joky_runtime_error_code()) abort(); }
        else if (joky_runtime_error_code() || value != 42) abort();
    }
    joky_runtime_error_clear();
    (void)owned_failure(0);
    if (!joky_runtime_error_code()) abort();
    joky_runtime_error_clear();
    if (owned_failure(1) != 42 || joky_runtime_error_code() || calls) abort();
    return NULL;
}
int main(void) {
    pthread_t thread; pthread_attr_t attr;
    if (pthread_attr_init(&attr) || pthread_attr_setstacksize(&attr, 512 * 1024)) return 1;
    if (pthread_create(&thread, &attr, run, NULL)) return 2;
    pthread_attr_destroy(&attr);
    return pthread_join(thread, NULL);
}
"#).unwrap();
    link(link_runtime(c_compiler().arg(&host).arg(&object)), &program);
    let output = Command::new(program).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("outside strict stack admission"));
    unsafe {
        module.free_memory();
    }
}

#[test]
fn strict_native_stack_rejects_unbudgeted_dependencies_before_publication() {
    let mut vm = VmEngine::new();
    vm.set_strict_native_stack(true).unwrap();
    vm.load_source("fn old() -> Int32 { 42 }").unwrap();
    assert!(vm.set_strict_native_stack(false).is_err());
    let failure = vm
        .load_source("import joky/io fn run() { io.println(\"no effect\") }")
        .unwrap_err();
    assert_eq!(failure.code(), ErrorCode::InvalidCall);
    assert!(failure.message().contains("Strict native stack admission"));
    assert_eq!(vm.call_int32_0("old").unwrap(), 42);

    for source in [
        "extern fn host() fn run() { host() }",
        "extern fn factory(tag:UInt64) -> Owner fn run() { let owner = factory(7) }",
        "extern fn factory(tag:UInt64) -> Owner fn consume(owner:Owner) {} fn run() { consume(factory(7)) }",
        "extern fn factory(tag:UInt64) -> Owner struct Wrapper { owner:Owner } fn consume(owner:Wrapper) {} fn run() { consume(Wrapper { owner:factory(7) }) }",
        "fn consume(owner:Owner) {} fn nested(callback:fn(Owner) -> Unit) {} fn run() { nested(consume) }",
        "trait View { fn value(&self) @noescape -> Int32 } fn consume(value:dyn View) {} fn take(callback:fn(dyn View) -> Unit) {} fn run() { take(consume) }",
        "fn pending() @suspends {}",
        "import joky/time fn clock() { let now = time.Instant.now() }",
        "import joky/vm fn nested() { let engine = vm.Vm.new() }",
        "import joky/arena fn region() { let arena = arena.Arena.new() }",
    ] {
        let mut compiler = AotCompiler::new("strict-reject").unwrap();
        compiler.set_strict_native_stack(true).unwrap();
        compiler.register_resource_type("Owner").unwrap();
        compiler
            .register_resource_factory("factory", "Owner")
            .unwrap();
        let failure = compiler
            .compile_module(&crate::parser::parse(source).unwrap())
            .err()
            .unwrap_or_else(|| panic!("unexpected admission: {source}"));
        assert_eq!(
            failure.code(),
            ErrorCode::InvalidCall,
            "{source}: {failure}"
        );
        assert!(
            failure.message().contains("Strict native stack admission"),
            "{source}: {failure}"
        );
        assert!(failure.span().is_some());
    }
}
