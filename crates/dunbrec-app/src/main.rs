//! Dunbrec desktop application.
//!
//! Usage: `dunbrec [FILE]` (`.dbr`, a SPICE netlist, or `.kicad_sch`) or
//! `dunbrec --example <N>` (1-based index into
//! File → Open example).

// No console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod about;
mod analyses_ui;
mod app;
mod canvas;
mod debugger_ui;
mod dock;
mod document;
mod examples;
mod instruments_ui;
mod library;
mod live;
mod logic_ui;
mod plot;
mod properties;
mod serial_ui;
mod settings;
mod symbols;

use std::path::PathBuf;

fn initial_document() -> Option<(dunbrec_core::Schematic, Option<PathBuf>)> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag, n] if flag == "--example" => {
            let i: usize = n.parse().ok()?;
            let ex = examples::examples().get(i.checked_sub(1)?)?;
            Some(((ex.build)(), None))
        }
        [path] => {
            let path = PathBuf::from(path);
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            // SPICE and KiCad files are imported as new, unsaved documents.
            let result = match ext.as_str() {
                "dbr" => dunbrec_io::dbr::load(&path)
                    .map(|s| (s, Some(path.clone())))
                    .map_err(|e| e.to_string()),
                "kicad_sch" => std::fs::read_to_string(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|t| dunbrec_io::kicad::import(&t).map_err(|e| e.to_string()))
                    .map(|imp| (imp.to_schematic(), None)),
                _ => std::fs::read_to_string(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|t| dunbrec_io::spice::import(&t).map_err(|e| e.to_string()))
                    .map(|imp| (imp.to_schematic(), None)),
            };
            match result {
                Ok(doc) => Some(doc),
                Err(e) => {
                    eprintln!("Could not open {}: {e}", path.display());
                    None
                }
            }
        }
        _ => None,
    }
}

fn main() -> eframe::Result {
    let initial = initial_document();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Dunbrec")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([800.0, 500.0])
            .with_icon(std::sync::Arc::new(
                eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/dunbrec.png"))
                    .unwrap_or_default(),
            )),
        ..Default::default()
    };
    eframe::run_native(
        "Dunbrec",
        options,
        Box::new(|cc| Ok(Box::new(app::DunbrecApp::new(cc, initial)))),
    )
}
