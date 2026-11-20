//! Device model parameter sets (the SPICE `.model` card).
//!
//! Parameters are stored by their SPICE names (`IS`, `BF`, `VTO`, ...) so
//! models round-trip through SPICE import/export unchanged. The simulator
//! supplies defaults for anything not given.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelKind {
    Diode,
    Npn,
    Pnp,
    Nmos,
    Pmos,
}

impl ModelKind {
    /// SPICE model type keyword (`D`, `NPN`, ...).
    pub fn spice_name(self) -> &'static str {
        match self {
            ModelKind::Diode => "D",
            ModelKind::Npn => "NPN",
            ModelKind::Pnp => "PNP",
            ModelKind::Nmos => "NMOS",
            ModelKind::Pmos => "PMOS",
        }
    }

    pub fn from_spice_name(name: &str) -> Option<Self> {
        match name.to_ascii_uppercase().as_str() {
            "D" => Some(ModelKind::Diode),
            "NPN" => Some(ModelKind::Npn),
            "PNP" => Some(ModelKind::Pnp),
            "NMOS" => Some(ModelKind::Nmos),
            "PMOS" => Some(ModelKind::Pmos),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceModel {
    pub name: String,
    pub kind: ModelKind,
    /// Parameter values keyed by upper-case SPICE name.
    #[serde(default)]
    pub params: BTreeMap<String, f64>,
}

impl DeviceModel {
    pub fn new(name: impl Into<String>, kind: ModelKind) -> Self {
        Self {
            name: name.into(),
            kind,
            params: BTreeMap::new(),
        }
    }

    pub fn with(mut self, param: &str, value: f64) -> Self {
        self.set(param, value);
        self
    }

    pub fn set(&mut self, param: &str, value: f64) {
        self.params.insert(param.to_ascii_uppercase(), value);
    }

    /// Parameter value, or `default` if not specified.
    pub fn get_or(&self, param: &str, default: f64) -> f64 {
        self.params
            .get(&param.to_ascii_uppercase())
            .copied()
            .unwrap_or(default)
    }
}

/// Named models available to a schematic. Lookups are case-insensitive, as in
/// SPICE.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ModelLibrary {
    models: BTreeMap<String, DeviceModel>,
}

impl ModelLibrary {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a model; returns the previous model of that name.
    pub fn insert(&mut self, model: DeviceModel) -> Option<DeviceModel> {
        self.models.insert(model.name.to_ascii_uppercase(), model)
    }

    pub fn get(&self, name: &str) -> Option<&DeviceModel> {
        self.models.get(&name.to_ascii_uppercase())
    }

    pub fn remove(&mut self, name: &str) -> Option<DeviceModel> {
        self.models.remove(&name.to_ascii_uppercase())
    }

    pub fn iter(&self) -> impl Iterator<Item = &DeviceModel> {
        self.models.values()
    }

    pub fn len(&self) -> usize {
        self.models.len()
    }

    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_insensitive() {
        let mut lib = ModelLibrary::new();
        lib.insert(DeviceModel::new("Q2N3904", ModelKind::Npn).with("bf", 300.0));
        let m = lib.get("q2n3904").unwrap();
        assert_eq!(m.get_or("BF", 100.0), 300.0);
        assert_eq!(m.get_or("Bf", 100.0), 300.0);
        assert_eq!(m.get_or("VAF", 1e30), 1e30);
        assert_eq!(ModelKind::from_spice_name("npn"), Some(ModelKind::Npn));
    }
}
