//! Isolated K1-3 prototype: guard an owning direct call before detaching its arguments.
use super::{instances::shared_runtime_object, AotCompiler, ScratchDir};
use crate::{
    backends::{jit, native::stack_probe},
    core::resolver::BindingRegistry,
    runtime::{arena::Arena, error, suspension::ModuleId},
    Error, ErrorCode, ErrorPhase, SourceId, Span, TypeRegistry,
};
use std::{cell::RefCell, collections::HashMap, process::Command};

const RESERVE: usize = 128 * 1024;
type Entry = unsafe extern "C" fn(i32, *mut Arena) -> i32;

#[derive(Default)]
struct Trace {
    low: usize,
    frame: usize,
    checks: Vec<usize>,
    entries: usize,
    drops: Vec<i32>,
}
thread_local! {
    static TRACE: RefCell<Trace> = RefCell::new(Trace::default());
}

extern "C" fn room(sp: usize, frame: usize, source: u64, start: usize, end: usize) -> i32 {
    let fits = TRACE.with_borrow_mut(|t| {
        t.frame = frame;
        t.checks.push(sp);
        sp.saturating_sub(t.low) >= RESERVE + t.frame
    });
    if !fits {
        error::record_error(
            Error::new_with_code(
                ErrorPhase::Runtime,
                ErrorCode::ResourceLimit,
                "Native stack limit exceeded",
            )
            .with_span(Span::new(start, end).with_source(SourceId::from_raw(source))),
        );
    }
    i32::from(fits)
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
    format!(
        r#"
        extern fn probe_enter()
        extern fn probe_owner(tag:UInt64) -> ProbeResource
        extern fn probe_stack_room(sp:UInt64, bytes:UInt64, source:UInt64, start:UInt64, end:UInt64) -> Int32
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
    "#
    )
}

fn exercise(entry: Entry, source: &str, backend: &str) {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn_scoped(scope, || {
                // Both pthread queries describe the thread actually running the code.
                let low = unsafe {
                    let thread = libc::pthread_self();
                    libc::pthread_get_stackaddr_np(thread) as usize
                        - libc::pthread_get_stacksize_np(thread)
                };
                let mut arena = Arena::default();
                let _restore = crate::runtime::float_env::Guard::enter();
                crate::runtime::float_env::testing::hostile();
                let fp_state = crate::runtime::float_env::testing::state();
                for n in [3, 10_000_000, 3] {
                    TRACE.set(Trace {
                        low,
                        ..Trace::default()
                    });
                    let result = error::checked(|| unsafe { entry(n, &mut arena) });
                    assert_eq!(crate::runtime::float_env::testing::state(), fp_state);
                    let failed = n > 3;
                    if failed {
                        let err = result.unwrap_err();
                        assert_eq!(err.code(), ErrorCode::ResourceLimit);
                        assert_eq!(
                            err.span().unwrap().slice(source),
                            Some("probe_descend(n - 1, owner)")
                        );
                    } else {
                        assert_eq!(result.unwrap(), 42);
                    }
                    TRACE.with_borrow(|t| {
                        let frame = t.frame;
                        assert!(frame >= 2048);
                        assert_eq!(t.drops, [7]);
                        assert_eq!(t.checks.len(), t.entries + usize::from(failed));
                        assert!(t.entries >= 4);
                        for pair in t.checks.windows(2) {
                            assert_eq!(pair[0] - pair[1], frame, "{backend} machine frame");
                        }
                        if failed {
                            let remaining = t.checks.last().unwrap() - low;
                            assert!((RESERVE..RESERVE + frame).contains(&remaining));
                            eprintln!(
                                "{backend}: frame={frame} entered={} remaining={remaining}",
                                t.entries
                            );
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
fn native_stack_preflight_prototype() {
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
        ("probe_stack_room", room as *const u8),
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
        .unwrap()
    };
    let (_, baseline_bytes) = stack_probe::compile(false, compile_jit);
    let (compiled, guarded_bytes) = stack_probe::compile(true, compile_jit);
    eprintln!("JIT private body bytes: baseline={baseline_bytes}, guarded={guarded_bytes}");
    let export = &compiled.exports["start"];
    assert!(export.info.needs_arena);
    exercise(
        unsafe { std::mem::transmute::<*const u8, Entry>(export.entry) },
        &source,
        "JIT",
    );

    let tmp = ScratchDir::new("stack-preflight");
    let compile_aot = || {
        let mut compiler = AotCompiler::new("stack-probe").unwrap();
        compiler.set_prelude_enabled(false).unwrap();
        compiler.register_resource_type("ProbeResource").unwrap();
        compiler
            .register_resource_factory("probe_owner", "ProbeResource")
            .unwrap();
        compiler.compile_module(&ast).unwrap();
        compiler
    };
    let (_, baseline_bytes) = stack_probe::compile(false, compile_aot);
    let (compiler, guarded_bytes) = stack_probe::compile(true, compile_aot);
    eprintln!("AOT private body bytes: baseline={baseline_bytes}, guarded={guarded_bytes}");
    let symbols = hosts
        .into_iter()
        .chain(crate::runtime::error::symbols())
        .chain(crate::runtime::buffer::symbols())
        .chain(crate::runtime::cleanup::symbols())
        .chain(crate::runtime::resource::symbols())
        .chain(crate::runtime::arena::symbols())
        .chain(crate::runtime::owned_text::symbols())
        .chain(crate::runtime::float_env::symbols())
        .map(|(name, address)| (name.to_owned(), address))
        .collect();
    let bridge = tmp.path().join("bridge.o");
    let object = tmp.path().join("probe.o");
    let library = tmp.path().join("probe.dylib");
    std::fs::write(&bridge, shared_runtime_object(&compiler.module, &symbols)).unwrap();
    compiler.write_object(&object).unwrap();
    let output = Command::new("cc")
        .arg("-dynamiclib")
        .arg(object)
        .arg(bridge)
        .arg("-o")
        .arg(&library)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let library = unsafe { libloading::Library::new(library).unwrap() };
    let entry = unsafe { library.get::<Entry>(b"start").unwrap() };
    exercise(*entry, &source, "AOT");
}
