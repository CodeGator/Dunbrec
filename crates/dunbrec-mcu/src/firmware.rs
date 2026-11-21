//! Locating and loading firmware named in a schematic.
//!
//! A firmware reference is either `builtin:<name>` (see
//! [`crate::programs`]) or the path of an Intel HEX file. Relative paths
//! are taken relative to the schematic's folder when it is known.

use std::path::{Path, PathBuf};

pub const BUILTIN_PREFIX: &str = "builtin:";

/// Resolve a firmware reference to a file path (`None` for built-ins).
pub fn resolve(reference: &str, base_dir: Option<&Path>) -> Option<PathBuf> {
    if reference.starts_with(BUILTIN_PREFIX) {
        return None;
    }
    let path = PathBuf::from(reference.trim());
    Some(match base_dir {
        Some(dir) if path.is_relative() => dir.join(path),
        _ => path,
    })
}

/// Load the flash image a reference names.
pub fn load(reference: &str, base_dir: Option<&Path>) -> Result<Vec<u8>, String> {
    let reference = reference.trim();
    if reference.is_empty() {
        return Err("no firmware selected: choose a .hex file in the part's properties".into());
    }
    if let Some(name) = reference.strip_prefix(BUILTIN_PREFIX) {
        return crate::programs::builtin(name)
            .ok_or_else(|| format!("there is no built-in program called '{name}'"));
    }
    let path = resolve(reference, base_dir).expect("not a built-in");
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("elf"))
    {
        return Err(format!(
            "{}: ELF files are not supported; export an Intel HEX file instead \
             (Arduino IDE: Sketch → Export Compiled Binary)",
            path.display()
        ));
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    crate::hex::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_and_files() {
        assert!(load("builtin:blink", None).is_ok());
        assert!(load("builtin:nope", None).unwrap_err().contains("nope"));
        assert!(load("  ", None).unwrap_err().contains("no firmware"));

        let dir = std::env::temp_dir().join(format!("dunbrec-fw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let image = crate::programs::builtin("fade").unwrap();
        std::fs::write(dir.join("fade.hex"), crate::hex::write(&image)).unwrap();
        assert_eq!(load("fade.hex", Some(&dir)).unwrap(), image);
        let abs = dir.join("fade.hex");
        assert_eq!(load(abs.to_str().unwrap(), None).unwrap(), image);
        assert!(
            load("missing.hex", Some(&dir))
                .unwrap_err()
                .contains("cannot read")
        );
        assert!(load("x.elf", Some(&dir)).unwrap_err().contains("ELF"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
