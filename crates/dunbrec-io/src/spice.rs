//! SPICE netlist import.
//!
//! Supported: R, C, L (with `IC=`), V and I sources (DC, SIN, PULSE, PWL),
//! D, Q, M (level 1), E, F, G, H, `.model`, `.subckt`/`X` (flattened),
//! continuation lines and comments. Analysis commands are kept as text.
//! Anything else is skipped with a warning rather than failing the import.
//!
//! The result is a list of [`Part`]s plus models, which
//! [`layout`](crate::layout::layout) turns into a schematic.

use std::collections::HashMap;

use dunbrec_core::{
    ComponentKind as K, DeviceModel, ModelKind, ModelLibrary, Polarity, Rotation, Schematic,
    Waveform, units::parse_spice,
};
use thiserror::Error;

use crate::layout::{Part, layout};

#[derive(Debug, Clone, PartialEq, Error)]
#[error("line {line}: {message}")]
pub struct SpiceError {
    pub line: usize,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpiceImport {
    pub title: String,
    pub parts: Vec<Part>,
    pub models: ModelLibrary,
    /// Analysis and other dot commands, as written (e.g. `.tran 1u 1m`).
    pub directives: Vec<String>,
    /// Things that were skipped or approximated.
    pub warnings: Vec<String>,
}

impl SpiceImport {
    /// Lay the parts out as a schematic.
    pub fn to_schematic(&self) -> Schematic {
        layout(&self.parts, self.models.clone())
    }
}

/// A logical line: continuation lines joined, comments removed.
#[derive(Debug, Clone)]
struct Line {
    number: usize,
    text: String,
}

fn logical_lines(text: &str) -> (String, Vec<Line>) {
    let mut lines = text.lines().enumerate();
    let title = lines
        .next()
        .map(|(_, t)| t.trim().to_string())
        .unwrap_or_default();
    let mut out: Vec<Line> = Vec::new();
    for (i, raw) in lines {
        let mut t = raw.trim_end().to_string();
        for marker in [";", " $ ", "\t$ ", "//"] {
            if let Some(pos) = t.find(marker) {
                t.truncate(pos);
            }
        }
        let trimmed = t.trim();
        if trimmed.is_empty() || trimmed.starts_with('*') {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('+')
            && let Some(last) = out.last_mut()
        {
            last.text.push(' ');
            last.text.push_str(rest.trim());
            continue;
        }
        out.push(Line {
            number: i + 1,
            text: trimmed.to_string(),
        });
    }
    (title, out)
}

/// Split a line into tokens: parentheses and commas separate tokens, and
/// `name = value` becomes `name=value`.
fn tokens(text: &str) -> Vec<String> {
    let spaced: String = text
        .chars()
        .map(|c| if matches!(c, '(' | ')' | ',') { ' ' } else { c })
        .collect();
    let raw: Vec<&str> = spaced.split_whitespace().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let t = raw[i];
        if t == "=" && !out.is_empty() && i + 1 < raw.len() {
            let last = out.pop().expect("checked non-empty");
            out.push(format!("{last}={}", raw[i + 1]));
            i += 2;
        } else if let (Some(stripped), true) = (t.strip_suffix('='), i + 1 < raw.len()) {
            out.push(format!("{stripped}={}", raw[i + 1]));
            i += 2;
        } else if t.starts_with('=') && !out.is_empty() {
            let last = out.pop().expect("checked non-empty");
            out.push(format!("{last}{t}"));
            i += 1;
        } else {
            out.push(t.to_string());
            i += 1;
        }
    }
    out
}

struct Subckt {
    ports: Vec<String>,
    body: Vec<Line>,
}

/// An element in progress (F/H sources are resolved at the end).
struct Pending {
    designator: String,
    kind: K,
    nets: Vec<String>,
}

struct Importer {
    subckts: HashMap<String, Subckt>,
    parts: Vec<Pending>,
    /// F/H elements: (index into parts, controlling source name).
    current_controlled: Vec<(usize, String)>,
    /// MOSFET instance W/L: (part index, model, W, L).
    mos_geometry: Vec<(usize, String, Option<f64>, Option<f64>)>,
    models: ModelLibrary,
    directives: Vec<String>,
    warnings: Vec<String>,
}

fn value(token: &str, line: usize, what: &str) -> Result<f64, SpiceError> {
    if token.contains('{') {
        return Err(SpiceError {
            line,
            message: format!(
                "{what} '{token}' uses a parameter expression, which is not supported"
            ),
        });
    }
    parse_spice(token).map_err(|e| SpiceError {
        line,
        message: format!("{what}: {e}"),
    })
}

fn is_keyword(t: &str) -> bool {
    matches!(
        t.to_ascii_uppercase().as_str(),
        "DC" | "AC" | "SIN" | "PULSE" | "PWL" | "EXP" | "SFFM" | "AM" | "DISTOF1" | "DISTOF2"
    )
}

impl Importer {
    fn warn(&mut self, line: usize, msg: impl Into<String>) {
        self.warnings.push(format!("line {line}: {}", msg.into()));
    }

    /// Parse a source's value specification (everything after the nodes).
    fn source(&mut self, args: &[String], line: usize) -> Result<Waveform, SpiceError> {
        let mut dc: Option<f64> = None;
        let mut wave: Option<Waveform> = None;
        let mut i = 0;
        while i < args.len() {
            let kw = args[i].to_ascii_uppercase();
            // Collect the numbers following a keyword.
            let numbers = |from: usize| -> Vec<&String> {
                args[from..].iter().take_while(|t| !is_keyword(t)).collect()
            };
            match kw.as_str() {
                "DC" => {
                    if let Some(t) = args.get(i + 1) {
                        dc = Some(value(t, line, "DC value")?);
                    }
                    i += 2;
                }
                "AC" => {
                    // Small-signal magnitude/phase: used by AC analysis later.
                    i += 1 + numbers(i + 1).len();
                }
                "SIN" => {
                    let n = numbers(i + 1);
                    let v: Vec<f64> = n
                        .iter()
                        .map(|t| value(t, line, "SIN parameter"))
                        .collect::<Result<_, _>>()?;
                    if v.len() < 3 {
                        return Err(SpiceError {
                            line,
                            message: "SIN needs at least offset, amplitude and frequency".into(),
                        });
                    }
                    let get = |k: usize| v.get(k).copied().unwrap_or(0.0);
                    wave = Some(Waveform::Sine {
                        offset: v[0],
                        amplitude: v[1],
                        frequency: v[2],
                        delay: get(3),
                        damping: get(4),
                        phase: get(5),
                    });
                    i += 1 + n.len();
                }
                "PULSE" => {
                    let n = numbers(i + 1);
                    let v: Vec<f64> = n
                        .iter()
                        .map(|t| value(t, line, "PULSE parameter"))
                        .collect::<Result<_, _>>()?;
                    if v.len() < 2 {
                        return Err(SpiceError {
                            line,
                            message: "PULSE needs at least the two levels".into(),
                        });
                    }
                    let get = |k: usize, d: f64| v.get(k).copied().unwrap_or(d);
                    if v.len() < 7 {
                        self.warn(line, "PULSE with missing timing values: defaults used");
                    }
                    wave = Some(Waveform::Pulse {
                        v1: v[0],
                        v2: v[1],
                        delay: get(2, 0.0),
                        rise: get(3, 0.0),
                        fall: get(4, 0.0),
                        width: get(5, 1e30),
                        period: get(6, 0.0),
                    });
                    i += 1 + n.len();
                }
                "PWL" => {
                    let n = numbers(i + 1);
                    let v: Vec<f64> = n
                        .iter()
                        .map(|t| value(t, line, "PWL point"))
                        .collect::<Result<_, _>>()?;
                    if v.len() < 2 || !v.len().is_multiple_of(2) {
                        return Err(SpiceError {
                            line,
                            message: "PWL needs time/value pairs".into(),
                        });
                    }
                    wave = Some(Waveform::Pwl(v.chunks(2).map(|p| (p[0], p[1])).collect()));
                    i += 1 + n.len();
                }
                "EXP" | "SFFM" | "AM" | "DISTOF1" | "DISTOF2" => {
                    self.warn(
                        line,
                        format!("{kw} sources are not supported yet; using the DC value"),
                    );
                    i += 1 + numbers(i + 1).len();
                }
                _ if args[i].contains('=') => {
                    self.warn(
                        line,
                        format!("source option '{}' is not supported; ignored", args[i]),
                    );
                    i += 1;
                }
                _ => {
                    if dc.is_none() && wave.is_none() {
                        dc = Some(value(&args[i], line, "source value")?);
                    } else {
                        self.warn(line, format!("ignored '{}'", args[i]));
                    }
                    i += 1;
                }
            }
        }
        Ok(wave.unwrap_or(Waveform::Dc(dc.unwrap_or(0.0))))
    }

    fn model(&mut self, t: &[String], line: usize) {
        if t.len() < 3 {
            self.warn(line, ".model needs a name and a type");
            return;
        }
        let Some(kind) = ModelKind::from_spice_name(&t[2]) else {
            self.warn(
                line,
                format!(
                    "model type '{}' is not supported; model {} skipped",
                    t[2], t[1]
                ),
            );
            return;
        };
        let mut m = DeviceModel::new(t[1].clone(), kind);
        for p in &t[3..] {
            let Some((name, v)) = p.split_once('=') else {
                self.warn(line, format!("model {}: ignored '{p}'", t[1]));
                continue;
            };
            match parse_spice(v) {
                Ok(v) => m.set(name, v),
                Err(_) => self.warn(line, format!("model {}: ignored {name}={v}", t[1])),
            }
        }
        if matches!(kind, ModelKind::Nmos | ModelKind::Pmos) && m.get_or("LEVEL", 1.0) != 1.0 {
            self.warn(
                line,
                format!(
                    "model {}: only MOSFET level 1 is supported; level-1 equations used",
                    t[1]
                ),
            );
        }
        self.models.insert(m);
    }

    /// Process element and command lines, expanding subcircuit instances.
    /// `prefix` and `ports` map names inside a subcircuit instance.
    fn lines(
        &mut self,
        lines: &[Line],
        prefix: &str,
        ports: &HashMap<String, String>,
        depth: usize,
    ) -> Result<(), SpiceError> {
        let net = |n: &str| -> String {
            if n == "0" || n.eq_ignore_ascii_case("gnd") {
                "0".into()
            } else if let Some(mapped) = ports.get(&n.to_ascii_lowercase()) {
                mapped.clone()
            } else if prefix.is_empty() {
                n.to_string()
            } else {
                format!("{prefix}.{n}")
            }
        };
        let name = |n: &str| -> String {
            if prefix.is_empty() {
                n.to_string()
            } else {
                format!("{prefix}.{n}")
            }
        };

        let mut i = 0;
        while i < lines.len() {
            let Line { number, text } = &lines[i];
            let number = *number;
            i += 1;
            let t = tokens(text);
            let Some(first) = t.first() else { continue };
            let upper = first.to_ascii_uppercase();

            if upper.starts_with('.') {
                match upper.as_str() {
                    ".MODEL" => self.model(&t, number),
                    ".SUBCKT" => {
                        // Collect the body (nested definitions are global).
                        let mut level = 1;
                        let start = i;
                        while i < lines.len() {
                            let u = lines[i].text.to_ascii_uppercase();
                            if u.starts_with(".SUBCKT") {
                                level += 1;
                            } else if u.starts_with(".ENDS") {
                                level -= 1;
                                if level == 0 {
                                    break;
                                }
                            }
                            i += 1;
                        }
                        let body = lines[start..i.min(lines.len())].to_vec();
                        i += 1; // skip .ends
                        if t.len() < 2 {
                            self.warn(number, ".subckt without a name");
                            continue;
                        }
                        let ports: Vec<String> = t[2..]
                            .iter()
                            .take_while(|p| !p.contains('=') && !p.eq_ignore_ascii_case("params:"))
                            .map(|p| p.to_ascii_lowercase())
                            .collect();
                        if t[2..].iter().any(|p| p.contains('=')) {
                            self.warn(
                                number,
                                format!("subcircuit {} parameters are not supported", t[1]),
                            );
                        }
                        self.subckts
                            .insert(t[1].to_ascii_lowercase(), Subckt { ports, body });
                    }
                    ".CONTROL" => {
                        while i < lines.len()
                            && !lines[i].text.to_ascii_uppercase().starts_with(".ENDC")
                        {
                            i += 1;
                        }
                        i += 1;
                        self.warn(number, "ngspice .control block skipped");
                    }
                    ".END" | ".ENDS" => {}
                    ".INCLUDE" | ".INC" | ".LIB" => {
                        self.warn(
                            number,
                            format!("{first} is not supported: referenced files were not read"),
                        );
                    }
                    ".PARAM" | ".IC" | ".NODESET" | ".FUNC" | ".GLOBAL" => {
                        self.warn(number, format!("{first} is not supported and was ignored"));
                    }
                    _ => {
                        if prefix.is_empty() {
                            self.directives.push(text.clone());
                        }
                    }
                }
                continue;
            }

            let letter = upper.chars().next().expect("non-empty token");
            let need = |n: usize| -> Result<(), SpiceError> {
                if t.len() < n {
                    Err(SpiceError {
                        line: number,
                        message: format!("{first}: expected at least {} fields", n),
                    })
                } else {
                    Ok(())
                }
            };
            let designator = name(first);
            // Optional KEY=VALUE parameters after the positional fields.
            let param = |key: &str| -> Option<&str> {
                t.iter().find_map(|x| {
                    x.split_once('=')
                        .filter(|(k, _)| k.eq_ignore_ascii_case(key))
                        .map(|(_, v)| v)
                })
            };

            let (kind, nets): (K, Vec<String>) = match letter {
                'R' => {
                    need(4)?;
                    let r = value(&t[3], number, "resistance")?;
                    (K::Resistor { resistance: r }, vec![net(&t[1]), net(&t[2])])
                }
                'C' => {
                    need(4)?;
                    let c = value(&t[3], number, "capacitance")?;
                    let ic = param("IC").map(|v| value(v, number, "IC")).transpose()?;
                    (
                        K::Capacitor {
                            capacitance: c,
                            initial_voltage: ic,
                        },
                        vec![net(&t[1]), net(&t[2])],
                    )
                }
                'L' => {
                    need(4)?;
                    let l = value(&t[3], number, "inductance")?;
                    let ic = param("IC").map(|v| value(v, number, "IC")).transpose()?;
                    (
                        K::Inductor {
                            inductance: l,
                            initial_current: ic,
                        },
                        vec![net(&t[1]), net(&t[2])],
                    )
                }
                'V' | 'I' => {
                    need(3)?;
                    let waveform = self.source(&t[3..], number)?;
                    let kind = if letter == 'V' {
                        K::VoltageSource { waveform }
                    } else {
                        K::CurrentSource { waveform }
                    };
                    (kind, vec![net(&t[1]), net(&t[2])])
                }
                'D' => {
                    need(4)?;
                    (
                        K::Diode {
                            model: t[3].clone(),
                        },
                        vec![net(&t[1]), net(&t[2])],
                    )
                }
                'Q' => {
                    need(5)?;
                    // Q c b e [substrate] model
                    let has_substrate =
                        t.len() >= 6 && !t[5].contains('=') && parse_spice(&t[5]).is_err();
                    let model = if has_substrate { &t[5] } else { &t[4] };
                    if has_substrate {
                        self.warn(number, format!("{first}: substrate node ignored"));
                    }
                    let polarity = match self.models.get(model).map(|m| m.kind) {
                        Some(ModelKind::Pnp) => Polarity::P,
                        _ => Polarity::N,
                    };
                    (
                        K::Bjt {
                            polarity,
                            model: model.clone(),
                        },
                        vec![net(&t[1]), net(&t[2]), net(&t[3])],
                    )
                }
                'M' => {
                    need(6)?;
                    // M d g s b model [W= L=]
                    if net(&t[4]) != net(&t[3]) {
                        self.warn(
                            number,
                            format!(
                                "{first}: body is not tied to source; Dunbrec ties it to the source"
                            ),
                        );
                    }
                    let model = t[5].clone();
                    let w = param("W").map(|v| value(v, number, "W")).transpose()?;
                    let l = param("L").map(|v| value(v, number, "L")).transpose()?;
                    if w.is_some() || l.is_some() {
                        self.mos_geometry
                            .push((self.parts.len(), model.clone(), w, l));
                    }
                    let polarity = match self.models.get(&model).map(|m| m.kind) {
                        Some(ModelKind::Pmos) => Polarity::P,
                        _ => Polarity::N,
                    };
                    (
                        K::Mosfet { polarity, model },
                        vec![net(&t[1]), net(&t[2]), net(&t[3])],
                    )
                }
                'E' | 'G' => {
                    need(6)?;
                    let g = value(&t[5], number, "gain")?;
                    let kind = if letter == 'E' {
                        K::Vcvs { gain: g }
                    } else {
                        K::Vccs {
                            transconductance: g,
                        }
                    };
                    (kind, vec![net(&t[1]), net(&t[2]), net(&t[3]), net(&t[4])])
                }
                'F' | 'H' => {
                    need(5)?;
                    let g = value(&t[4], number, "gain")?;
                    let kind = if letter == 'F' {
                        K::Cccs { gain: g }
                    } else {
                        K::Ccvs { transresistance: g }
                    };
                    self.current_controlled
                        .push((self.parts.len(), name(&t[3])));
                    // Control nets are filled in when the source is resolved.
                    (
                        kind,
                        vec![net(&t[1]), net(&t[2]), String::new(), String::new()],
                    )
                }
                'X' => {
                    need(2)?;
                    if depth > 20 {
                        return Err(SpiceError {
                            line: number,
                            message: "subcircuits nested too deeply (recursive definition?)".into(),
                        });
                    }
                    let args: Vec<&String> =
                        t[1..].iter().take_while(|a| !a.contains('=')).collect();
                    let sub_name = args.last().expect("need(2) checked").to_ascii_lowercase();
                    let Some(sub) = self.subckts.get(&sub_name) else {
                        return Err(SpiceError {
                            line: number,
                            message: format!(
                                "subcircuit '{sub_name}' is not defined (before its use)"
                            ),
                        });
                    };
                    let actual: Vec<String> =
                        args[..args.len() - 1].iter().map(|a| net(a)).collect();
                    if actual.len() != sub.ports.len() {
                        return Err(SpiceError {
                            line: number,
                            message: format!(
                                "{first}: subcircuit {sub_name} has {} ports, {} given",
                                sub.ports.len(),
                                actual.len()
                            ),
                        });
                    }
                    let map: HashMap<String, String> =
                        sub.ports.iter().cloned().zip(actual).collect();
                    let body = sub.body.clone();
                    self.lines(&body, &designator, &map, depth + 1)?;
                    continue;
                }
                _ => {
                    self.warn(
                        number,
                        format!("element type '{letter}' ({first}) is not supported; skipped"),
                    );
                    continue;
                }
            };
            self.parts.push(Pending {
                designator,
                kind,
                nets,
            });
        }
        Ok(())
    }

    /// Give each F/H a current sensor in series with its controlling
    /// voltage source.
    fn resolve_current_controlled(&mut self) -> Result<(), SpiceError> {
        for (index, source) in std::mem::take(&mut self.current_controlled) {
            let found = self.parts.iter().position(|p| {
                p.designator.eq_ignore_ascii_case(&source)
                    && matches!(p.kind, K::VoltageSource { .. })
            });
            let Some(v) = found else {
                return Err(SpiceError {
                    line: 0,
                    message: format!(
                        "{} is controlled by '{source}', which is not a voltage source",
                        self.parts[index].designator
                    ),
                });
            };
            // V(n+) ... V(n-)  becomes  V(n+) ... sense node, sensor → n-.
            let sense = format!("{}#sense", self.parts[index].designator);
            let original = std::mem::replace(&mut self.parts[v].nets[1], sense.clone());
            self.parts[index].nets[2] = sense;
            self.parts[index].nets[3] = original;
        }
        Ok(())
    }

    /// MOSFETs with their own W/L get a copy of their model with that size.
    fn apply_mos_geometry(&mut self) {
        for (index, model, w, l) in std::mem::take(&mut self.mos_geometry) {
            let Some(base) = self.models.get(&model).cloned() else {
                continue;
            };
            let designator = self.parts[index].designator.clone();
            let mut sized = base;
            sized.name = format!("{model}_{designator}");
            if let Some(w) = w {
                sized.set("W", w);
            }
            if let Some(l) = l {
                sized.set("L", l);
            }
            if let K::Mosfet { model, .. } = &mut self.parts[index].kind {
                *model = sized.name.clone();
            }
            self.models.insert(sized);
        }
    }
}

pub fn import(text: &str) -> Result<SpiceImport, SpiceError> {
    let (title, lines) = logical_lines(text);
    let mut imp = Importer {
        subckts: HashMap::new(),
        parts: Vec::new(),
        current_controlled: Vec::new(),
        mos_geometry: Vec::new(),
        models: ModelLibrary::new(),
        directives: Vec::new(),
        warnings: Vec::new(),
    };
    // Models may follow the elements that use them: read them first.
    for line in &lines {
        let t = tokens(&line.text);
        if t.first().is_some_and(|f| f.eq_ignore_ascii_case(".model")) {
            imp.model(&t, line.number);
        }
    }
    let model_lines: Vec<Line> = lines
        .iter()
        .filter(|l| !l.text.to_ascii_lowercase().starts_with(".model"))
        .cloned()
        .collect();
    imp.lines(&model_lines, "", &HashMap::new(), 0)?;
    imp.resolve_current_controlled()?;
    imp.apply_mos_geometry();

    // Polarity of transistors whose model appeared after them was guessed
    // as N; fix it now all models are known.
    for p in &mut imp.parts {
        match &mut p.kind {
            K::Bjt { polarity, model } => {
                if let Some(m) = imp.models.get(model) {
                    *polarity = if m.kind == ModelKind::Pnp {
                        Polarity::P
                    } else {
                        Polarity::N
                    };
                }
            }
            K::Mosfet { polarity, model } => {
                if let Some(m) = imp.models.get(model) {
                    *polarity = if m.kind == ModelKind::Pmos {
                        Polarity::P
                    } else {
                        Polarity::N
                    };
                }
            }
            _ => {}
        }
    }

    let parts = imp
        .parts
        .into_iter()
        .map(|p| Part {
            designator: p.designator,
            kind: p.kind,
            nets: p.nets,
            position: None,
            rotation: Rotation::R0,
        })
        .collect();
    Ok(SpiceImport {
        title,
        parts,
        models: imp.models,
        directives: imp.directives,
        warnings: imp.warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dunbrec_core::build_netlist;

    fn part<'a>(imp: &'a SpiceImport, des: &str) -> &'a Part {
        imp.parts
            .iter()
            .find(|p| p.designator == des)
            .unwrap_or_else(|| panic!("no {des}"))
    }

    #[test]
    fn basic_elements_and_values() {
        let imp = import(
            "Voltage divider\n\
             * a comment\n\
             V1 in 0 DC 10\n\
             R1 in out 4k7 ; inline comment\n\
             R2 out 0\n\
             + 10MEG\n\
             C1 out 0 1u IC=2.5\n\
             L1 out 0 1m\n\
             .tran 1u 1m\n\
             .end\n",
        )
        .unwrap();
        assert_eq!(imp.title, "Voltage divider");
        assert_eq!(part(&imp, "R1").kind, K::Resistor { resistance: 4700.0 });
        assert_eq!(part(&imp, "R2").kind, K::Resistor { resistance: 10e6 });
        assert_eq!(part(&imp, "R1").nets, vec!["in", "out"]);
        assert_eq!(
            part(&imp, "C1").kind,
            K::Capacitor {
                capacitance: 1e-6,
                initial_voltage: Some(2.5)
            }
        );
        assert_eq!(
            part(&imp, "V1").kind,
            K::VoltageSource {
                waveform: Waveform::Dc(10.0)
            }
        );
        assert_eq!(imp.directives, vec![".tran 1u 1m"]);
        assert!(imp.warnings.is_empty(), "{:?}", imp.warnings);
    }

    #[test]
    fn source_waveforms() {
        let imp = import(
            "sources\n\
             V1 a 0 SIN(0 1 1k)\n\
             V2 b 0 PULSE(0 5 1u 10n 10n 0.5m 1m)\n\
             V3 c 0 PWL(0 0 1m 5 2m 0)\n\
             V4 d 0 DC 3 AC 1\n\
             I1 0 e 2m\n\
             V5 f 0 dc=1.5\n",
        )
        .unwrap();
        assert!(matches!(
            part(&imp, "V1").kind,
            K::VoltageSource { waveform: Waveform::Sine { amplitude, frequency, .. } } if amplitude == 1.0 && frequency == 1e3
        ));
        assert!(matches!(
            part(&imp, "V2").kind,
            K::VoltageSource { waveform: Waveform::Pulse { v2, period, .. } } if v2 == 5.0 && period == 1e-3
        ));
        assert_eq!(
            part(&imp, "V3").kind,
            K::VoltageSource {
                waveform: Waveform::Pwl(vec![(0.0, 0.0), (1e-3, 5.0), (2e-3, 0.0)])
            }
        );
        assert_eq!(
            part(&imp, "V4").kind,
            K::VoltageSource {
                waveform: Waveform::Dc(3.0)
            }
        );
        assert_eq!(
            part(&imp, "I1").kind,
            K::CurrentSource {
                waveform: Waveform::Dc(2e-3)
            }
        );
        assert_eq!(part(&imp, "I1").nets, vec!["0", "e"]);
        // "dc=1.5" is not standard SPICE: reported, not silently misread.
        assert_eq!(
            part(&imp, "V5").kind,
            K::VoltageSource {
                waveform: Waveform::Dc(0.0)
            }
        );
        assert_eq!(imp.warnings.len(), 1, "{:?}", imp.warnings);
    }

    #[test]
    fn models_semiconductors_and_late_models() {
        let imp = import(
            "amp\n\
             Q1 c b e QP\n\
             M1 d g s s MN W=10u L=2u\n\
             D1 a k DX\n\
             .model QP PNP(IS=1e-14 BF=150)\n\
             .model MN NMOS (VTO = 1.5 KP=2e-5)\n\
             .model DX D IS=2.52n RS=.568\n",
        )
        .unwrap();
        assert_eq!(
            part(&imp, "Q1").kind,
            K::Bjt {
                polarity: Polarity::P,
                model: "QP".into()
            }
        );
        assert_eq!(imp.models.get("qp").unwrap().get_or("BF", 0.0), 150.0);
        assert_eq!(imp.models.get("MN").unwrap().get_or("VTO", 0.0), 1.5);
        assert_eq!(imp.models.get("DX").unwrap().get_or("RS", 0.0), 0.568);
        // The MOSFET got its own sized copy of the model.
        let K::Mosfet { model, polarity } = &part(&imp, "M1").kind else {
            panic!()
        };
        assert_eq!(*polarity, Polarity::N);
        let sized = imp.models.get(model).unwrap();
        assert_eq!(sized.get_or("W", 0.0), 10e-6);
        assert_eq!(sized.get_or("KP", 0.0), 2e-5);
    }

    #[test]
    fn subcircuits_are_flattened() {
        let imp = import(
            "subckt test\n\
             .subckt divider top mid\n\
             R1 top mid 1k\n\
             R2 mid 0 1k\n\
             .ends divider\n\
             V1 in 0 10\n\
             X1 in a divider\n\
             X2 a b divider\n",
        )
        .unwrap();
        assert_eq!(part(&imp, "X1.R1").nets, vec!["in", "a"]);
        assert_eq!(part(&imp, "X1.R2").nets, vec!["a", "0"]);
        assert_eq!(part(&imp, "X2.R1").nets, vec!["a", "b"]);
        assert_eq!(imp.parts.len(), 5);
    }

    #[test]
    fn current_controlled_sources_get_a_sensor() {
        let imp = import(
            "cccs\n\
             V1 in 0 1\n\
             VSENSE in s 0\n\
             R1 s 0 1k\n\
             F1 x 0 VSENSE 2\n\
             RL x 0 1k\n",
        )
        .unwrap();
        // VSENSE now ends at the sensor node; F1 senses from there to s.
        assert_eq!(part(&imp, "VSENSE").nets, vec!["in", "F1#sense"]);
        assert_eq!(part(&imp, "F1").nets, vec!["x", "0", "F1#sense", "s"]);

        // And it simulates as SPICE would: Vx = -2 V.
        let s = imp.to_schematic();
        let netlist = build_netlist(&s);
        assert!(netlist.diagnostics.is_empty(), "{:?}", netlist.diagnostics);
        let c = dunbrec_sim::Circuit::from_netlist(&netlist, &s.models).unwrap();
        let op = dunbrec_sim::operating_point(&c).unwrap();
        assert!((op.voltage_by_name("x").unwrap() + 2.0).abs() < 1e-9);
    }

    #[test]
    fn unsupported_things_warn_and_errors_have_line_numbers() {
        let imp = import(
            "warnings\n\
             .include models.lib\n\
             .param rval=1k\n\
             B1 a 0 V=1\n\
             R1 a 0 1k\n\
             .control\n\
             run\n\
             .endc\n",
        )
        .unwrap();
        assert_eq!(imp.parts.len(), 1);
        assert_eq!(imp.warnings.len(), 4, "{:?}", imp.warnings);

        let err = import("bad\nR1 a 0 {rval}\n").unwrap_err();
        assert_eq!(err.line, 2);
        let err = import("bad\nR1 a 0\n").unwrap_err();
        assert!(err.message.contains("expected"));
        let err = import("bad\nX1 a b nosuch\n").unwrap_err();
        assert!(err.message.contains("not defined"));
    }

    #[test]
    fn imported_circuit_simulates() {
        let imp = import(
            "diode clamp\n\
             V1 in 0 5\n\
             R1 in a 1k\n\
             D1 a 0 D1N\n\
             .model D1N D(IS=1e-14)\n",
        )
        .unwrap();
        let s = imp.to_schematic();
        let netlist = build_netlist(&s);
        assert!(netlist.diagnostics.is_empty(), "{:?}", netlist.diagnostics);
        let c = dunbrec_sim::Circuit::from_netlist(&netlist, &s.models).unwrap();
        let op = dunbrec_sim::operating_point(&c).unwrap();
        let va = op.voltage_by_name("a").unwrap();
        assert!(va > 0.6 && va < 0.75, "{va}");
    }
}
