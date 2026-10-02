// Shared test oracle only. The language implementation never uses this map.
pub fn events() -> Vec<i32> {
    let mut map = std::collections::HashMap::<i32, i32>::new();
    let mut rng = 123456789u32;
    let mut events = Vec::new();
    for step in 0..192 {
        rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
        let key = ((rng >> 8) & 63) as i32 - 32;
        let value = (rng & 0x7fffffff) as i32;
        let op = (rng >> 16) & 7;
        let result = match op {
            0 | 1 | 7 => map.insert(key, value).unwrap_or(-1),
            2 => map.remove(&key).unwrap_or(-1),
            3 => map.get(&key).copied().unwrap_or(-1),
            4 => {
                map.reserve((rng & 7) as usize);
                0
            }
            5 if step & 31 == 0 => {
                map.clear();
                0
            }
            6 => i32::from(map.contains_key(&key)),
            _ => map.insert(key, value).unwrap_or(-1),
        };
        events.extend((-32..32).map(|key| map.get(&key).copied().unwrap_or(-1)));
        let digest = (-32..32).fold(0i32, |sum, key| {
            sum.wrapping_add(map.get(&key).copied().unwrap_or(0).wrapping_mul(key + 33))
        });
        events.extend([result, map.len() as i32, digest]);
    }
    events
}
