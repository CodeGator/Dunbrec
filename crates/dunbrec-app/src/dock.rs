//! The dockable panel layout: which panels exist, where they start, and how
//! the arrangement is saved and restored. What a panel shows lives with the
//! code that owns its data (see the pane viewer in `app.rs`).
//!
//! Instruments are deliberately not part of this: they are floating windows
//! that never dock.

use egui_dock::{DockState, Node, NodeIndex, Split, SurfaceIndex};
use serde::{Deserialize, Serialize};

/// Storage key for the saved arrangement. Bump the suffix when panes change
/// in a way old layouts cannot express.
pub const LAYOUT_KEY: &str = "dunbrec.layout.v1";

/// Everything that can sit in a dock tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Pane {
    Palette,
    Properties,
    Checks,
    Values,
    /// The files of the open project.
    Project,
    /// Chat with a language model about the open circuit and firmware.
    Assistant,
    /// Nothing: a tab drawn invisible that never leaves the central area. It
    /// marks that area, which the open circuits share, and keeps it there
    /// when none is open, so the panels around it do not spread into it.
    /// Layouts saved when the schematic was a panel put this in its place.
    #[serde(alias = "Schematic")]
    Workspace,
    /// An open circuit, by [`OpenCircuit::id`](crate::open_circuit::OpenCircuit):
    /// a tab in the central area, holding the circuit's own row of tabs.
    /// Never kept between sessions.
    Circuit(u32),
    /// A panel an older build could dock (the instruments, the Code panel).
    /// Layouts saved by those builds still load; these tabs are dropped on
    /// the way in.
    #[serde(other)]
    Retired,
}

impl Pane {
    /// The panels the user can bring back from View → Panels.
    pub const SIDE_PANELS: [Pane; 6] = [
        Pane::Palette,
        Pane::Project,
        Pane::Properties,
        Pane::Checks,
        Pane::Values,
        Pane::Assistant,
    ];

    /// The panel's title. A circuit's tab is named after the circuit.
    pub fn title(self) -> &'static str {
        match self {
            Pane::Palette => "Palette",
            Pane::Properties => "Properties",
            Pane::Checks => "Checks",
            Pane::Values => "Values",
            Pane::Project => "Project",
            Pane::Assistant => "Assistant",
            Pane::Workspace | Pane::Circuit(_) | Pane::Retired => "",
        }
    }

    /// Every tab can be closed except the one that marks the central area.
    pub fn closeable(self) -> bool {
        self != Pane::Workspace
    }

    /// Whether the tab may be dragged out into a window of its own. The
    /// central area's contents stay in it.
    pub fn floats(self) -> bool {
        !matches!(self, Pane::Workspace | Pane::Circuit(_))
    }
}

/// Share of its area the palette starts with (and gets back when reopened).
const PALETTE_SHARE: f32 = 0.15;
/// Share the central area keeps of the width left of the properties panel.
const CENTRE_SHARE: f32 = 0.78;
/// Share of the height the panels above the checks and values keep.
const UPPER_SHARE: f32 = 0.85;

/// The arrangement a new user starts with, and what View → Reset layout
/// returns to: circuits in the middle, palette on the left, properties on
/// the right, and checks and values in a row along the bottom.
///
/// In every split, the fraction is the share taken by the *first* child (the
/// left or top one): the new panel for `split_left`, the existing one for
/// `split_right` and `split_below`.
pub fn default_layout() -> DockState<Pane> {
    let mut state = DockState::new(vec![Pane::Workspace]);
    let tree = state.main_surface_mut();
    let [upper, _bottom] = tree.split_below(
        NodeIndex::root(),
        UPPER_SHARE,
        vec![Pane::Checks, Pane::Values],
    );
    let [middle, _palette] = tree.split_left(upper, PALETTE_SHARE, vec![Pane::Palette]);
    let [_centre, _properties] = tree.split_right(middle, CENTRE_SHARE, vec![Pane::Properties]);
    state
}

pub fn is_open(dock: &DockState<Pane>, pane: Pane) -> bool {
    dock.find_tab(&pane).is_some()
}

/// Remove `pane`'s tab, if it has one.
pub fn close(dock: &mut DockState<Pane>, pane: Pane) {
    if let Some(at) = dock.find_tab(&pane) {
        dock.remove_tab(at);
    }
}

/// Bring `pane`'s tab to the front of its tab group, if it is open.
pub fn bring_to_front(dock: &mut DockState<Pane>, pane: Pane) {
    if let Some(at) = dock.find_tab(&pane) {
        dock.set_active_tab(at);
    }
}

/// Add `pane` where it belongs by default. A panel that is already open is
/// left where the user put it. (Circuits are added by [`sync_documents`].)
pub fn open(dock: &mut DockState<Pane>, pane: Pane) {
    if is_open(dock, pane) {
        return;
    }
    match pane {
        Pane::Palette => split_centre(dock, Split::Left, PALETTE_SHARE, pane),
        // A tab beside the palette, or a column of its own on the left.
        Pane::Project => {
            if !join(dock, &[Pane::Palette], pane) {
                split_centre(dock, Split::Left, PALETTE_SHARE, pane);
            }
        }
        // A tab beside the properties, or a column of its own on the right.
        Pane::Assistant => {
            if !join(dock, &[Pane::Properties], pane) {
                split_centre(dock, Split::Right, CENTRE_SHARE, pane);
            }
        }
        Pane::Properties => split_centre(dock, Split::Right, CENTRE_SHARE, pane),
        Pane::Checks | Pane::Values => {
            if !join(dock, &[Pane::Checks, Pane::Values], pane) {
                dock.main_surface_mut()
                    .split_below(NodeIndex::root(), UPPER_SHARE, vec![pane]);
            }
        }
        Pane::Workspace => dock.push_to_first_leaf(pane),
        Pane::Circuit(_) | Pane::Retired => {}
    }
}

/// Put `pane` beside the central area. `fraction` is the share of the first
/// (left or top) child: the new panel for `Left`, the central area otherwise.
fn split_centre(dock: &mut DockState<Pane>, side: Split, fraction: f32, pane: Pane) {
    match dock.find_tab(&Pane::Workspace) {
        Some((surface, node, _)) => {
            dock.split((surface, node), side, fraction, Node::leaf(pane));
        }
        None => dock.push_to_first_leaf(pane),
    }
}

/// Add `pane` to the tab group of the first open member of `group`. Returns
/// false if none is open.
fn join(dock: &mut DockState<Pane>, group: &[Pane], pane: Pane) -> bool {
    let Some((surface, node)) = group
        .iter()
        .find_map(|p| dock.find_tab(p).map(|(s, n, _)| (s, n)))
    else {
        return false;
    };
    dock.set_focused_node_and_surface((surface, node));
    dock.push_to_focused_leaf(pane);
    true
}

/// Make the circuit tabs match the open circuits: the central area holds one
/// tab for each id in `circuits`, and no other circuit tab exists. One that
/// was dragged elsewhere is put back (egui_dock cannot forbid the drop).
/// True if anything changed.
pub fn sync_documents(dock: &mut DockState<Pane>, circuits: &[u32]) -> bool {
    let mut changed = false;
    if !is_open(dock, Pane::Workspace) {
        open(dock, Pane::Workspace);
        changed = true;
    }
    // Removing a tab can renumber the areas, so each one is looked for afresh.
    loop {
        let Some((surface, centre, _)) = dock.find_tab(&Pane::Workspace) else {
            return changed;
        };
        let wrong = dock.iter_all_tabs().map(|(_, p)| *p).find(|p| match p {
            Pane::Circuit(id) => {
                !circuits.contains(id)
                    || dock
                        .find_tab(p)
                        .is_some_and(|(s, n, _)| (s, n) != (surface, centre))
            }
            _ => false,
        });
        let Some(wrong) = wrong else { break };
        close(dock, wrong);
        changed = true;
    }
    for &id in circuits {
        if !is_open(dock, Pane::Circuit(id))
            && let Some((surface, centre, _)) = dock.find_tab(&Pane::Workspace)
        {
            dock[surface][centre].append_tab(Pane::Circuit(id));
            changed = true;
        }
    }
    if circuits.is_empty() {
        bring_to_front(dock, Pane::Workspace);
    }
    changed
}

/// The open circuits in the order of their tabs, which the user can change.
pub fn circuit_order(dock: &DockState<Pane>) -> Vec<u32> {
    let Some((surface, node, _)) = dock.find_tab(&Pane::Workspace) else {
        return Vec::new();
    };
    match &dock[surface][node] {
        Node::Leaf(leaf) => leaf
            .tabs
            .iter()
            .filter_map(|tab| match tab {
                Pane::Circuit(id) => Some(*id),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The circuit whose tab is in front in the central area, if one is.
pub fn front_circuit(dock: &DockState<Pane>) -> Option<u32> {
    let (surface, node, _) = dock.find_tab(&Pane::Workspace)?;
    match &dock[surface][node] {
        Node::Leaf(leaf) => match leaf.tabs.get(leaf.active.0) {
            Some(Pane::Circuit(id)) => Some(*id),
            _ => None,
        },
        _ => None,
    }
}

/// Make a restored layout safe to use: tabs from panels that no longer dock,
/// and circuits that were open, are dropped (along with any floating window
/// they leave empty, which the dock cannot draw); a panel appearing twice
/// keeps its first tab; and a layout with no central area is replaced by the
/// default.
pub fn sanitize(mut state: DockState<Pane>) -> DockState<Pane> {
    state.retain_tabs(|p| !matches!(p, Pane::Retired | Pane::Circuit(_)));
    let mut seen: Vec<Pane> = Vec::new();
    state.retain_tabs(|p| {
        if seen.contains(p) {
            false
        } else {
            seen.push(*p);
            true
        }
    });
    let emptied: Vec<SurfaceIndex> = (1..state.surfaces_count())
        .map(SurfaceIndex)
        .filter(|&i| {
            state
                .get_surface(i)
                .and_then(|surface| surface.node_tree())
                .is_some_and(|tree| tree.num_tabs() == 0)
        })
        .collect();
    for surface in emptied.into_iter().rev() {
        state.remove_surface(surface);
    }
    if seen.contains(&Pane::Workspace) {
        state
    } else {
        default_layout()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panes(dock: &DockState<Pane>) -> Vec<Pane> {
        dock.iter_all_tabs().map(|(_, p)| *p).collect()
    }

    fn leaf_of(dock: &DockState<Pane>, pane: Pane) -> (egui_dock::SurfaceIndex, NodeIndex) {
        let (surface, node, _) = dock.find_tab(&pane).expect("pane is open");
        (surface, node)
    }

    /// The share of its parent's area that `pane`'s tab group occupies.
    fn share_of(dock: &DockState<Pane>, pane: Pane) -> f32 {
        let (_, node, _) = dock.find_tab(&pane).expect("pane is open");
        let parent = node.parent().expect("not the root");
        let (Node::Horizontal(split) | Node::Vertical(split)) = &dock.main_surface()[parent] else {
            panic!("parent is a split");
        };
        if node.is_left() {
            split.fraction
        } else {
            1.0 - split.fraction
        }
    }

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    fn saved_and_loaded(dock: &DockState<Pane>) -> DockState<Pane> {
        ron::from_str(&ron::to_string(dock).unwrap()).unwrap()
    }

    #[test]
    fn panels_start_with_sensible_proportions() {
        let dock = default_layout();
        assert!(near(share_of(&dock, Pane::Palette), PALETTE_SHARE));
        assert!(near(share_of(&dock, Pane::Workspace), CENTRE_SHARE));
        assert!(near(share_of(&dock, Pane::Properties), 1.0 - CENTRE_SHARE));
        assert!(near(share_of(&dock, Pane::Checks), 1.0 - UPPER_SHARE));
        // Reopened panels get the same shares back.
        let mut dock = default_layout();
        close(&mut dock, Pane::Palette);
        open(&mut dock, Pane::Palette);
        assert!(near(share_of(&dock, Pane::Palette), PALETTE_SHARE));
    }

    #[test]
    fn default_layout_has_each_core_panel_once() {
        let dock = default_layout();
        let mut found = panes(&dock);
        found.sort_by_key(|p| p.title());
        let mut expected = vec![
            Pane::Workspace,
            Pane::Palette,
            Pane::Properties,
            Pane::Checks,
            Pane::Values,
        ];
        expected.sort_by_key(|p| p.title());
        assert_eq!(found, expected);
        // Checks and values share a tab strip; the others have their own.
        assert_eq!(leaf_of(&dock, Pane::Checks), leaf_of(&dock, Pane::Values));
        assert_ne!(
            leaf_of(&dock, Pane::Workspace),
            leaf_of(&dock, Pane::Palette)
        );
        assert_ne!(
            leaf_of(&dock, Pane::Workspace),
            leaf_of(&dock, Pane::Properties)
        );
    }

    #[test]
    fn open_circuits_share_the_central_area_which_never_goes() {
        let mut dock = default_layout();
        assert!(!sync_documents(&mut dock, &[]), "nothing to do");
        assert!(sync_documents(&mut dock, &[1, 2]));
        let centre = leaf_of(&dock, Pane::Workspace);
        assert_eq!(leaf_of(&dock, Pane::Circuit(1)), centre);
        assert_eq!(leaf_of(&dock, Pane::Circuit(2)), centre);
        bring_to_front(&mut dock, Pane::Circuit(1));
        assert_eq!(front_circuit(&dock), Some(1));
        assert_eq!(circuit_order(&dock), [1, 2]);
        assert!(!sync_documents(&mut dock, &[1, 2]));
        // Closing them all leaves the area, and the panels where they were.
        assert!(sync_documents(&mut dock, &[2]));
        assert!(!is_open(&dock, Pane::Circuit(1)));
        assert!(sync_documents(&mut dock, &[]));
        assert_eq!(front_circuit(&dock), None);
        assert_eq!(leaf_of(&dock, Pane::Workspace), centre);
        assert!(near(share_of(&dock, Pane::Palette), PALETTE_SHARE));
        assert!(near(share_of(&dock, Pane::Properties), 1.0 - CENTRE_SHARE));
    }

    #[test]
    fn a_circuit_dragged_elsewhere_is_put_back() {
        let mut dock = default_layout();
        sync_documents(&mut dock, &[1, 2]);
        // Into another panel's tab strip.
        close(&mut dock, Pane::Circuit(2));
        let (surface, node) = leaf_of(&dock, Pane::Palette);
        dock[surface][node].append_tab(Pane::Circuit(2));
        assert!(sync_documents(&mut dock, &[1, 2]));
        assert_eq!(
            leaf_of(&dock, Pane::Circuit(2)),
            leaf_of(&dock, Pane::Workspace)
        );
        // Into an area of its own.
        close(&mut dock, Pane::Circuit(1));
        let centre = leaf_of(&dock, Pane::Workspace);
        dock.split(centre, Split::Right, 0.5, Node::leaf(Pane::Circuit(1)));
        assert!(sync_documents(&mut dock, &[1, 2]));
        assert_eq!(
            leaf_of(&dock, Pane::Circuit(1)),
            leaf_of(&dock, Pane::Workspace)
        );
        assert_eq!(panes(&dock).len(), 7);
    }

    #[test]
    fn a_central_area_is_made_if_there_is_none() {
        let mut dock = DockState::new(vec![Pane::Palette]);
        assert!(sync_documents(&mut dock, &[4]));
        assert_eq!(
            leaf_of(&dock, Pane::Circuit(4)),
            leaf_of(&dock, Pane::Workspace)
        );
    }

    #[test]
    fn only_side_panels_float_and_the_marker_cannot_close() {
        assert!(Pane::Circuit(1).closeable() && Pane::Palette.closeable());
        assert!(!Pane::Workspace.closeable());
        assert!(!Pane::Circuit(1).floats() && !Pane::Workspace.floats());
        assert!(Pane::Palette.floats() && Pane::Assistant.floats());
    }

    /// Which tab of `pane`'s group is in front.
    fn front_of(dock: &DockState<Pane>, pane: Pane) -> usize {
        let (surface, node, _) = dock.find_tab(&pane).expect("pane is open");
        match &dock[surface][node] {
            Node::Leaf(leaf) => leaf.active.0,
            _ => panic!("a pane sits in a leaf"),
        }
    }

    #[test]
    fn bringing_a_tab_to_the_front_shows_it_over_its_neighbour() {
        let mut dock = default_layout();
        let checks = dock.find_tab(&Pane::Checks).unwrap();
        let values = dock.find_tab(&Pane::Values).unwrap();
        bring_to_front(&mut dock, Pane::Values);
        assert_eq!(front_of(&dock, Pane::Checks), values.2.0);
        bring_to_front(&mut dock, Pane::Checks);
        assert_eq!(front_of(&dock, Pane::Checks), checks.2.0);
        // A pane that is not open is left alone.
        close(&mut dock, Pane::Values);
        bring_to_front(&mut dock, Pane::Values);
    }

    #[test]
    fn side_panels_come_back_where_they_belong() {
        let mut dock = default_layout();
        for p in Pane::SIDE_PANELS {
            close(&mut dock, p);
            assert!(!is_open(&dock, p), "{p:?}");
        }
        assert_eq!(panes(&dock), vec![Pane::Workspace]);
        for p in Pane::SIDE_PANELS {
            open(&mut dock, p);
            assert!(is_open(&dock, p), "{p:?}");
        }
        assert_eq!(leaf_of(&dock, Pane::Checks), leaf_of(&dock, Pane::Values));
        // The project panel is a tab beside the palette.
        assert_eq!(leaf_of(&dock, Pane::Project), leaf_of(&dock, Pane::Palette));
        // The assistant is a tab beside the properties.
        assert_eq!(
            leaf_of(&dock, Pane::Assistant),
            leaf_of(&dock, Pane::Properties)
        );
        assert_eq!(panes(&dock).len(), 7);
        // Opening one that is already open leaves it alone.
        let before = leaf_of(&dock, Pane::Palette);
        open(&mut dock, Pane::Palette);
        assert_eq!(leaf_of(&dock, Pane::Palette), before);
        assert_eq!(panes(&dock).len(), 7);
    }

    #[test]
    fn layout_survives_a_save_and_load_without_the_circuits() {
        let mut dock = default_layout();
        close(&mut dock, Pane::Palette);
        sync_documents(&mut dock, &[1, 2]);
        let back = saved_and_loaded(&dock);
        assert_eq!(panes(&back), panes(&dock));
        let restored = sanitize(back);
        assert!(!is_open(&restored, Pane::Palette));
        assert!(is_open(&restored, Pane::Workspace));
        assert!(!is_open(&restored, Pane::Circuit(1)));
        assert!(!is_open(&restored, Pane::Circuit(2)));
    }

    #[test]
    fn a_layout_from_before_circuit_tabs_keeps_its_arrangement() {
        // Saved with the schematic as a panel, and the Code panel beside it.
        let mut old = default_layout();
        let centre = leaf_of(&old, Pane::Workspace);
        old[centre.0][centre.1].append_tab(Pane::Retired);
        close(&mut old, Pane::Values);
        let text = ron::to_string(&old)
            .unwrap()
            .replacen("Workspace", "Schematic", 1)
            .replacen("Retired", "Code", 1);
        assert!(text.contains("Schematic") && text.contains("Code"));
        let restored = sanitize(ron::from_str(&text).unwrap());
        assert!(is_open(&restored, Pane::Workspace));
        assert!(!is_open(&restored, Pane::Retired));
        assert!(!is_open(&restored, Pane::Values), "not the default layout");
        assert!(near(share_of(&restored, Pane::Workspace), CENTRE_SHARE));
    }

    #[test]
    fn layouts_saved_when_instruments_could_dock_still_load() {
        // An older build wrote tabs named after the instruments. They load
        // as retired panels and are dropped; everything else is kept.
        let text = ron::to_string(&default_layout())
            .unwrap()
            .replacen("Values", "Scope", 1);
        assert!(text.contains("Scope"));
        let loaded: DockState<Pane> = ron::from_str(&text).unwrap();
        assert!(panes(&loaded).contains(&Pane::Retired));
        let restored = sanitize(loaded);
        let mut found = panes(&restored);
        found.sort_by_key(|p| p.title());
        let mut expected = vec![
            Pane::Workspace,
            Pane::Palette,
            Pane::Properties,
            Pane::Checks,
        ];
        expected.sort_by_key(|p| p.title());
        assert_eq!(found, expected);
    }

    /// A viewer that draws nothing, to run the dock headlessly.
    struct Blank;

    impl egui_dock::TabViewer for Blank {
        type Tab = Pane;
        fn title(&mut self, tab: &mut Pane) -> egui::WidgetText {
            tab.title().into()
        }
        fn ui(&mut self, _ui: &mut egui::Ui, _tab: &mut Pane) {}
    }

    #[test]
    fn a_floating_window_emptied_by_the_cleanup_is_removed_and_nothing_panics() {
        // A layout saved when an instrument had floated out of the dock: a
        // window surface holding only a retired tab.
        let mut state = default_layout();
        state.add_window(vec![Pane::Retired]);
        let restored = sanitize(state);
        assert!(
            (1..restored.surfaces_count()).all(|i| !restored.is_surface_valid(SurfaceIndex(i))),
            "an empty window was kept"
        );

        // The dock draws it without a problem (an emptied window used to
        // panic inside egui_dock).
        let mut restored = restored;
        let ctx = egui::Context::default();
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui_dock::DockArea::new(&mut restored).show(ctx, &mut Blank);
            });
        }
        assert!(is_open(&restored, Pane::Workspace));
    }

    #[test]
    fn unusable_layouts_fall_back_to_the_default() {
        // No central area.
        let none = DockState::new(vec![Pane::Palette, Pane::Checks]);
        assert_eq!(panes(&sanitize(none)), panes(&default_layout()));
        // A duplicated panel keeps one tab.
        let twice = DockState::new(vec![Pane::Workspace, Pane::Palette, Pane::Palette]);
        let fixed = sanitize(twice);
        assert_eq!(
            panes(&fixed)
                .iter()
                .filter(|p| **p == Pane::Palette)
                .count(),
            1
        );
        // Garbage text does not parse at all (the caller then uses the default).
        assert!(ron::from_str::<DockState<Pane>>("not a layout").is_err());
    }
}
