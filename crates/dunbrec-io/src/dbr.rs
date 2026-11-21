//! The native `.dbr` file format: a versioned RON document wrapping a
//! [`Schematic`].
//!
//! ```ron
//! (
//!     format: "dunbrec",
//!     version: 1,
//!     schematic: ( components: [...], wires: [...], ... ),
//! )
//! ```
//!
//! Older versions are upgraded on load by [`migrate`]; files from a newer
//! Dunbrec are rejected rather than silently misread.

use std::fs;
use std::path::Path;

use dunbrec_core::Schematic;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const FORMAT_TAG: &str = "dunbrec";
pub const CURRENT_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum DbrError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("not a Dunbrec file (format tag '{0}')")]
    WrongFormat(String),
    #[error("file version {found} is newer than this Dunbrec supports (up to {supported})")]
    UnsupportedVersion { found: u32, supported: u32 },
    #[error("malformed file: {0}")]
    Parse(String),
    #[error("could not serialise schematic: {0}")]
    Serialise(String),
}

#[derive(Serialize)]
struct DbrOut<'a> {
    format: &'a str,
    version: u32,
    schematic: &'a Schematic,
}

/// Read only the header first, so a future version with an incompatible body
/// gives a clear version error rather than a parse error.
#[derive(Deserialize)]
struct Header {
    format: String,
    version: u32,
}

#[derive(Deserialize)]
struct DbrIn {
    schematic: Schematic,
}

pub fn to_string(schematic: &Schematic) -> Result<String, DbrError> {
    let doc = DbrOut {
        format: FORMAT_TAG,
        version: CURRENT_VERSION,
        schematic,
    };
    let config = ron::ser::PrettyConfig::new().struct_names(false);
    ron::ser::to_string_pretty(&doc, config).map_err(|e| DbrError::Serialise(e.to_string()))
}

pub fn from_str(text: &str) -> Result<Schematic, DbrError> {
    let options = ron::Options::default();
    let header: HeaderOnly = options
        .from_str(text)
        .map_err(|e| DbrError::Parse(e.to_string()))?;
    let header = header.0;
    if header.format != FORMAT_TAG {
        return Err(DbrError::WrongFormat(header.format));
    }
    if header.version > CURRENT_VERSION {
        return Err(DbrError::UnsupportedVersion {
            found: header.version,
            supported: CURRENT_VERSION,
        });
    }
    let text = migrate(text, header.version)?;
    let doc: DbrIn = options
        .from_str(&text)
        .map_err(|e| DbrError::Parse(e.to_string()))?;
    Ok(doc.schematic)
}

pub fn save(schematic: &Schematic, path: impl AsRef<Path>) -> Result<(), DbrError> {
    let text = to_string(schematic)?;
    // Write to a temporary file then rename, so a crash never leaves a
    // half-written schematic behind.
    let path = path.as_ref();
    let tmp = path.with_extension("dbr.tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn load(path: impl AsRef<Path>) -> Result<Schematic, DbrError> {
    from_str(&fs::read_to_string(path)?)
}

/// Upgrade a document from `version` to [`CURRENT_VERSION`]. Version 1 is the
/// first format, so there is nothing to do yet; future format changes add a
/// step here.
fn migrate(text: &str, version: u32) -> Result<String, DbrError> {
    match version {
        CURRENT_VERSION => Ok(text.to_string()),
        other => Err(DbrError::Parse(format!("unknown file version {other}"))),
    }
}

/// Deserialises just `format` and `version`, ignoring the rest of the document.
struct HeaderOnly(Header);

impl<'de> Deserialize<'de> for HeaderOnly {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Loose {
            format: String,
            version: u32,
            #[allow(dead_code)]
            schematic: serde::de::IgnoredAny,
        }
        let l = Loose::deserialize(d)?;
        Ok(HeaderOnly(Header {
            format: l.format,
            version: l.version,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dunbrec_core::{
        ComponentKind, DeviceModel, ModelKind, Point, Polarity, Rotation, Waveform,
    };

    fn sample() -> Schematic {
        let mut s = Schematic::new();
        s.add_component(
            ComponentKind::VoltageSource {
                waveform: Waveform::Sine {
                    offset: 0.0,
                    amplitude: 1.0,
                    frequency: 1e3,
                    delay: 0.0,
                    damping: 0.0,
                    phase: 0.0,
                },
            },
            Point::new(0, 0),
        );
        let q = s.add_component(
            ComponentKind::Bjt {
                polarity: Polarity::N,
                model: "Q2N3904".into(),
            },
            Point::new(10, 0),
        );
        s.component_mut(q).unwrap().rotation = Rotation::R180;
        s.add_component(
            ComponentKind::Resistor { resistance: 4.7e3 },
            Point::new(5, -5),
        );
        s.add_component(ComponentKind::Ground, Point::new(0, 4));
        s.add_wire(Point::new(0, -2), Point::new(3, -5));
        s.add_label("VOUT", Point::new(12, -2));
        s.models
            .insert(DeviceModel::new("Q2N3904", ModelKind::Npn).with("BF", 300.0));
        s
    }

    #[test]
    fn round_trip() {
        let s = sample();
        let text = to_string(&s).unwrap();
        let back = from_str(&text).unwrap();
        assert_eq!(s, back);
        // Ids keep counting after reload.
        let mut back = back;
        let id = back.add_wire(Point::new(0, 0), Point::new(1, 0));
        assert!(s.components.iter().all(|c| c.id.0 != id.0));
    }

    #[test]
    fn source_files_round_trip_and_old_files_load_without_them() {
        use dunbrec_core::{SourceFile, SourceLanguage};
        let mut s = sample();
        s.sources.insert(
            "blink".into(),
            SourceFile {
                language: SourceLanguage::Asm,
                text: "loop: rjmp loop ; \"quoted\"
  nop
"
                .into(),
            },
        );
        let text = to_string(&s).unwrap();
        assert!(text.contains("blink"));
        assert_eq!(from_str(&text).unwrap(), s);

        // A file saved before sources existed has no `sources` entry.
        let plain = to_string(&sample()).unwrap();
        let old: String = plain
            .lines()
            .filter(|l| !l.trim_start().starts_with("sources:"))
            .collect::<Vec<_>>()
            .join(
                "
",
            );
        assert!(!old.contains("sources"));
        assert_eq!(from_str(&old).unwrap(), sample());
    }

    #[test]
    fn switches_round_trip_and_old_switches_load_as_not_momentary() {
        use dunbrec_core::SwitchForm;
        let mut s = sample();
        s.add_component(
            ComponentKind::MultiSwitch {
                form: SwitchForm::Dpdt,
                thrown: true,
                r_on: 0.01,
                r_off: 1e9,
            },
            Point::new(20, 0),
        );
        s.add_component(
            ComponentKind::Switch {
                closed: true,
                r_on: 0.01,
                r_off: 1e9,
                momentary: true,
            },
            Point::new(30, 0),
        );
        let text = to_string(&s).unwrap();
        assert_eq!(from_str(&text).unwrap(), s);

        // A file saved before push buttons existed has no `momentary` entry.
        let old: String = text
            .lines()
            .filter(|l| !l.trim_start().starts_with("momentary:"))
            .collect::<Vec<_>>()
            .join(
                "
",
            );
        assert!(!old.contains("momentary"));
        let loaded = from_str(&old).unwrap();
        assert!(loaded.components.iter().any(|c| matches!(
            c.kind,
            ComponentKind::Switch {
                momentary: false,
                closed: true,
                ..
            }
        )));
    }

    #[test]
    fn round_trip_file() {
        let dir = std::env::temp_dir().join(format!("dunbrec-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.dbr");
        save(&sample(), &path).unwrap();
        assert_eq!(load(&path).unwrap(), sample());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_newer_version() {
        let text = to_string(&sample())
            .unwrap()
            .replace("version: 1", "version: 99");
        assert!(matches!(
            from_str(&text),
            Err(DbrError::UnsupportedVersion { found: 99, .. })
        ));
    }

    #[test]
    fn rejects_wrong_format() {
        let text = to_string(&sample())
            .unwrap()
            .replace("\"dunbrec\"", "\"other\"");
        assert!(matches!(from_str(&text), Err(DbrError::WrongFormat(_))));
    }

    #[test]
    fn rejects_garbage() {
        assert!(matches!(from_str("not ron"), Err(DbrError::Parse(_))));
    }
}
