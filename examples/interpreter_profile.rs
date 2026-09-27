//! Sustained interpreter workload for an external CPU sampler.
//! Usage: cargo run --release --example interpreter_profile -- while_sum 100000 15
use joky::vm::VmEngine;
use std::{
    error::Error,
    hint::black_box,
    io::Write,
    time::{Duration, Instant},
};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let kernel = args.next().unwrap_or_else(|| "while_sum".into());
    let n: i32 = args.next().unwrap_or_else(|| "100000".into()).parse()?;
    let seconds: u64 = args.next().unwrap_or_else(|| "15".into()).parse()?;
    if !matches!(
        kernel.as_str(),
        "while_sum" | "while_recurrence" | "while_sequence"
    ) || !(1..=100_000_000).contains(&n)
        || !(1..=300).contains(&seconds)
        || args.next().is_some()
    {
        return Err("Usage: interpreter_profile [while_sum|while_recurrence|while_sequence] [iterations:1..100000000] [seconds:1..300]".into());
    }
    let mut vm = VmEngine::new_interpreter();
    vm.load_source(include_str!("../benches/programs/while.jk"))?;
    let expected = (0..n).fold(7_i32, |value, i| {
        if kernel == "while_recurrence" {
            value
                .wrapping_mul(1_664_525)
                .wrapping_add(1_013_904_223)
                .wrapping_add(i)
        } else {
            value.wrapping_add(i)
        }
    });
    assert_eq!(vm.call_int32_2(&kernel, n, 7)?, expected);
    println!(
        "Ready: pid={} kernel={kernel} iterations={n} seconds={seconds} debug_assertions={}",
        std::process::id(),
        cfg!(debug_assertions)
    );
    std::io::stdout().flush()?;
    let start = Instant::now();
    let mut calls = 0_u64;
    let mut checksum = 0_i32;
    while start.elapsed() < Duration::from_secs(seconds) {
        let value = vm.call_int32_2(black_box(&kernel), black_box(n), black_box(7))?;
        checksum = checksum.wrapping_add(black_box(value));
        calls += 1;
    }
    assert_eq!(checksum, expected.wrapping_mul(calls as i32));
    println!(
        "Complete: calls={calls} elapsed_ms={:.3} checksum={checksum}",
        start.elapsed().as_secs_f64() * 1000.0
    );
    Ok(())
}
