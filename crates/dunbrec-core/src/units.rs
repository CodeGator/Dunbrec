//! Parsing and formatting of engineering values such as `4k7`, `10uF`, `1meg`.
//!
//! Two parsing dialects exist because SPICE and humans disagree about `M`:
//!
//! * [`Dialect::Spice`]: case-insensitive, SPICE semantics. `m` and `M` are
//!   milli, `meg` is mega, `mil` is 25.4 µm, `f`/`F` is femto.
//! * [`Dialect::Engineering`]: case-sensitive SI prefixes, used by the UI.
//!   `m` is milli, `M` is mega, `f` is femto. Uppercase `F` (farad), `H`, `V`
//!   etc. are treated as unit names and ignored.
//!
//! Both dialects accept "RKM" notation where the prefix replaces the decimal
//! point (`4k7` = 4.7e3, `2R2` = 2.2, `1M5` = 1.5e6 in engineering dialect),
//! and both ignore trailing unit letters (`10uF`, `4.7kΩ`, `5V`).

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Spice,
    Engineering,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum UnitError {
    #[error("empty value")]
    Empty,
    #[error("'{0}' does not start with a number")]
    NoNumber(String),
    #[error("unexpected text '{rest}' in value '{input}'")]
    TrailingGarbage { input: String, rest: String },
}

/// Parse a value using SPICE semantics (see module docs).
pub fn parse_spice(input: &str) -> Result<f64, UnitError> {
    parse(input, Dialect::Spice)
}

/// Parse a value using case-sensitive SI semantics (see module docs).
pub fn parse_eng(input: &str) -> Result<f64, UnitError> {
    parse(input, Dialect::Engineering)
}

pub fn parse(input: &str, dialect: Dialect) -> Result<f64, UnitError> {
    let s = input.trim();
    if s.is_empty() {
        return Err(UnitError::Empty);
    }

    let (number, rest) = split_number(s).ok_or_else(|| UnitError::NoNumber(s.to_string()))?;
    let rest = rest.trim_start();
    let mut value: f64 = number
        .parse()
        .map_err(|_| UnitError::NoNumber(s.to_string()))?;

    let (scale, consumed) = match_prefix(rest, dialect);
    let mut rest = &rest[consumed..];

    // RKM notation: "4k7" -> digits directly after the prefix are fractional.
    // Only valid when the leading number had no decimal point or exponent.
    let mut fraction = "";
    if consumed > 0
        && number
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'-' || b == b'+')
    {
        let frac_len = rest.bytes().take_while(u8::is_ascii_digit).count();
        fraction = &rest[..frac_len];
        rest = &rest[frac_len..];
    }

    match scale {
        // Build the decimal text and let the standard parser round it, so
        // "10u" is exactly the double nearest 1e-5 (multiplying by 1e-6,
        // which is not exact in binary, would be off in the last bit).
        Scale::Pow10(exp) => {
            let (mantissa, own_exp) = match number.find(['e', 'E']) {
                Some(i) => (&number[..i], number[i + 1..].parse::<i32>().unwrap_or(0)),
                None => (number, 0),
            };
            let text = if fraction.is_empty() {
                format!("{mantissa}e{}", own_exp + exp)
            } else {
                format!("{mantissa}.{fraction}e{}", own_exp + exp)
            };
            value = text
                .parse()
                .map_err(|_| UnitError::NoNumber(s.to_string()))?;
        }
        Scale::Factor(f) => {
            if !fraction.is_empty() {
                let frac: f64 = format!("0.{fraction}").parse().unwrap_or(0.0);
                value += if number.starts_with('-') { -frac } else { frac };
            }
            value *= f;
        }
    }

    // Remaining characters must be a unit name (letters / Ω / µ), which we ignore.
    if rest
        .chars()
        .all(|c| c.is_alphabetic() || c == 'Ω' || c == 'µ' || c == 'μ')
    {
        Ok(value)
    } else {
        Err(UnitError::TrailingGarbage {
            input: s.to_string(),
            rest: rest.to_string(),
        })
    }
}

/// Split the leading numeric literal (sign, digits, fraction, exponent) off `s`.
fn split_number(s: &str) -> Option<(&str, &str)> {
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let int_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let mut digits = i - int_start;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let frac_start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        digits += i - frac_start;
    }
    if digits == 0 {
        return None;
    }
    // Exponent only if 'e' is followed by an (optionally signed) digit,
    // so that e.g. "1meg" is not misread.
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        if j < b.len() && b[j].is_ascii_digit() {
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            i = j;
        }
    }
    Some((&s[..i], &s[i..]))
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Scale {
    /// Multiply by 10^n (all SI prefixes).
    Pow10(i32),
    /// Multiply by an arbitrary factor (`mil`).
    Factor(f64),
}

/// Returns (scale, bytes consumed) for a scale prefix at the start of `rest`.
fn match_prefix(rest: &str, dialect: Dialect) -> (Scale, usize) {
    match dialect {
        Dialect::Spice => {
            let lower = rest.to_ascii_lowercase();
            if lower.starts_with("mil") {
                return (Scale::Factor(25.4e-6), 3);
            }
            const TABLE: &[(&str, i32)] = &[
                ("meg", 6),
                ("t", 12),
                ("g", 9),
                ("k", 3),
                ("m", -3),
                ("u", -6),
                ("µ", -6),
                ("μ", -6),
                ("n", -9),
                ("p", -12),
                ("f", -15),
                ("r", 0), // RKM "2R2"
            ];
            for (p, exp) in TABLE {
                if lower.starts_with(p) {
                    return (Scale::Pow10(*exp), p.len());
                }
            }
            (Scale::Pow10(0), 0)
        }
        Dialect::Engineering => {
            const TABLE: &[(&str, i32)] = &[
                ("T", 12),
                ("G", 9),
                ("M", 6),
                ("k", 3),
                ("K", 3),
                ("m", -3),
                ("u", -6),
                ("µ", -6),
                ("μ", -6),
                ("n", -9),
                ("p", -12),
                ("f", -15),
            ];
            for (p, exp) in TABLE {
                if rest.starts_with(p) {
                    return (Scale::Pow10(*exp), p.len());
                }
            }
            // RKM "2R2": an R immediately followed by a digit.
            let b = rest.as_bytes();
            if b.len() >= 2 && (b[0] == b'R' || b[0] == b'r') && b[1].is_ascii_digit() {
                return (Scale::Pow10(0), 1);
            }
            (Scale::Pow10(0), 0)
        }
    }
}

/// Format a value with an SI prefix and unit, e.g. `format_value(4700.0, "Ω")`
/// gives `"4.7 kΩ"`. Uses up to 4 significant digits, trailing zeros trimmed.
pub fn format_value(value: f64, unit: &str) -> String {
    let sep = if unit.is_empty() { "" } else { " " };
    if value == 0.0 || !value.is_finite() {
        return format!("{value}{sep}{unit}");
    }
    const PREFIXES: &[(f64, &str)] = &[
        (1e12, "T"),
        (1e9, "G"),
        (1e6, "M"),
        (1e3, "k"),
        (1.0, ""),
        (1e-3, "m"),
        (1e-6, "µ"),
        (1e-9, "n"),
        (1e-12, "p"),
        (1e-15, "f"),
    ];
    let mag = value.abs();
    let (scale, prefix) = PREFIXES
        .iter()
        .copied()
        .find(|(s, _)| mag >= *s * 0.999_95)
        .unwrap_or((1e-15, "f"));
    let scaled = value / scale;
    let decimals = if scaled.abs() >= 100.0 {
        1
    } else if scaled.abs() >= 10.0 {
        2
    } else {
        3
    };
    let mut text = format!("{scaled:.decimals$}");
    if text.contains('.') {
        text = text.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    let space = if prefix.is_empty() && unit.is_empty() {
        ""
    } else {
        " "
    };
    format!("{text}{space}{prefix}{unit}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    #[test]
    fn spice_basic() {
        assert_relative_eq!(parse_spice("10").unwrap(), 10.0);
        assert_relative_eq!(parse_spice("1k").unwrap(), 1e3);
        assert_relative_eq!(parse_spice("1K").unwrap(), 1e3);
        assert_relative_eq!(parse_spice("1meg").unwrap(), 1e6);
        assert_relative_eq!(parse_spice("1MEG").unwrap(), 1e6);
        assert_relative_eq!(parse_spice("1M").unwrap(), 1e-3);
        assert_relative_eq!(parse_spice("10uF").unwrap(), 10e-6);
        assert_relative_eq!(parse_spice("1F").unwrap(), 1e-15);
        assert_relative_eq!(parse_spice("2.2e-3").unwrap(), 2.2e-3);
        assert_relative_eq!(parse_spice("1e3k").unwrap(), 1e6);
        assert_relative_eq!(parse_spice("-5V").unwrap(), -5.0);
        assert_relative_eq!(parse_spice(".5").unwrap(), 0.5);
        assert_relative_eq!(parse_spice("10mil").unwrap(), 254e-6);
    }

    #[test]
    fn rkm_notation() {
        assert_relative_eq!(parse_spice("4k7").unwrap(), 4700.0);
        assert_relative_eq!(parse_spice("2R2").unwrap(), 2.2);
        assert_relative_eq!(parse_eng("1M5").unwrap(), 1.5e6);
        assert_relative_eq!(parse_eng("4n7").unwrap(), 4.7e-9);
        assert_relative_eq!(parse_eng("2R2").unwrap(), 2.2);
    }

    #[test]
    fn engineering_is_case_sensitive() {
        assert_relative_eq!(parse_eng("1M").unwrap(), 1e6);
        assert_relative_eq!(parse_eng("1m").unwrap(), 1e-3);
        assert_relative_eq!(parse_eng("1F").unwrap(), 1.0);
        assert_relative_eq!(parse_eng("1f").unwrap(), 1e-15);
        assert_relative_eq!(parse_eng("10mF").unwrap(), 10e-3);
        assert_relative_eq!(parse_eng("4.7kΩ").unwrap(), 4700.0);
        assert_relative_eq!(parse_eng("100µH").unwrap(), 100e-6);
        assert_relative_eq!(parse_eng("1 k").unwrap(), 1e3);
    }

    #[test]
    fn prefixes_are_exact() {
        // Bit-for-bit equal to the literal, not just close.
        assert_eq!(parse_spice("10u").unwrap(), 1e-5);
        assert_eq!(parse_spice("4k7").unwrap(), 4.7e3);
        assert_eq!(parse_spice("2.2n").unwrap(), 2.2e-9);
        assert_eq!(parse_spice("-4k7").unwrap(), -4.7e3);
        assert_eq!(parse_spice("1e3k").unwrap(), 1e6);
        assert_eq!(parse_eng("0.1u").unwrap(), 1e-7);
        assert_eq!(parse_eng("4n7").unwrap(), 4.7e-9);
        assert_eq!(parse_eng("100").unwrap(), 100.0);
    }

    #[test]
    fn errors() {
        assert_eq!(parse_spice(""), Err(UnitError::Empty));
        assert!(matches!(parse_spice("abc"), Err(UnitError::NoNumber(_))));
        assert!(matches!(
            parse_eng("10k$"),
            Err(UnitError::TrailingGarbage { .. })
        ));
    }

    #[test]
    fn formatting() {
        assert_eq!(format_value(4700.0, "Ω"), "4.7 kΩ");
        assert_eq!(format_value(1e-6, "F"), "1 µF");
        assert_eq!(format_value(0.0, "V"), "0 V");
        assert_eq!(format_value(-0.0123, "A"), "-12.3 mA");
        assert_eq!(format_value(150e3, "Hz"), "150 kHz");
        assert_eq!(format_value(3.3, "V"), "3.3 V");
        assert_eq!(format_value(999.9999, ""), "1 k");
    }
}
