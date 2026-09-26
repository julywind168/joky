/// Joky VM 互操作验证项目 — 主程序
mod parser;
mod runtime;
mod jit;
mod aot;
mod vm;

use parser::{ast, parse};
use vm::engine::VmEngine;

fn main() {
    println!("=== Joky VM 互操作验证项目 ===\n");

    // ── 阶段1：解析验证 ────────────────────────────────────────────────────
    println!("── 阶段1：解析器验证 ──");
    let src_add = "fn add(x: Int32, y: Int32) -> Int32 { x + y }";
    match parse(src_add) {
        Ok(module) => {
            println!("解析成功：");
            print_module(&module);
        }
        Err(e) => {
            eprintln!("解析失败：{}", e);
            std::process::exit(1);
        }
    }

    println!();

    // ── 阶段2：JIT 编译与执行 ──────────────────────────────────────────────
    println!("── 阶段2：JIT 编译验证 ──");

    // 场景：add(10, 20) = 30
    {
        let src = "fn add(x: Int32, y: Int32) -> Int32 { x + y }";
        let mut vm = VmEngine::new();
        match vm.load_source(src) {
            Ok(names) => println!("已编译函数：{:?}", names),
            Err(e) => { eprintln!("JIT 编译失败：{}", e); std::process::exit(1); }
        }
        match vm.call_int32_2("add", 10, 20) {
            Ok(result) => {
                println!("add(10, 20) = {}", result);
                assert_eq!(result, 30, "期望 30，实际 {}", result);
                println!("✓ add(10, 20) == 30");
            }
            Err(e) => { eprintln!("调用失败：{}", e); std::process::exit(1); }
        }
    }

    println!();

    // 场景：sub、mul、div
    {
        let src = r#"
fn sub(x: Int32, y: Int32) -> Int32 { x - y }
fn mul(x: Int32, y: Int32) -> Int32 { x * y }
fn div(x: Int32, y: Int32) -> Int32 { x / y }
"#;
        let mut vm = VmEngine::new();
        vm.load_source(src).expect("编译失败");
        let r_sub = vm.call_int32_2("sub", 20, 8).expect("sub 调用失败");
        let r_mul = vm.call_int32_2("mul", 6, 7).expect("mul 调用失败");
        let r_div = vm.call_int32_2("div", 100, 4).expect("div 调用失败");
        println!("sub(20, 8)  = {} (期望 12)", r_sub);
        println!("mul(6, 7)   = {} (期望 42)", r_mul);
        println!("div(100, 4) = {} (期望 25)", r_div);
        assert_eq!(r_sub, 12);
        assert_eq!(r_mul, 42);
        assert_eq!(r_div, 25);
        println!("✓ sub/mul/div 全部正确");
    }

    println!();

    // 场景：函数互调（compute 调用 add）
    {
        let src = r#"
fn add(x: Int32, y: Int32) -> Int32 { x + y }
fn double_add(a: Int32, b: Int32) -> Int32 { add(a, b) + add(a, b) }
"#;
        let mut vm = VmEngine::new();
        vm.load_source(src).expect("编译失败");
        // double_add(3, 7) = add(3,7) + add(3,7) = 10 + 10 = 20
        let r = vm.call_int32_2("double_add", 3, 7).expect("调用失败");
        println!("double_add(3, 7) = {} (期望 20)", r);
        assert_eq!(r, 20);
        println!("✓ 函数互调正确");
    }

    println!();

    // 场景：let 绑定
    {
        let src = r#"
fn compute(x: Int32) -> Int32 {
    let a = x + 5
    a * 2
}
"#;
        let mut vm = VmEngine::new();
        vm.load_source(src).expect("编译失败");
        // compute(10) = (10+5)*2 = 30
        let r = vm.call_int32_1("compute", 10).expect("调用失败");
        println!("compute(10) = {} (期望 30)", r);
        assert_eq!(r, 30);
        println!("✓ let 绑定正确");
    }

    println!("\n=== 阶段2验证完成 ✓ ===");
}

fn print_module(module: &ast::Module) {
    for (i, item) in module.items.iter().enumerate() {
        match item {
            ast::Item::FnDef(f) => {
                let suspends = if f.suspends { " @suspends" } else { "" };
                println!(
                    "  [{}] fn {}({}) {} -> {:?}",
                    i, f.name,
                    f.params.iter()
                        .map(|p| format!("{}: {:?}", p.name, p.ty))
                        .collect::<Vec<_>>().join(", "),
                    suspends, f.ret_ty
                );
            }
            ast::Item::ExternFn(e) => {
                println!(
                    "  [{}] extern fn {}({}) -> {:?}",
                    i, e.name,
                    e.params.iter()
                        .map(|p| format!("{}: {:?}", p.name, p.ty))
                        .collect::<Vec<_>>().join(", "),
                    e.ret_ty
                );
            }
        }
    }
}
