//! DC operating point analysis.

use dunbrec_core::NodeId;

use crate::circuit::{Circuit, FLOATING_SHUNT};
use crate::error::{SimError, SimWarning};
use crate::linalg::{DenseLu, DenseMatrix, LinearSolver};
use crate::stamp::{SolutionView, Stamper, node_rows};

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
/// sources at their t = 0 value.
pub fn operating_point(circuit: &Circuit) -> Result<OperatingPoint, SimError> {
    let size = circuit.unknown_count();
    let rows = node_rows(circuit.node_count());
    let mut matrix = DenseMatrix::zeros(size);
    let mut x = vec![0.0; size];

    {
        let mut s = Stamper::new(&mut matrix, &mut x, rows);
        for d in &circuit.devices {
            d.stamp_dc(&mut s);
        }
        for &node in &circuit.shunted {
            s.conductance(node, NodeId::GROUND, FLOATING_SHUNT);
        }
    }

    DenseLu::default()
        .solve(&mut matrix, &mut x)
        .map_err(|e| SimError::Singular {
            unknown: circuit.describe_unknown(e.index),
        })?;

    let sol = SolutionView {
        x: &x,
        node_rows: rows,
    };
    let voltages = (0..circuit.node_count())
        .map(|i| sol.voltage(NodeId(i)))
        .collect();
    let currents = circuit
        .devices
        .iter()
        .map(|d| (d.designator().to_string(), d.current(&sol)))
        .collect();

    Ok(OperatingPoint {
        node_names: circuit.node_names.clone(),
        voltages,
        currents,
        warnings: circuit.warnings.clone(),
    })
}
