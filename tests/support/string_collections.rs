//! Container fixture groups and independent bulk text reference cases.
pub const SOURCE: &str = include_str!("../fixtures/string_collections.jk");

pub struct Entry {
    pub name: String,
    pub text: Option<String>,
    pub drops: Vec<i32>,
    pub failure: bool,
}
pub struct Group {
    pub source: String,
    pub entries: Vec<Entry>,
}

fn group(cases: &[(&str, Option<&str>, &[i32], bool)]) -> Group {
    let mut group = Group {
        source: SOURCE.to_owned(),
        entries: Vec::new(),
    };
    for &(target, text, drops, failure) in cases {
        let name = format!("probe_{target}");
        let ret = if text.is_some() { "String" } else { "Int32" };
        group
            .source
            .push_str(&format!("\nfn {name}() -> {ret} {{ {target}::<Unit>() }}"));
        group.entries.push(Entry {
            name,
            text: text.map(str::to_owned),
            drops: drops.to_vec(),
            failure,
        });
    }
    group
}
pub fn ownership_groups() -> Vec<Group> {
    vec![
        group(&[
            ("record_text", Some("中🙂"), &[1, 4], false),
            ("fields", None, &[], false),
            ("shapes", None, &[2, 3], false),
            ("arrays", Some("A|tail"), &[], false),
            ("array_records", None, &[10, 11, 12, 13, 14, 15], false),
        ]),
        group(&[("nested", None, &[], false)]),
        group(&[("map_values", None, &[], false)]),
        group(&[("builders", None, &[], false)]),
        group(&[("enum_arrays", None, &[50], false)]),
        group(&[
            ("tracked_failure", None, &[20, 21], true),
            ("partial_failure", None, &[30], true),
        ]),
    ]
}

fn literal(value: &str) -> String {
    let mut text = String::from("\"");
    for scalar in value.chars() {
        text.push_str(&format!("\\u{{{:x}}}", u32::from(scalar)));
    }
    text.push('"');
    text
}

pub fn bulk_group() -> Group {
    let mut group = group(&[("bulk_work", None, &[], false)]);
    let mut cases = vec![
        ("", ","),
        ("a", ","),
        (",a,,", ","),
        ("a,b,c", ","),
        ("aaa", "aa"),
        ("aaaa", "aa"),
        ("aaaaa", "aa"),
        ("--a---b--", "--"),
        ("中🙂中", "中"),
        ("\0a\0\0", "\0"),
        ("e\u{301}e\u{301}", "\u{301}"),
        ("é中🙂", "🙂中"),
        ("éé", "e"),
        ("ababa", "aba"),
    ]
    .into_iter()
    .map(|(text, separator)| (text.to_owned(), separator.to_owned()))
    .collect::<Vec<_>>();
    let alphabet = ['a', 'b', ',', '|', '中', '🙂', '\0', 'é', 'e', '\u{301}'];
    let mut seed = 0x7529_ac31u32;
    for index in 0..32 {
        let mut text = String::new();
        for _ in 0..index % 9 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            text.push(alphabet[seed as usize % alphabet.len()]);
        }
        let separator = if index % 3 == 0 {
            "中🙂".to_owned()
        } else {
            alphabet[index % alphabet.len()].to_string()
        };
        cases.push((text, separator));
    }
    for (index, (text, separator)) in cases.iter().enumerate() {
        let parts = text.split(separator).collect::<Vec<_>>();
        let name = format!("bulk_{index}");
        group.source.push_str(&format!("\nfn {name}() -> Int32 {{ let input = {} let separator = {} let parts = input.split(&separator).unwrap_or(Array.new::<String>()) input.release() if parts.len() != {} {{ return 1 }} ", literal(text), literal(separator), parts.len()));
        for (index, part) in parts.iter().enumerate() {
            group.source.push_str(&format!("{{ let expected = {} if !parts.borrow_ref({index}).equals(&expected) {{ return 2 }} }} ", literal(part)));
        }
        let joined = parts.join(separator);
        group.source.push_str(&format!("let joined = text.join(&parts, &separator) parts.release() separator.release() let expected = {} if !joined.equals(&expected) {{ return 3 }} 42 }}", literal(&joined)));
        group.entries.push(Entry {
            name,
            text: None,
            drops: vec![],
            failure: false,
        });
    }
    for (name, values, separator) in [
        ("join_empty", vec![], "中"),
        ("join_single", vec!["🙂\0"], "中"),
        ("join_empty_separator", vec!["a", "中", "🙂"], ""),
        ("join_empty_parts", vec!["", "", ""], "|"),
    ] {
        group.source.push_str(&format!(
            "\nfn {name}() -> String {{ var parts = Array.new::<String>() "
        ));
        for value in &values {
            group
                .source
                .push_str(&format!("parts.push({}) ", literal(value)));
        }
        group.source.push_str(&format!("let separator = {} let result = text.join(&parts, &separator) parts.release() separator.release() result }}", literal(separator)));
        group.entries.push(Entry {
            name: name.into(),
            text: Some(values.join(separator)),
            drops: vec![],
            failure: false,
        });
    }
    group.source.push_str("\nfn empty_separator() -> Int32 { let input = \"中🙂\" let separator = \"\" let result = input.split(&separator) if input.byte_length() != 7 { return 1 } match result { Err(text.SplitError.EmptySeparator) => 42, Ok(_) => 2 } }");
    group.entries.push(Entry {
        name: "empty_separator".into(),
        text: None,
        drops: vec![],
        failure: false,
    });
    group
}
