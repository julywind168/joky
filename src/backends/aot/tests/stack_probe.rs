//! Ordinary-build stack admission, isolated from the parent test process.
use super::{c_compiler, link, link_runtime, AotCompiler, ScratchDir};
use crate::{
    backends::jit,
    core::resolver::BindingRegistry,
    runtime::{error, suspension::ModuleId},
    ErrorCode, Span, TypeRegistry,
};
use std::{cell::RefCell, collections::HashMap, process::Command};

type Entry = unsafe extern "C" fn(i32) -> i32;

#[derive(Default)]
struct Trace {
    entries: usize,
    drops: Vec<i32>,
}
thread_local! {
    static TRACE: RefCell<Trace> = RefCell::new(Trace::default());
}

extern "C" fn enter() {
    TRACE.with_borrow_mut(|t| t.entries += 1);
}
unsafe extern "C" fn create_owner(tag: u64, output: *mut *mut std::ffi::c_void) -> i32 {
    *output = Box::into_raw(Box::new(tag as i32)).cast();
    0
}
unsafe extern "C" fn release_owner(token: *mut std::ffi::c_void) {
    let tag = *Box::from_raw(token.cast::<i32>());
    TRACE.with_borrow_mut(|t| t.drops.push(tag));
}
static FACTORY: crate::runtime::resource::ResourceFactoryV1 =
    crate::runtime::resource::ResourceFactoryV1 {
        abi_version: 1,
        create: Some(create_owner),
        release: Some(release_owner),
    };

fn source() -> String {
    // A returned aggregate reserves a real caller-side stack slot in descend.
    let fields = vec!["Int32"; 256].join(",");
    let values = vec!["n"; 256].join(",");
    let mut source = format!(
        r#"
        import joky/fixed_array.{{type FixedArray}}
        import joky/mem
        extern fn probe_enter()
        extern fn probe_owner(tag:UInt64) -> ProbeResource
        struct Owner {{ resource:ProbeResource, text:String }}

        fn padding(n:Int32) -> ({fields}) {{ ({values}) }}
        fn probe_descend(n:Int32, owner:Owner) -> Int32 {{
            probe_enter()
            let p = padding(n)
            if n == 0 {{ return 42 / (n + 1) }}
            let next = probe_descend(n - 1, owner)
            next + p.0 - n
        }}
        fn start(n:Int32) -> Int32 {{ probe_descend(n, Owner {{ resource:probe_owner(7), text:"owned" }}) }}
        fn indirect(n:Int32, owner:Owner) -> Int32 {{
            probe_enter()
            let p = padding(n)
            if n == 0 {{ return 42 }}
            let f:fn(Int32, Owner) -> Int32 = indirect
            let next = f(n - 1, owner)
            next + p.0 - n
        }}
        fn mutual_a(n:Int32, owner:Owner) -> Int32 {{
            probe_enter()
            if n == 0 {{ return 42 }}
            mutual_b(n - 1, owner)
        }}
        fn mutual_b(n:Int32, owner:Owner) -> Int32 {{
            probe_enter()
            if n == 0 {{ return 42 }}
            mutual_a(n - 1, owner)
        }}
        trait Descend {{ fn run(&self, n:Int32) @noescape -> Int32 }}
        impl Descend for Owner {{
            fn run(&self, n:Int32) @noescape -> Int32 {{
                probe_enter()
                let p = padding(n)
                if n == 0 {{ return 42 }}
                let view:&dyn Descend = self
                let next = view.run(n - 1)
                next + p.0 - n
            }}
        }}
        fn start_indirect(n:Int32) -> Int32 {{ indirect(n, Owner {{ resource:probe_owner(7), text:"owned" }}) }}
        fn start_mutual(n:Int32) -> Int32 {{ mutual_a(n, Owner {{ resource:probe_owner(7), text:"owned" }}) }}
        fn indirect_array(n:Int32, owner:FixedArray<String, 2>) -> Int32 {{
            probe_enter()
            let p = padding(n)
            if n == 0 {{ return 42 }}
            let f:fn(Int32, FixedArray<String, 2>) -> Int32 = indirect_array
            let next = f(n - 1, owner)
            next + p.0 - n
        }}
        fn start_array(n:Int32) -> Int32 {{
            let resource = probe_owner(7)
            let owners:FixedArray<String, 2> = ["first", "second"]
            let result = indirect_array(n, owners)
            mem.drop(resource)
            result
        }}
        enum Maybe {{ Empty, Full(String) }}
        fn indirect_nullable(n:Int32, owner:Maybe) -> Int32 {{
            probe_enter()
            let p = padding(n)
            if n == 0 {{ return 42 }}
            let f:fn(Int32, Maybe) -> Int32 = indirect_nullable
            let next = f(n - 1, owner)
            next + p.0 - n
        }}
        fn start_nullable(n:Int32) -> Int32 {{
            let resource = probe_owner(7)
            let result = indirect_nullable(n, Maybe.Empty)
            mem.drop(resource)
            result
        }}
        fn start_scalar(n:Int32) -> Int32 {{
            if n == 0 {{ 42 }} else {{ start_scalar(n - 1) }}
        }}
        fn start_dyn(n:Int32) -> Int32 {{
            let value = Owner {{ resource:probe_owner(7), text:"owned" }}
            let view:&dyn Descend = &value
            view.run(n)
        }}

    "#
    );
    // Each aggregate result needs its own explicit caller slot. Together these
    // exceed the entire small execution stack, not just its admission reserve.
    source.push_str("fn oversized_body(n:Int32) -> Int32 { probe_enter()\n");
    for index in 0..256 {
        source.push_str(&format!("let p{index} = padding(n)\n"));
    }
    source.push_str(
        &(0..256)
            .map(|index| format!("p{index}.0"))
            .collect::<Vec<_>>()
            .join(" + "),
    );
    source.push_str("\n}");
    source
}

fn exercise(entry: Entry, source: &str, owns_resource: bool, rejected_entry: Option<Span>) {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn_scoped(scope, || {
                let _restore = crate::runtime::float_env::Guard::enter();
                crate::runtime::float_env::testing::hostile();
                let fp_state = crate::runtime::float_env::testing::state();
                for n in [3, 10_000_000, 3] {
                    TRACE.set(Trace::default());
                    error::joky_runtime_error_clear();
                    let value = unsafe { entry(n) };
                    let result = error::current_error().map_or(Ok(value), Err);
                    error::joky_runtime_error_clear();
                    assert_eq!(crate::runtime::float_env::testing::state(), fp_state);
                    let failed = n > 3;
                    if failed {
                        let err = result.unwrap_err();
                        assert_eq!(err.code(), ErrorCode::ResourceLimit);
                        assert!(err.span().unwrap().slice(source).is_some());
                        if let Some(span) = rejected_entry {
                            assert_eq!(
                                err.span(),
                                Some(span),
                                "must exercise the owning entry rejection"
                            );
                        }
                    } else {
                        assert_eq!(result.unwrap(), 42);
                    }
                    TRACE.with_borrow(|t| {
                        if owns_resource {
                            assert_eq!(t.drops, [7]);
                            assert!(t.entries >= 4);
                        } else {
                            assert!(t.drops.is_empty());
                        }
                    });
                    crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
                }
            })
            .unwrap()
            .join()
            .unwrap();
    });
}

#[test]
fn native_stack_sync_production() {
    // A regressed check must not take down the surrounding test runner.
    const CHILD: &str = "JOKY_STACK_PROBE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let test = std::thread::current().name().unwrap().to_owned();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &test, "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        return;
    }

    let source = source();
    let ast = crate::parser::parse(&source).unwrap();
    let hosts = [
        ("probe_enter", enter as *const u8),
        (
            "__joky_resource_factory_v1_probe_owner",
            &FACTORY as *const _ as *const u8,
        ),
    ];
    let mut types = TypeRegistry::new().with_prelude(false);
    let resource = types.register_resource("ProbeResource").unwrap();
    types.resource_factories.insert(
        "probe_owner".into(),
        crate::FunctionSignature::new(vec![crate::Type::UInt64], resource),
    );
    let compile_jit = || {
        jit::compile_with_registry(
            &ast,
            &hosts,
            &HashMap::new(),
            vec![],
            ModuleId(1),
            &BindingRegistry::default(),
            &types,
        )
        .unwrap_or_else(|error| panic!("{error}"))
    };
    let compiled = compile_jit();
    let declaration_span = |name: &str| {
        ast.items
            .iter()
            .find_map(|item| match item {
                crate::parser::ast::Item::FnDef(function) if function.name == name => {
                    Some(function.span)
                }
                _ => None,
            })
            .unwrap()
    };
    for name in [
        "start",
        "start_indirect",
        "start_mutual",
        "start_dyn",
        "start_array",
        "start_nullable",
        "start_scalar",
    ] {
        let export = &compiled.exports[name];
        exercise(
            unsafe { std::mem::transmute::<*const u8, Entry>(export.entry) },
            &source,
            name != "start_scalar",
            match name {
                "start_indirect" => Some(declaration_span("indirect")),
                "start_array" => Some(declaration_span("indirect_array")),
                "start_nullable" => Some(declaration_span("indirect_nullable")),
                _ => None,
            },
        );
    }

    let oversized: Entry = unsafe { std::mem::transmute(compiled.exports["oversized_body"].entry) };
    error::joky_runtime_error_clear();
    assert_eq!(unsafe { oversized(1) }, 256);
    assert_eq!(error::joky_runtime_error_code(), 0);
    let span = declaration_span("oversized_body");
    std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(move || {
            TRACE.set(Trace::default());
            let error = error::checked(|| unsafe { oversized(1) }).unwrap_err();
            assert_eq!(error.code(), ErrorCode::ResourceLimit);
            assert_eq!(error.span(), Some(span));
            TRACE.with_borrow(|trace| assert_eq!(trace.entries, 0, "body must not be entered"));
        })
        .unwrap()
        .join()
        .unwrap();

    let tmp = ScratchDir::new("stack-preflight");
    let compile_aot = || {
        let mut compiler = AotCompiler::new("stack-probe").unwrap();
        compiler.set_prelude_enabled(false).unwrap();
        compiler.register_resource_type("ProbeResource").unwrap();
        compiler
            .register_resource_factory("probe_owner", "ProbeResource")
            .unwrap();
        compiler
            .compile_module(&ast)
            .unwrap_or_else(|error| panic!("{error}"));
        compiler
    };
    let compiler = compile_aot();
    assert!(compiler.requires_runtime());
    let object = tmp.path().join("probe.o");
    let host = tmp.path().join("host.c");
    let program = tmp.path().join("probe");
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, r#"
        #include <pthread.h>
        #include <stdlib.h>
        #include "joky_runtime.h"
        #include "joky_resource.h"
        static int entries, drops;
        void probe_enter(void) { ++entries; }
        static int32_t create(uint64_t tag, void **output) {
            int *value = malloc(sizeof(int));
            if (!value) abort();
            *value = (int)tag; *output = value; return 0;
        }
        static void release(void *token) {
            if (*(int *)token != 7) abort();
            ++drops; free(token);
        }
        const JokyResourceFactoryV1 __joky_resource_factory_v1_probe_owner = {1, create, release};
        extern int32_t start(int32_t), start_indirect(int32_t), start_mutual(int32_t), start_dyn(int32_t), start_array(int32_t), start_nullable(int32_t), start_scalar(int32_t);
        extern int32_t oversized_body(int32_t);
        static void *run(void *unused) {
            (void)unused;
            const int depths[] = {3, 10000000, 3};
            uint64_t hostile_cr = (1u << 24) | (1u << 22), hostile_sr = 16;
            __asm__ volatile("msr fpcr, %0" :: "r"(hostile_cr));
            __asm__ volatile("msr fpsr, %0" :: "r"(hostile_sr));
            int32_t (*entries_to_test[])(int32_t) = {start, start_indirect, start_mutual, start_dyn, start_array, start_nullable, start_scalar};
            for (int route = 0; route < 7; ++route) for (int i = 0; i < 3; ++i) {
                entries = drops = 0;
                joky_runtime_error_clear();
                uint64_t fpcr, fpsr, after_cr, after_sr;
                __asm__ volatile("mrs %0, fpcr" : "=r"(fpcr));
                __asm__ volatile("mrs %0, fpsr" : "=r"(fpsr));
                int32_t value = entries_to_test[route](depths[i]);
                __asm__ volatile("mrs %0, fpcr" : "=r"(after_cr));
                __asm__ volatile("mrs %0, fpsr" : "=r"(after_sr));
                if (fpcr != after_cr || fpsr != after_sr) abort();
                if (route < 6 && (drops != 1 || entries < 4)) abort();
                if (route == 6 && drops != 0) abort();
                if (i == 1) {
                    if (!joky_runtime_error_code() || joky_runtime_error_end() <= joky_runtime_error_start()) abort();
                    joky_runtime_print_error();
                } else if (joky_runtime_error_code() || value != 42) abort();
            }
            entries = 0;
            joky_runtime_error_clear();
            (void)oversized_body(1);
            if (!joky_runtime_error_code() || entries != 0) abort();
            return NULL;
        }
        int main(void) {
            joky_runtime_error_clear();
            if (oversized_body(1) != 256 || joky_runtime_error_code()) return 3;
            pthread_attr_t attr;
            pthread_t thread;
            if (pthread_attr_init(&attr) || pthread_attr_setstacksize(&attr, 512 * 1024)) return 1;
            if (pthread_create(&thread, &attr, run, NULL)) return 2;
            pthread_attr_destroy(&attr);
            return pthread_join(thread, NULL);
        }
    "#).unwrap();
    link(link_runtime(c_compiler().arg(&host).arg(&object)), &program);
    let output = Command::new(program).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("Native stack limit exceeded"));
}

#[test]
fn native_stack_rejects_oversized_abi_adapters() {
    let parameters = (0..1024)
        .map(|index| format!("p{index}:Int32"))
        .collect::<Vec<_>>()
        .join(",");
    let source = format!("fn too_wide({parameters}) -> Int32 {{ p0 }}");
    let mut compiler = AotCompiler::new("oversized-stack-entry").unwrap();
    let error = compiler
        .compile_module(&crate::parser::parse(&source).unwrap())
        .unwrap_err();
    assert!(error.message().contains("adapter exceeds"), "{error}");

    let mut compiler = AotCompiler::new("template-only").unwrap();
    compiler
        .compile_module(&crate::parser::parse("fn unused<T>(x:T) -> T { x }").unwrap())
        .unwrap();
    assert!(!compiler.requires_runtime());
}
