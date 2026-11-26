//! DC operating point analysis.

use dunbrec_core::NodeId;

use crate::circuit::Circuit;
use crate::device::Tolerances;
use crate::error::{SimError, SimWarning};
use crate::newton::Workspace;
use crate::stamp::{SolutionView, node_rows};

/// What an LED of the display `designator` is called where currents are
/// listed, from its segment's name: "DS1.3", "DS2.dp".
pub fn lamp_name(designator: &str, segment: &str) -> String {
    format!("{designator}.{segment}")
}

/// Result of a DC operating point analysis.
#[derive(Debug, Clone, PartialEq)]
pub struct OperatingPoint {
    node_names: Vec<String>,
    /// Voltage of every node, indexed by [`NodeId`]; ground is 0.
    voltages: Vec<f64>,
    /// (designator, current entering the device's first pin), or for each
    /// LED of a display ([`lamp_name`], its current).
    currents: Vec<(String, f64)>,
    pub warnings: Vec<SimWarning>,
}

impl OperatingPoint {
    pub fn voltage(&self, node: NodeId) -> f64 {
        self.voltages[node.0]
    }

    pub fn voltage_by_name(&self, name: &str) -> Option<f64> {
        let i = self.node_names.iter().position(|n| n == name)?;
        Some(self.voltages[i])
    }

    /// Current entering the named device at its first pin. For a voltage
    /// source that is the current into its + terminal, so a source
    /// delivering power reports a negative current (SPICE convention).
    pub fn current(&self, designator: &str) -> Option<f64> {
        self.currents
            .iter()
            .find(|(d, _)| d == designator)
            .map(|(_, i)| *i)
    }

    pub fn voltages(&self) -> impl Iterator<Item = (&str, f64)> {
        self.node_names
            .iter()
            .map(String::as_str)
            .zip(self.voltages.iter().copied())
    }

    pub fn currents(&self) -> impl Iterator<Item = (&str, f64)> {
        self.currents.iter().map(|(d, i)| (d.as_str(), *i))
    }
}

/// Solve for the DC operating point: capacitors open, inductors shorted,
/// sources at their t = 0 value. Nonlinear circuits are solved by
/// Newton-Raphson iteration with gmin and source stepping as fallbacks.
pub fn operating_point(circuit: &Circuit) -> Result<OperatingPoint, SimError> {
    let mut ws = Workspace::new(circuit.unknown_count());
    let x = crate::digital::operating_solution(circuit, &mut ws, &Tolerances::default())?;
    Ok(OperatingPoint::from_solution(circuit, &x))
}

impl OperatingPoint {
    pub(crate) fn from_solution(circuit: &Circuit, x: &[f64]) -> Self {
        let rows = node_rows(circuit.node_count());
        let sol = SolutionView { x, node_rows: rows };
        let voltages = (0..circuit.node_count())
            .map(|i| sol.voltage(NodeId(i)))
            .collect();
        // A display has no current of its own: each of its LEDs is listed
        // instead, as "DS1.1", "DS1.2" and so on.
        let mut currents = Vec::with_capacity(circuit.devices.len());
        for (index, d) in circuit.devices.iter().enumerate() {
            let lamps = d.lamp_currents(&sol);
            if lamps.is_empty() {
                currents.push((d.designator().to_string(), d.current(&sol)));
            } else {
                let kind = &circuit.displays[&index];
                let named = lamps.into_iter().enumerate();
                currents.extend(
                    named.map(|(k, i)| (lamp_name(d.designator(), kind.segment_name(k)), i)),
                );
            }
        }
        OperatingPoint {
            node_names: circuit.node_names.clone(),
            voltages,
            currents,
            warnings: circuit.warnings.clone(),
        }
    }
}
