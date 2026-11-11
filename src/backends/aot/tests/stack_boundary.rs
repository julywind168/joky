//! Stage-one acceptance at the actual pthread admission threshold.
use super::{runtime::runtime_archive, AotCompiler, ScratchDir};
use crate::{
    backends::jit,
    core::{ir::Op, prepared::PreparedModule, types::memory_layout::MAX_DEPTH},
    runtime::{error, suspension::ModuleId},
    vm::{VmEngine, VmMode},
    TypeRegistry,
};
use std::{collections::HashMap, process::Command};

#[path = "../../../../tests/fixtures/native_stack_boundary.rs"]
mod boundary;

fn source() -> String {
    // Alternate nominal records and arrays: reach the layout limit without
    // repeatedly expanding hundreds of transparent aliases in the frontend.
    let depth = MAX_DEPTH / 2 - 2;
    let mut source = String::from(
        "import joky/fixed_array.{type FixedArray}\n\
         fn leaf(n:Int32) -> String { let value=42/n; \"owned\" }\n\
         struct D0 { text:String }\n",
    );
    for level in 1..=depth {
        source.push_str(&format!(
            "struct D{level} {{ items:FixedArray<D{},1> }}\n",
            level - 1
        ));
    }
    source.push_str(&format!(
        "struct Middle {{ inner:D{depth} }}\n\
         type Deep = FixedArray<Middle,2>\n\
         fn chain0(n:Int32) -> D0 {{ D0 {{ text:leaf(n) }} }}\n"
    ));
    let mut previous = 0;
    for last in (16..depth).step_by(16).chain(std::iter::once(depth)) {
        source.push_str(&format!(
            "fn chain{last}(n:Int32) -> D{last} {{ let v{previous}=chain{previous}(n)\n"
        ));
        for level in previous + 1..=last {
            source.push_str(&format!(
                "let v{level}=D{level} {{ items:[v{}] }}\n",
                level - 1
            ));
        }
        source.push_str(&format!("v{last}\n}}\n"));
        previous = last;
    }
    source.push_str(&format!(
        "fn middle(n:Int32) -> Middle {{ Middle {{ inner:chain{depth}(n) }} }}\n\
         fn make(n:Int32) -> Deep {{ let value:Deep=[middle(1),middle(n)]; value }}\n\
         fn construct(n:Int32) -> Int32 {{ let value=make(n); 42 }}\n\
         fn direct(n:Int32) -> Int32 {{ let value=make(n); heavy(n,value) }}\n\
         fn indirect(n:Int32) -> Int32 {{ let value=make(n); let f:fn(Int32,Deep)->Int32=heavy; f(n,value) }}\n"
    ));
    let fields = vec!["Int32"; 256].join(",");
    let values = vec!["n"; 256].join(",");
    source.push_str(&format!(
        "fn padding(n:Int32) -> ({fields}) {{ ({values}) }}\n"
    ));
    source.push_str("fn heavy(n:Int32,owner:Deep) -> Int32 {\n");
    for i in 0..32 {
        source.push_str(&format!("let p{i}=padding(n)\n"));
    }
    source.push_str("42");
    for i in 0..32 {
        source.push_str(&format!(" + p{i}.0 - n"));
    }
    source.push_str("\n}\n");
    source
}

#[test]
fn native_stack_stage_one_boundary_matrix() {
    const CHILD: &str = "JOKY_STACK_BOUNDARY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let name = std::thread::current().name().unwrap().to_owned();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &name, "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    // Compile on the normal compiler stack, then execute on a distinct small
    // thread. Source, IR, frame descriptors and admission remain unmodified.
    let source = source();
    let ast = crate::parser::parse(&source).unwrap();
    let types = TypeRegistry::default().with_prelude(false);
    let prepared = PreparedModule::new(&ast, &types, &Default::default()).unwrap_or_else(|error| {
        panic!(
            "{error}: {:?}",
            error.span().and_then(|span| span.slice(&source))
        )
    });
    let plan = prepared.plan(&HashMap::new()).unwrap_or_else(|error| {
        panic!(
            "{error}: {:?}",
            error.span().and_then(|span| span.slice(&source))
        )
    });
    let nodes = plan.functions["heavy"].signature.params[1].place_copy_program();
    // The initialization token/reference consumes a frontend graph level.
    // This is the deepest source-constructible value; the raw runtime descriptor
    // limit (one level deeper) is covered by the 64 KiB emergency probe.
    assert_eq!(nodes.len(), MAX_DEPTH);
    let depth = MAX_DEPTH / 2 - 2;
    let too_deep = source
        .replace(
            &format!("struct Middle {{ inner:D{depth} }}"),
            &format!("struct Extra {{ inner:D{depth} }} struct Middle {{ inner:Extra }}"),
        )
        .replace(
            &format!("Middle {{ inner:chain{depth}(n) }}"),
            &format!("Middle {{ inner:Extra {{ inner:chain{depth}(n) }} }}"),
        );
    let rejected = crate::parser::parse(&too_deep).unwrap();
    let rejected = PreparedModule::new(&rejected, &types, &Default::default()).unwrap();
    let error = rejected
        .plan(&HashMap::new())
        .err()
        .expect("one more level must be rejected");
    assert!(error.message().contains("type work limit"), "{error}");
    let declaration = plan.functions["heavy"].span;
    let call = plan.bodies["direct"]
        .ops
        .iter_spanned()
        .find_map(|(op, span)| {
            matches!(op, Op::Call { name, .. } if name == "heavy").then_some(*span)
        })
        .unwrap();
    let arithmetic = plan.bodies["leaf"]
        .ops
        .iter_spanned()
        .find_map(|(op, span)| {
            matches!(op, Op::Binary(_, op, _, _) if op.can_fail()).then_some(*span)
        })
        .unwrap();
    let spans = [arithmetic, call, declaration].map(|span| (span.start(), span.end()));
    // Semantic oracle for the partial constructor and the same valid program.
    let mut vm = VmEngine::with_mode(VmMode::Interpreter);
    vm.set_prelude_enabled(false);
    vm.load_source(&source).unwrap_or_else(|error| {
        panic!(
            "{error}: {:?}",
            error.span().and_then(|span| span.slice(&source))
        )
    });
    for name in ["construct", "direct", "indirect"] {
        assert_eq!(vm.call_int32_1(name, 1).unwrap(), 42);
        assert!(vm.call_int32_1(name, 0).is_err());
        crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
    }
    let compiled = jit::compile(&prepared, &[], &HashMap::new(), vec![], ModuleId(1), true)
        .unwrap_or_else(|error| {
            panic!(
                "{error}: {:?}",
                error.span().and_then(|span| span.slice(&source))
            )
        });
    let entries: [boundary::Entry; 3] = ["construct", "direct", "indirect"]
        .map(|name| unsafe { std::mem::transmute(compiled.exports[name].entry) });
    std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(move || {
            boundary::exercise(
                entries,
                boundary::Errors {
                    clear: error::joky_runtime_error_clear,
                    code: error::joky_runtime_error_code,
                    start: error::joky_runtime_error_start,
                    end: error::joky_runtime_error_end,
                },
                || {},
                || crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0)),
                spans,
            );
        })
        .unwrap()
        .join()
        .unwrap();

    let tmp = ScratchDir::new("stack-boundary");
    let object = tmp.path().join("program.o");
    let host = tmp.path().join("host.rs");
    let program = tmp.path().join("host");
    let mut compiler = AotCompiler::new("stack-boundary").unwrap();
    compiler.set_strict_native_stack(true).unwrap();
    compiler.set_prelude_enabled(false).unwrap();
    compiler.compile_module(&ast).unwrap_or_else(|error| {
        panic!(
            "{error}: {:?}",
            error.span().and_then(|span| span.slice(&source))
        )
    });
    compiler.write_object(&object).unwrap();
    std::fs::write(
        tmp.path().join("boundary.rs"),
        include_str!("../../../../tests/fixtures/native_stack_boundary.rs"),
    )
    .unwrap();
    std::fs::write(
        &host,
        format!(
            "const SPANS: [(usize,usize);3] = {spans:?};\n{}",
            include_str!("../../../../tests/fixtures/native_stack_boundary_host.rs")
                .replacen("//!", "//", 1)
        ),
    )
    .unwrap();
    let library = runtime_archive().with_extension("rlib");
    let output = Command::new("rustc")
        .arg("--edition=2021")
        .arg(&host)
        .arg("--extern")
        .arg(format!("joky={}", library.display()))
        .arg("-L")
        .arg(format!(
            "dependency={}",
            library.parent().unwrap().display()
        ))
        .arg("-C")
        .arg(format!("link-arg={}", object.display()))
        .arg("-o")
        .arg(&program)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new(program).output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
