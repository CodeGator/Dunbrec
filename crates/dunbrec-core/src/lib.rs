//! Core circuit data model for Dunbrec.
//!
//! * [`units`]: engineering value parsing/formatting (`4k7`, `10uF`).
//! * [`schematic`]: the graphical schematic (components, wires, labels).
//! * [`models`]: device model parameter sets (`.model` cards).
//! * [`netlist`]: converts a schematic into the node/element form the
//!   simulator consumes, with diagnostics for the editor to display.

pub mod models;
pub mod netlist;
pub mod schematic;
pub mod units;

pub use models::{DeviceModel, ModelKind, ModelLibrary};
pub use netlist::{
    Diagnostic, Element, Netlist, NetlistBuilder, Node, NodeId, Severity, build_netlist,
};
pub use schematic::{
    Component, ComponentId, ComponentKind, LabelId, NetLabel, Point, Polarity, Rotation, Schematic,
    Waveform, Wire, WireId,
};
