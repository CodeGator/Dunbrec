//! KiCad schematic (`.kicad_sch`, KiCad 6 and later) import.
//!
//! Connectivity is computed from KiCad's own drawing: symbol pins (from the
//! `lib_symbols` embedded in every schematic), wires, junctions, labels and
//! power symbols, with the same rules as Dunbrec's netlister. Parts are then
//! placed at their KiCad positions and connected with net-label stubs
//! ([`layout`]), since Dunbrec's symbols differ in size from KiCad's.
//!
//! Parts are recognised by their library symbol (`Device:R`, `Device:Q_NPN_BCE`,
//! `Simulation_SPICE:VSIN`, ...) or by KiCad's simulation fields (`Sim.Device`,
//! `Sim.Type`, `Sim.Params`, `Sim.Pins`). Anything else (connectors, ICs) is
//! skipped with a warning. Hierarchical sheets are not supported yet.

use std::collections::{BTreeMap, HashMap};

use dunbrec_core::{
    ComponentKind as K, DeviceModel, ModelKind, ModelLibrary, Point, Polarity, Rotation, Schematic,
    Waveform,
    units::{parse_eng, parse_spice},
};
use thiserror::Error;

use crate::layout::{Part, layout};
use crate::sexpr::{self, Sexp};

#[derive(Debug, Clone, PartialEq, Error)]
#[error("KiCad import: {0}")]
pub struct KicadError(pub String);

#[derive(Debug, Clone, PartialEq)]
pub struct KicadImport {
    pub parts: Vec<Part>,
    pub models: ModelLibrary,
    pub warnings: Vec<String>,
}

impl KicadImport {
    pub fn to_schematic(&self) -> Schematic {
        layout(&self.parts, self.models.clone())
    }
}

/// KiCad's grid (50 mil); one Dunbrec grid unit per KiCad grid step.
const KICAD_GRID_MM: f64 = 1.27;

// ------------------------------------------------------------ geometry ---

/// A point in KiCad millimetres, quantised to 0.1 µm so equal points match.
type Key = (i64, i64);

fn key(x: f64, y: f64) -> Key {
    ((x * 1e4).round() as i64, (y * 1e4).round() as i64)
}

/// True if `p` lies strictly inside the segment `a`–`b`.
fn on_interior(p: Key, a: Key, b: Key) -> bool {
    if p == a || p == b || a == b {
        return false;
    }
    let (dx, dy) = ((b.0 - a.0) as i128, (b.1 - a.1) as i128);
    let (ex, ey) = ((p.0 - a.0) as i128, (p.1 - a.1) as i128);
    if dx * ey - dy * ex != 0 {
        return false;
    }
    let dot = dx * ex + dy * ey;
    dot > 0 && dot < dx * dx + dy * dy
}

/// Place a library pin (y up) on the sheet (y down) for a symbol at `at`
/// rotated by `angle` degrees (counter-clockwise on screen) and mirrored.
/// `extra` (0..8) tries the other orientations when the file's do not fit.
fn place(
    pin: (f64, f64),
    at: (f64, f64),
    angle: f64,
    mirror: Option<char>,
    extra: usize,
) -> (f64, f64) {
    let (mut x, mut y) = (pin.0, -pin.1);
    match mirror {
        Some('x') => y = -y,
        Some('y') => x = -x,
        _ => {}
    }
    if extra >= 4 {
        y = -y;
    }
    let a = (angle + 90.0 * (extra % 4) as f64).to_radians();
    let (s, c) = (a.sin().round_to(1e-9), a.cos().round_to(1e-9));
    (at.0 + x * c + y * s, at.1 - x * s + y * c)
}

trait RoundTo {
    fn round_to(self, eps: f64) -> f64;
}
impl RoundTo for f64 {
    /// Snap sin/cos of multiples of 90° to exact 0/±1.
    fn round_to(self, eps: f64) -> f64 {
        let r = self.round();
        if (self - r).abs() < eps { r } else { self }
    }
}

struct Dsu(Vec<usize>);
impl Dsu {
    fn make(&mut self) -> usize {
        self.0.push(self.0.len());
        self.0.len() - 1
    }
    fn find(&mut self, mut x: usize) -> usize {
        while self.0[x] != x {
            self.0[x] = self.0[self.0[x]];
            x = self.0[x];
        }
        x
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.0[rb] = ra;
        }
    }
}

// --------------------------------------------------------- the drawing ---

#[derive(Debug, Clone)]
struct LibPin {
    number: String,
    name: String,
    at: (f64, f64),
}

#[derive(Debug, Clone)]
struct LibSymbol {
    /// Pins per unit (unit 0 = common to all units).
    pins: Vec<(u32, LibPin)>,
    power: bool,
}

struct Instance {
    lib: String,
    reference: String,
    value: String,
    properties: HashMap<String, String>,
    at: (f64, f64),
    angle: f64,
    mirror: Option<char>,
    unit: u32,
    pins: Vec<LibPin>,
}

fn xy(s: &Sexp) -> Option<(f64, f64)> {
    Some((s.arg_number(0)?, s.arg_number(1)?))
}

fn lib_symbols(root: &Sexp) -> HashMap<String, LibSymbol> {
    let mut out = HashMap::new();
    let Some(libs) = root.child("lib_symbols") else {
        return out;
    };
    for sym in libs.children("symbol") {
        let Some(name) = sym.arg(0) else { continue };
        let mut pins = Vec::new();
        // Pins live in sub-symbols named "<name>_<unit>_<style>".
        let mut collect = |unit: u32, holder: &Sexp| {
            for p in holder.children("pin") {
                let at = p.child("at").and_then(xy);
                let number = p
                    .child("number")
                    .and_then(|n| n.arg(0))
                    .unwrap_or("")
                    .to_string();
                let pname = p
                    .child("name")
                    .and_then(|n| n.arg(0))
                    .unwrap_or("")
                    .to_string();
                if let Some(at) = at {
                    pins.push((
                        unit,
                        LibPin {
                            number,
                            name: pname,
                            at,
                        },
                    ));
                }
            }
        };
        collect(0, sym);
        for sub in sym.children("symbol") {
            let sub_name = sub.arg(0).unwrap_or("");
            let mut parts = sub_name.rsplitn(3, '_');
            let style: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
            let unit: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            if style <= 1 {
                collect(unit, sub);
            }
        }
        out.insert(
            name.to_string(),
            LibSymbol {
                pins,
                power: sym.child("power").is_some(),
            },
        );
    }
    out
}

fn instances(
    root: &Sexp,
    libs: &HashMap<String, LibSymbol>,
    warnings: &mut Vec<String>,
) -> Vec<(Instance, bool)> {
    let mut out = Vec::new();
    for sym in root.children("symbol") {
        let lib_id = sym
            .child("lib_id")
            .and_then(|l| l.arg(0))
            .unwrap_or("")
            .to_string();
        let lib_name = sym
            .child("lib_name")
            .and_then(|l| l.arg(0))
            .map(str::to_string);
        let mut properties = HashMap::new();
        for p in sym.children("property") {
            if let (Some(k), Some(v)) = (p.arg(0), p.arg(1)) {
                properties.insert(k.to_string(), v.to_string());
            }
        }
        let reference = properties.get("Reference").cloned().unwrap_or_default();
        let Some(lib) = libs.get(lib_name.as_deref().unwrap_or(&lib_id)) else {
            warnings.push(format!(
                "{reference}: library symbol {lib_id} not found in the file; skipped"
            ));
            continue;
        };
        let at_list = sym.child("at");
        let at = at_list.and_then(xy).unwrap_or((0.0, 0.0));
        let angle = at_list.and_then(|a| a.arg_number(2)).unwrap_or(0.0);
        let mirror = sym
            .child("mirror")
            .and_then(|m| m.arg(0))
            .and_then(|m| m.chars().next());
        let unit = sym
            .child("unit")
            .and_then(|u| u.arg_number(0))
            .unwrap_or(1.0) as u32;
        let pins = lib
            .pins
            .iter()
            .filter(|(u, _)| *u == 0 || *u == unit)
            .map(|(_, p)| p.clone())
            .collect();
        let value = properties.get("Value").cloned().unwrap_or_default();
        out.push((
            Instance {
                lib: lib_id,
                reference,
                value,
                properties,
                at,
                angle,
                mirror,
                unit,
                pins,
            },
            lib.power,
        ));
    }
    out
}

fn is_ground_name(name: &str) -> bool {
    let u = name.to_ascii_uppercase();
    u.starts_with("GND") || u == "EARTH" || u == "0" || u == "VSS_GND"
}

// ------------------------------------------------------ part mapping ---

/// What a KiCad symbol becomes, and the Dunbrec pin names in order.
fn classify(inst: &Instance) -> Option<(K, &'static [&'static str])> {
    let device = inst
        .properties
        .get("Sim.Device")
        .map(|d| d.to_ascii_uppercase());
    let name = inst
        .lib
        .rsplit(':')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    let lib = inst
        .lib
        .split(':')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    const TWO: &[&str] = &["1", "2"];
    let model = || {
        inst.properties
            .get("Sim.Name")
            .filter(|n| !n.is_empty())
            .cloned()
            .unwrap_or_else(|| inst.value.clone())
    };
    let kind_of = |d: &str| -> Option<(K, &'static [&'static str])> {
        Some(match d {
            "R" => (K::Resistor { resistance: 1e3 }, TWO),
            "C" => (
                K::Capacitor {
                    capacitance: 1e-6,
                    initial_voltage: None,
                },
                TWO,
            ),
            "L" => (
                K::Inductor {
                    inductance: 1e-3,
                    initial_current: None,
                },
                TWO,
            ),
            "V" => (
                K::VoltageSource {
                    waveform: Waveform::Dc(0.0),
                },
                &["+", "-"],
            ),
            "I" => (
                K::CurrentSource {
                    waveform: Waveform::Dc(0.0),
                },
                &["+", "-"],
            ),
            "D" => (K::Diode { model: model() }, &["A", "K"]),
            "NPN" | "PNP" => (
                K::Bjt {
                    polarity: if d == "NPN" { Polarity::N } else { Polarity::P },
                    model: model(),
                },
                &["C", "B", "E"],
            ),
            "NMOS" | "PMOS" => (
                K::Mosfet {
                    polarity: if d == "NMOS" {
                        Polarity::N
                    } else {
                        Polarity::P
                    },
                    model: model(),
                },
                &["D", "G", "S"],
            ),
            _ => return None,
        })
    };
    if let Some(d) = device.as_deref() {
        // KiCad 7 writes e.g. Sim.Device=NPN, or Sim.Device=SPICE for raw lines.
        if let Some(k) = kind_of(d) {
            return Some(k);
        }
    }
    let starts = |p: &str| name.starts_with(p);
    if starts("R_POT") || name.contains("POTENTIOMETER") {
        return Some((
            K::Potentiometer {
                resistance: 10e3,
                wiper: 0.5,
            },
            &["1", "3", "2"],
        ));
    }
    if lib == "SWITCH" && (starts("SW_SPST") || starts("SW_PUSH")) {
        return Some((
            K::Switch {
                closed: false,
                r_on: 0.01,
                r_off: 1e9,
                momentary: starts("SW_PUSH"),
            },
            TWO,
        ));
    }
    if lib == "SIMULATION_SPICE" {
        if starts("V") {
            return kind_of("V");
        }
        if starts("I") {
            return kind_of("I");
        }
    }
    if name == "R" || starts("R_") {
        return kind_of("R");
    }
    if name == "C" || starts("C_") || starts("CP") {
        return kind_of("C");
    }
    if name == "L" || starts("L_") {
        return kind_of("L");
    }
    if name == "D" || starts("D_") || starts("LED") {
        return kind_of("D");
    }
    if name.contains("NPN") {
        return kind_of("NPN");
    }
    if name.contains("PNP") {
        return kind_of("PNP");
    }
    if name.contains("NMOS") || name.contains("NFET") {
        return kind_of("NMOS");
    }
    if name.contains("PMOS") || name.contains("PFET") {
        return kind_of("PMOS");
    }
    if lib == "TRANSISTOR_BJT" {
        return kind_of("NPN");
    }
    None
}

/// `key=value` pairs of a `Sim.Params` field.
fn sim_params(inst: &Instance) -> BTreeMap<String, f64> {
    inst.properties
        .get("Sim.Params")
        .map(|p| {
            p.split_whitespace()
                .filter_map(|kv| kv.split_once('='))
                .filter_map(|(k, v)| Some((k.to_ascii_lowercase(), parse_spice(v).ok()?)))
                .collect()
        })
        .unwrap_or_default()
}

/// A source's waveform from `Sim.Type`/`Sim.Params`, or from a SPICE-style
/// value such as `dc 5` or `sin(0 1 1k)`.
fn source_waveform(inst: &Instance, warnings: &mut Vec<String>) -> Waveform {
    let p = sim_params(inst);
    let get = |k: &str| p.get(k).copied().unwrap_or(0.0);
    let sim_type = inst
        .properties
        .get("Sim.Type")
        .map(|t| t.to_ascii_uppercase());
    match sim_type.as_deref() {
        Some("SIN") => {
            return Waveform::Sine {
                offset: get("dc"),
                amplitude: get("ampl"),
                frequency: get("f"),
                delay: get("td"),
                damping: get("theta"),
                phase: get("phase"),
            };
        }
        Some("PULSE") => {
            return Waveform::Pulse {
                v1: get("y1"),
                v2: get("y2"),
                delay: get("td"),
                rise: get("tr"),
                fall: get("tf"),
                width: p.get("tw").copied().unwrap_or(1e30),
                period: get("per"),
            };
        }
        Some("DC") | None if p.contains_key("dc") => return Waveform::Dc(get("dc")),
        Some(other) if other != "DC" => {
            warnings.push(format!(
                "{}: source type {other} is not supported; using 0",
                inst.reference
            ));
            return Waveform::Dc(0.0);
        }
        _ => {}
    }
    // Legacy: the Value field holds a SPICE source specification.
    let deck = format!("kicad\nV1 a 0 {}\n", inst.value);
    match crate::spice::import(&deck) {
        Ok(imp) if !imp.parts.is_empty() => match &imp.parts[0].kind {
            K::VoltageSource { waveform } => waveform.clone(),
            _ => Waveform::Dc(0.0),
        },
        _ => {
            warnings.push(format!(
                "{}: could not read source value '{}'; using 0",
                inst.reference, inst.value
            ));
            Waveform::Dc(0.0)
        }
    }
}

/// Fill in a classified part's value from the KiCad fields.
fn apply_value(kind: &mut K, inst: &Instance, warnings: &mut Vec<String>) {
    let params = sim_params(inst);
    let numeric = |param: &str| -> Option<f64> {
        params
            .get(param)
            .copied()
            .or_else(|| parse_eng(&inst.value).ok())
    };
    let mut warn_value = |what: &str| {
        warnings.push(format!(
            "{}: could not read {what} from value '{}'; default used",
            inst.reference, inst.value
        ))
    };
    match kind {
        K::Resistor { resistance } => match numeric("r") {
            Some(v) => *resistance = v,
            None => warn_value("a resistance"),
        },
        K::Capacitor { capacitance, .. } => match numeric("c") {
            Some(v) => *capacitance = v,
            None => warn_value("a capacitance"),
        },
        K::Inductor { inductance, .. } => match numeric("l") {
            Some(v) => *inductance = v,
            None => warn_value("an inductance"),
        },
        K::Potentiometer { resistance, .. } => match numeric("r") {
            Some(v) => *resistance = v,
            None => warn_value("a resistance"),
        },
        K::VoltageSource { waveform } | K::CurrentSource { waveform } => {
            *waveform = source_waveform(inst, warnings);
        }
        _ => {}
    }
}

/// Symbol pin for each of the part's pins, using `Sim.Pins` if present,
/// else pin names, else pin numbers.
fn map_pins<'a>(inst: &'a Instance, wanted: &[&str]) -> Option<Vec<&'a LibPin>> {
    if let Some(spec) = inst.properties.get("Sim.Pins") {
        let map: HashMap<String, String> = spec
            .split_whitespace()
            .filter_map(|kv| kv.split_once('='))
            .map(|(num, model_pin)| (model_pin.to_ascii_uppercase(), num.to_string()))
            .collect();
        let found: Option<Vec<&LibPin>> = wanted
            .iter()
            .map(|w| {
                // Two-terminal parts call their pins "+"/"-" or "1"/"2".
                let alias = match *w {
                    "1" => "+",
                    "2" => "-",
                    other => other,
                };
                let num = map
                    .get(&w.to_ascii_uppercase())
                    .or_else(|| map.get(alias))?;
                inst.pins.iter().find(|p| &p.number == num)
            })
            .collect();
        if found.is_some() {
            return found;
        }
    }
    let by_name: Option<Vec<&LibPin>> = wanted
        .iter()
        .map(|w| inst.pins.iter().find(|p| p.name.eq_ignore_ascii_case(w)))
        .collect();
    if by_name.is_some() {
        return by_name;
    }
    let numbers: &[&str] = match wanted {
        ["+", "-"] => &["1", "2"],
        // KiCad's Device library numbers a diode's cathode 1.
        ["A", "K"] => &["2", "1"],
        _ => wanted,
    };
    numbers
        .iter()
        .map(|n| inst.pins.iter().find(|p| p.number == *n))
        .collect()
}

/// Dunbrec rotation whose pin-0 → pin-1 direction best matches `target`.
fn best_rotation(kind: &K, target: (f64, f64)) -> Rotation {
    let pins = kind.pins();
    if pins.len() < 2 {
        return Rotation::R0;
    }
    let r = pins[1].offset - pins[0].offset;
    [Rotation::R0, Rotation::R90, Rotation::R180, Rotation::R270]
        .into_iter()
        .max_by(|a, b| {
            let dot = |rot: Rotation| {
                let v = rot.apply(r);
                v.x as f64 * target.0 + v.y as f64 * target.1
            };
            dot(*a).total_cmp(&dot(*b))
        })
        .unwrap_or(Rotation::R0)
}

pub fn import(text: &str) -> Result<KicadImport, KicadError> {
    let root = sexpr::parse(text).map_err(|e| KicadError(e.to_string()))?;
    if root.head() != Some("kicad_sch") {
        return Err(KicadError("not a KiCad schematic (.kicad_sch) file".into()));
    }
    let mut warnings = Vec::new();
    if root.child("sheet").is_some() {
        warnings.push(
            "hierarchical sheets are not supported yet: only the top sheet was imported".into(),
        );
    }
    let libs = lib_symbols(&root);
    let insts = instances(&root, &libs, &mut warnings);

    // ---- drawing geometry ----
    let wires: Vec<(Key, Key)> = root
        .children("wire")
        .filter_map(|w| {
            let pts: Vec<(f64, f64)> = w.child("pts")?.children("xy").filter_map(xy).collect();
            (pts.len() == 2).then(|| (key(pts[0].0, pts[0].1), key(pts[1].0, pts[1].1)))
        })
        .collect();
    let junctions: Vec<Key> = root
        .children("junction")
        .filter_map(|j| j.child("at").and_then(xy))
        .map(|(x, y)| key(x, y))
        .collect();
    let labels: Vec<(String, Key)> = ["label", "global_label"]
        .iter()
        .flat_map(|kind| root.children(kind))
        .filter_map(|l| Some((l.arg(0)?.to_string(), l.child("at").and_then(xy)?)))
        .map(|(n, (x, y))| (n, key(x, y)))
        .collect();

    // ---- pin positions, with an orientation check ----
    let fixed_points: Vec<Key> = wires
        .iter()
        .flat_map(|(a, b)| [*a, *b])
        .chain(junctions.iter().copied())
        .chain(labels.iter().map(|(_, k)| *k))
        .collect();
    let primary: Vec<Vec<Key>> = insts
        .iter()
        .map(|(inst, _)| {
            inst.pins
                .iter()
                .map(|p| {
                    let (x, y) = place(p.at, inst.at, inst.angle, inst.mirror, 0);
                    key(x, y)
                })
                .collect()
        })
        .collect();
    let all_primary: Vec<Key> = primary.iter().flatten().copied().collect();
    let hits = |pts: &[Key], own: &[Key]| -> usize {
        pts.iter()
            .filter(|p| {
                fixed_points.contains(p)
                    || wires.iter().any(|(a, b)| on_interior(**p, *a, *b))
                    || all_primary.iter().filter(|q| q == p).count()
                        > own.iter().filter(|q| q == p).count()
            })
            .count()
    };
    let mut pin_keys: Vec<Vec<Key>> = Vec::with_capacity(insts.len());
    for (i, (inst, power)) in insts.iter().enumerate() {
        let base = &primary[i];
        let base_hits = hits(base, base);
        if base_hits == base.len() || inst.pins.is_empty() {
            pin_keys.push(base.clone());
            continue;
        }
        let mut best = (base_hits, base.clone(), 0);
        for extra in 1..8 {
            let cand: Vec<Key> = inst
                .pins
                .iter()
                .map(|p| {
                    let (x, y) = place(p.at, inst.at, inst.angle, inst.mirror, extra);
                    key(x, y)
                })
                .collect();
            let h = hits(&cand, base);
            if h > best.0 {
                best = (h, cand, extra);
            }
        }
        if best.2 != 0 {
            warnings.push(format!(
                "{}: its pins only meet the wires in a different orientation; that orientation was used",
                inst.reference
            ));
        }
        if best.0 < inst.pins.len() && !power {
            warnings.push(format!(
                "{}: {} of {} pins are not connected in the KiCad drawing",
                inst.reference,
                inst.pins.len() - best.0,
                inst.pins.len()
            ));
        }
        pin_keys.push(best.1);
    }

    // ---- connectivity ----
    let mut dsu = Dsu(Vec::new());
    let mut ids: HashMap<Key, usize> = HashMap::new();
    let mut id = |dsu: &mut Dsu, k: Key| *ids.entry(k).or_insert_with(|| dsu.make());
    for (a, b) in &wires {
        let (x, y) = (id(&mut dsu, *a), id(&mut dsu, *b));
        dsu.union(x, y);
    }
    for k in pin_keys
        .iter()
        .flatten()
        .chain(&junctions)
        .chain(labels.iter().map(|(_, k)| k))
    {
        id(&mut dsu, *k);
    }
    let points: Vec<(Key, usize)> = ids.iter().map(|(k, v)| (*k, *v)).collect();
    for (a, b) in &wires {
        let w = ids[a];
        for (p, pid) in &points {
            if on_interior(*p, *a, *b) {
                dsu.union(w, *pid);
            }
        }
    }
    // Same-named labels, and power symbols by their value.
    let mut by_name: HashMap<String, usize> = HashMap::new();
    let mut named: Vec<(String, usize)> = labels.iter().map(|(n, k)| (n.clone(), ids[k])).collect();
    for ((inst, power), keys) in insts.iter().zip(&pin_keys) {
        if *power {
            let name = if is_ground_name(&inst.value) {
                "0".to_string()
            } else {
                inst.value.clone()
            };
            for k in keys {
                named.push((name.clone(), ids[k]));
            }
        }
    }
    for (name, pid) in &named {
        match by_name.get(name) {
            Some(&other) => dsu.union(other, *pid),
            None => {
                by_name.insert(name.clone(), *pid);
            }
        }
    }

    // Net names: ground, then a label or power name, then automatic.
    let mut net_names: HashMap<usize, String> = HashMap::new();
    let mut sorted_named = named.clone();
    sorted_named.sort_by(|a, b| (a.0 != "0", &a.0).cmp(&(b.0 != "0", &b.0)));
    for (name, pid) in sorted_named {
        let root = dsu.find(pid);
        net_names.entry(root).or_insert(name);
    }
    let mut auto = 0;
    let mut net_of = |dsu: &mut Dsu, k: Key| -> String {
        let root = dsu.find(ids[&k]);
        net_names
            .entry(root)
            .or_insert_with(|| {
                auto += 1;
                format!("Net{auto}")
            })
            .clone()
    };

    // ---- parts ----
    let mut parts = Vec::new();
    let mut models = ModelLibrary::new();
    for ((inst, power), keys) in insts.iter().zip(&pin_keys) {
        if *power || inst.reference.starts_with('#') {
            continue;
        }
        if inst.properties.get("Sim.Enable").is_some_and(|v| v == "0")
            || inst
                .properties
                .get("Spice_Netlist_Enabled")
                .is_some_and(|v| v.eq_ignore_ascii_case("N"))
        {
            continue;
        }
        let Some((mut kind, wanted)) = classify(inst) else {
            warnings.push(format!(
                "{} ({}): not a part Dunbrec can simulate; skipped",
                inst.reference, inst.lib
            ));
            continue;
        };
        let Some(pins) = map_pins(inst, wanted) else {
            warnings.push(format!(
                "{} ({}): could not match its pins; skipped",
                inst.reference, inst.lib
            ));
            continue;
        };
        if inst.unit > 1 {
            warnings.push(format!(
                "{}: unit {} of a multi-unit symbol",
                inst.reference, inst.unit
            ));
        }
        apply_value(&mut kind, inst, &mut warnings);

        // Make sure semiconductor models exist.
        let model_kind = match &kind {
            K::Diode { .. } => Some(ModelKind::Diode),
            K::Bjt {
                polarity: Polarity::N,
                ..
            } => Some(ModelKind::Npn),
            K::Bjt { .. } => Some(ModelKind::Pnp),
            K::Mosfet {
                polarity: Polarity::N,
                ..
            } => Some(ModelKind::Nmos),
            K::Mosfet { .. } => Some(ModelKind::Pmos),
            _ => None,
        };
        if let (Some(mk), Some(name)) = (model_kind, kind.model_name())
            && models.get(name).is_none()
        {
            warnings.push(format!(
                    "{}: model '{name}' parameters are not in the schematic; SPICE defaults used (edit the model to match the real part)",
                    inst.reference
                ));
            models.insert(DeviceModel::new(name, mk));
        }

        let index_of = |p: &LibPin| {
            inst.pins
                .iter()
                .position(|q| q.number == p.number && q.at == p.at)
        };
        let nets: Vec<String> = pins
            .iter()
            .map(|p| index_of(p).map_or_else(|| "?".into(), |i| net_of(&mut dsu, keys[i])))
            .collect();
        let first = index_of(pins[0]).map(|i| keys[i]);
        let second = pins.get(1).and_then(|p| index_of(p)).map(|i| keys[i]);
        let rotation = match (first, second) {
            (Some(a), Some(b)) => best_rotation(&kind, ((b.0 - a.0) as f64, (b.1 - a.1) as f64)),
            _ => Rotation::R0,
        };
        parts.push(Part {
            designator: inst.reference.clone(),
            kind,
            nets,
            position: Some(Point::new(
                (inst.at.0 / KICAD_GRID_MM).round() as i32,
                (inst.at.1 / KICAD_GRID_MM).round() as i32,
            )),
            rotation,
        });
    }
    Ok(KicadImport {
        parts,
        models,
        warnings,
    })
}

#[cfg(test)]
mod tests;
