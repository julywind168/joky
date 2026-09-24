use super::*;

#[test]
fn integer_casts_follow_the_losslessness_matrix() {
    run_program(include_str!("../../../tests/fixtures/casts.jk"));
}

#[test]
fn checked_cast_accepts_the_signed_minimum() {
    run_program(
        r#"
        fn narrow(value: Int64) -> Result(Int8, String) { value as? Int8 }
        fn main() {
            match narrow(-128) {
                Ok(value) => if value as Int32 != -128 { panic("wrong minimum") }
                Err(message) => panic(message)
            }
        }
        "#,
    );
}

#[test]
fn integer_casts_cover_every_type_pair_and_boundary() {
    let types = [
        ("Int8", 8, true),
        ("Int16", 16, true),
        ("Int32", 32, true),
        ("Int64", 64, true),
        ("UInt8", 8, false),
        ("UInt16", 16, false),
        ("UInt32", 32, false),
        ("UInt64", 64, false),
    ];
    let bounds = |width: u32, signed: bool| -> (i128, i128) {
        if signed {
            (-(1i128 << (width - 1)), (1i128 << (width - 1)) - 1)
        } else {
            (0, (1i128 << width) - 1)
        }
    };
    for (source, source_width, source_signed) in types {
        let (source_min, source_max) = bounds(source_width, source_signed);
        let mut functions = String::new();
        let mut body = String::new();
        let mut case = 0;
        for (target, target_width, target_signed) in types {
            let (target_min, target_max) = bounds(target_width, target_signed);
            let lossless = target_min <= source_min && source_max <= target_max;
            let mode = if lossless { "as" } else { "as%" };
            functions.push_str(&format!(
                "fn convert_{target}(value: {source}) -> {target} {{ value {mode} {target} }}\n"
            ));
            if !lossless {
                functions.push_str(&format!(
                    "fn checked_{target}(value: {source}) -> Result({target}, String) {{ value as? {target} }}\n"
                ));
                functions.push_str(&format!(
                    "fn saturated_{target}(value: {source}) -> {target} {{ value as| {target} }}\n"
                ));
            }
            let values = [
                source_min,
                source_min + 1,
                source_max - 1,
                source_max,
                -1,
                0,
                1,
                target_min - 1,
                target_min,
                target_min + 1,
                target_max - 1,
                target_max,
                target_max + 1,
            ]
            .into_iter()
            .filter(|value| (source_min..=source_max).contains(value))
            .collect::<std::collections::BTreeSet<_>>();
            for value in values {
                case += 1;
                let modulus = 1i128 << target_width;
                let bits = value.rem_euclid(modulus);
                let expected = if target_signed && bits > target_max {
                    bits - modulus
                } else {
                    bits
                };
                body.push_str(&format!(
                    "let input_{case}: {source} = {value}; let expected_{case}: {target} = {expected}; \
                     if convert_{target}(input_{case}) != expected_{case} {{ panic(\"{source} {value} {mode} {target}\") }}\n"
                ));
                if !lossless {
                    let clamped = value.clamp(target_min, target_max);
                    body.push_str(&format!(
                        "let clamped_{case}: {target} = {clamped}; \
                         if saturated_{target}(input_{case}) != clamped_{case} {{ panic(\"{source} {value} as| {target}\") }}\n"
                    ));
                    let valid = (target_min..=target_max).contains(&value);
                    let arms = if valid {
                        format!(
                            "Ok(actual) => if actual != expected_{case} {{ panic(\"checked value changed\") }}; \
                             Err(message) => panic(message)"
                        )
                    } else {
                        format!(
                            "Ok(_) => panic(\"accepted {source} {value} as? {target}\"); \
                             Err(message) => if message != \"integer value out of range for {target}\" {{ panic(message) }}"
                        )
                    };
                    body.push_str(&format!(
                        "match checked_{target}(input_{case}) {{ {arms} }}\n"
                    ));
                }
            }
        }
        run_program(&format!("{functions} fn main() {{ {body} }}"));
    }
}

#[test]
fn integer_casts_fold_and_evaluate_operands_once() {
    run_program(
        r#"
        class Counter {
            var calls: Int32 = 0
            fn next(&self) -> Int32 { self.calls += 1; 255 }
        }
        fn propagate(value: Int32) -> Result(Int64, String) {
            let byte = value as? Int8 ?
            Ok(byte as Int64)
        }
        fn main() {
            let folded = 255 as% Int8 as% UInt64
            let expected: UInt64 = 18446744073709551615
            if folded != expected { panic("folded sign extension") }
            let unsigned = 65535 as% UInt8 as UInt64
            let expected_unsigned: UInt64 = 255
            if unsigned != expected_unsigned { panic("folded zero extension") }
            let minimum = ((-128) as? Int8)!
            if minimum as Int32 != -128 { panic("constant checked minimum") }
            if !((-129) as? Int8).is_err() { panic("constant checked underflow") }
            if !propagate(128).is_err() { panic("propagation failed") }
            if propagate(-128)! as% Int32 != -128 { panic("propagated minimum") }
            let counter = Counter()
            let wrapped = counter.next() as% Int8
            if wrapped as Int32 != -1 { panic("wrapping") }
            let checked = (counter.next() as? UInt8)!
            if checked as Int32 != 255 { panic("checked") }
            let widened = counter.next() as Int64
            if widened as% Int32 != 255 { panic("lossless") }
            let saturated = counter.next() as| Int8
            if saturated as Int32 != 127 { panic("saturating") }
            if counter.calls != 4 { panic("cast evaluated operand more than once") }
        }
    "#,
    );
}
