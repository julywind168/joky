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
            "\nfn {name}() -> Int32 {{ var data = Array.new::<UInt8>() "
        ));
        for byte in bytes {
            cases.source.push_str(&format!("data.push({byte}) "));
        }
        cases.source.push_str("match text.from_utf8(data) { ");
        match std::str::from_utf8(bytes) {
            Ok(value) => {
                cases.source.push_str(&format!("| Err(_) => 1 | Ok(value) => {{ let expected = {} if !value.equals(&expected) {{ return 2 }} if value.rune_count() != {} {{ return 3 }} ", literal(value), value.chars().count()));
                for (offset, scalar) in value.char_indices() {
                    let next = offset + scalar.len_utf8();
                    cases.source.push_str(&format!("match value.next_rune({offset}) {{ | Some((rune, next)) => {{ if rune.to_u32() != {} || next != {next} {{ return 4 }} }} | None => {{ return 5 }} }} ", u32::from(scalar)));
                }
                cases.source.push_str(
                    "if value.next_rune(value.byte_length()).is_some() { return 6 } 42 }",
                );
            }
            Err(error) => {
                let code = if error.error_len().is_some() {
                    "InvalidSequence"
                } else {
                    "UnexpectedEnd"
                };
                cases.source.push_str(&format!("| Ok(_) => 7 | Err(error) => {{ if error.offset != {} {{ return 8 }} match error.code {{ | text.Utf8ErrorCode.{code} => 42 | _ => 9 }} }}", error.valid_up_to()));
            }
        }
        cases.source.push_str("} }\n");
        cases.integers.push((name, 42));
    }
    for (text_index, value) in ["", "aé中🙂\0", "e\u{301}"].into_iter().enumerate() {
        for start in 0..=value.len() + 1 {
            for end in 0..=value.len() + 1 {
                let name = format!("substring_{text_index}_{start}_{end}");
                cases.source.push_str(&format!("fn {name}() -> Int32 {{ let original = {} let result = original.substring({start}, {end}) original.release() match result {{ ", literal(value)));
                if let Some(expected) = value.get(start..end) {
                    cases.source.push_str(&format!("| None => 1 | Some(value) => {{ let expected = {} if value.equals(&expected) {{ 42 }} else {{ 2 }} }}", literal(expected)));
                } else {
                    cases.source.push_str("| None => 42 | Some(_) => 3");
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
                "fn {name}() -> String {{ let value: {ty} = {value} value.to_string() }}\n"
            ));
            cases.strings.push((name, value));
        }
    }
    cases
}
