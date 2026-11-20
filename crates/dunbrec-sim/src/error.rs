use dunbrec_core::Diagnostic;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Error)]
pub enum SimError {
    #[error("the schematic has errors that must be fixed before simulating")]
    Netlist(Vec<Diagnostic>),

    #[error("{designator}: {message}")]
    InvalidParameter { designator: String, message: String },

    #[error(
        "{designator} forms a loop of voltage sources and/or inductors; \
         the voltages around the loop cannot be satisfied"
    )]
    VoltageLoop { designator: String },

    #[error(
        "{designator} forces a current into node(s) {nodes:?}, which have no DC path to ground"
    )]
    CurrentSourceNoReturn {
        designator: String,
        nodes: Vec<String>,
    },

    #[error("the circuit equations cannot be solved (problem at {unknown})")]
    Singular { unknown: String },

    #[error(
        "the simulation did not converge (last unsettled value: {unknown}); \
         check for unrealistic component values or missing bias paths"
    )]
    NoConvergence { unknown: String },

    #[error("invalid analysis settings: {0}")]
    InvalidOptions(String),

    #[error(
        "the time step became too small at t = {time} s; the circuit may be changing too abruptly"
    )]
    TimestepTooSmall { time: f64 },
}

/// Non-fatal issues found while preparing or running a simulation.
#[derive(Debug, Clone, PartialEq)]
pub enum SimWarning {
    /// These nodes have no DC path to ground (e.g. they connect only through
    /// capacitors). A 1 TΩ resistor to ground was added to each so the
    /// operating point can be found; their DC voltage is not meaningful.
    FloatingNodes { nodes: Vec<String> },
}
