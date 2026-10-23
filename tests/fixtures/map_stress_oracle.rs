// Independent ordered-map model and resource ledger, shared by VM and CLI tests.
use std::collections::{BTreeMap, BTreeSet};
pub const SEEDS: [u32; 2] = [123456789, 0xfedcba98];
pub const STEPS: i32 = 4096;
pub const SOURCE: &str = include_str!("map_stress.jk");
pub fn source(seed: u32, distribution: i32, steps: i32, invariants: bool) -> String {
    let prefix = if invariants {
        SOURCE.to_owned()
    } else {
        SOURCE
            .replace("map.test_invariant(m)", "true")
            .replace("map.test_deleted_count(&m) == 56", "true")
            .replace("map.test_deleted_count(&m) == 0", "true")
    };
    format!("{prefix}\nfn probe() -> Int32 {{ stress({seed}, {distribution}, {steps}) }}\nfn task() @suspends -> Int32 {{ probe() }}")
}
pub fn verify(events: &[i32], seed: u32, steps: i32, complete: bool) {
    let mut live = BTreeSet::new();
    let mut created = BTreeSet::new();
    let mut map = BTreeMap::new();
    let mut rng = seed;
    let mut step = 0;
    let mut pos = 0;
    while pos < events.len() {
        let event = events[pos];
        pos += 1;
        if event == i32::MIN {
            assert!(pos + 4 <= events.len(), "incomplete checkpoint");
            rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
            let phase = step & 255;
            let mut key = ((rng >> 8) & 63) as i32;
            let mut op = (rng >> 16) & 7;
            if phase < 64 {
                key = phase;
                op = 0;
            }
            if (64..120).contains(&phase) {
                key = phase - 64;
                op = 2;
            }
            if phase == 120 {
                op = 4;
            }
            if phase == 255 {
                op = 5;
            }
            let result = match op {
                0 | 1 | 7 => map.insert(key, step + 1).unwrap_or(0),
                2 => map.remove(&key).unwrap_or(0),
                3 | 6 => map.get(&key).copied().unwrap_or(0),
                4 => 0,
                5 => {
                    map.clear();
                    0
                }
                _ => unreachable!(),
            };
            let digest: i32 = map.iter().map(|(k, v)| (k + 1) * v).sum();
            assert_eq!(
                &events[pos..pos + 4],
                &[step, result, map.len() as i32, digest],
                "step {step}"
            );
            assert_eq!(
                live,
                map.values().copied().collect(),
                "live owners at step {step}"
            );
            pos += 4;
            step += 1;
        } else if event < 0 {
            assert!(created.insert(-event), "duplicate creation {event}");
            assert!(live.insert(-event));
        } else {
            assert!(
                live.remove(&event),
                "duplicate or uncreated drop {event} at step {step}"
            );
        }
    }
    assert!(live.is_empty(), "unreleased owners: {live:?}");
    if complete {
        assert_eq!(step, steps);
    }
}
