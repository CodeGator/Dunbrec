//! Everything that belongs to one open circuit: the document, how it is being
//! looked at and edited, its row of tabs (the schematic and its source
//! files), its instruments and what has been worked out from it.
//!
//! What is *not* here belongs to the application: the live simulation (only
//! the active circuit can run) and the panels around the circuit. See
//! `docs/multi-document-editor.md`.
//!
//! The caches are keyed on [`Document::generation`], which is unique across
//! documents, so one can never be mistaken for another circuit's.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use dunbrec_core::{
    ComponentKind, Netlist, NodeId, Point, SOURCE_PREFIX, Schematic, SourceFile, SourceLanguage,
    build_netlist, units::format_value,
};
use dunbrec_sim::{Circuit, SimWarning, operating_point};
use egui_dock::{DockState, Node};

use crate::analyses_ui::Analyses;
use crate::assistant::Assistant;
use crate::canvas::{self, Annotation, Canvas, Item, NodeMap, TagPlace, node_tag_positions};
use crate::code_ui::CodeEditor;
use crate::debugger_ui::Debugger;
use crate::document::Document;
use crate::instruments_ui::Bench;
use crate::project::{self, CircuitCode, Use};
use crate::properties::{DemoRequest, Properties};
use crate::serial_ui::SerialMonitor;

/// `name` itself, or the first of `name2`, `name3`... (before the extension)
/// that is not `taken`.
fn free_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(name) {
        return name.to_string();
    }
    let (stem, extension) = name.rsplit_once('.').unwrap_or((name, ""));
    (2u32..)
        .map(|n| format!("{stem}{n}.{extension}"))
        .find(|candidate| !taken(candidate))
        .expect("some number is free")
}

/// A tab in a circuit's own row.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DocTab {
    /// The drawing. Always there.
    Schematic,
    /// A source file, by its name in [`Schematic::sources`].
    Source(String),
}

/// The instruments and the assistant as one circuit left them: probes sit on
/// its wires, plots and serial output came from it, breakpoints are in its
/// chips and the conversation is about it. The application holds the active
/// circuit's; each of the others keeps its own here.
#[derive(Default)]
pub struct Tools {
    pub bench: Bench,
    pub analyses: Analyses,
    pub serial: SerialMonitor,
    pub debugger: Debugger,
    pub assistant: Assistant,
}

/// The source file made for a microcontroller.
pub struct NewFile {
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
    /// Tells this circuit's tab from the others'; never reused in a session.
    pub id: u32,
    /// What to call it while it has no file (an example or an import).
    pub title: Option<String>,
    /// The schematic, its file and its undo history.
    pub doc: Document,
    /// View, selection, current tool and clipboard.
    pub canvas: Canvas,
    pub properties: Properties,
    /// The editor of the circuit's firmware source files.
    pub code: CodeEditor,
    /// How the circuit's own tabs are arranged.
    pub tabs: DockState<DocTab>,
    /// Source files whose tabs the user has closed.
    pub hidden: BTreeSet<String>,
    /// Source files that exist only here: saving gives them names no file has.
    unwritten: BTreeSet<String>,
    /// The files the chips ran when last looked at, and the document
    /// generation then: a file that is new among them is checked.
    pub checked: (u64, BTreeSet<PathBuf>),
    /// This circuit's instruments while another circuit is the active one.
    pub tools: Tools,
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
        let mut circuit = Self {
            id: 0,
            title: None,
            node_map: (doc.generation(), node_map, tags),
            netlist: (doc.generation(), netlist),
            doc,
            canvas: Canvas::default(),
            properties: Properties::default(),
            code: CodeEditor::default(),
            tabs: DockState::new(vec![DocTab::Schematic]),
            hidden: BTreeSet::new(),
            unwritten: BTreeSet::new(),
            checked: Default::default(),
            tools: Tools::default(),
            dc: None,
        };
        circuit.sync_tabs();
        circuit.checked = (circuit.doc.generation(), circuit.used_paths());
        circuit
    }

    /// What the circuit's microcontrollers run, as it is now; `None` while it
    /// has no file.
    pub fn code(&self) -> Option<CircuitCode> {
        self.doc
            .path()
            .map(|path| project::code_of(self.doc.schematic(), path))
    }

    /// Where the source files the chips run are.
    fn used_paths(&self) -> BTreeSet<PathBuf> {
        self.code()
            .map(|code| code.uses.into_iter().map(|u| u.path).collect())
            .unwrap_or_default()
    }

    /// The source files the chips have started to run since the last call.
    /// Those on the disk are opened, so that they have their tabs.
    pub fn newly_used(&mut self) -> Vec<Use> {
        if self.checked.0 == self.doc.generation() {
            return Vec::new();
        }
        let Some(code) = self.code() else {
            self.checked = (self.doc.generation(), BTreeSet::new());
            return Vec::new();
        };
        let new: Vec<Use> = code
            .uses
            .into_iter()
            .filter(|u| !self.checked.1.contains(&u.path))
            .collect();
        if let Some(dir) = self.firmware_dir() {
            for u in &new {
                let open = self.doc.schematic().sources.contains_key(&u.name);
                if !open && u.path.is_file() {
                    // A file that cannot be read says so when it is run.
                    let _ = project::attach(self.doc.schematic_mut(), &dir, &u.name);
                }
            }
        }
        self.checked = (self.doc.generation(), self.used_paths());
        new
    }

    /// The circuit's name, as its tab shows it: its file's name, or for one
    /// without a file what it came from.
    pub fn name(&self) -> String {
        self.doc
            .path()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .or_else(|| self.title.clone())
            .unwrap_or_else(|| "Untitled".to_string())
    }

    /// The folder of the circuit's file, which relative firmware paths start
    /// from. An unsaved circuit has none.
    pub fn firmware_dir(&self) -> Option<PathBuf> {
        self.doc
            .path()
            .and_then(|p| p.parent())
            .map(|d| d.to_path_buf())
    }

    // --------------------------------------------------------- the tabs ---

    /// Make the row of tabs match the circuit: the schematic, then one tab
    /// for each source file that has not been closed.
    pub fn sync_tabs(&mut self) {
        let sources = &self.doc.schematic().sources;
        let wanted: Vec<DocTab> = sources
            .keys()
            .filter(|name| !self.hidden.contains(*name))
            .map(|name| DocTab::Source(name.clone()))
            .collect();
        loop {
            let stale = self
                .tabs
                .iter_all_tabs()
                .map(|(_, tab)| tab)
                .find(|tab| **tab != DocTab::Schematic && !wanted.contains(tab))
                .cloned();
            let Some(at) = stale.and_then(|tab| self.tabs.find_tab(&tab)) else {
                break;
            };
            self.tabs.remove_tab(at);
        }
        for tab in wanted {
            if self.tabs.find_tab(&tab).is_some() {
                continue;
            }
            // Beside the schematic, without taking its place in front.
            match self.tabs.find_tab(&DocTab::Schematic) {
                Some((surface, node, _)) => {
                    let front = match &self.tabs[surface][node] {
                        Node::Leaf(leaf) => Some(leaf.active),
                        _ => None,
                    };
                    self.tabs[surface][node].append_tab(tab);
                    if let Some(front) = front {
                        self.tabs.set_active_tab((surface, node, front));
                    }
                }
                None => self.tabs.push_to_first_leaf(tab),
            }
        }
    }

    /// Bring the tab of the source file `name` to the front, opening it again
    /// if it was closed.
    pub fn show_source(&mut self, name: &str) {
        self.hidden.remove(name);
        self.sync_tabs();
        if let Some(at) = self.tabs.find_tab(&DocTab::Source(name.to_string())) {
            self.tabs.set_active_tab(at);
            self.tabs.set_focused_node_and_surface((at.0, at.1));
        }
    }

    /// Whether the tab of `tab` is the one in front in its group.
    #[cfg(test)]
    pub fn is_in_front(&self, tab: &DocTab) -> bool {
        self.tabs.find_tab(tab).is_some_and(|(surface, node, index)| {
            matches!(&self.tabs[surface][node], Node::Leaf(leaf) if leaf.active == index)
        })
    }

    // ------------------------------------------------- source files ---

    /// The circuit's source files on disk: name and full path.
    pub fn source_paths(&self) -> Vec<(String, PathBuf)> {
        let Some(dir) = self.firmware_dir() else {
            return Vec::new();
        };
        self.doc
            .schematic()
            .sources
            .keys()
            .filter(|name| project::is_file_source(name))
            .map(|name| (name.clone(), project::normalize(&dir.join(name))))
            .collect()
    }

    /// The name this circuit knows the file at `path` by, if it has it open.
    pub fn source_at(&self, path: &Path) -> Option<String> {
        let wanted = project::normalize(path);
        self.source_paths()
            .into_iter()
            .find(|(_, full)| project::path_eq(full, &wanted))
            .map(|(name, _)| name)
    }

    /// Give a microcontroller its own copy of a demo program (see
    /// [`add_source`](Self::add_source)).
    pub fn add_demo(&mut self, request: DemoRequest) -> Result<NewFile, String> {
        let demo = dunbrec_mcu::demos::demo(request.demo)
            .ok_or_else(|| format!("there is no demo called {}", request.demo))?;
        let text = match request.language {
            SourceLanguage::Asm => demo.assembly,
            SourceLanguage::Cpp => demo.sketch,
        };
        self.add_source(request.chip, demo.name, request.language, text)
    }

    /// Make a new source file for a microcontroller beside the circuit, called
    /// after `base`, and set the chip to run it. Existing files are never
    /// written over. Undo restores the chip's setting; the file stays on disk.
    pub fn add_source(
        &mut self,
        chip: dunbrec_core::ComponentId,
        base: &str,
        language: SourceLanguage,
        text: &str,
    ) -> Result<NewFile, String> {
        let is_chip = self
            .doc
            .schematic()
            .component(chip)
            .is_some_and(|c| matches!(c.kind, ComponentKind::Mcu { .. }));
        if !is_chip {
            return Err("that microcontroller is no longer in the circuit".into());
        }
        let extension = match language {
            SourceLanguage::Asm => "asm",
            SourceLanguage::Cpp => "ino",
        };
        // The programs are built into Dunbrec with the line endings of the
        // machine that built it.
        let text = text.replace("\r\n", "\n");
        let sources = &self.doc.schematic().sources;
        let taken = |name: &str| sources.keys().any(|k| k.eq_ignore_ascii_case(name));
        let (name, path) = match self.firmware_dir() {
            Some(dir) => {
                let name = project::new_source_copy(&dir, base, extension, &text, taken)?;
                let path = dir.join(&name);
                (name, Some(path))
            }
            None => {
                let name = crate::code_ui::unused_file_name(sources, base, extension);
                self.unwritten.insert(name.clone());
                (name, None)
            }
        };
        let file = SourceFile {
            language,
            text: text.clone(),
        };
        self.doc.edit(|s| {
            s.sources.insert(name.clone(), file);
            if let Some(c) = s.component_mut(chip)
                && let ComponentKind::Mcu { firmware, .. } = &mut c.kind
            {
                *firmware = name.clone();
            }
        });
        self.show_source(&name);
        Ok(NewFile { name, path, text })
    }

    /// Give every microcontroller that runs a program built into Dunbrec (an
    /// example's, or an older circuit's) the sketch of that program as a
    /// source file of the circuit's own, so that what a chip runs can always
    /// be read and changed. Chips that ran the same program share the file,
    /// which is written when the circuit is saved. `used` says whether another
    /// circuit already uses a path.
    pub fn give_builtins_files(&mut self, used: impl Fn(&Path) -> bool) {
        let dir = self.firmware_dir();
        let mut wanted: Vec<(&'static dunbrec_mcu::demos::Demo, Vec<_>)> = Vec::new();
        for c in &self.doc.schematic().components {
            if let ComponentKind::Mcu { firmware, .. } = &c.kind
                && let Some(name) = firmware.strip_prefix(dunbrec_mcu::firmware::BUILTIN_PREFIX)
                && let Some(demo) = dunbrec_mcu::demos::demo(name)
            {
                match wanted.iter_mut().find(|(d, _)| d.name == demo.name) {
                    Some((_, chips)) => chips.push(c.id),
                    None => wanted.push((demo, vec![c.id])),
                }
            }
        }
        if wanted.is_empty() {
            return;
        }
        for (demo, chips) in wanted {
            let sources = &self.doc.schematic().sources;
            let name = free_name(&format!("{}.ino", demo.name), |name| {
                sources.keys().any(|k| k.eq_ignore_ascii_case(name))
                    || dir.as_ref().is_some_and(|dir| {
                        let path = project::normalize(&dir.join(name));
                        path.exists() || used(&path)
                    })
            });
            let file = SourceFile {
                language: SourceLanguage::Cpp,
                text: demo.sketch.replace("\r\n", "\n"),
            };
            // Not an undo step: the circuit opens this way.
            let s = self.doc.schematic_mut();
            s.sources.insert(name.clone(), file);
            for id in chips {
                if let Some(c) = s.component_mut(id)
                    && let ComponentKind::Mcu { firmware, .. } = &mut c.kind
                {
                    *firmware = name.clone();
                }
            }
            self.unwritten.insert(name);
        }
        // A circuit with a file now differs from it.
        if dir.is_some() {
            self.doc.mark_unsaved();
        }
        self.sync_tabs();
        self.checked = (self.doc.generation(), self.used_paths());
    }

    /// Before saving into `dir`: rename the source files that would write over
    /// a file there, or come to share it. Those that exist only here avoid
    /// every file; those being copied to another folder avoid files that hold
    /// something else.
    pub fn avoid_files_in(&mut self, dir: &Path) {
        let moving = self.firmware_dir().as_deref() != Some(dir);
        let clashing: Vec<String> = self
            .doc
            .schematic()
            .sources
            .iter()
            .filter(|(name, _)| project::is_file_source(name))
            .filter(|(name, file)| {
                let path = dir.join(name);
                if self.unwritten.contains(*name) {
                    path.exists()
                } else {
                    moving && path.is_file() && {
                        std::fs::read_to_string(&path).map_or(true, |disk| disk != file.text)
                    }
                }
            })
            .map(|(name, _)| name.clone())
            .collect();
        for old in clashing {
            let sources = &self.doc.schematic().sources;
            let new = free_name(&old, |name| {
                sources.keys().any(|k| k.eq_ignore_ascii_case(name)) || dir.join(name).exists()
            });
            let in_front = self
                .tabs
                .find_tab(&DocTab::Source(old.clone()))
                .is_some_and(|(surface, node, index)| {
                    matches!(&self.tabs[surface][node], Node::Leaf(leaf) if leaf.active == index)
                });
            let s = self.doc.schematic_mut();
            if let Some(file) = s.sources.remove(&old) {
                s.sources.insert(new.clone(), file);
            }
            for c in &mut s.components {
                if let ComponentKind::Mcu { firmware, .. } = &mut c.kind
                    && *firmware == old
                {
                    *firmware = new.clone();
                }
            }
            // The old name stays here too: undo can bring it back.
            self.unwritten.insert(new.clone());
            if self.hidden.contains(&old) {
                self.hidden.insert(new.clone());
            }
            self.sync_tabs();
            if in_front {
                self.show_source(&new);
            }
        }
    }

    /// Note that every source file the circuit has is now on the disk.
    pub fn sources_written(&mut self) {
        for name in self.doc.schematic().sources.keys() {
            self.unwritten.remove(name);
        }
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

/// The title of a source file's tab: the microcontrollers that run it, then
/// its name ("U1: blink.ino"), or just the name if none does.
pub fn source_title(schematic: &Schematic, name: &str) -> String {
    let chips: Vec<&str> = schematic
        .components
        .iter()
        .filter(|c| match &c.kind {
            ComponentKind::Mcu { firmware, .. } => {
                let firmware = firmware.trim();
                // A file is named as it is; text kept inside the circuit by
                // `source:<name>`.
                firmware == name || firmware.strip_prefix(SOURCE_PREFIX) == Some(name)
            }
            _ => false,
        })
        .map(|c| c.designator.as_str())
        .collect();
    if chips.is_empty() {
        name.to_string()
    } else {
        format!("{}: {name}", chips.join(", "))
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
    fn a_circuit_is_named_after_its_file_or_where_it_came_from() {
        let mut c = OpenCircuit::default();
        assert_eq!(c.name(), "Untitled");
        c.title = Some("Voltage divider".into());
        assert_eq!(c.name(), "Voltage divider");
        c.doc.mark_saved(PathBuf::from("folder").join("Amp.dbr"));
        assert_eq!(c.name(), "Amp");
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

    fn titles(c: &OpenCircuit) -> Vec<String> {
        c.tabs
            .iter_all_tabs()
            .map(|(_, tab)| match tab {
                DocTab::Schematic => "Schematic".to_string(),
                DocTab::Source(name) => source_title(c.doc.schematic(), name),
            })
            .collect()
    }

    fn add_file(c: &mut OpenCircuit, name: &str) {
        c.doc.edit(|s| {
            s.sources.insert(
                name.into(),
                SourceFile {
                    language: SourceLanguage::Asm,
                    text: "nop\n".into(),
                },
            )
        });
        c.sync_tabs();
    }

    #[test]
    fn each_source_file_has_a_tab_named_after_the_chips_that_run_it() {
        let (mut c, id) = with_chip();
        assert_eq!(titles(&c), ["Schematic"]);
        add_file(&mut c, "a.asm");
        add_file(&mut c, "b.asm");
        assert_eq!(titles(&c), ["Schematic", "a.asm", "b.asm"]);
        assert!(
            c.is_in_front(&DocTab::Schematic),
            "new tabs do not take over"
        );
        c.doc.edit(|s| {
            s.add_component(
                ComponentKind::Mcu {
                    firmware: "a.asm".into(),
                    clock: 16e6,
                },
                Point::new(20, 0),
            );
            if let ComponentKind::Mcu { firmware, .. } = &mut s.component_mut(id).unwrap().kind {
                *firmware = "a.asm".into();
            }
        });
        assert_eq!(titles(&c)[1], "U1, U2: a.asm");
        // The older way of naming text kept inside the circuit.
        c.doc.edit(|s| {
            if let ComponentKind::Mcu { firmware, .. } = &mut s.component_mut(id).unwrap().kind {
                *firmware = "source:b.asm".into();
            }
        });
        assert_eq!(titles(&c)[2], "U1: b.asm");
    }

    #[test]
    fn closing_a_file_tab_hides_it_until_it_is_asked_for() {
        let mut c = OpenCircuit::default();
        add_file(&mut c, "a.asm");
        c.show_source("a.asm");
        assert!(c.is_in_front(&DocTab::Source("a.asm".into())));
        c.hidden.insert("a.asm".into());
        c.sync_tabs();
        assert_eq!(titles(&c), ["Schematic"]);
        assert!(c.doc.schematic().sources.contains_key("a.asm"), "kept");
        c.show_source("a.asm");
        assert!(c.is_in_front(&DocTab::Source("a.asm".into())));
        // A file that goes takes its tab, and undo brings both back.
        c.doc.edit(|s| s.sources.clear());
        c.sync_tabs();
        assert_eq!(titles(&c), ["Schematic"]);
        c.doc.undo();
        c.sync_tabs();
        assert_eq!(titles(&c), ["Schematic", "a.asm"]);
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
        assert!(c.is_in_front(&DocTab::Source("blink2.ino".into())));
        assert_eq!(c.source_at(&dir.join("blink2.ino")).unwrap(), "blink2.ino");
        assert_eq!(c.source_at(&dir.join("other.ino")), None);

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
    fn a_new_file_for_an_unsaved_circuit_waits_in_memory() {
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
    fn a_file_that_cannot_be_made_is_reported() {
        let dir = std::env::temp_dir().join(format!("dunbrec-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (mut c, id) = with_chip();
        // The circuit's folder does not exist, so the file cannot be written.
        c.doc.mark_saved(dir.join("x.dbr"));
        let made = c.add_source(id, "main", SourceLanguage::Asm, "nop\n");
        assert!(made.is_err());
        assert!(c.doc.schematic().sources.is_empty());
        assert_eq!(firmware(&c, id), "builtin:blink");
    }

    #[test]
    fn an_unsaved_circuit_has_no_firmware_folder() {
        let mut c = OpenCircuit::default();
        assert_eq!(c.firmware_dir(), None);
        c.doc.mark_saved(PathBuf::from("folder").join("a.dbr"));
        assert_eq!(c.firmware_dir(), Some(PathBuf::from("folder")));
    }
}
