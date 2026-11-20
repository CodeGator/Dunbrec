//! DC operating point analysis.

use dunbrec_core::NodeId;

use crate::circuit::Circuit;
use crate::device::Tolerances;
use crate::error::{SimError, SimWarning};
use crate::newton::{Workspace, dc_solve};
use crate::stamp::{SolutionView, node_rows};

/// Result of a DC operating point analysis.
#[derive(Debug, Clone, PartialEq)]
pub struct OperatingPoint {
    node_names: Vec<String>,
    /// Voltage of every node, indexed by [`NodeId`]; ground is 0.
    voltages: Vec<f64>,
    /// (designator, current entering the device's first pin).
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
    let x = dc_solve(circuit, false, &mut ws, &Tolerances::default())?;
    Ok(OperatingPoint::from_solution(circuit, &x))
}

impl OperatingPoint {
    pub(crate) fn from_solution(circuit: &Circuit, x: &[f64]) -> Self {
        let rows = node_rows(circuit.node_count());
        let sol = SolutionView { x, node_rows: rows };
        let voltages = (0..circuit.node_count())
            .map(|i| sol.voltage(NodeId(i)))
            .collect();
        let currents = circuit
            .devices
            .iter()
            .map(|d| (d.designator().to_string(), d.current(&sol)))
            .collect();
        OperatingPoint {
            node_names: circuit.node_names.clone(),
            voltages,
            currents,
            warnings: circuit.warnings.clone(),
        }
    }
}
