//! Storage-only timings; callbacks deliberately exclude language/JIT hashing costs.
use super::*;
use crate::runtime::hashing::KeyCallbacks;
use std::hint::black_box;
use std::time::Instant;

unsafe extern "C" fn hash(key: *const u64) -> u64 {
    (*key).wrapping_mul(0x9e3779b97f4a7c15)
}

unsafe extern "C" fn equal(left: *const u64, right: *const u64) -> u8 {
    u8::from(*left == *right)
}

#[test]
#[ignore = "benchmark: run --release bench_mut_map_index -- --ignored --nocapture"]
fn bench_mut_map_index() {
    let callbacks = KeyCallbacks { hash, equal };
    let ops = &callbacks as *const _ as usize;
    let masks = [0];
    for size in [128, 1024, 8192] {
        let mut samples = [Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        for sample in 0..6 {
            let map = jk_mut_map_new(1, 1, 1, 1, ops);
            let mut out = 0;
            let mut timings = [0.0; 5];
            for (phase, timing) in timings.iter_mut().enumerate() {
                let count = if matches!(phase, 1 | 2) { 20_000 } else { size };
                let start = Instant::now();
                for i in 0..count {
                    let key = black_box(((i * 7919) % size) as u64);
                    let missing = key + size as u64;
                    match phase {
                        0 | 3 => {
                            let value = key + phase as u64;
                            assert_eq!(
                                jk_mut_map_insert(
                                    map,
                                    &key,
                                    1,
                                    masks.as_ptr(),
                                    1,
                                    &value,
                                    1,
                                    masks.as_ptr(),
                                    1,
                                    &mut out,
                                    1
                                ),
                                u8::from(phase == 3)
                            );
                        }
                        1 => {
                            assert_eq!(jk_mut_map_get(map, &key, 1, ops, &mut out, 1), 1);
                            assert_eq!(black_box(out), key);
                        }
                        2 => assert_eq!(jk_mut_map_contains_key(map, &missing, 1, ops), 0),
                        4 => {
                            assert_eq!(jk_mut_map_remove(map, &key, 1, ops, &mut out, 1), 1);
                            assert_eq!(black_box(out), key + 3);
                        }
                        _ => unreachable!(),
                    }
                }
                *timing = start.elapsed().as_nanos() as f64 / count as f64;
            }
            assert_eq!(jk_mut_map_length(map), 0);
            jk_drop(map);
            if sample != 0 {
                for (samples, timing) in samples.iter_mut().zip(timings) {
                    samples.push(timing);
                }
            }
        }
        let medians = samples.map(|mut values| {
            values.sort_by(f64::total_cmp);
            values[values.len() / 2]
        });
        eprintln!(
            "[mut-map ns/op] n={size} insert={:.0} get={:.0} miss={:.0} replace={:.0} remove={:.0}",
            medians[0], medians[1], medians[2], medians[3], medians[4]
        );
    }
}
