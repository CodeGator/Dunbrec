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
//! Analyses: DC operating point ([`operating_point`]), DC sweep
//! ([`dc_sweep`], [`DcSweeper`]), AC small-signal ([`ac_analysis`]) and
//! transient ([`transient`], or step by step with [`Transient`]). Diodes,
//! BJTs and MOSFETs are solved with Newton-Raphson iteration.

pub mod ac;
pub mod circuit;
pub mod dc;
pub mod device;
pub mod digital;
pub mod error;
pub mod linalg;
pub mod macromodel;
pub mod mcu;
mod newton;
pub mod semiconductor;
pub mod stamp;
pub mod sweep;
pub mod transient;
pub mod waveform;

pub use ac::{AcResult, ac_analysis, log_frequencies};
pub use circuit::Circuit;
pub use dc::{OperatingPoint, lamp_name, operating_point};
pub use error::{SimError, SimWarning};
pub use mcu::{HaltReason, McuHalt, McuInfo, SerialByte, load_firmware, unbuilt_sketch};
pub use sweep::{DcSweeper, dc_sweep, linear_steps};
pub use transient::{
    Crossing, Edge, Sample, StepResult, Transient, TransientOptions, TransientResult, Watch,
    WatchId, transient,
};

#[cfg(test)]
mod ac_tests;
#[cfg(test)]
mod digital_tests;
#[cfg(test)]
mod ic_tests;
#[cfg(test)]
mod mcu_tests;
#[cfg(test)]
mod nonlinear_tests;
#[cfg(test)]
mod parts_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod transient_tests;
