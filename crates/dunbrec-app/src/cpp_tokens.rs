//! The coloured pieces of an Arduino sketch, for the source view.
//!
//! This is deliberately basic: comments, text, numbers, preprocessor lines,
//! the words of the C++ language, and the names Arduino and the chip define.
//! It does not parse the program, so the user's own names stay plain.

use std::ops::Range;

use dunbrec_mcu::asm_text;

use crate::code_ui::Kind;

/// Words of the language, and the usual type names.
const KEYWORDS: &[&str] = &[
    "alignas",
    "auto",
    "bool",
    "boolean",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "const_cast",
    "constexpr",
    "continue",
    "default",
    "delete",
    "do",
    "double",
    "dynamic_cast",
    "else",
    "enum",
    "explicit",
    "extern",
    "false",
    "float",
    "for",
    "friend",
    "goto",
    "if",
    "inline",
    "int",
    "int16_t",
    "int32_t",
    "int64_t",
    "int8_t",
    "long",
    "mutable",
    "namespace",
    "new",
    "noexcept",
    "nullptr",
    "operator",
    "private",
    "protected",
    "public",
    "register",
    "reinterpret_cast",
    "return",
    "short",
    "signed",
    "size_t",
    "sizeof",
    "static",
    "static_cast",
    "String",
    "struct",
    "switch",
    "template",
    "this",
    "throw",
    "true",
    "try",
    "typedef",
    "typename",
    "uint16_t",
    "uint32_t",
    "uint64_t",
    "uint8_t",
    "union",
    "unsigned",
    "using",
    "virtual",
    "void",
    "volatile",
    "while",
    "word",
];

/// Functions of Arduino (and the few of avr-libc that sketches often use),
/// coloured where they are called.
const FUNCTIONS: &[&str] = &[
    "_delay_ms",
    "_delay_us",
    "abs",
    "analogRead",
    "analogReference",
    "analogWrite",
    "attachInterrupt",
    "bit",
    "bitClear",
    "bitRead",
    "bitSet",
    "bitWrite",
    "cli",
    "constrain",
    "cos",
    "delay",
    "delayMicroseconds",
    "detachInterrupt",
    "digitalPinToInterrupt",
    "digitalRead",
    "digitalWrite",
    "F",
    "highByte",
    "interrupts",
    "loop",
    "lowByte",
    "map",
    "max",
    "micros",
    "millis",
    "min",
    "noInterrupts",
    "noTone",
    "pinMode",
    "pow",
    "pulseIn",
    "pulseInLong",
    "random",
    "randomSeed",
    "sei",
    "setup",
    "shiftIn",
    "shiftOut",
    "sin",
    "sq",
    "sqrt",
    "tan",
    "tone",
];

/// Objects Arduino provides, coloured like its functions wherever they appear.
const OBJECTS: &[&str] = &["Serial"];

/// Names Arduino defines. The chip's own registers and bits are recognised
/// too.
const CONSTANTS: &[&str] = &[
    "A0",
    "A1",
    "A2",
    "A3",
    "A4",
    "A5",
    "A6",
    "A7",
    "BIN",
    "CHANGE",
    "DEC",
    "DEFAULT",
    "EXTERNAL",
    "F_CPU",
    "FALLING",
    "HALF_PI",
    "HEX",
    "HIGH",
    "INPUT",
    "INPUT_PULLUP",
    "INTERNAL",
    "LED_BUILTIN",
    "LOW",
    "LSBFIRST",
    "MSBFIRST",
    "OCT",
    "OUTPUT",
    "PI",
    "PROGMEM",
    "RISING",
    "TWO_PI",
];

fn ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// What colour a whole word gets, if any. `called` says that a `(` follows.
fn word_kind(word: &str, called: bool) -> Option<Kind> {
    if KEYWORDS.contains(&word) {
        Some(Kind::Keyword)
    } else if OBJECTS.contains(&word) || (called && FUNCTIONS.contains(&word)) {
        Some(Kind::Function)
    } else if CONSTANTS.contains(&word)
        // PORTB and TCCR0A, but not a variable that happens to be called "sp".
        || (word.len() > 1
            && !word.bytes().any(|c| c.is_ascii_lowercase())
            && asm_text::is_chip_name(word))
    {
        Some(Kind::Constant)
    } else {
        None
    }
}

/// The end of the quoted text starting at `from`, which is its quote mark; the
/// end of the line if it is never closed.
fn quoted_end(b: &[u8], from: usize) -> usize {
    let quote = b[from];
    let mut i = from + 1;
    while i < b.len() && b[i] != quote {
        if b[i] == b'\\' {
            i += 1;
        }
        i += 1;
    }
    (i + 1).min(b.len())
}

/// The coloured pieces of one line of a sketch, in order. Gaps are plain.
///
/// `in_comment` says whether the line starts inside a `/* ... */` comment, and
/// is left saying whether the next one does.
pub fn tokenize(line: &str, in_comment: &mut bool) -> Vec<(Range<usize>, Kind)> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    // The end of the comment that is open at `from`, if it ends on this line.
    let close = |from: usize| line[from..].find("*/").map(|at| from + at + 2);

    if *in_comment {
        let end = close(0);
        *in_comment = end.is_none();
        i = end.unwrap_or(b.len());
        if i > 0 {
            out.push((0..i, Kind::Comment));
        }
    } else if line.trim_start().starts_with('#') {
        let hash = line.len() - line.trim_start().len();
        let skip_spaces = |mut at: usize| {
            while at < b.len() && b[at].is_ascii_whitespace() {
                at += 1;
            }
            at
        };
        let start = skip_spaces(hash + 1);
        i = start;
        while i < b.len() && ident(b[i]) {
            i += 1;
        }
        let name = &line[start..i];
        out.push((hash..i, Kind::Directive));
        if name == "include" {
            let from = skip_spaces(i);
            if b.get(from) == Some(&b'<') {
                i = line[from..].find('>').map_or(b.len(), |at| from + at + 1);
                out.push((from..i, Kind::Text));
            }
        }
    }

    while i < b.len() {
        let c = b[i];
        let next = b.get(i + 1).copied();
        let start = i;
        if c == b'/' && next == Some(b'/') {
            out.push((i..b.len(), Kind::Comment));
            break;
        } else if c == b'/' && next == Some(b'*') {
            let end = close(i + 2);
            *in_comment = end.is_none();
            i = end.unwrap_or(b.len());
            out.push((start..i, Kind::Comment));
        } else if c == b'"' || c == b'\'' {
            i = quoted_end(b, i);
            out.push((start..i, Kind::Text));
        } else if c.is_ascii_digit() || (c == b'.' && next.is_some_and(|n| n.is_ascii_digit())) {
            let hex = c == b'0' && matches!(next, Some(b'x' | b'X'));
            i += 1;
            while i < b.len() {
                // The sign of an exponent, as in 1.5e-3.
                let sign = !hex && matches!(b[i], b'+' | b'-') && matches!(b[i - 1], b'e' | b'E');
                if !(ident(b[i]) || b[i] == b'.' || b[i] == b'\'' || sign) {
                    break;
                }
                i += 1;
            }
            out.push((start..i, Kind::Number));
        } else if ident(c) {
            while i < b.len() && ident(b[i]) {
                i += 1;
            }
            let called = line[i..].trim_start().starts_with('(');
            if let Some(kind) = word_kind(&line[start..i], called) {
                out.push((start..i, kind));
            }
        } else {
            // Punctuation (and any multi-byte character).
            i += line[i..].chars().next().map_or(1, char::len_utf8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The coloured pieces of a line that starts outside any comment.
    fn kinds(line: &str) -> Vec<(&str, Kind)> {
        tokenize(line, &mut false)
            .into_iter()
            .map(|(r, k)| (&line[r], k))
            .collect()
    }

    #[test]
    fn a_sketch_line_is_coloured_by_what_its_words_are() {
        assert_eq!(
            kinds("  pinMode(LED_BUILTIN, OUTPUT); // the LED"),
            [
                ("pinMode", Kind::Function),
                ("LED_BUILTIN", Kind::Constant),
                ("OUTPUT", Kind::Constant),
                ("// the LED", Kind::Comment),
            ]
        );
        assert_eq!(
            kinds("const int ledPin = 13;"),
            [
                ("const", Kind::Keyword),
                ("int", Kind::Keyword),
                ("13", Kind::Number),
            ]
        );
        assert_eq!(
            kinds("void loop () { Serial.println(\"on\"); }"),
            [
                ("void", Kind::Keyword),
                ("loop", Kind::Function),
                ("Serial", Kind::Function),
                ("\"on\"", Kind::Text),
            ]
        );
        assert_eq!(
            kinds("PORTB |= _BV(PB5); DDRB = 0x20;"),
            [
                ("PORTB", Kind::Constant),
                ("PB5", Kind::Constant),
                ("DDRB", Kind::Constant),
                ("0x20", Kind::Number),
            ]
        );
    }

    #[test]
    fn the_users_own_names_stay_plain() {
        // Even ones spelt like a function or a register, in another way.
        assert_eq!(kinds("map = min + sp + brightness2;"), []);
        assert_eq!(kinds("blink(delayTime);"), []);
    }

    #[test]
    fn numbers_of_every_shape_are_one_piece() {
        for number in [
            "0", "500UL", "0xFF", "0b1010", "3.14", ".5", "1.5e-3", "1'000",
        ] {
            let line = format!("x = {number};");
            assert_eq!(kinds(&line), [(number, Kind::Number)], "{number}");
        }
        // A hexadecimal E is a digit, not an exponent.
        assert_eq!(kinds("0xE-1"), [("0xE", Kind::Number), ("1", Kind::Number)]);
    }

    #[test]
    fn text_keeps_what_looks_like_code_to_itself() {
        assert_eq!(
            kinds(r#"s = "a // b \" int"; c = '\''; d = '"';"#),
            [
                (r#""a // b \" int""#, Kind::Text),
                (r"'\''", Kind::Text),
                (r#"'"'"#, Kind::Text),
            ]
        );
        assert_eq!(kinds("\"open // x"), [("\"open // x", Kind::Text)]);
    }

    #[test]
    fn preprocessor_lines() {
        assert_eq!(
            kinds("#include <Servo.h>"),
            [("#include", Kind::Directive), ("<Servo.h>", Kind::Text)]
        );
        assert_eq!(
            kinds("  #  include \"pins.h\" // mine"),
            [
                ("#  include", Kind::Directive),
                ("\"pins.h\"", Kind::Text),
                ("// mine", Kind::Comment),
            ]
        );
        assert_eq!(
            kinds("#define PERIOD 500"),
            [("#define", Kind::Directive), ("500", Kind::Number)]
        );
    }

    #[test]
    fn a_block_comment_runs_over_lines_until_it_is_closed() {
        let mut open = false;
        let mut seen = Vec::new();
        for line in ["int a; /* one", "", "two int */ int b; /**/ /*/", "x */ if"] {
            for (r, k) in tokenize(line, &mut open) {
                seen.push((&line[r], k));
            }
        }
        assert!(!open);
        assert_eq!(
            seen,
            [
                ("int", Kind::Keyword),
                ("/* one", Kind::Comment),
                ("two int */", Kind::Comment),
                ("int", Kind::Keyword),
                ("/**/", Kind::Comment),
                ("/*/", Kind::Comment),
                ("x */", Kind::Comment),
                ("if", Kind::Keyword),
            ]
        );
    }

    #[test]
    fn any_text_gives_pieces_in_order_on_whole_characters() {
        for line in [
            "é ü “smart” 'é' \"é\\é",
            "#include <é",
            "#é",
            "#",
            "\\",
            "'\\",
            "/*é*/é//é",
            "1é.é",
        ] {
            for start_inside in [false, true] {
                let mut open = start_inside;
                let mut end = 0;
                for (r, _) in tokenize(line, &mut open) {
                    assert!(r.start >= end && r.end >= r.start, "{line}: {r:?}");
                    assert!(line.get(r.clone()).is_some(), "{line}: {r:?}");
                    end = r.end;
                }
            }
        }
    }

    #[test]
    fn the_lists_have_no_name_twice() {
        let mut all: Vec<&str> = [KEYWORDS, FUNCTIONS, OBJECTS, CONSTANTS].concat();
        let n = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), n);
    }
}
