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
#[ignore = "benchmark: run --release bench_persistent_map -- --ignored --nocapture"]
fn bench_persistent_map() {
    let callbacks = KeyCallbacks { hash, equal };
    let ops = &callbacks as *const _ as usize;
    for size in [128, 1024, 2048] {
        let mut samples: [Vec<f64>; 5] = std::array::from_fn(|_| Vec::new());
        for sample in 0..6 {
            let mut root = std::ptr::null_mut();
            let mut times = [0.0; 5];
            let start = Instant::now();
            for key in 0..size {
                root = jk_map_insert(root, [key, key + 1].as_ptr(), 2, [0].as_ptr(), 1, 1, ops);
                assert!(!root.is_null());
            }
            times[0] = start.elapsed().as_nanos() as f64 / size as f64;
            for (phase, time) in times.iter_mut().enumerate().take(3).skip(1) {
                let start = Instant::now();
                for i in 0..20_000 {
                    let key = black_box(i * 7919 % size + if phase == 2 { size } else { 0 });
                    let mut out = 0;
                    assert_eq!(
                        jk_map_get(root, &key, 1, ops, &mut out, 1),
                        u8::from(phase == 1)
                    );
                    if phase == 1 {
                        assert_eq!(black_box(out), key + 1);
                    }
                }
                *time = start.elapsed().as_nanos() as f64 / 20_000.0;
            }
            for (phase, time) in times.iter_mut().enumerate().skip(3) {
                let mut versions = Vec::with_capacity(64);
                let start = Instant::now();
                for i in 0..64 {
                    let key = black_box(i * 7919 % size);
                    versions.push(if phase == 3 {
                        jk_map_insert(
                            jk_dup(root),
                            [key, size].as_ptr(),
                            2,
                            [0].as_ptr(),
                            1,
                            1,
                            ops,
                        )
                    } else {
                        jk_map_remove(jk_dup(root), &key, 1, ops)
                    });
                }
                *time = start.elapsed().as_nanos() as f64 / 64.0;
                for version in versions {
                    assert_eq!(
                        jk_map_length(version),
                        (size - u64::from(phase == 4)) as usize
                    );
                    jk_drop(version);
                }
            }
            assert_eq!(jk_map_length(root), size as usize);
            jk_drop(root);
            if sample != 0 {
                for (values, time) in samples.iter_mut().zip(times) {
                    values.push(time);
                }
            }
        }
        let medians = samples.map(|mut values| {
            values.sort_by(f64::total_cmp);
            values[2]
        });
        eprintln!("[map ns/op] n={size} insert={:.0} get={:.0} miss={:.0} persistent-replace={:.0} persistent-remove={:.0}", medians[0], medians[1], medians[2], medians[3], medians[4]);
    }
}
