//! A small S-expression reader for KiCad files.

use thiserror::Error;

#[derive(Debug, Clone, PartialEq)]
pub enum Sexp {
    List(Vec<Sexp>),
    /// A bare word or number.
    Atom(String),
    /// A quoted string (quotes removed, escapes resolved).
    Str(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("S-expression error at byte {offset}: {message}")]
pub struct SexpError {
    pub offset: usize,
    pub message: String,
}

impl Sexp {
    /// Text of an atom or string.
    pub fn text(&self) -> Option<&str> {
        match self {
            Sexp::Atom(s) | Sexp::Str(s) => Some(s),
            Sexp::List(_) => None,
        }
    }

    pub fn number(&self) -> Option<f64> {
        self.text()?.parse().ok()
    }

    pub fn items(&self) -> &[Sexp] {
        match self {
            Sexp::List(v) => v,
            _ => &[],
        }
    }

    /// The first word of a list, e.g. `symbol` in `(symbol ...)`.
    pub fn head(&self) -> Option<&str> {
        self.items().first().and_then(Sexp::text)
    }

    /// Child lists whose head is `name`.
    pub fn children<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Sexp> + 'a {
        self.items().iter().filter(move |c| c.head() == Some(name))
    }

    pub fn child(&self, name: &str) -> Option<&Sexp> {
        self.items().iter().find(|c| c.head() == Some(name))
    }

    /// The `i`-th argument (after the head) as text.
    pub fn arg(&self, i: usize) -> Option<&str> {
        self.items().get(i + 1).and_then(Sexp::text)
    }

    pub fn arg_number(&self, i: usize) -> Option<f64> {
        self.items().get(i + 1).and_then(Sexp::number)
    }
}

pub fn parse(text: &str) -> Result<Sexp, SexpError> {
    let bytes = text.as_bytes();
    let mut pos = 0;
    let skip_ws = |pos: &mut usize| {
        while *pos < bytes.len() && bytes[*pos].is_ascii_whitespace() {
            *pos += 1;
        }
    };
    let mut stack: Vec<Vec<Sexp>> = Vec::new();
    loop {
        skip_ws(&mut pos);
        if pos >= bytes.len() {
            return Err(SexpError {
                offset: pos,
                message: "unexpected end of file (unbalanced parentheses)".into(),
            });
        }
        match bytes[pos] {
            b'(' => {
                stack.push(Vec::new());
                pos += 1;
            }
            b')' => {
                pos += 1;
                let done = stack.pop().ok_or(SexpError {
                    offset: pos,
                    message: "unexpected ')'".into(),
                })?;
                let list = Sexp::List(done);
                match stack.last_mut() {
                    Some(parent) => parent.push(list),
                    None => return Ok(list),
                }
            }
            b'"' => {
                pos += 1;
                let mut s = String::new();
                let start = pos;
                loop {
                    let Some(&c) = bytes.get(pos) else {
                        return Err(SexpError {
                            offset: start,
                            message: "unterminated string".into(),
                        });
                    };
                    pos += 1;
                    match c {
                        b'"' => break,
                        b'\\' => {
                            let e = bytes.get(pos).copied().unwrap_or(b'\\');
                            pos += 1;
                            s.push(match e {
                                b'n' => '\n',
                                b't' => '\t',
                                other => other as char,
                            });
                        }
                        _ => {
                            // Copy whole UTF-8 sequences unchanged.
                            let len = utf8_len(c);
                            let end = (pos - 1 + len).min(bytes.len());
                            s.push_str(&text[pos - 1..end]);
                            pos = end;
                        }
                    }
                }
                push_atom(&mut stack, Sexp::Str(s), pos)?;
            }
            _ => {
                let start = pos;
                while pos < bytes.len()
                    && !bytes[pos].is_ascii_whitespace()
                    && bytes[pos] != b'('
                    && bytes[pos] != b')'
                {
                    pos += 1;
                }
                push_atom(&mut stack, Sexp::Atom(text[start..pos].to_string()), pos)?;
            }
        }
    }
}

fn utf8_len(first: u8) -> usize {
    match first {
        0xF0..=0xF7 => 4,
        0xE0..=0xEF => 3,
        0xC0..=0xDF => 2,
        _ => 1,
    }
}

fn push_atom(stack: &mut [Vec<Sexp>], atom: Sexp, pos: usize) -> Result<(), SexpError> {
    match stack.last_mut() {
        Some(list) => {
            list.push(atom);
            Ok(())
        }
        None => Err(SexpError {
            offset: pos,
            message: "text outside any list".into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_lists_strings_and_numbers() {
        let s = parse(r#"(kicad_sch (version 20230121) (label "V OUT \"x\"" (at 1.27 -2.54 90)) (title "Ω µ"))"#)
            .unwrap();
        assert_eq!(s.head(), Some("kicad_sch"));
        assert_eq!(s.child("version").unwrap().arg_number(0), Some(20230121.0));
        let label = s.child("label").unwrap();
        assert_eq!(label.arg(0), Some("V OUT \"x\""));
        let at = label.child("at").unwrap();
        assert_eq!(
            (at.arg_number(0), at.arg_number(1), at.arg_number(2)),
            (Some(1.27), Some(-2.54), Some(90.0))
        );
        assert_eq!(s.child("title").unwrap().arg(0), Some("Ω µ"));
    }

    #[test]
    fn errors() {
        assert!(parse("(a (b)").is_err());
        assert!(parse("(a \"unterminated)").is_err());
        assert!(parse(")").is_err());
    }
}
