//! The open schematic, its file path, and undo/redo history.
//!
//! Undo works by snapshots: before each user action the whole schematic is
//! copied onto the undo stack. Schematics are small, so this is cheap, and it
//! cannot get out of step with the data the way per-operation undo can.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use dunbrec_core::Schematic;

/// Most snapshots kept for undo.
const UNDO_LIMIT: usize = 200;

/// A generation number no document has used before. Generations are unique
/// across documents, so a cache keyed on one can never mistake a newly opened
/// document for the one it replaced.
fn next_generation() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

pub struct Document {
    schematic: Schematic,
    path: Option<PathBuf>,
    /// Snapshots and the state id each one had.
    undo: Vec<(Schematic, u64)>,
    redo: Vec<(Schematic, u64)>,
    /// Id of the current state; changes with every checkpoint, undo, redo.
    state_id: u64,
    next_state_id: u64,
    saved_state_id: u64,
    /// Renewed on every mutable access, so caches can tell when to rebuild.
    generation: u64,
}

impl Default for Document {
    fn default() -> Self {
        Self::new(Schematic::new(), None)
    }
}

impl Document {
    pub fn new(schematic: Schematic, path: Option<PathBuf>) -> Self {
        Self {
            schematic,
            path,
            undo: Vec::new(),
            redo: Vec::new(),
            state_id: 0,
            next_state_id: 1,
            saved_state_id: 0,
            generation: next_generation(),
        }
    }

    pub fn schematic(&self) -> &Schematic {
        &self.schematic
    }

    /// Mutable access without an undo checkpoint (for continuous edits such
    /// as dragging, after a [`checkpoint`](Self::checkpoint) at the start).
    pub fn schematic_mut(&mut self) -> &mut Schematic {
        self.generation = next_generation();
        &mut self.schematic
    }

    /// Record the current state for undo before a change.
    pub fn checkpoint(&mut self) {
        self.undo.push((self.schematic.clone(), self.state_id));
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.state_id = self.next_state_id;
        self.next_state_id += 1;
    }

    /// Checkpoint, then apply `f`.
    pub fn edit<R>(&mut self, f: impl FnOnce(&mut Schematic) -> R) -> R {
        self.checkpoint();
        f(self.schematic_mut())
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        let Some((previous, id)) = self.undo.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.schematic, previous);
        self.redo.push((current, self.state_id));
        self.state_id = id;
        self.generation = next_generation();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some((next, id)) = self.redo.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.schematic, next);
        self.undo.push((current, self.state_id));
        self.state_id = id;
        self.generation = next_generation();
        true
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn is_dirty(&self) -> bool {
        self.state_id != self.saved_state_id
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn mark_saved(&mut self, path: PathBuf) {
        self.path = Some(path);
        self.saved_state_id = self.state_id;
    }

    /// File name for the window title, e.g. "amp.dbr" or "Untitled".
    pub fn display_name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dunbrec_core::{ComponentKind, Point};

    fn add_resistor(doc: &mut Document) {
        doc.edit(|s| {
            s.add_component(
                ComponentKind::Resistor { resistance: 1e3 },
                Point::default(),
            )
        });
    }

    #[test]
    fn undo_redo_round_trip() {
        let mut doc = Document::default();
        assert!(!doc.is_dirty());
        add_resistor(&mut doc);
        add_resistor(&mut doc);
        assert_eq!(doc.schematic().components.len(), 2);
        assert!(doc.is_dirty());

        assert!(doc.undo());
        assert_eq!(doc.schematic().components.len(), 1);
        assert!(doc.undo());
        assert_eq!(doc.schematic().components.len(), 0);
        assert!(!doc.is_dirty(), "back at the saved state");
        assert!(!doc.undo());

        assert!(doc.redo());
        assert!(doc.redo());
        assert_eq!(doc.schematic().components.len(), 2);
        assert!(!doc.redo());
    }

    #[test]
    fn new_edit_clears_redo() {
        let mut doc = Document::default();
        add_resistor(&mut doc);
        doc.undo();
        assert!(doc.can_redo());
        add_resistor(&mut doc);
        assert!(!doc.can_redo());
    }

    #[test]
    fn dirty_tracking_follows_saved_state() {
        let mut doc = Document::default();
        add_resistor(&mut doc);
        doc.mark_saved(PathBuf::from("x.dbr"));
        assert!(!doc.is_dirty());
        add_resistor(&mut doc);
        assert!(doc.is_dirty());
        doc.undo();
        assert!(!doc.is_dirty());
        doc.undo();
        assert!(doc.is_dirty());
        assert_eq!(doc.display_name(), "x.dbr");
    }

    #[test]
    fn generation_changes_on_every_mutation() {
        let mut doc = Document::default();
        let g0 = doc.generation();
        doc.schematic_mut();
        let g1 = doc.generation();
        add_resistor(&mut doc);
        let g2 = doc.generation();
        doc.undo();
        assert!(g0 < g1 && g1 < g2 && g2 < doc.generation());
    }

    #[test]
    fn new_documents_never_reuse_a_generation() {
        // Caches keyed on the generation must rebuild when a document is
        // replaced, e.g. opening an example over an untouched empty schematic.
        let a = Document::default();
        let b = Document::default();
        assert_ne!(a.generation(), b.generation());
    }
}
