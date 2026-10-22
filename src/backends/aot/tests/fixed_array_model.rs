//! Independent Vec model, real C checkpoints, and task-budget reuse across backends.
use super::*;
use crate::core::types::{ConstArgument, FixedArrayType};
use crate::{
    runtime::arena,
    vm::{InterpreterLimits, VmEngine, VmMode},
};
use std::{fmt::Write, rc::Rc, sync::Arc};

const SOURCE: &str = include_str!("../../../../tests/fixtures/fixed_array_f5.jk");
const HOST: &str = include_str!("../../../../tests/fixtures/fixed_array_f5.c");
const SEEDS: &[u64] = &[0, 1, 0x55aa_0123_9876_fedc, u64::MAX];

struct Step {
    command: [i32; 4],
    current: Vec<i32>,
    saved: Vec<i32>,
    rotations: i32,
}
fn model(seed: u64, rounds: usize) -> Vec<Step> {
    let mut random = seed ^ 0xd1b5_4a32_d192_ed03;
    let mut next = || {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        random
    };
    let mut current = vec![0; 8];
    let mut saved = current.clone();
    let mut rotations = 0;
    (0..rounds)
        .map(|step| {
            let op = if step < 12 {
                step
            } else {
                next() as usize % 12
            };
            let mut first = next() as usize % 9;
            let mut end = first + next() as usize % (9 - first);
            if step == 4 {
                first = 8;
            }
            if step == 5 {
                first = 0;
                end = 8;
            }
            let value = (next() % 1000) as i32;
            match op {
                0 => current.fill(value),
                1 => current = (0..8).map(|i| value + i).collect(),
                2 => saved.clone_from(&current),
                3 => current.clone_from(&saved),
                4 => {
                    if let Some(element) = current.get_mut(first) {
                        *element = value;
                    }
                }
                5 => {
                    for (i, element) in current[first..end].iter_mut().enumerate() {
                        *element = value + i as i32;
                    }
                }
                6 => std::mem::swap(&mut current, &mut saved),
                7 => saved = std::mem::replace(&mut current, vec![value; 8]),
                8 => current[first % 8] = value,
                9 => {
                    saved.clone_from(&current);
                    current[first % 8] = value;
                }
                10 => {}
                11 => {
                    current.rotate_left(1);
                    for element in &mut current {
                        *element = (*element + value) % 10000;
                    }
                    rotations += 1;
                }
                _ => unreachable!(),
            }
            Step {
                command: [op as i32, first as i32, end as i32, value],
                current: current.clone(),
                saved: saved.clone(),
                rotations,
            }
        })
        .collect()
}
fn numbers(values: &[i32]) -> String {
    format!(
        "{{{}}}",
        values
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",")
    )
}
fn model_header(seeds: &[u64], rounds: usize) -> String {
    let models: Vec<_> = seeds.iter().map(|&seed| model(seed, rounds)).collect();
    let mut output = format!("#define MODEL_ROUNDS {rounds}\nstatic const int32_t model_commands[{}][MODEL_ROUNDS][4]={{\n",seeds.len());
    for steps in &models {
        writeln!(
            output,
            "{{{}}},",
            steps
                .iter()
                .map(|s| numbers(&s.command))
                .collect::<Vec<_>>()
                .join(",")
        )
        .unwrap();
    }
    writeln!(
        output,
        "}};\nstatic const int32_t model_values[{}][MODEL_ROUNDS][2][8]={{",
        seeds.len()
    )
    .unwrap();
    for steps in &models {
        writeln!(
            output,
            "{{{}}},",
            steps
                .iter()
                .map(|s| format!("{{{},{}}}", numbers(&s.current), numbers(&s.saved)))
                .collect::<Vec<_>>()
                .join(",")
        )
        .unwrap();
    }
    writeln!(
        output,
        "}};\nstatic const int32_t model_rotations[{}][MODEL_ROUNDS]={{",
        seeds.len()
    )
    .unwrap();
    for steps in &models {
        writeln!(
            output,
            "{},",
            numbers(&steps.iter().map(|s| s.rotations).collect::<Vec<_>>())
        )
        .unwrap();
    }
    output.push_str("};\n");
    output
}
fn array(length: u64) -> Type {
    Type::FixedArray(Arc::new(FixedArrayType {
        element: Type::Int32,
        length: ConstArgument::Value(length),
    }))
}
struct Fixture {
    library: Rc<libloading::Library>,
    directory: ScratchDir,
}
impl Fixture {
    fn new(seeds: &[u64], rounds: usize) -> Self {
        let directory = ScratchDir::new("fixed-array-model");
        let dir = directory.path();
        let ast = crate::parser::parse(SOURCE).unwrap();
        std::fs::write(dir.join("bindings.h"), crate::ffi::c_header(&ast).unwrap()).unwrap();
        std::fs::write(dir.join("model.h"), model_header(seeds, rounds)).unwrap();
        std::fs::write(dir.join("host.c"), HOST).unwrap();
        let path = dir.join("host.so");
        let output = Command::new("cc")
            .args([
                "-std=c11",
                "-pedantic-errors",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-shared",
                "-fPIC",
            ])
            .arg(dir.join("host.c"))
            .arg("-o")
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let library = Rc::new(unsafe { libloading::Library::new(path) }.unwrap());
        Self { library, directory }
    }
    fn vm(&self, mode: VmMode) -> VmEngine {
        let mut vm = VmEngine::with_mode(mode);
        let r = Type::reference;
        for (name, params, ret) in [
            (
                "model_command",
                vec![r(array(4), true), r(Type::Int32, false)],
                Type::Unit,
            ),
            (
                "model_check",
                vec![
                    r(array(8), false),
                    r(array(8), false),
                    r(Type::Int32, false),
                ],
                Type::Int32,
            ),
            (
                "model_rotate",
                vec![r(array(8), true), r(Type::Int32, false)],
                Type::Int32,
            ),
        ] {
            unsafe {
                vm.register_c_fn(
                    name,
                    FunctionSignature::new(params, ret),
                    *self.library.get::<*const u8>(name.as_bytes()).unwrap(),
                    Some(self.library.clone()),
                )
            }
            .unwrap();
        }
        unsafe {
            vm.register_fn(
                "model_drop",
                FunctionSignature::new(vec![], Type::Unit),
                *self.library.get::<*const u8>(b"model_drop").unwrap(),
            )
        }
        .unwrap();
        // The fixture remains alive until all VMs and tasks have been dropped.
        vm.load_source(SOURCE).unwrap_or_else(|e| {
            panic!(
                "{mode:?}: {e}: {:?}",
                e.span().and_then(|s| s.slice(SOURCE))
            )
        });
        vm
    }
    fn reset(&self, index: i32) {
        unsafe {
            self.library
                .get::<extern "C" fn(i32)>(b"model_reset")
                .unwrap()(index)
        }
    }
    fn cleaned(&self) {
        assert_eq!(
            unsafe {
                self.library
                    .get::<extern "C" fn() -> i32>(b"model_clean")
                    .unwrap()()
            },
            1
        );
        arena::LIVE.with(|live| assert_eq!(live.get(), 0));
    }
    fn complete(&self, rounds: i32) {
        assert_eq!(
            unsafe {
                self.library
                    .get::<extern "C" fn(i32) -> i32>(b"model_complete")
                    .unwrap()(rounds)
            },
            1
        );
        self.cleaned();
    }
    fn aot(&self, seeds: usize, rounds: i32) {
        let dir = self.directory.path();
        let mut compiler = AotCompiler::new("fixed-array-model").unwrap();
        compiler
            .compile_module(&crate::parser::parse(SOURCE).unwrap())
            .unwrap();
        let object = dir.join("model.o");
        compiler.write_object(&object).unwrap();
        std::fs::write(dir.join("main.c"),format!(r#"
#include "joky_runtime.h"
extern int32_t model_run(int32_t), model_complete(int32_t);
extern void model_reset(int32_t);
int main(void) {{
    for (int seed=0; seed<{seeds}; ++seed) {{
        for (int repeat=0; repeat<2; ++repeat) {{
            model_reset(seed);
            joky_runtime_error_clear();
            if (model_run({rounds})!=42 || joky_runtime_error_code() || !model_complete({rounds})) return 1;
        }}
    }}
    return 0;
}}
"#)).unwrap();
        let program = dir.join("model");
        link(
            link_runtime(
                c_compiler()
                    .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
                    .arg(dir.join("main.c"))
                    .arg(dir.join("host.c"))
                    .arg(object),
            ),
            &program,
        );
        let output = Command::new(program).output().unwrap();
        assert!(output.status.success(), "{output:?}");
    }
}
struct RestoreLimit(usize);
impl RestoreLimit {
    fn set(limit: usize) -> Self {
        Self(arena::LIMIT.with(|l| l.replace(limit)))
    }
}
impl Drop for RestoreLimit {
    fn drop(&mut self) {
        arena::LIMIT.with(|l| l.set(self.0));
    }
}

fn run_model(rounds: usize, budget: usize) {
    let fixture = Fixture::new(SEEDS, rounds);
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = fixture.vm(mode);
        let _limit = RestoreLimit::set(budget);
        for (index, seed) in SEEDS.iter().enumerate() {
            for _ in 0..2 {
                fixture.reset(index as i32);
                assert_eq!(
                    vm.call_int32_1("model_run", rounds as i32)
                        .unwrap_or_else(|e| panic!("{mode:?} seed={seed} rounds={rounds}: {e}")),
                    42
                );
                fixture.complete(rounds as i32);
            }
        }
    }
    fixture.aot(SEEDS.len(), rounds as i32);
}
#[test]
fn fixed_array_f5_seeded_model_matches_all_backends_and_reuses_budget() {
    run_model(48, 65536);
}
#[test]
#[ignore = "exhaustive: long fixed-array model with real C roundtrips in all backends"]
fn exhaustive_fixed_array_f5_model() {
    run_model(1024, 512 * 1024);
}

#[test]
fn fixed_array_f5_budget_failures_recover_without_duplicate_cleanup() {
    let fixture = Fixture::new(&[0], 12);
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = fixture.vm(mode);
        let _limit = RestoreLimit::set(arena::MAX_BYTES);
        let mut failed = false;
        let mut succeeded = false;
        for budget in (0..32768).step_by(1024) {
            arena::LIMIT.with(|l| l.set(budget));
            fixture.reset(0);
            match vm.call_int32_1("model_run", 12) {
                Ok(value) => {
                    assert_eq!(value, 42);
                    fixture.complete(12);
                    succeeded = true;
                }
                Err(error) => {
                    assert_eq!(
                        error.code(),
                        crate::ErrorCode::ResourceLimit,
                        "{mode:?} {budget}: {error}"
                    );
                    failed = true;
                }
            }
            fixture.cleaned();
        }
        assert!(failed && succeeded);
        arena::LIMIT.with(|l| l.set(65536));
        fixture.reset(0);
        assert_eq!(vm.call_int32_1("model_run", 12).unwrap(), 42);
        fixture.complete(12);
    }
}
fn cancellation(exhaustive: bool) {
    let fixture = Fixture::new(&[0], 12);
    let mut vm = fixture.vm(VmMode::Interpreter);
    vm.set_interpreter_limits(InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    fixture.reset(0);
    let progress = || unsafe {
        fixture
            .library
            .get::<extern "C" fn() -> i32>(b"model_progress")
            .unwrap()()
    };
    let mut previous = progress();
    let mut cuts = std::collections::BTreeSet::new();
    let mut task = vm.call_async("model_task", &[]).unwrap();
    let mut instructions: u64 = 1;
    loop {
        let current = progress();
        if current != previous {
            cuts.extend(instructions.saturating_sub(2)..=instructions + 2);
            previous = current;
        }
        if instructions <= 16 || instructions.is_multiple_of(1024) {
            cuts.insert(instructions);
        }
        if task.is_finished() {
            break;
        }
        assert!(instructions < 200_000);
        task.resume().unwrap();
        instructions += 1;
    }
    assert_eq!(task.cps_result(), Some(42));
    drop(task);
    fixture.complete(12);
    cuts.insert(instructions);
    for budget in (1..=instructions).filter(|n| exhaustive || cuts.contains(n)) {
        vm.set_interpreter_limits(InterpreterLimits {
            poll_budget: Some(budget),
            ..Default::default()
        })
        .unwrap();
        fixture.reset(0);
        drop(vm.call_async("model_task", &[]).unwrap());
        fixture.cleaned();
    }
}
#[test]
fn fixed_array_f5_model_cancellation_boundaries() {
    cancellation(false);
}
#[test]
#[ignore = "exhaustive: cancel the fixed-array C model at each interpreter instruction"]
fn exhaustive_fixed_array_f5_model_cancellation() {
    cancellation(true);
}

#[test]
fn fixed_array_f5_c_embedding_example_executes() {
    let fixture = ScratchDir::new("fixed-array-example");
    let dir = fixture.path();
    let ast =
        crate::parser::parse(include_str!("../../../../examples/fixed_array_ffi.jk")).unwrap();
    std::fs::write(dir.join("bindings.h"), crate::ffi::c_header(&ast).unwrap()).unwrap();
    std::fs::write(
        dir.join("host.c"),
        include_str!("../../../../examples/fixed_array_ffi.c"),
    )
    .unwrap();
    let mut compiler = AotCompiler::new("fixed-array-example").unwrap();
    compiler.compile_module(&ast).unwrap();
    let object = dir.join("fixed_array.o");
    compiler.write_object(&object).unwrap();
    let program = dir.join("example");
    link(
        link_runtime(
            c_compiler()
                .args([
                    "-std=c11",
                    "-pedantic-errors",
                    "-Wall",
                    "-Wextra",
                    "-Werror",
                ])
                .arg(dir.join("host.c"))
                .arg(object),
        ),
        &program,
    );
    assert!(Command::new(program).status().unwrap().success());
}
