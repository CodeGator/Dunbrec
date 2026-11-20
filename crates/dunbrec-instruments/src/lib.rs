//! Virtual instruments for Dunbrec, independent of any user interface.
//!
//! * [`scope`]: four-channel oscilloscope acquisition with edge triggering
//!   (auto, normal, single).
//! * [`meter`]: digital multimeter (DC and true-RMS AC, volts and amps).
//! * [`measure`]: time-weighted waveform statistics and frequency.

pub mod measure;
pub mod meter;
pub mod scope;

pub use meter::{Meter, MeterMode};
pub use scope::{Scope, Status as ScopeStatus, Sweep, Trigger, TriggerEdge, TriggerMode};
