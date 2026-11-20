//! DC sweeps: the operating point solved repeatedly while sources step
//! through values. Each point starts from the previous solution, which is
//! both faster and more robust than solving every point from scratch.

use dunbrec_core::{ComponentKind, Element, ModelLibrary, Netlist, Waveform};

use crate::circuit::Circuit;
use crate::dc::OperatingPoint;
use crate::device::{StampContext, Tolerances};
use crate::error::SimError;
use crate::newton::{DC_MAX_ITERATIONS, Workspace, dc_solve, newton};

pub struct DcSweeper {
    circuit: Circuit,
    elements: Vec<Element>,
    models: ModelLibrary,
    workspace: Workspace,
    /// Previous solution, the starting guess for the next point.
    x: Option<Vec<f64>>,
}

impl DcSweeper {
    pub fn new(netlist: &Netlist, models: &ModelLibrary) -> Result<Self, SimError> {
        let circuit = Circuit::from_netlist(netlist, models)?;
        let workspace = Workspace::new(circuit.unknown_count());
        Ok(Self {
            circuit,
            elements: netlist.elements.clone(),
            models: models.clone(),
            workspace,
            x: None,
        })
    }

    /// Set the DC value of an independent source (by designator).
    pub fn set_source(&mut self, designator: &str, value: f64) -> Result<(), SimError> {
        let index = self
            .elements
            .iter()
            .position(|e| {
                e.designator == designator
                    && matches!(
                        e.kind,
                        ComponentKind::VoltageSource { .. } | ComponentKind::CurrentSource { .. }
                    )
            })
            .ok_or_else(|| {
                SimError::InvalidOptions(format!(
                    "'{designator}' is not a voltage or current source"
                ))
            })?;
        let element = &mut self.elements[index];
        match &mut element.kind {
            ComponentKind::VoltageSource { waveform }
            | ComponentKind::CurrentSource { waveform } => {
                *waveform = Waveform::Dc(value);
            }
            _ => unreachable!("checked above"),
        }
        let replaced = self.circuit.replace_device(index, element, &self.models)?;
        debug_assert!(replaced, "a source keeps its structure");
        Ok(())
    }

    /// Solve the operating point for the present source values.
    pub fn solve(&mut self) -> Result<OperatingPoint, SimError> {
        let tol = Tolerances::default();
        if let Some(x) = &mut self.x {
            let mut guess = x.clone();
            if newton(
                &self.circuit,
                &StampContext::DC,
                &mut guess,
                &mut self.workspace,
                DC_MAX_ITERATIONS,
                &tol,
            )
            .is_ok()
            {
                *x = guess;
                return Ok(OperatingPoint::from_solution(&self.circuit, x));
            }
        }
        let x = dc_solve(&self.circuit, false, &mut self.workspace, &tol)?;
        let op = OperatingPoint::from_solution(&self.circuit, &x);
        self.x = Some(x);
        Ok(op)
    }
}

/// `count` evenly spaced values from `start` to `stop` inclusive.
pub fn linear_steps(start: f64, stop: f64, count: usize) -> Vec<f64> {
    if count < 2 {
        return vec![start];
    }
    (0..count)
        .map(|i| start + (stop - start) * i as f64 / (count - 1) as f64)
        .collect()
}

/// Operating points as `source` steps through `values`.
pub fn dc_sweep(
    netlist: &Netlist,
    models: &ModelLibrary,
    source: &str,
    values: &[f64],
) -> Result<Vec<OperatingPoint>, SimError> {
    let mut sweeper = DcSweeper::new(netlist, models)?;
    values
        .iter()
        .map(|&v| {
            sweeper.set_source(source, v)?;
            sweeper.solve()
        })
        .collect()
}
