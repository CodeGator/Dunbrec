//! Dunbrec simulation engine.
//!
//! The circuit is solved with Modified Nodal Analysis (MNA): every node
//! voltage and every voltage-source/inductor current becomes an unknown, each
//! device "stamps" its equations into a matrix, and the matrix is solved.
//!
//! ```
//! use dunbrec_core::{ComponentKind, ModelLibrary, NetlistBuilder, Waveform};
//! use dunbrec_sim::{Circuit, operating_point};
//!
//! let netlist = NetlistBuilder::new()
//!     .element("V1", ComponentKind::VoltageSource { waveform: Waveform::Dc(10.0) }, &["in", "0"])
//!     .element("R1", ComponentKind::Resistor { resistance: 1e3 }, &["in", "out"])
//!     .element("R2", ComponentKind::Resistor { resistance: 1e3 }, &["out", "0"])
//!     .build();
//! let circuit = Circuit::from_netlist(&netlist, &ModelLibrary::new()).unwrap();
//! let op = operating_point(&circuit).unwrap();
//! assert!((op.voltage_by_name("out").unwrap() - 5.0).abs() < 1e-9);
//! ```
//!
//! Current status: linear devices and DC operating point. Transient analysis,
//! the time scheduler and nonlinear devices follow.

pub mod circuit;
pub mod dc;
pub mod device;
pub mod error;
pub mod linalg;
pub mod stamp;

pub use circuit::Circuit;
pub use dc::{OperatingPoint, operating_point};
pub use error::{SimError, SimWarning};

#[cfg(test)]
mod tests;
