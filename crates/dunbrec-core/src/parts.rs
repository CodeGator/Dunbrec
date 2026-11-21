//! Device models of real, widely used parts.
//!
//! These are the published manufacturer SPICE parameter sets (Fairchild /
//! ON Semiconductor for the transistors; the industry-standard models for
//! the diodes, as distributed with common SPICE tools). Dunbrec's
//! simulator tests check each against the part's datasheet limits.
//!
//! Parameters Dunbrec does not model yet (temperature coefficients such as
//! XTI/XTB/EG, ITF/VTF/XTF, soft-breakdown terms) are kept so the models
//! export unchanged, and are ignored by the simulator.

use crate::models::{DeviceModel, ModelKind};

/// A library part: its model and a one-line description.
pub struct LibraryPart {
    pub model: DeviceModel,
    pub description: &'static str,
}

fn m(name: &str, kind: ModelKind, params: &[(&str, f64)]) -> DeviceModel {
    params
        .iter()
        .fold(DeviceModel::new(name, kind), |m, (k, v)| m.with(k, *v))
}

pub fn standard_parts() -> Vec<LibraryPart> {
    vec![
        LibraryPart {
            description: "General-purpose small-signal NPN transistor (40 V, 200 mA)",
            model: m(
                "2N3904",
                ModelKind::Npn,
                &[
                    ("IS", 6.734e-15),
                    ("XTI", 3.0),
                    ("EG", 1.11),
                    ("VAF", 74.03),
                    ("BF", 416.4),
                    ("NE", 1.259),
                    ("ISE", 6.734e-15),
                    ("IKF", 66.78e-3),
                    ("XTB", 1.5),
                    ("BR", 0.7371),
                    ("NC", 2.0),
                    ("ISC", 0.0),
                    ("IKR", 0.0),
                    ("RC", 1.0),
                    ("CJC", 3.638e-12),
                    ("MJC", 0.3085),
                    ("VJC", 0.75),
                    ("FC", 0.5),
                    ("CJE", 4.493e-12),
                    ("MJE", 0.2593),
                    ("VJE", 0.75),
                    ("TR", 239.5e-9),
                    ("TF", 301.2e-12),
                    ("ITF", 0.4),
                    ("VTF", 4.0),
                    ("XTF", 2.0),
                    ("RB", 10.0),
                ],
            ),
        },
        LibraryPart {
            description: "General-purpose small-signal PNP transistor (40 V, 200 mA)",
            model: m(
                "2N3906",
                ModelKind::Pnp,
                &[
                    ("IS", 1.41e-15),
                    ("XTI", 3.0),
                    ("EG", 1.11),
                    ("VAF", 18.7),
                    ("BF", 180.7),
                    ("NE", 1.5),
                    ("ISE", 0.0),
                    ("IKF", 80e-3),
                    ("XTB", 1.5),
                    ("BR", 4.977),
                    ("NC", 2.0),
                    ("ISC", 0.0),
                    ("IKR", 0.0),
                    ("RC", 2.5),
                    ("CJC", 9.728e-12),
                    ("MJC", 0.5776),
                    ("VJC", 0.75),
                    ("FC", 0.5),
                    ("CJE", 8.063e-12),
                    ("MJE", 0.3677),
                    ("VJE", 0.75),
                    ("TR", 33.42e-9),
                    ("TF", 179.3e-12),
                    ("ITF", 0.4),
                    ("VTF", 4.0),
                    ("XTF", 6.0),
                    ("RB", 10.0),
                ],
            ),
        },
        LibraryPart {
            description: "Small-signal switching diode (100 V, 200 mA)",
            model: m(
                "1N4148",
                ModelKind::Diode,
                &[
                    ("IS", 2.52e-9),
                    ("RS", 0.568),
                    ("N", 1.752),
                    ("CJO", 4e-12),
                    ("M", 0.4),
                    ("TT", 20e-9),
                ],
            ),
        },
        LibraryPart {
            description: "Small-signal switching diode (100 V, 200 mA); the same die and SPICE model as the 1N4148",
            model: m(
                "1N914",
                ModelKind::Diode,
                &[
                    ("IS", 2.52e-9),
                    ("RS", 0.568),
                    ("N", 1.752),
                    ("CJO", 4e-12),
                    ("M", 0.4),
                    ("TT", 20e-9),
                ],
            ),
        },
        LibraryPart {
            description: "4.7 V zener diode (500 mW)",
            model: m(
                "1N750",
                ModelKind::Diode,
                &[
                    ("IS", 880.5e-18),
                    ("RS", 0.25),
                    ("N", 1.0),
                    ("CJO", 175e-12),
                    ("M", 0.5516),
                    ("VJ", 0.75),
                    ("FC", 0.5),
                    ("ISR", 1.859e-9),
                    ("NR", 2.0),
                    ("BV", 4.7),
                    ("IBV", 20.245e-3),
                    ("NBV", 1.6989),
                    ("IBVL", 1.9556e-3),
                    ("NBVL", 14.976),
                ],
            ),
        },
    ]
}

/// The library model called `name` (case-insensitive), if there is one.
pub fn standard_model(name: &str) -> Option<DeviceModel> {
    standard_parts()
        .into_iter()
        .map(|p| p.model)
        .find(|m| m.name.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup() {
        assert_eq!(standard_model("2n3904").unwrap().kind, ModelKind::Npn);
        assert_eq!(standard_model("1N750").unwrap().get_or("BV", 0.0), 4.7);
        assert_eq!(standard_model("1n914").unwrap().kind, ModelKind::Diode);
        assert!(standard_model("BC999").is_none());
    }
}
