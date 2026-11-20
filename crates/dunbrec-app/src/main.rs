//! Dunbrec desktop application.
//!
//! Usage: `dunbrec [FILE.dbr]` or `dunbrec --example <N>` (1-based index into
//! File → Open example).

// No console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod canvas;
mod document;
mod examples;
mod instruments_ui;
mod library;
mod live;
mod properties;
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
            match dunbrec_io::dbr::load(&path) {
                Ok(s) => Some((s, Some(path))),
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
            .with_min_inner_size([800.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Dunbrec",
        options,
        Box::new(|cc| Ok(Box::new(app::DunbrecApp::new(cc, initial)))),
    )
}
