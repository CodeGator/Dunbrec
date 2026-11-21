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
    Schematic,
    Palette,
    Properties,
    Checks,
    Values,
    /// The firmware source editor.
    Code,
    /// The files of the open project.
    Project,
    /// A panel an older build could dock (the instruments). Layouts saved by
    /// those builds still load; these tabs are dropped on the way in.
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
        Pane::Code,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Pane::Schematic => "Schematic",
            Pane::Palette => "Palette",
            Pane::Properties => "Properties",
            Pane::Checks => "Checks",
            Pane::Values => "Values",
            Pane::Code => "Code",
            Pane::Project => "Project",
            Pane::Retired => "",
        }
    }

    /// The schematic is the one panel that cannot be closed.
    pub fn closeable(self) -> bool {
        self != Pane::Schematic
    }
}

/// Share of its area the palette starts with (and gets back when reopened).
const PALETTE_SHARE: f32 = 0.15;
/// Share the schematic keeps of the width left of the properties panel.
const SCHEMATIC_SHARE: f32 = 0.78;
/// Share of the height the panels above the checks and values keep.
const UPPER_SHARE: f32 = 0.85;

/// The arrangement a new user starts with, and what View → Reset layout
/// returns to: schematic in the middle, palette on the left, properties on
/// the right, and checks and values in a row along the bottom.
///
/// In every split, the fraction is the share taken by the *first* child (the
/// left or top one): the new panel for `split_left`, the existing one for
/// `split_right` and `split_below`.
pub fn default_layout() -> DockState<Pane> {
    let mut state = DockState::new(vec![Pane::Schematic]);
    let tree = state.main_surface_mut();
    let [upper, _bottom] = tree.split_below(
        NodeIndex::root(),
        UPPER_SHARE,
        vec![Pane::Checks, Pane::Values],
    );
    let [middle, _palette] = tree.split_left(upper, PALETTE_SHARE, vec![Pane::Palette]);
    let [_schematic, _properties] =
        tree.split_right(middle, SCHEMATIC_SHARE, vec![Pane::Properties]);
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

/// Add `pane` where it belongs by default. A panel that is already open is
/// left where the user put it.
pub fn open(dock: &mut DockState<Pane>, pane: Pane) {
    if is_open(dock, pane) {
        return;
    }
    match pane {
        Pane::Schematic => dock.push_to_first_leaf(pane),
        Pane::Palette => split_schematic(dock, Split::Left, PALETTE_SHARE, pane),
        // A tab beside the palette, or a column of its own on the left.
        Pane::Project => {
            if !join(dock, &[Pane::Palette], pane) {
                split_schematic(dock, Split::Left, PALETTE_SHARE, pane);
            }
        }
        Pane::Properties => split_schematic(dock, Split::Right, SCHEMATIC_SHARE, pane),
        Pane::Checks | Pane::Values => {
            if !join(dock, &[Pane::Checks, Pane::Values], pane) {
                dock.main_surface_mut()
                    .split_below(NodeIndex::root(), UPPER_SHARE, vec![pane]);
            }
        }
        // The editor is a tab beside the schematic, like an editor in an IDE.
        Pane::Code => {
            if !join(dock, &[Pane::Schematic], pane) {
                dock.push_to_first_leaf(pane);
            }
        }
        Pane::Retired => {}
    }
}

/// Put `pane` beside the schematic's tab group. `fraction` is the share of
/// the first (left or top) child: the new panel for `Left`, the schematic's
/// group otherwise.
fn split_schematic(dock: &mut DockState<Pane>, side: Split, fraction: f32, pane: Pane) {
    match dock.find_tab(&Pane::Schematic) {
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

/// Make a restored layout safe to use: tabs from panels that no longer dock
/// are dropped (along with any floating window they leave empty, which the
/// dock cannot draw); a panel appearing twice keeps its first tab; and a
/// layout with no schematic is replaced by the default.
pub fn sanitize(mut state: DockState<Pane>) -> DockState<Pane> {
    state.retain_tabs(|p| *p != Pane::Retired);
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
    if seen.contains(&Pane::Schematic) {
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

    #[test]
    fn panels_start_with_sensible_proportions() {
        let dock = default_layout();
        let near = |a: f32, b: f32| (a - b).abs() < 1e-4;
        assert!(near(share_of(&dock, Pane::Palette), PALETTE_SHARE));
        assert!(near(share_of(&dock, Pane::Schematic), SCHEMATIC_SHARE));
        assert!(near(
            share_of(&dock, Pane::Properties),
            1.0 - SCHEMATIC_SHARE
        ));
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
            Pane::Schematic,
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
            leaf_of(&dock, Pane::Schematic),
            leaf_of(&dock, Pane::Palette)
        );
        assert_ne!(
            leaf_of(&dock, Pane::Schematic),
            leaf_of(&dock, Pane::Properties)
        );
    }

    #[test]
    fn side_panels_come_back_where_they_belong() {
        let mut dock = default_layout();
        for p in Pane::SIDE_PANELS {
            close(&mut dock, p);
            assert!(!is_open(&dock, p), "{p:?}");
        }
        assert_eq!(panes(&dock), vec![Pane::Schematic]);
        for p in Pane::SIDE_PANELS {
            open(&mut dock, p);
            assert!(is_open(&dock, p), "{p:?}");
        }
        assert_eq!(leaf_of(&dock, Pane::Checks), leaf_of(&dock, Pane::Values));
        // The code editor is a tab beside the schematic.
        assert_eq!(leaf_of(&dock, Pane::Code), leaf_of(&dock, Pane::Schematic));
        // The project panel is a tab beside the palette.
        assert_eq!(leaf_of(&dock, Pane::Project), leaf_of(&dock, Pane::Palette));
        assert_eq!(panes(&dock).len(), 7);
        // Opening one that is already open leaves it alone.
        let before = leaf_of(&dock, Pane::Palette);
        open(&mut dock, Pane::Palette);
        assert_eq!(leaf_of(&dock, Pane::Palette), before);
        assert_eq!(panes(&dock).len(), 7);
    }

    #[test]
    fn layout_survives_a_save_and_load() {
        let mut dock = default_layout();
        close(&mut dock, Pane::Palette);
        let text = ron::to_string(&dock).unwrap();
        let back: DockState<Pane> = ron::from_str(&text).unwrap();
        assert_eq!(panes(&back), panes(&dock));
        let restored = sanitize(back);
        assert!(!is_open(&restored, Pane::Palette));
        assert!(is_open(&restored, Pane::Schematic));
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
            Pane::Schematic,
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
        assert!(is_open(&restored, Pane::Schematic));
    }

    #[test]
    fn unusable_layouts_fall_back_to_the_default() {
        // No schematic.
        let no_schematic = DockState::new(vec![Pane::Palette, Pane::Checks]);
        assert_eq!(panes(&sanitize(no_schematic)), panes(&default_layout()));
        // A duplicated panel keeps one tab.
        let twice = DockState::new(vec![Pane::Schematic, Pane::Palette, Pane::Palette]);
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
