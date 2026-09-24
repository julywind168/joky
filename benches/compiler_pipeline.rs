use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use joky::{syntax, Compiler};

const SMALL_PROGRAM: &str = r#"
    fn add(a: Int32, b: Int32) -> Int32 {
        a + b
    }

    fn main() {
        let value = add(a: 20, b: 22)
    }
"#;

const CONTROL_FLOW_PROGRAM: &str = r#"
    fn main() {
        let value = loop {
            if true {
                break 42
            } else {
                continue
            }
        }
    }
"#;

fn bench_parser(c: &mut Criterion) {
    let mut group = c.benchmark_group("parser");
    group.bench_function("small_program", |benchmark| {
        benchmark.iter(|| {
            syntax::parse_program(black_box(SMALL_PROGRAM)).expect("benchmark source should parse")
        });
    });
    group.bench_function("control_flow_program", |benchmark| {
        benchmark.iter(|| {
            syntax::parse_program(black_box(CONTROL_FLOW_PROGRAM))
                .expect("benchmark source should parse")
        });
    });
    group.finish();
}

fn bench_compiler(c: &mut Criterion) {
    let mut group = c.benchmark_group("compiler");
    group.bench_function("run_small_program", |benchmark| {
        benchmark.iter_batched(
            || Compiler::new().expect("compiler backend should initialize"),
            |mut compiler| {
                compiler
                    .run_program(black_box(SMALL_PROGRAM))
                    .expect("benchmark source should compile");
            },
            BatchSize::SmallInput,
        );
    });
    group.bench_function("run_control_flow_program", |benchmark| {
        benchmark.iter_batched(
            || Compiler::new().expect("compiler backend should initialize"),
            |mut compiler| {
                compiler
                    .run_program(black_box(CONTROL_FLOW_PROGRAM))
                    .expect("benchmark source should compile");
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group!(benches, bench_parser, bench_compiler);
criterion_main!(benches);
