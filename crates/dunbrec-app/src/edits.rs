//! Changes the assistant can suggest, and how they are checked and applied.
//!
//! The assistant writes its suggestions as fenced blocks in its reply:
//!
//! ````text
//! ```dunbrec-set
//! R2 = 1k
//! V1 = 5
//! RV1.wiper = 0.5
//! ```
//!
//! ```dunbrec-file main.ino
//! (the complete new text of the file)
//! ```
//! ````
//!
//! Blocks that add and remove parts are in [`crate::part_edits`].
//!
//! Nothing here touches the document until [`apply`] is called, which the
//! user triggers with an Apply button. Applying is one undo step.

use dunbrec_core::units::{format_value, parse_eng};
use dunbrec_core::{ComponentId, ComponentKind as K, Schematic, Waveform};

use crate::document::Document;
use crate::part_edits::{self, NewPart};

/// Fence tag of a block that changes part values.
pub const SET_TAG: &str = "dunbrec-set";
/// Fence tag of a block that replaces a firmware file (the file name follows).
pub const FILE_TAG: &str = "dunbrec-file";

/// One suggested change.
#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    Set(Vec<SetLine>),
    File {
        name: String,
        text: String,
    },
    /// New parts, below the drawing.
    Add(Vec<NewPart>),
    /// Parts to take out, by designator.
    Remove(Vec<String>),
}

/// `R2 = 1k`, or `RV1.wiper = 0.5`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetLine {
    pub part: String,
    pub wiper: bool,
    pub value: String,
}

/// Read the lines of a `dunbrec-set` block. Blank lines and `#` or `//`
/// comments are skipped.
pub fn parse_set(body: &str) -> Result<Vec<SetLine>, String> {
    let mut lines = Vec::new();
    for raw in body.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            return Err(format!("“{line}” is not of the form NAME = value."));
        };
        let (part, wiper) = match name.trim().split_once('.') {
            Some((part, prop)) if prop.trim().eq_ignore_ascii_case("wiper") => (part.trim(), true),
            Some(_) => return Err(format!("“{}”: only .wiper is understood.", name.trim())),
            None => (name.trim(), false),
        };
        if part.is_empty() || value.trim().is_empty() {
            return Err(format!("“{line}” is missing a name or a value."));
        }
        lines.push(SetLine {
            part: part.to_string(),
            wiper,
            value: value.trim().to_string(),
        });
    }
    if lines.is_empty() {
        return Err("The block has no changes in it.".into());
    }
    Ok(lines)
}

/// A value change that has been checked against the schematic.
#[derive(Debug, Clone, PartialEq)]
pub struct Planned {
    pub id: ComponentId,
    pub kind: K,
    /// "R2: 100 Ω → 1 kΩ".
    pub summary: String,
}

fn change(name: &str, old: f64, new: f64, unit: &str) -> String {
    format!(
        "{name}: {} → {}",
        format_value(old, unit),
        format_value(new, unit)
    )
}

/// Work out what `line` would do, or why it cannot be done.
pub fn plan_set(s: &Schematic, line: &SetLine) -> Result<Planned, String> {
    let component = s
        .components
        .iter()
        .find(|c| !c.designator.is_empty() && c.designator.eq_ignore_ascii_case(&line.part))
        .ok_or_else(|| format!("There is no part called {}.", line.part))?;
    let name = component.designator.clone();
    let number = || parse_eng(&line.value).map_err(|e| format!("{name}: {e}"));
    let positive = || {
        number().and_then(|v| {
            if v > 0.0 && v.is_finite() {
                Ok(v)
            } else {
                Err(format!("{name}: the value must be above zero."))
            }
        })
    };

    let mut kind = component.kind.clone();
    let summary = match (&mut kind, line.wiper) {
        (K::Resistor { resistance }, false) => {
            let (old, new) = (*resistance, positive()?);
            *resistance = new;
            change(&name, old, new, "Ω")
        }
        (K::Capacitor { capacitance, .. }, false) => {
            let (old, new) = (*capacitance, positive()?);
            *capacitance = new;
            change(&name, old, new, "F")
        }
        (K::Inductor { inductance, .. }, false) => {
            let (old, new) = (*inductance, positive()?);
            *inductance = new;
            change(&name, old, new, "H")
        }
        (
            K::VoltageSource {
                waveform: Waveform::Dc(v),
            },
            false,
        ) => {
            let (old, new) = (*v, number()?);
            *v = new;
            change(&name, old, new, "V")
        }
        (
            K::CurrentSource {
                waveform: Waveform::Dc(v),
            },
            false,
        ) => {
            let (old, new) = (*v, number()?);
            *v = new;
            change(&name, old, new, "A")
        }
        (K::VoltageSource { .. } | K::CurrentSource { .. }, false) => {
            return Err(format!(
                "{name} is not a plain DC source; change it in the Properties panel."
            ));
        }
        (K::Potentiometer { resistance, .. }, false) => {
            let (old, new) = (*resistance, positive()?);
            *resistance = new;
            change(&name, old, new, "Ω")
        }
        (K::Potentiometer { wiper, .. }, true) => {
            let (old, new) = (*wiper, number()?);
            if !(0.0..=1.0).contains(&new) {
                return Err(format!("{name}: the wiper position is between 0 and 1."));
            }
            *wiper = new;
            format!("{name} wiper: {old:.2} → {new:.2}")
        }
        (K::Switch { closed, .. }, false) => {
            let new = match line.value.to_ascii_lowercase().as_str() {
                "closed" | "on" | "close" => true,
                "open" | "off" => false,
                _ => return Err(format!("{name}: use closed or open.")),
            };
            let old = *closed;
            *closed = new;
            let word = |b| if b { "closed" } else { "open" };
            format!("{name}: {} → {}", word(old), word(new))
        }
        _ => {
            return Err(format!(
                "{name} cannot be changed this way; use the Properties panel."
            ));
        }
    };
    Ok(Planned {
        id: component.id,
        kind,
        summary,
    })
}

/// How a line of a diff relates to the old text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    Same,
    Added,
    Removed,
}

/// Largest product of line counts that is compared line by line.
const DIFF_LIMIT: usize = 4_000_000;

/// Line-by-line difference between two texts, or `None` if they are too
/// large to compare.
pub fn diff(old: &str, new: &str) -> Option<Vec<(DiffKind, String)>> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    if a.len().saturating_mul(b.len()) > DIFF_LIMIT {
        return None;
    }
    // lcs[i][j]: length of the common run of a[i..] and b[j..].
    let width = b.len() + 1;
    let mut lcs = vec![0u32; (a.len() + 1) * width];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i * width + j] = if a[i] == b[j] {
                lcs[(i + 1) * width + j + 1] + 1
            } else {
                lcs[(i + 1) * width + j].max(lcs[i * width + j + 1])
            };
        }
    }
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            out.push((DiffKind::Same, a[i].to_string()));
            i += 1;
            j += 1;
        } else if lcs[(i + 1) * width + j] >= lcs[i * width + j + 1] {
            out.push((DiffKind::Removed, a[i].to_string()));
            i += 1;
        } else {
            out.push((DiffKind::Added, b[j].to_string()));
            j += 1;
        }
    }
    out.extend(a[i..].iter().map(|l| (DiffKind::Removed, l.to_string())));
    out.extend(b[j..].iter().map(|l| (DiffKind::Added, l.to_string())));
    Some(out)
}

/// A diff cut down to the changes and `context` lines around them. `None`
/// marks where unchanged lines were left out.
pub fn hunks(lines: &[(DiffKind, String)], context: usize) -> Vec<Option<&(DiffKind, String)>> {
    let mut show = vec![false; lines.len()];
    for (i, (kind, _)) in lines.iter().enumerate() {
        if *kind != DiffKind::Same {
            let from = i.saturating_sub(context);
            let to = (i + context + 1).min(lines.len());
            show[from..to].iter_mut().for_each(|s| *s = true);
        }
    }
    let mut out = Vec::new();
    let mut gap = false;
    for (line, shown) in lines.iter().zip(show) {
        if shown {
            out.push(Some(line));
            gap = false;
        } else if !gap {
            out.push(None);
            gap = true;
        }
    }
    out
}

/// The text a file block replaces a file with: the block's body with
/// exactly one final line break.
pub fn file_text(body: &str) -> String {
    format!("{}\n", body.trim_end_matches(['\n', '\r']))
}

/// Check that a file replacement names a firmware file that exists.
pub fn check_file(s: &Schematic, name: &str) -> Result<(), String> {
    if s.sources.contains_key(name) {
        return Ok(());
    }
    let have: Vec<&str> = s.sources.keys().map(String::as_str).collect();
    Err(if have.is_empty() {
        format!("There is no firmware file called {name}: this circuit has none.")
    } else {
        format!(
            "There is no firmware file called {name}. Files: {}.",
            have.join(", ")
        )
    })
}

/// Make the change, as one undo step. Returns a sentence saying what
/// happened. Nothing changes if any part of the edit cannot be made.
pub fn apply(doc: &mut Document, edit: &Edit) -> Result<String, String> {
    match edit {
        Edit::Set(lines) => {
            let plans = lines
                .iter()
                .map(|l| plan_set(doc.schematic(), l))
                .collect::<Result<Vec<_>, _>>()?;
            doc.edit(|s| {
                for p in &plans {
                    if let Some(c) = s.component_mut(p.id) {
                        c.kind = p.kind.clone();
                    }
                }
            });
            let summaries: Vec<&str> = plans.iter().map(|p| p.summary.as_str()).collect();
            Ok(summaries.join("; "))
        }
        Edit::Add(parts) => {
            let notes = part_edits::check_add(doc.schematic(), parts)?;
            doc.edit(|s| {
                let _ = part_edits::add_parts(s, parts);
            });
            Ok(notes.join(" "))
        }
        Edit::Remove(names) => {
            part_edits::check_remove(doc.schematic(), names)?;
            doc.edit(|s| {
                for n in names {
                    let _ = part_edits::remove_part(s, n);
                }
            });
            Ok(format!("Removed {}.", names.join(", ")))
        }
        Edit::File { name, text } => {
            check_file(doc.schematic(), name)?;
            doc.edit(|s| {
                if let Some(f) = s.sources.get_mut(name) {
                    f.text = text.clone();
                }
            });
            Ok(format!("Replaced {name}."))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dunbrec_core::{Point, SourceFile, SourceLanguage};

    fn doc() -> Document {
        let mut s = Schematic::new();
        s.add_component(K::Resistor { resistance: 100.0 }, Point::new(0, 0));
        s.add_component(
            K::VoltageSource {
                waveform: Waveform::Dc(10.0),
            },
            Point::new(4, 0),
        );
        s.add_component(
            K::Potentiometer {
                resistance: 10e3,
                wiper: 0.5,
            },
            Point::new(8, 0),
        );
        s.sources.insert(
            "main.ino".into(),
            SourceFile {
                language: SourceLanguage::Cpp,
                text: "void setup() {}\nvoid loop() {}\n".into(),
            },
        );
        Document::new(s, None)
    }

    fn line(part: &str, value: &str) -> SetLine {
        SetLine {
            part: part.into(),
            wiper: false,
            value: value.into(),
        }
    }

    fn designator(doc: &Document, prefix: char) -> String {
        doc.schematic()
            .components
            .iter()
            .find(|c| c.designator.starts_with(prefix))
            .unwrap()
            .designator
            .clone()
    }

    #[test]
    fn set_blocks_are_read() {
        let l = parse_set("R2 = 1k\n# note\n\nrv1.wiper = 0.3\n").unwrap();
        assert_eq!(l.len(), 2);
        assert_eq!(l[0], line("R2", "1k"));
        assert!(l[1].wiper && l[1].part == "rv1");
        assert!(parse_set("R2 1k").is_err());
        assert!(parse_set("R2.colour = red").is_err());
        assert!(parse_set("R2 =").is_err());
        assert!(parse_set("# nothing").is_err());
    }

    #[test]
    fn a_value_change_is_checked_and_described() {
        let d = doc();
        let r = designator(&d, 'R');
        let p = plan_set(d.schematic(), &line(&r.to_lowercase(), "1k")).unwrap();
        assert!(
            p.summary.contains("100 Ω") && p.summary.contains("1 kΩ"),
            "{}",
            p.summary
        );
        assert!(matches!(p.kind, K::Resistor { resistance } if resistance == 1000.0));
        assert!(plan_set(d.schematic(), &line("R99", "1k")).is_err());
        assert!(plan_set(d.schematic(), &line(&r, "banana")).is_err());
        assert!(plan_set(d.schematic(), &line(&r, "-5")).is_err());
    }

    #[test]
    fn sources_potentiometers_and_wipers_can_be_set() {
        let d = doc();
        let v = designator(&d, 'V');
        let p = plan_set(d.schematic(), &line(&v, "5")).unwrap();
        assert!(matches!(
            p.kind,
            K::VoltageSource { waveform: Waveform::Dc(x) } if x == 5.0
        ));
        let rv = d
            .schematic()
            .components
            .iter()
            .find(|c| matches!(c.kind, K::Potentiometer { .. }))
            .unwrap()
            .designator
            .clone();
        let mut w = line(&rv, "0.25");
        w.wiper = true;
        assert!(plan_set(d.schematic(), &w).is_ok());
        w.value = "2".into();
        assert!(plan_set(d.schematic(), &w).is_err());
    }

    #[test]
    fn applying_changes_the_value_in_one_undo_step() {
        let mut d = doc();
        let r = designator(&d, 'R');
        let msg = apply(&mut d, &Edit::Set(vec![line(&r, "4k7")])).unwrap();
        assert!(msg.contains("4.7 kΩ"), "{msg}");
        let value = |d: &Document| {
            d.schematic()
                .components
                .iter()
                .find_map(|c| match c.kind {
                    K::Resistor { resistance } => Some(resistance),
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(value(&d), 4700.0);
        assert!(d.undo());
        assert_eq!(value(&d), 100.0);
    }

    #[test]
    fn a_bad_line_changes_nothing() {
        let mut d = doc();
        let r = designator(&d, 'R');
        let before = d.generation();
        let result = apply(&mut d, &Edit::Set(vec![line(&r, "1k"), line("R99", "1k")]));
        assert!(result.is_err());
        assert_eq!(d.generation(), before);
        assert!(!d.can_undo());
    }

    #[test]
    fn a_firmware_file_is_replaced_or_refused() {
        let mut d = doc();
        let ok = Edit::File {
            name: "main.ino".into(),
            text: file_text("void setup() { pinMode(13, OUTPUT); }\n\n"),
        };
        assert!(apply(&mut d, &ok).is_ok());
        assert_eq!(
            d.schematic().sources["main.ino"].text,
            "void setup() { pinMode(13, OUTPUT); }\n"
        );
        assert!(d.undo());
        assert!(d.schematic().sources["main.ino"].text.contains("loop"));
        let missing = Edit::File {
            name: "other.ino".into(),
            text: "x\n".into(),
        };
        let e = apply(&mut d, &missing).unwrap_err();
        assert!(e.contains("main.ino"), "{e}");
    }

    #[test]
    fn adding_and_removing_parts_are_one_undo_step_each() {
        let mut d = Document::new(crate::examples::voltage_divider(), None);
        let count = |d: &Document| d.schematic().components.len();
        let start = count(&d);
        let parts = part_edits::parse_add("R9 resistor 4k7 VOUT 0").unwrap();
        let msg = apply(&mut d, &Edit::Add(parts)).unwrap();
        assert!(msg.contains("R9"), "{msg}");
        assert!(count(&d) > start);
        assert!(d.undo());
        assert_eq!(count(&d), start);
        assert!(d.redo());
        let msg = apply(&mut d, &Edit::Remove(vec!["R9".into()])).unwrap();
        assert!(msg.contains("R9"), "{msg}");
        assert_eq!(count(&d), start);
        // An impossible edit changes nothing and leaves no undo step.
        let steps = d.can_undo();
        let bad = Edit::Remove(vec!["R99".into()]);
        assert!(apply(&mut d, &bad).is_err());
        assert_eq!(d.can_undo(), steps);
    }

    #[test]
    fn diffs_show_added_and_removed_lines() {
        let d = diff("a\nb\nc\n", "a\nB\nc\nd\n").unwrap();
        let kinds: Vec<DiffKind> = d.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            kinds,
            [
                DiffKind::Same,
                DiffKind::Removed,
                DiffKind::Added,
                DiffKind::Same,
                DiffKind::Added
            ]
        );
        assert!(
            diff("same\n", "same\n")
                .unwrap()
                .iter()
                .all(|(k, _)| *k == DiffKind::Same)
        );
    }

    #[test]
    fn hunks_keep_context_and_mark_gaps() {
        let old: String = (0..20).map(|i| format!("line{i}\n")).collect();
        let new = old.replace("line10", "changed");
        let lines = diff(&old, &new).unwrap();
        let h = hunks(&lines, 2);
        // gap, 2 lines of context, removed, added, 2 lines of context, gap
        assert_eq!(h.len(), 8);
        assert!(h[0].is_none() && h[7].is_none());
    }
}
