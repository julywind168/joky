//! The same owning-key cases execute in all three backends.
use crate::string_collection_model::{Entry, Group};
pub fn groups() -> Vec<Group> {
    let cases: &[(&str, &[i32], bool)] = &[
        ("basic", &[], false),
        ("query_lifetime", &[], false),
        ("copy_views", &[], false),
        ("collision_growth", &[1, 2, 3, 4, 5], false),
        ("batch", &[10, 11, 12, 13], false),
        ("hash_failure", &[20, 21], true),
        ("eq_failure", &[30, 31], true),
        ("duplicate_drop_failure", &[40, 41], true),
        ("remove_drop_failure", &[50, 51, 52], true),
        ("batch_failure", &[60, 61, 62], true),
        ("clear_failure", &[70, 71], true),
        ("release_failure", &[80, 81], true),
    ];
    let mut groups: Vec<_> = cases
        .iter()
        .map(|&(name, ids, failure)| Group {
            source: format!(
                "{}\nfn probe() -> Int32 {{ {name}::<Unit>() }}",
                include_str!("../fixtures/string_map.jk")
            ),
            entries: vec![Entry {
                name: "probe".into(),
                text: None,
                drops: ids.to_vec(),
                failure,
            }],
        })
        .collect();
    groups.push(model_group());
    groups
}

fn model_group() -> Group {
    let mut source = String::from("import joky/map.{type Map} import joky/check.{require} fn probe() -> Int32 { var m = Map.new::<String, Int32>() let message = \"String key model\"\n");
    let mut model = std::collections::BTreeMap::new();
    let mut seed = 31_u32;
    for step in 0..160 {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let id = (seed >> 16) % 43;
        let key = format!("key-{id}-中🙂");
        if step % 5 == 0 {
            let expected = model.remove(&key).unwrap_or(-1);
            source.push_str(&format!("{{ let query = \"{key}\" require(m.remove_by_ref(&query).unwrap_or(-1) == {expected}, &message) }}\n"));
        } else {
            let expected = model.insert(key.clone(), step).unwrap_or(-1);
            source.push_str(&format!(
                "require(m.insert(\"{key}\", {step}).unwrap_or(-1) == {expected}, &message)\n"
            ));
        }
        source.push_str(&format!("require(m.len() == {}, &message)\n", model.len()));
        if step % 19 == 0 {
            source.push_str("m.reserve(12)\n");
        }
    }
    for (key, value) in model {
        source.push_str(&format!("{{ let query = \"{key}\" require(m.get_by_ref(&query).unwrap_or(-1) == {value}, &message) }}\n"));
    }
    source.push_str("42 }");
    // Bound each function's reference-analysis work while preserving one table.
    let statements = source
        .split_once('\n')
        .unwrap()
        .1
        .strip_suffix("42 }")
        .unwrap();
    let mut split = String::from("import joky/map.{type Map} import joky/check.{require}\n");
    let lines: Vec<_> = statements.lines().collect();
    let mut calls = String::new();
    for (index, chunk) in lines.chunks(40).enumerate() {
        split.push_str(&format!("fn stage_{index}(m: &mut Map<String, Int32>) {{ let message = \"String key model\"\n{}\n}}\n", chunk.join("\n")));
        calls.push_str(&format!("stage_{index}(&mut m)\n"));
    }
    split.push_str(&format!(
        "fn probe() -> Int32 {{ var m = Map.new::<String, Int32>() {calls} 42 }}"
    ));
    source = split;
    Group {
        source,
        entries: vec![Entry {
            name: "probe".into(),
            text: None,
            drops: vec![],
            failure: false,
        }],
    }
}
