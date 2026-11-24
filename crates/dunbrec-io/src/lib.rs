//! File formats for Dunbrec.
//!
//! * [`dbr`]: the native `.dbr` schematic format.
//! * [`spice`]: SPICE netlist import; [`spice_export`]: SPICE export.
//! * [`kicad`]: KiCad schematic import ([`sexpr`] reads the file format).
//! * [`layout`]: turns imported parts and nets into a drawable schematic.
//! * [`tidy`]: draws new parts with real wires, rails and ground symbols.

pub mod dbr;
pub mod kicad;
pub mod layout;
pub mod sexpr;
pub mod spice;
pub mod spice_export;
pub mod tidy;
