//! Core circuit data model for Dunbrec.
//!
//! * [`units`]: engineering value parsing/formatting (`4k7`, `10uF`).
//! * [`schematic`]: the graphical schematic (components, wires, labels).
//! * [`models`]: device model parameter sets (`.model` cards).
//! * [`parts`]: models of real parts (2N3904, 1N4148, ...).
//! * [`netlist`]: converts a schematic into the node/element form the
//!   simulator consumes, with diagnostics for the editor to display.

pub mod models;
pub mod netlist;
pub mod parts;
pub mod schematic;
pub mod units;

pub use models::{DeviceModel, ModelKind, ModelLibrary};
pub use netlist::{
    Diagnostic, Element, Netlist, NetlistBuilder, Node, NodeId, Severity, build_netlist,
};
pub use schematic::{
    Component, ComponentId, ComponentKind, DEFAULT_LOGIC_DELAY, DEFAULT_MCU_CLOCK, GateOp, LabelId,
    NetLabel, OPAMP_PARTS, PinDomain, Point, Polarity, Rotation, SOURCE_PREFIX, Schematic,
    SourceFile, SourceLanguage, SwitchForm, Waveform, Wire, WireId,
};
