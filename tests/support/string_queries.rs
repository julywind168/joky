//! Independent Rust UTF-8 and decimal models shared by all three backend tests.
pub struct Cases {
    pub source: String,
    pub integers: Vec<(String, i32)>,
    pub strings: Vec<(String, String)>,
}

fn literal(value: &str) -> String {
    let mut result = String::from("\"");
    for scalar in value.chars() {
        result.push_str(&format!("\\u{{{:x}}}", u32::from(scalar)));
    }
    result.push('"');
    result
}

pub fn cases() -> Cases {
    let mut cases = Cases {
        source: include_str!("../fixtures/string_queries.jk").to_owned(),
        integers: vec![
            ("queries".into(), 42),
            ("work".into(), 42),
            ("invalid_repeated".into(), 42),
        ],
        strings: [
            ("roundtrip", "aé中🙂\0e\u{301}"),
            ("piece", "中🙂"),
            ("combined", "中\0🙂e\u{301}"),
            ("signed_min", "-9223372036854775808"),
            ("unsigned_max", "18446744073709551615"),
            ("concat_empty", ""),
            ("concat_empty_left", "🙂"),
            ("concat_empty_right", "中"),
        ]
        .map(|(name, value)| (name.into(), value.into()))
        .to_vec(),
    };
    text_helpers(&mut cases);
    ascii_and_replace(&mut cases);
    // Exercise scalar boundaries on both sides of the runtime's bounded scans.
    // Build long prefixes as literals, keeping the generated program compact.
    for (index, (padding, tail)) in [
        (16381, "🙂!".as_bytes()),
        (16382, "中!".as_bytes()),
        (16383, "é!".as_bytes()),
        (16384, "🙂!".as_bytes()),
        (32767, "🙂!".as_bytes()),
        (16383, &[0xf0, 0x90][..]),
        (16383, &[0xf0, 0x90, 0x80, 0x41][..]),
        (16384, &[0xff][..]),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("byte_chunks_{index}");
        let prefix = "a".repeat(padding);
        let mut input = prefix.as_bytes().to_vec();
        input.extend_from_slice(tail);
        cases.source.push_str(&format!(
            "\nfn {name}() -> Int32 {{ var data = \"{prefix}\".into_bytes(); "
        ));
        for byte in tail {
            cases.source.push_str(&format!("data.push({byte}); "));
        }
        cases.source.push_str("match string.from_utf8(data) { ");
        match std::str::from_utf8(&input) {
            Ok(text) => {
                let expected = literal(&text[padding..]);
                cases.source.push_str(&format!(
                    "Err(_) => 1, Ok(value) => {{ \
                     if value.find(\"!\").unwrap_or(0) != {} {{ return 2 }}; \
                     if value.find(\"z\").is_some() {{ return 3 }}; \
                     let tail = value.substring({padding}, value.byte_length()).unwrap_or(\"\"); \
                     if tail != {expected} {{ return 4 }}; \
                     var copy = Array.new::<UInt8>(); copy.push(0); \
                     array.extend_bytes(&mut copy, value.bytes(), 1, value.byte_length()); \
                     array.extend_bytes(&mut copy, value.bytes(), 0, 0); \
                     if copy.len() != value.byte_length() || copy[0] != 0 || copy[{}] != 33 {{ return 5 }}; \
                     42 }}",
                    text.len() - 1,
                    text.len() - 1,
                ));
            }
            Err(error) => {
                let code = if error.error_len().is_some() {
                    "InvalidSequence"
                } else {
                    "UnexpectedEnd"
                };
                cases.source.push_str(&format!(
                    "Ok(_) => 6, Err(error) => {{ if error.offset != {} {{ return 7 }}; \
                     match error.code {{ string.Utf8ErrorCode.{code} => 42, _ => 8 }} }}",
                    error.valid_up_to(),
                ));
            }
        }
        cases.source.push_str("} }\n");
        cases.integers.push((name, 42));
    }
    let mut corpus = vec![
        vec![],
        b"ASCII\0tail".to_vec(),
        "e\u{301}中🙂".as_bytes().to_vec(),
    ];
    for scalar in [
        0, 0x7f, 0x80, 0x7ff, 0x800, 0xd7ff, 0xe000, 0xffff, 0x10000, 0x10ffff,
    ] {
        corpus.push(char::from_u32(scalar).unwrap().to_string().into_bytes());
    }
    // Lead/continuation bounds, overlong encodings, surrogate and scalar bounds.
    for bytes in [
        vec![0x80],
        vec![0xbf],
        vec![0xc0, 0x80],
        vec![0xc1, 0xbf],
        vec![0xc2],
        vec![0xc2, 0x7f],
        vec![0xdf, 0xc0],
        vec![0xe0, 0x9f, 0xbf],
        vec![0xe0, 0xa0],
        vec![0xe1, 0x80],
        vec![0xe1, 0x80, 0x41],
        vec![0xed, 0xa0, 0x80],
        vec![0xed, 0xbf, 0xbf],
        vec![0xf0, 0x8f, 0xbf, 0xbf],
        vec![0xf0, 0x90],
        vec![0xf1, 0x80, 0x80],
        vec![0xf1, 0x80, 0x80, 0xc0],
        vec![0xf4, 0x90, 0x80, 0x80],
        vec![0xf5, 0x80, 0x80, 0x80],
        vec![0xff],
        vec![0xfe],
    ] {
        corpus.push(bytes.clone());
        let mut prefixed = b"a\0b".to_vec();
        prefixed.extend(bytes);
        corpus.push(prefixed);
    }
    let mut seed = 0x7c19_a531u32;
    for _ in 0..32 {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        corpus.push(seed.to_le_bytes().to_vec());
        if let Some(scalar) = char::from_u32(seed % 0x110000) {
            corpus.push(scalar.to_string().into_bytes());
        }
    }
    for (index, bytes) in corpus.iter().enumerate() {
        let name = format!("utf8_{index}");
        cases.source.push_str(&format!(
            "\nfn {name}() -> Int32 {{ var data = Array.new::<UInt8>(); "
        ));
        for byte in bytes {
            cases.source.push_str(&format!("data.push({byte}); "));
        }
        cases.source.push_str("match string.from_utf8(data) { ");
        match std::str::from_utf8(bytes) {
            Ok(value) => {
                cases.source.push_str(&format!("Err(_) => 1, Ok(value) => {{ let expected = {}; if value != expected {{ return 2 }}; if value.rune_count() != {} {{ return 3 }}; ", literal(value), value.chars().count()));
                for (offset, scalar) in value.char_indices() {
                    let next = offset + scalar.len_utf8();
                    cases.source.push_str(&format!("match value.next_rune({offset}) {{ Some((rune, next)) => {{ if rune.to_u32() != {} || next != {next} {{ return 4 }} }}, None => {{ return 5 }} }}; ", u32::from(scalar)));
                }
                cases.source.push_str(
                    "if value.next_rune(value.byte_length()).is_some() { return 6 }; 42 }",
                );
            }
            Err(error) => {
                let code = if error.error_len().is_some() {
                    "InvalidSequence"
                } else {
                    "UnexpectedEnd"
                };
                cases.source.push_str(&format!("Ok(_) => 7, Err(error) => {{ if error.offset != {} {{ return 8 }}; match error.code {{ string.Utf8ErrorCode.{code} => 42, _ => 9 }} }}", error.valid_up_to()));
            }
        }
        cases.source.push_str("} }\n");
        cases.integers.push((name, 42));
    }
    for (text_index, value) in ["", "aé中🙂\0", "e\u{301}"].into_iter().enumerate() {
        let name = format!("runes_{text_index}");
        cases.source.push_str(&format!("fn {name}() -> Int32 {{ let text = {}; let expected: Array<UInt32> = [{}]; var count: UInt64 = 0; for rune in text.runes() {{ if expected.get(count).unwrap_or(UInt32.MAX) != rune.to_u32() {{ return 1 }}; count += 1 }}; if count != {} {{ return 2 }}; var cursor = string.runes(&text); ", literal(value), value.chars().map(|rune| u32::from(rune).to_string()).collect::<Vec<_>>().join(", "), value.chars().count()));
        for scalar in value.chars() {
            cases.source.push_str(&format!("{{ let (next, item) = iter.step_shared(cursor); cursor = next; match item {{ Some(rune) => {{ if rune.to_u32() != {} {{ return 3 }} }}, None => {{ return 6 }} }} }}; ", u32::from(scalar)));
        }
        cases.source.push_str("let (end, item) = iter.step_shared(cursor); if item.is_some() { return 4 }; let (_, item) = iter.step_shared(end); if item.is_some() { return 5 }; 42 }\n");
        cases.integers.push((name, 42));
        for start in 0..=value.len() + 1 {
            for end in 0..=value.len() + 1 {
                let name = format!("substring_{text_index}_{start}_{end}");
                cases.source.push_str(&format!("fn {name}() -> Int32 {{ let original = {}; let result = original.substring({start}, {end}); original.release(); match result {{ ", literal(value)));
                if let Some(expected) = value.get(start..end) {
                    cases.source.push_str(&format!("None => 1, Some(value) => {{ let expected = {}; if value == expected {{ 42 }} else {{ 2 }} }}", literal(expected)));
                } else {
                    cases.source.push_str("None => 42, Some(_) => 3");
                }
                cases.source.push_str("} }\n");
                cases.integers.push((name, 42));
            }
        }
    }
    for (ty, values) in [
        (
            "Int32",
            vec![
                i32::MIN.to_string(),
                "-10".into(),
                "-1".into(),
                "0".into(),
                "9".into(),
                "10".into(),
                i32::MAX.to_string(),
            ],
        ),
        (
            "Int64",
            vec![
                i64::MIN.to_string(),
                "-1000000000000000000".into(),
                "-1".into(),
                "0".into(),
                "10".into(),
                i64::MAX.to_string(),
            ],
        ),
        (
            "UInt32",
            vec!["0".into(), "1".into(), "10".into(), u32::MAX.to_string()],
        ),
        (
            "UInt64",
            vec![
                "0".into(),
                "9".into(),
                "10".into(),
                "10000000000000000000".into(),
                u64::MAX.to_string(),
            ],
        ),
    ] {
        for (index, value) in values.into_iter().enumerate() {
            let name = format!("decimal_{ty}_{index}");
            cases.source.push_str(&format!(
                "fn {name}() -> String {{ let value: {ty} = {value}; value.to_string() }}\n"
            ));
            cases.strings.push((name, value));
        }
    }
    cases
}

fn text_helpers(cases: &mut Cases) {
    let whitespace: String = (0..=0x3000)
        .filter_map(char::from_u32)
        .filter(|c| c.is_whitespace())
        .collect();
    for (index, input) in [
        String::new(),
        whitespace.clone(),
        format!("{whitespace}中 🙂{whitespace}"),
        " \tleft right\r\n".into(),
        "🙂中".into(),
        " \u{200b}text\u{feff} ".into(),
        "\0 middle \0".into(),
    ]
    .into_iter()
    .enumerate()
    {
        for (method, expected) in [
            ("trim", input.trim()),
            ("trim_start", input.trim_start()),
            ("trim_end", input.trim_end()),
        ] {
            let name = format!("{method}_{index}");
            let call = if index % 2 == 0 {
                format!("original.{method}()")
            } else {
                format!("string.{method}(&original)")
            };
            cases.source.push_str(&format!("\nfn {name}() -> String {{ let original = {}; let result = {call}; original.release(); result }}", literal(&input)));
            cases.strings.push((name, expected.into()));
        }
    }
    for (index, (input, needle)) in [
        ("", ""),
        ("", "x"),
        ("text", ""),
        ("中🙂中", "中"),
        ("中🙂中", "🙂"),
        ("a=b=c", "="),
        ("same", "same"),
        ("a\0b", "\0"),
        ("é", "e"),
        ("a", "longer"),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("text_helpers_{index}");
        cases.source.push_str(&format!("\nfn {name}() -> Int32 {{ let input = {}; let needle = {}; if input.contains(&needle) != {} {{ return 1 }}; ", literal(input), literal(needle), input.contains(needle)));
        for (method, expected) in [
            ("strip_prefix", input.strip_prefix(needle)),
            ("strip_suffix", input.strip_suffix(needle)),
        ] {
            let check = expected.map_or("None => {}, Some(_) => return 2".into(), |value| {
                format!(
                    "Some(value) => {{ if value != {} {{ return 3 }} }}, None => return 4",
                    literal(value)
                )
            });
            cases.source.push_str(&format!(
                "match string.{method}(&input, &needle) {{ {check} }}; "
            ));
        }
        cases.source.push_str("let result = input.split_once(&needle); input.release(); needle.release(); match result { ");
        if let Some((left, right)) = input.split_once(needle) {
            cases.source.push_str(&format!("Some((left, right)) => {{ if left == {} && right == {} {{ 42 }} else {{ 5 }} }}, None => 6", literal(left), literal(right)));
        } else {
            cases.source.push_str("None => 42, Some(_) => 7");
        }
        cases.source.push_str("} }\n");
        cases.integers.push((name, 42));
    }
}

fn ascii_and_replace(cases: &mut Cases) {
    let ascii: String = (0..=127).map(char::from).collect();
    for (index, input) in ["", ascii.as_str(), "HeLLo Éé中🙂\0İß"]
        .into_iter()
        .enumerate()
    {
        for (method, expected) in [
            ("to_ascii_lowercase", input.to_ascii_lowercase()),
            ("to_ascii_uppercase", input.to_ascii_uppercase()),
        ] {
            let name = format!("{method}_{index}");
            let call = if index % 2 == 0 {
                format!("input.{method}()")
            } else {
                format!("string.{method}(&input)")
            };
            cases.source.push_str(&format!("\nfn {name}() -> String {{ let input = {}; let result = {call}; input.release(); result }}", literal(input)));
            cases.strings.push((name, expected));
        }
    }
    for (index, (left, right)) in [
        ("", ""),
        ("Content-TYPE", "content-type"),
        ("A", "aa"),
        ("ABC", "abd"),
        ("ÉA中🙂\0z", "Éa中🙂\0Z"),
        ("É", "é"),
        ("[\\]^_`", "{|}~_`"),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("ascii_equal_{index}");
        cases.source.push_str(&format!("\nfn {name}() -> Int32 {{ let left = {}; let right = {}; if left.eq_ignore_ascii_case(&right) == {} && string.eq_ignore_ascii_case(&left, &right) == {} {{ 42 }} else {{ 1 }} }}", literal(left), literal(right), left.eq_ignore_ascii_case(right), left.eq_ignore_ascii_case(right)));
        cases.integers.push((name, 42));
    }
    for (index, (input, from, to)) in [
        ("", "", "-"),
        ("", "x", "y"),
        ("aé中🙂", "", "|"),
        ("text", "", ""),
        ("aaaaa", "aa", "b"),
        ("aaa", "a", "aa"),
        ("a-b-a", "a", ""),
        ("中🙂中", "中", "é"),
        ("x\0y\0", "\0", "🙂"),
        ("é", "e", "X"),
        ("small", "longer pattern", ""),
        ("same", "same", "same"),
        ("ends!", "!", "?"),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("replace_{index}");
        let call = if index % 2 == 0 {
            "input.replace(&from, &to)"
        } else {
            "string.replace(&input, &from, &to)"
        };
        cases.source.push_str(&format!("\nfn {name}() -> String {{ let input = {}; let from = {}; let to = {}; let result = {call}; input.release(); from.release(); to.release(); result }}", literal(input), literal(from), literal(to)));
        cases.strings.push((name, input.replace(from, to)));
    }
}
