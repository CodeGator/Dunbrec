//! KiCad import tests on hand-written schematics in the KiCad 7 format.
//! Pin positions in the comments are worked out by hand from the library
//! pin coordinates (y up) and the symbol placement (y down).

use super::*;
use dunbrec_core::build_netlist;

const LIB_R: &str = r#"
    (symbol "Device:R" (pin_numbers hide) (pin_names (offset 0)) (in_bom yes) (on_board yes)
      (property "Reference" "R" (at 2.032 0 90) (effects (font (size 1.27 1.27))))
      (property "Value" "R" (at 0 0 90) (effects (font (size 1.27 1.27))))
      (symbol "R_0_1"
        (rectangle (start -1.016 -2.54) (end 1.016 2.54) (stroke (width 0.254) (type default)) (fill (type none))))
      (symbol "R_1_1"
        (pin passive line (at 0 3.81 270) (length 1.27)
          (name "~" (effects (font (size 1.27 1.27))))
          (number "1" (effects (font (size 1.27 1.27)))))
        (pin passive line (at 0 -3.81 90) (length 1.27)
          (name "~" (effects (font (size 1.27 1.27))))
          (number "2" (effects (font (size 1.27 1.27)))))))"#;

const LIB_VDC: &str = r#"
    (symbol "Simulation_SPICE:VDC" (pin_numbers hide) (pin_names (offset 0.0254)) (in_bom yes) (on_board yes)
      (property "Reference" "V" (at 2.54 2.54 0) (effects (font (size 1.27 1.27))))
      (symbol "VDC_1_1"
        (pin passive line (at 0 5.08 270) (length 2.54) (name "~" (effects (font (size 1.27 1.27)))) (number "1" (effects (font (size 1.27 1.27)))))
        (pin passive line (at 0 -5.08 90) (length 2.54) (name "~" (effects (font (size 1.27 1.27)))) (number "2" (effects (font (size 1.27 1.27)))))))"#;

const LIB_GND: &str = r##"
    (symbol "power:GND" (power) (pin_names (offset 0)) (in_bom yes) (on_board yes)
      (property "Reference" "#PWR" (at 0 -6.35 0) (effects (font (size 1.27 1.27)) hide))
      (property "Value" "GND" (at 0 -3.81 0) (effects (font (size 1.27 1.27))))
      (symbol "GND_1_1"
        (pin power_in line (at 0 0 270) (length 0) hide (name "GND" (effects (font (size 1.27 1.27)))) (number "1" (effects (font (size 1.27 1.27)))))))"##;

const LIB_NPN: &str = r#"
    (symbol "Device:Q_NPN_BCE" (pin_names (offset 0) hide) (in_bom yes) (on_board yes)
      (property "Reference" "Q" (at 5.08 1.27 0) (effects (font (size 1.27 1.27))))
      (symbol "Q_NPN_BCE_1_1"
        (pin input line (at -5.08 0 0) (length 5.715) (name "B" (effects (font (size 1.27 1.27)))) (number "1" (effects (font (size 1.27 1.27)))))
        (pin passive line (at 2.54 5.08 270) (length 2.54) (name "C" (effects (font (size 1.27 1.27)))) (number "2" (effects (font (size 1.27 1.27)))))
        (pin passive line (at 2.54 -5.08 90) (length 2.54) (name "E" (effects (font (size 1.27 1.27)))) (number "3" (effects (font (size 1.27 1.27)))))))"#;

const LIB_CONN: &str = r#"
    (symbol "Connector:Conn_01x02" (pin_names (offset 1.016) hide) (in_bom yes) (on_board yes)
      (property "Reference" "J" (at 0 2.54 0) (effects (font (size 1.27 1.27))))
      (symbol "Conn_01x02_1_1"
        (pin passive line (at -5.08 0 0) (length 3.81) (name "Pin_1" (effects (font (size 1.27 1.27)))) (number "1" (effects (font (size 1.27 1.27)))))
        (pin passive line (at -5.08 -2.54 0) (length 3.81) (name "Pin_2" (effects (font (size 1.27 1.27)))) (number "2" (effects (font (size 1.27 1.27)))))))"#;

fn symbol(lib: &str, reference: &str, value: &str, at: (f64, f64, f64), extra: &str) -> String {
    format!(
        r#"  (symbol (lib_id "{lib}") (at {} {} {}) (unit 1) (in_bom yes) (on_board yes) (dnp no)
    (uuid 00000000-0000-0000-0000-000000000000)
    (property "Reference" "{reference}" (at 0 0 0) (effects (font (size 1.27 1.27))))
    (property "Value" "{value}" (at 0 0 0) (effects (font (size 1.27 1.27))))
    {extra})
"#,
        at.0, at.1, at.2
    )
}

fn wire(a: (f64, f64), b: (f64, f64)) -> String {
    format!(
        "  (wire (pts (xy {} {}) (xy {} {})) (stroke (width 0) (type default)) (uuid 00000000-0000-0000-0000-000000000001))\n",
        a.0, a.1, b.0, b.1
    )
}

fn label(name: &str, at: (f64, f64)) -> String {
    format!(
        "  (label \"{name}\" (at {} {} 0) (fields_autoplaced) (effects (font (size 1.27 1.27)) (justify left bottom)) (uuid 00000000-0000-0000-0000-000000000002))\n",
        at.0, at.1
    )
}

fn sheet(libs: &[&str], body: &str) -> String {
    format!(
        "(kicad_sch (version 20230121) (generator eeschema)\n  (uuid 00000000-0000-0000-0000-00000000000a)\n  (paper \"A4\")\n  (lib_symbols{})\n{body})\n",
        libs.concat()
    )
}

/// 10 V source, R1 from the supply to VOUT, R2 and R3 (rotated 90°) from
/// VOUT to ground; an unconnected connector that cannot be simulated.
fn divider() -> String {
    let mut b = String::new();
    // V1 at (100,100): pin 1 (+) at (100,94.92), pin 2 at (100,105.08).
    b += &symbol(
        "Simulation_SPICE:VDC",
        "V1",
        "10",
        (100.0, 100.0, 0.0),
        r#"(property "Sim.Device" "V" (at 0 0 0)) (property "Sim.Type" "DC" (at 0 0 0)) (property "Sim.Params" "dc=10" (at 0 0 0))"#,
    );
    // R1 at (120,90): pins (120,86.19) and (120,93.81).
    b += &symbol("Device:R", "R1", "1k", (120.0, 90.0, 0.0), "");
    // R2 at (120,110): pins (120,106.19) and (120,113.81).
    b += &symbol("Device:R", "R2", "1k", (120.0, 110.0, 0.0), "");
    // R3 at (140,100) rotated 90°: pins (136.19,100) and (143.81,100).
    b += &symbol("Device:R", "R3", "1k", (140.0, 100.0, 90.0), "");
    b += &symbol("power:GND", "#PWR01", "GND", (110.0, 120.0, 0.0), "");
    b += &symbol(
        "Connector:Conn_01x02",
        "J1",
        "Conn_01x02",
        (200.0, 100.0, 0.0),
        "",
    );
    b += &wire((100.0, 94.92), (100.0, 86.19));
    b += &wire((100.0, 86.19), (120.0, 86.19));
    b += &wire((120.0, 93.81), (120.0, 106.19));
    b += &wire((120.0, 100.0), (136.19, 100.0)); // T-junction on the R1–R2 wire
    b += &wire((143.81, 100.0), (143.81, 120.0));
    b += &wire((100.0, 105.08), (100.0, 120.0));
    b += &wire((100.0, 120.0), (143.81, 120.0)); // GND symbol sits on this wire
    b += &wire((120.0, 113.81), (120.0, 120.0));
    b += &label("VOUT", (125.0, 100.0));
    sheet(&[LIB_R, LIB_VDC, LIB_GND, LIB_CONN], &b)
}

#[test]
fn divider_imports_and_simulates() {
    let imp = import(&divider()).unwrap();
    // Only the connector is reported: no orientation guesses, no other gaps.
    assert!(
        imp.warnings.iter().all(|w| w.starts_with("J1")),
        "{:?}",
        imp.warnings
    );
    assert_eq!(imp.parts.len(), 4);

    let r3 = imp.parts.iter().find(|p| p.designator == "R3").unwrap();
    assert_eq!(r3.nets, vec!["VOUT", "0"]);
    assert_eq!(
        r3.rotation,
        Rotation::R0,
        "horizontal in KiCad, horizontal here"
    );
    let r1 = imp.parts.iter().find(|p| p.designator == "R1").unwrap();
    assert_eq!(
        r1.rotation,
        Rotation::R90,
        "vertical in KiCad, vertical here"
    );
    let v1 = imp.parts.iter().find(|p| p.designator == "V1").unwrap();
    assert_eq!(
        v1.kind,
        K::VoltageSource {
            waveform: Waveform::Dc(10.0)
        }
    );

    // 10 V × (R2 ∥ R3) / (R1 + R2 ∥ R3) = 10/3 V.
    let s = imp.to_schematic();
    let netlist = build_netlist(&s);
    assert!(netlist.diagnostics.is_empty(), "{:?}", netlist.diagnostics);
    let c = dunbrec_sim::Circuit::from_netlist(&netlist, &s.models).unwrap();
    let op = dunbrec_sim::operating_point(&c).unwrap();
    assert!((op.voltage_by_name("VOUT").unwrap() - 10.0 / 3.0).abs() < 1e-9);
}

#[test]
fn transistor_pins_by_name_and_rotation() {
    let mut b = String::new();
    // Q1 at (50,50): B (44.92,50), C (52.54,44.92), E (52.54,55.08).
    b += &symbol("Device:Q_NPN_BCE", "Q1", "2N3904", (50.0, 50.0, 0.0), "");
    b += &label("B1", (44.92, 50.0));
    b += &label("C1", (52.54, 44.92));
    b += &label("E1", (52.54, 55.08));
    // Q2 at (80,50) rotated 180°: B (85.08,50), C (77.46,55.08), E (77.46,44.92).
    b += &symbol("Device:Q_NPN_BCE", "Q2", "2N3904", (80.0, 50.0, 180.0), "");
    b += &label("B2", (85.08, 50.0));
    b += &label("C2", (77.46, 55.08));
    b += &label("E2", (77.46, 44.92));
    let imp = import(&sheet(&[LIB_NPN], &b)).unwrap();

    let q = |d: &str| imp.parts.iter().find(|p| p.designator == d).unwrap();
    assert_eq!(q("Q1").nets, vec!["C1", "B1", "E1"]);
    assert_eq!(q("Q2").nets, vec!["C2", "B2", "E2"]);
    assert_eq!(
        q("Q1").kind,
        K::Bjt {
            polarity: Polarity::N,
            model: "2N3904".into()
        }
    );
    // The model is not in the file: a placeholder plus one warning.
    assert_eq!(imp.models.get("2N3904").unwrap().kind, ModelKind::Npn);
    let model_warnings = imp.warnings.iter().filter(|w| w.contains("model")).count();
    assert_eq!(model_warnings, 1, "{:?}", imp.warnings);
    assert!(
        !imp.warnings.iter().any(|w| w.contains("orientation")),
        "{:?}",
        imp.warnings
    );
}

#[test]
fn sim_pins_and_sim_params_are_used() {
    let mut b = String::new();
    // A sine source whose pins are swapped by Sim.Pins: symbol pin 2 is "+".
    b += &symbol(
        "Simulation_SPICE:VDC",
        "V1",
        "",
        (100.0, 100.0, 0.0),
        r#"(property "Sim.Device" "V" (at 0 0 0)) (property "Sim.Type" "SIN" (at 0 0 0))
           (property "Sim.Params" "dc=1 ampl=2 f=1k" (at 0 0 0)) (property "Sim.Pins" "1=- 2=+" (at 0 0 0))"#,
    );
    b += &label("TOP", (100.0, 94.92));
    b += &label("BOTTOM", (100.0, 105.08));
    let imp = import(&sheet(&[LIB_VDC], &b)).unwrap();
    let v1 = &imp.parts[0];
    assert_eq!(v1.nets, vec!["BOTTOM", "TOP"]);
    assert_eq!(
        v1.kind,
        K::VoltageSource {
            waveform: Waveform::Sine {
                offset: 1.0,
                amplitude: 2.0,
                frequency: 1e3,
                delay: 0.0,
                damping: 0.0,
                phase: 0.0
            }
        }
    );
}

#[test]
fn wrong_orientation_is_detected_and_fixed() {
    // R1 says rotation 0 (vertical) but is wired as if horizontal.
    let mut b = String::new();
    b += &symbol("Device:R", "R1", "1k", (50.0, 50.0, 0.0), "");
    b += &label("LEFT", (46.19, 50.0));
    b += &label("RIGHT", (53.81, 50.0));
    let imp = import(&sheet(&[LIB_R], &b)).unwrap();
    assert!(
        imp.warnings.iter().any(|w| w.contains("orientation")),
        "{:?}",
        imp.warnings
    );
    let mut nets = imp.parts[0].nets.clone();
    nets.sort();
    assert_eq!(nets, vec!["LEFT", "RIGHT"]);
}

#[test]
fn rejects_other_files() {
    assert!(import("(kicad_pcb (version 1))").is_err());
    assert!(import("not even an s-expression").is_err());
}
