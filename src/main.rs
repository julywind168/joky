/// Joky VM 互操作验证项目 — 主程序
mod parser;
mod runtime;
mod jit;
mod aot;
mod vm;

use parser::{ast, parse};
use vm::engine::VmEngine;
use aot::AotCompiler;
use std::path::Path;
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

    // ── 阶段3：AOT 编译 → .o → 链接 → 执行 ──────────────────────────────
    println!("\n── 阶段3：AOT 编译验证 ──");

    let src_aot = r#"
fn add(x: Int32, y: Int32) -> Int32 { x + y }
fn mul(x: Int32, y: Int32) -> Int32 { x * y }
"#;

    // 1. 用 AOT 编译器编译出 .o 文件
    let obj_path = Path::new("/tmp/jk_aot_test.o");
    {
        let mut aot = AotCompiler::new("jk_aot_test")
            .expect("创建 AOT 编译器失败");
        let module = parse(src_aot).expect("解析失败");
        let compiled = aot.compile_module(&module).expect("AOT 编译失败");
        println!("AOT 编译函数：{:?}", compiled);
        aot.write_object(obj_path).expect("写入 .o 失败");
        println!("目标文件：{}", obj_path.display());
    }

    // 2. 写 C 语言驱动文件，调用 AOT 编译的 add/mul
    let c_src = r#"
#include <stdio.h>
#include <stdint.h>

// 声明 Joky-Lite AOT 编译的函数（System V ABI，与 Cranelift 一致）
extern int32_t add(int32_t x, int32_t y);
extern int32_t mul(int32_t x, int32_t y);

int main(void) {
    int32_t r1 = add(10, 20);
    int32_t r2 = mul(6, 7);
    printf("add(10,20)=%d\n", r1);
    printf("mul(6,7)=%d\n", r2);
    // 用非零退出码表示失败，让 Rust 侧捕获
    if (r1 != 30 || r2 != 42) return 1;
    return 0;
}
"#;
    let c_path = Path::new("/tmp/jk_aot_driver.c");
    let exe_path = Path::new("/tmp/jk_aot_test");
    std::fs::write(c_path, c_src).expect("写 C 驱动失败");

    // 3. 用系统 cc 链接：.o + C 驱动 → 可执行文件
    let link_status = std::process::Command::new("cc")
        .args([
            c_path.to_str().unwrap(),
            obj_path.to_str().unwrap(),
            "-o", exe_path.to_str().unwrap(),
        ])
        .status()
        .expect("cc 链接失败");
    if !link_status.success() {
        eprintln!("链接失败，退出码 {:?}", link_status.code());
        std::process::exit(1);
    }
    println!("链接成功：{}", exe_path.display());

    // 4. 执行并验证输出
    let output = std::process::Command::new(exe_path)
        .output()
        .expect("执行 AOT 程序失败");
    let stdout = String::from_utf8_lossy(&output.stdout);
    print!("{}", stdout);
    if !output.status.success() {
        eprintln!("AOT 程序返回失败：{:?}", output.status.code());
        std::process::exit(1);
    }
    assert!(stdout.contains("add(10,20)=30"), "AOT add 结果错误");
    assert!(stdout.contains("mul(6,7)=42"),   "AOT mul 结果错误");
    println!("✓ AOT 调用约定验证正确（与 JIT 一致）");

    println!("\n=== 阶段3验证完成 ✓ ===");

    // ── 阶段4：跨边界调用 ──────────────────────────────────────────────────
    println!("\n── 阶段4：跨边界调用验证 ──");

    // 场景1：AOT 侧（Rust）调用 VM 函数，传递 Int32 参数
    println!("\n[场景1] Rust 宿主调用 VM 函数");
    {
        let script_src = r#"
fn add(x: Int32, y: Int32) -> Int32 { x + y }
fn greet_len(name: Int32) -> Int32 { name + 1 }
"#;
        let mut vm = VmEngine::new();
        vm.load_source(script_src).expect("场景1 JIT 编译失败");

        let r1 = vm.call_int32_2("add", 10, 20).expect("add 调用失败");
        println!("  vm_call add(10, 20) = {} (期望 30)", r1);
        assert_eq!(r1, 30);

        let r2 = vm.call_int32_1("greet_len", 41).expect("greet_len 调用失败");
        println!("  vm_call greet_len(41) = {} (期望 42)", r2);
        assert_eq!(r2, 42);
        println!("  ✓ 场景1通过：宿主成功调用 VM 函数，参数与返回值正确传递");
    }

    // 场景2：VM 代码回调 AOT（Rust）注册的宿主函数
    println!("\n[场景2] VM 代码回调宿主函数");
    {
        // 宿主函数：host_multiply(x, y) = x * y，用 extern "C" 保证 ABI 一致
        extern "C" fn host_multiply(x: i32, y: i32) -> i32 {
            println!("  [宿主] host_multiply({}, {}) 被调用", x, y);
            x * y
        }

        let script_src = r#"
extern fn host_multiply(x: Int32, y: Int32) -> Int32

fn compute(x: Int32) -> Int32 {
    host_multiply(x + 5, 5)
}
"#;
        let mut vm = VmEngine::new();
        // 注册宿主函数指针
        vm.register_fn("host_multiply", host_multiply as *const u8);
        vm.load_source(script_src).expect("场景2 JIT 编译失败");

        // compute(5) = host_multiply(5+5, 5) = host_multiply(10, 5) = 50
        let result = vm.call_int32_1("compute", 5).expect("compute 调用失败");
        println!("  vm_call compute(5) = {} (期望 50)", result);
        assert_eq!(result, 50);
        println!("  ✓ 场景2通过：VM 成功回调宿主函数，AOT→VM→AOT 调用栈正确");
    }

    // 验证版本化入口表
    println!("\n[版本化入口表] 验证基础热替换（场景5前置）");
    {
        let src_v1 = "fn compute(x: Int32) -> Int32 { x + 10 }";
        let src_v2 = "fn compute(x: Int32) -> Int32 { x * 10 }";

        let mut vm = VmEngine::new();
        vm.load_source(src_v1).expect("v1 编译失败");
        let r1 = vm.call_int32_1("compute", 10).expect("v1 调用失败");
        println!("  v1: compute(10) = {} (期望 20)", r1);
        assert_eq!(r1, 20);

        vm.reload_source(src_v2).expect("v2 热替换失败");
        let r2 = vm.call_int32_1("compute", 10).expect("v2 调用失败");
        println!("  v2: compute(10) = {} (期望 100)", r2);
        assert_eq!(r2, 100);
        println!("  ✓ 版本化入口表切换正确");
    }

    println!("\n=== 阶段4验证完成 ✓ ===");
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
