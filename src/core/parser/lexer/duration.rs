//! Exact decimal duration literals, independent of floating-point rounding.
pub(super) fn parse(spelling: &str) -> Option<Result<u64, &'static str>> {
    let (number, scale) = [
        ("min", 60_000_000_000u64),
        ("ns", 1),
        ("us", 1_000),
        ("ms", 1_000_000),
        ("s", 1_000_000_000),
        ("h", 3_600_000_000_000),
    ]
    .into_iter()
    .find_map(|(unit, scale)| spelling.strip_suffix(unit).map(|n| (n, scale)))?;
    Some(convert(number, scale))
}

fn convert(number: &str, scale: u64) -> Result<u64, &'static str> {
    const MALFORMED: &str = "Duration literals require decimal digits with optional fractional digits; underscores must separate digits";
    const RANGE: &str = "Duration literal exceeds UInt64 nanoseconds";
    const PRECISION: &str = "Duration literal must represent a whole number of nanoseconds";
    fn digits(part: &str) -> bool {
        !part.is_empty()
            && part.bytes().enumerate().all(|(i, c)| {
                c.is_ascii_digit()
                    || c == b'_'
                        && i > 0
                        && i + 1 < part.len()
                        && part.as_bytes()[i - 1].is_ascii_digit()
                        && part.as_bytes()[i + 1].is_ascii_digit()
            })
    }
    let (whole, fraction) = number
        .split_once('.')
        .map_or((number, None), |(w, f)| (w, Some(f)));
    if !digits(whole) || fraction.is_some_and(|f| !digits(f)) {
        return Err(MALFORMED);
    }
    let whole = whole.replace('_', "").parse::<u64>().map_err(|_| RANGE)?;
    let whole = whole.checked_mul(scale).ok_or(RANGE)?;
    let fraction = fraction.unwrap_or("").replace('_', "");
    let fraction = fraction.trim_end_matches('0');
    if fraction.is_empty() {
        return Ok(whole);
    }
    let power = u32::try_from(fraction.len()).map_err(|_| PRECISION)?;
    let denominator = 10u128.checked_pow(power).ok_or(PRECISION)?;
    let numerator = fraction
        .parse::<u128>()
        .ok()
        .and_then(|n| n.checked_mul(u128::from(scale)))
        .ok_or(PRECISION)?;
    if numerator % denominator != 0 {
        return Err(PRECISION);
    }
    whole
        .checked_add((numerator / denominator) as u64)
        .ok_or(RANGE)
}

#[cfg(test)]
mod tests {
    use crate::core::parser::{Lexer, TokenKind};
    #[test]
    fn duration_literals_are_exact_and_bounded() {
        for (source, nanos) in [
            ("250ms", 250_000_000),
            ("1.5s", 1_500_000_000),
            ("2min", 120_000_000_000),
            ("1h", 3_600_000_000_000),
            ("1_000.000_001ms", 1_000_000_001),
            ("0.0000000000025h", 9),
            ("0.00000000005min", 3),
            ("0.0000000000000000000000000000000000000000000s", 0),
            ("18446744073709551615ns", u64::MAX),
            ("18446744073.709551615s", u64::MAX),
        ] {
            let tokens = Lexer::tokenize(source).unwrap_or_else(|e| panic!("{source}: {e}"));
            assert_eq!(tokens[0].kind, TokenKind::DurationLit(nanos), "{source}");
        }
        for source in ["1.1ns", "0.1us", "1.5s"] {
            // Fractional units are valid exactly when their nanosecond count is integral.
            assert_eq!(Lexer::tokenize(source).is_ok(), source != "1.1ns");
        }
        for source in [
            "18446744073709551616ns",
            "18446744073.709551616s",
            "18446744073709551615h",
            "0.0000000001s",
            "1__0ms",
            "1_ms",
            "1.5_ms",
            "1e3ms",
            "1.5e-3s",
            "0x10ms",
            "1m",
            "1MS",
            "1msmore",
        ] {
            assert!(Lexer::tokenize(source).is_err(), "accepted {source}");
        }
    }
}
