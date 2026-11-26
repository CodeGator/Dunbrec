//! Everything that belongs to one open circuit: the document, how it is being
//! looked at and edited, and what has been worked out from it.
//!
//! What is *not* here belongs to the application: the live simulation and the
//! debugger (only one circuit can run), the instruments, and the panels around
//! the circuit. See `docs/multi-document-editor.md`.
//!
//! The caches are keyed on [`Document::generation`], which is unique across
//! documents, so one can never be mistaken for another circuit's.

use std::path::PathBuf;

use dunbrec_core::{
    ComponentKind, Netlist, NodeId, Point, Schematic, SourceFile, SourceLanguage, build_netlist,
    units::format_value,
};
use dunbrec_sim::{Circuit, SimWarning, operating_point};

use crate::canvas::{self, Annotation, Canvas, Item, NodeMap, TagPlace, node_tag_positions};
use crate::code_ui::CodeEditor;
use crate::document::Document;
use crate::project;
use crate::properties::{DemoRequest, Properties};

/// The source file made for a demo.
pub struct DemoFile {
    /// Its name, which is also the chip's firmware setting.
    pub name: String,
    /// Where it was written; `None` while the circuit has no folder, so the
    /// file is made when the circuit is first saved.
    pub path: Option<PathBuf>,
    pub text: String,
}

/// DC operating point shown on the canvas and in the results panel.
pub struct DcResults {
    pub generation: u64,
    pub outcome: Result<DcTable, String>,
}

pub struct DcTable {
    pub annotations: Vec<Annotation>,
    /// Voltage of every node, indexed by node number.
    pub node_voltages: Vec<f64>,
    pub voltages: Vec<(String, f64)>,
    pub currents: Vec<(String, f64)>,
    pub warnings: Vec<String>,
}

pub struct OpenCircuit {
    /// The schematic, its file and its undo history.
    pub doc: Document,
    /// View, selection, current tool and clipboard.
    pub canvas: Canvas,
    pub properties: Properties,
    /// The editor of the circuit's firmware source files.
    pub code: CodeEditor,
    /// The netlist, and the document generation it was built from.
    pub netlist: (u64, Netlist),
    /// Node lookup and voltage-tag positions, keyed by generation.
    pub node_map: (u64, NodeMap, Vec<TagPlace>),
    pub dc: Option<DcResults>,
}

impl Default for OpenCircuit {
    fn default() -> Self {
        Self::new(Document::default())
    }
}

impl OpenCircuit {
    pub fn new(doc: Document) -> Self {
        let netlist = build_netlist(doc.schematic());
        let node_map = NodeMap::new(doc.schematic(), &netlist);
        let tags = node_tag_positions(doc.schematic(), &netlist);
        Self {
            node_map: (doc.generation(), node_map, tags),
            netlist: (doc.generation(), netlist),
            doc,
            canvas: Canvas::default(),
            properties: Properties::default(),
            code: CodeEditor::default(),
            dc: None,
        }
    }

    /// Put another document in this one's place. The view, the clipboard and
    /// the editors stay; what referred to the old document's parts goes.
    pub fn replace_document(&mut self, doc: Document) {
        self.doc = doc;
        self.canvas.selection.clear();
        self.canvas.cancel();
        self.dc = None;
    }

    /// The folder of the circuit's file, which relative firmware paths start
    /// from. An unsaved circuit has none.
    pub fn firmware_dir(&self) -> Option<PathBuf> {
        self.doc
            .path()
            .and_then(|p| p.parent())
            .map(|d| d.to_path_buf())
    }

    // ------------------------------------------------ derived results ---

    /// Bring the netlist up to date with the schematic, and with it the DC
    /// operating point: recalculated if `live_dc`, otherwise dropped once it
    /// no longer matches.
    pub fn refresh_netlist(&mut self, live_dc: bool) {
        if self.netlist.0 != self.doc.generation() {
            self.netlist = (self.doc.generation(), build_netlist(self.doc.schematic()));
        }
        let stale = self
            .dc
            .as_ref()
            .is_some_and(|d| d.generation != self.doc.generation());
        if live_dc && (self.dc.is_none() || stale) {
            self.run_dc();
        } else if stale {
            self.dc = None;
        }
    }

    /// Bring the node lookup and voltage-tag positions up to date. Call after
    /// [`refresh_netlist`](Self::refresh_netlist).
    pub fn refresh_node_map(&mut self) {
        let generation = self.doc.generation();
        if self.node_map.0 != generation {
            let s = self.doc.schematic();
            self.node_map = (
                generation,
                NodeMap::new(s, &self.netlist.1),
                node_tag_positions(s, &self.netlist.1),
            );
        }
    }

    /// Calculate the DC operating point of the current netlist.
    pub fn run_dc(&mut self) {
        let outcome = dc_table(self.doc.schematic(), &self.netlist.1);
        self.dc = Some(DcResults {
            generation: self.doc.generation(),
            outcome,
        });
    }

    // ------------------------------------------------- source files ---

    /// Give a microcontroller its own copy of a demo program: a new source
    /// file beside the circuit, which the chip is set to run. Existing files
    /// are never written over. Undo restores the chip's setting; the file
    /// stays on disk.
    pub fn add_demo(&mut self, request: DemoRequest) -> Result<DemoFile, String> {
        let demo = dunbrec_mcu::demos::demo(request.demo)
            .ok_or_else(|| format!("there is no demo called {}", request.demo))?;
        let is_chip = self
            .doc
            .schematic()
            .component(request.chip)
            .is_some_and(|c| matches!(c.kind, ComponentKind::Mcu { .. }));
        if !is_chip {
            return Err("that microcontroller is no longer in the circuit".into());
        }
        let (extension, text) = match request.language {
            SourceLanguage::Asm => ("asm", demo.assembly),
            SourceLanguage::Cpp => ("ino", demo.sketch),
        };
        // The programs are built into Dunbrec with the line endings of the
        // machine that built it.
        let text = text.replace("\r\n", "\n");
        let sources = &self.doc.schematic().sources;
        let taken = |name: &str| sources.keys().any(|k| k.eq_ignore_ascii_case(name));
        let (name, path) = match self.firmware_dir() {
            Some(dir) => {
                let name = project::new_source_copy(&dir, demo.name, extension, &text, taken)?;
                let path = dir.join(&name);
                (name, Some(path))
            }
            None => {
                let name = crate::code_ui::unused_file_name(sources, demo.name, extension);
                (name, None)
            }
        };
        let file = SourceFile {
            language: request.language,
            text: text.clone(),
        };
        self.doc.edit(|s| {
            s.sources.insert(name.clone(), file);
            if let Some(c) = s.component_mut(request.chip)
                && let ComponentKind::Mcu { firmware, .. } = &mut c.kind
            {
                *firmware = name.clone();
            }
        });
        self.code.select(&name);
        Ok(DemoFile { name, path, text })
    }

    // ------------------------------------------------------ editing ---

    /// Put the selected items on the clipboard. With nothing to copy, the
    /// clipboard keeps what it had.
    pub fn copy(&mut self) {
        if let Some(clip) = canvas::copy_items(self.doc.schematic(), &self.canvas.selection) {
            self.canvas.clipboard = Some(clip);
            self.canvas.paste_count = 0;
        }
    }

    /// Paste the clipboard, each time a step further from the original, and
    /// select what was pasted.
    pub fn paste(&mut self) {
        let Some(clip) = self.canvas.clipboard.clone() else {
            return;
        };
        self.canvas.paste_count += 1;
        let step = 2 * self.canvas.paste_count;
        let offset = Point::new(step, step);
        let pasted = self.doc.edit(|s| canvas::paste(s, &clip, offset));
        self.canvas.selection = pasted;
    }

    pub fn delete_selection(&mut self) {
        let items = std::mem::take(&mut self.canvas.selection);
        self.doc.edit(|s| canvas::delete_items(s, &items));
    }

    /// Turn the selected parts, or mirror them.
    pub fn transform_selection(&mut self, mirror: bool) {
        let items = self.canvas.selection.clone();
        self.doc
            .edit(|s| canvas::transform_components(s, &items, mirror));
    }

    pub fn select_all(&mut self) {
        let s = self.doc.schematic();
        self.canvas.selection = s
            .components
            .iter()
            .map(|c| Item::Component(c.id))
            .chain(s.wires.iter().map(|w| Item::Wire(w.id)))
            .chain(s.labels.iter().map(|l| Item::Label(l.id)))
            .collect();
    }

    /// Drop selections of items that no longer exist (after undo etc.).
    pub fn prune_selection(&mut self) {
        let s = self.doc.schematic();
        self.canvas.selection.retain(|item| match item {
            Item::Component(id) => s.component(*id).is_some(),
            Item::Wire(id) => s.wires.iter().any(|w| w.id == *id),
            Item::Label(id) => s.labels.iter().any(|l| l.id == *id),
        });
    }
}

/// The DC operating point of `netlist`, laid out for showing, or why there is
/// none.
fn dc_table(s: &Schematic, netlist: &Netlist) -> Result<DcTable, String> {
    if s.components.is_empty() {
        return Err("Place some parts to see the DC operating point.".to_string());
    }
    Circuit::from_netlist(netlist, &s.models)
        .and_then(|c| operating_point(&c).map(|op| (c, op)))
        .map(|(circuit, op)| {
            let annotations = node_tag_positions(s, netlist)
                .into_iter()
                .map(|place| Annotation {
                    text: format_value(op.voltage(place.node), "V"),
                    place,
                })
                .collect();
            let unused = netlist.unused_io_nets();
            let voltages = (1..circuit.node_count())
                .map(NodeId)
                .filter(|n| !unused.contains(n))
                .map(|n| (circuit.node_name(n).to_string(), op.voltage(n)))
                .filter(|(name, _)| !name.contains('#'))
                .collect();
            let currents = op.currents().map(|(d, i)| (d.to_string(), i)).collect();
            let warnings = op
                .warnings
                .iter()
                .map(|w| match w {
                    SimWarning::FloatingNodes { nodes } => format!(
                        "No DC path to ground at {} (tied down with 1 TΩ).",
                        nodes.join(", ")
                    ),
                })
                .collect();
            DcTable {
                node_voltages: (0..circuit.node_count())
                    .map(|n| op.voltage(NodeId(n)))
                    .collect(),
                annotations,
                voltages,
                currents,
                warnings,
            }
        })
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::examples::examples;
    use dunbrec_core::ComponentId;

    fn example() -> OpenCircuit {
        OpenCircuit::new(Document::new((examples()[0].build)(), None))
    }

    fn add_resistor(c: &mut OpenCircuit) {
        c.doc.edit(|s| {
            s.add_component(
                ComponentKind::Resistor { resistance: 1e3 },
                Point::new(40, 40),
            )
        });
    }

    #[test]
    fn the_dc_point_follows_the_schematic() {
        let mut c = example();
        c.refresh_netlist(true);
        c.refresh_node_map();
        let first = c.dc.as_ref().expect("calculated").generation;
        assert_eq!(first, c.doc.generation());
        assert!(c.dc.as_ref().unwrap().outcome.is_ok());
        add_resistor(&mut c);
        c.refresh_netlist(true);
        c.refresh_node_map();
        assert_ne!(c.dc.as_ref().unwrap().generation, first);
        assert_eq!(c.netlist.0, c.doc.generation());
        assert_eq!(c.node_map.0, c.doc.generation());
    }

    #[test]
    fn without_live_dc_an_outdated_point_is_dropped() {
        let mut c = example();
        c.refresh_netlist(false);
        assert!(c.dc.is_none(), "not calculated unasked");
        c.run_dc();
        c.refresh_netlist(false);
        assert!(c.dc.is_some(), "still matches");
        add_resistor(&mut c);
        c.refresh_netlist(false);
        assert!(c.dc.is_none());
    }

    #[test]
    fn an_empty_circuit_says_why_there_is_no_dc_point() {
        let mut c = OpenCircuit::default();
        c.run_dc();
        let outcome = &c.dc.as_ref().unwrap().outcome;
        assert!(
            outcome
                .as_ref()
                .is_err_and(|e| e.contains("Place some parts"))
        );
    }

    #[test]
    fn copy_paste_delete_and_undo_keep_the_selection_honest() {
        let mut c = OpenCircuit::default();
        add_resistor(&mut c);
        c.select_all();
        c.copy();
        c.paste();
        assert_eq!(c.doc.schematic().components.len(), 2);
        assert_eq!(c.canvas.selection.len(), 1, "the pasted part is selected");
        c.doc.undo();
        c.prune_selection();
        assert!(c.canvas.selection.is_empty());
        c.select_all();
        c.delete_selection();
        assert!(c.doc.schematic().components.is_empty());
        // Copying nothing keeps the clipboard.
        c.copy();
        assert!(c.canvas.clipboard.is_some());
    }

    #[test]
    fn another_document_keeps_the_clipboard_and_drops_the_selection() {
        let mut c = OpenCircuit::default();
        add_resistor(&mut c);
        c.select_all();
        c.copy();
        c.run_dc();
        c.replace_document(Document::default());
        assert!(c.canvas.selection.is_empty() && c.dc.is_none());
        assert!(c.canvas.clipboard.is_some());
        // Paste into the new document.
        c.paste();
        assert_eq!(c.doc.schematic().components.len(), 1);
    }

    /// A circuit with one microcontroller, and its id.
    fn with_chip() -> (OpenCircuit, ComponentId) {
        let mut c = OpenCircuit::default();
        let id = c.doc.edit(|s| {
            s.add_component(
                ComponentKind::Mcu {
                    firmware: "builtin:blink".into(),
                    clock: 16e6,
                },
                Point::default(),
            )
        });
        (c, id)
    }

    fn firmware(c: &OpenCircuit, id: ComponentId) -> String {
        match &c.doc.schematic().component(id).unwrap().kind {
            ComponentKind::Mcu { firmware, .. } => firmware.clone(),
            other => panic!("not a chip: {other:?}"),
        }
    }

    #[test]
    fn a_demo_becomes_the_chips_own_file_and_never_replaces_one() {
        let dir = std::env::temp_dir().join(format!("dunbrec-demo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A file of the user's own that happens to have the demo's name.
        std::fs::write(dir.join("blink.ino"), "mine").unwrap();
        let (mut c, id) = with_chip();
        c.doc.mark_saved(dir.join("a.dbr"));
        let request = DemoRequest {
            chip: id,
            demo: "blink",
            language: SourceLanguage::Cpp,
        };

        let first = c.add_demo(request).unwrap();
        assert_eq!(first.name, "blink2.ino");
        assert_eq!(firmware(&c, id), "blink2.ino");
        let written = std::fs::read_to_string(dir.join("blink2.ino")).unwrap();
        assert!(written.contains("LED_PIN = 13") && !written.contains('\r'));
        assert_eq!(c.doc.schematic().sources["blink2.ino"].text, written);
        assert_eq!(first.path, Some(dir.join("blink2.ino")));

        // The user's change survives choosing the demo again.
        std::fs::write(dir.join("blink2.ino"), "changed").unwrap();
        assert_eq!(c.add_demo(request).unwrap().name, "blink3.ino");
        for (file, text) in [("blink.ino", "mine"), ("blink2.ino", "changed")] {
            assert_eq!(std::fs::read_to_string(dir.join(file)).unwrap(), text);
        }

        // Undo gives the chip its old setting back; the file stays.
        c.doc.undo();
        c.doc.undo();
        assert_eq!(firmware(&c, id), "builtin:blink");
        assert!(c.doc.schematic().sources.is_empty());
        assert!(dir.join("blink3.ino").is_file());

        // Assembly is a different file, and it builds.
        let asm = c
            .add_demo(DemoRequest {
                language: SourceLanguage::Asm,
                ..request
            })
            .unwrap();
        assert_eq!(asm.name, "blink.asm");
        assert!(dunbrec_mcu::asm_text::assemble(&asm.text).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_demo_for_an_unsaved_circuit_waits_in_memory() {
        let (mut c, id) = with_chip();
        let request = DemoRequest {
            chip: id,
            demo: "fade",
            language: SourceLanguage::Asm,
        };
        let file = c.add_demo(request).unwrap();
        assert_eq!((file.name.as_str(), file.path), ("fade.asm", None));
        assert_eq!(c.add_demo(request).unwrap().name, "fade2.asm");
        // A chip that has gone, or a demo there is not.
        c.doc.edit(|s| s.components.clear());
        assert!(c.add_demo(request).is_err());
        let (mut c, id) = with_chip();
        let unknown = DemoRequest {
            chip: id,
            demo: "nope",
            ..request
        };
        assert!(c.add_demo(unknown).is_err());
        assert!(c.doc.schematic().sources.is_empty());
    }

    #[test]
    fn an_unsaved_circuit_has_no_firmware_folder() {
        let mut c = OpenCircuit::default();
        assert_eq!(c.firmware_dir(), None);
        c.doc.mark_saved(PathBuf::from("folder").join("a.dbr"));
        assert_eq!(c.firmware_dir(), Some(PathBuf::from("folder")));
    }
}
