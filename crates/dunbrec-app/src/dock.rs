//! The dockable panel layout: which panels exist, where they start, how the
//! arrangement is saved and restored, and keeping instrument tabs in step
//! with their on/off switches. What a panel shows lives with the code that
//! owns its data (see the pane viewer in `app.rs`).

use egui_dock::{DockState, Node, NodeIndex, Split};
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
    Scope,
    Meter,
    Logic,
    Bode,
    Transfer,
    Tracer,
    Fgen,
    Serial,
}

impl Pane {
    /// The panels the user can bring back from View → Panels.
    pub const SIDE_PANELS: [Pane; 4] =
        [Pane::Palette, Pane::Properties, Pane::Checks, Pane::Values];

    /// Instruments, whose visibility is a flag the toolbar and menus own.
    pub const INSTRUMENTS: [Pane; 8] = [
        Pane::Scope,
        Pane::Meter,
        Pane::Logic,
        Pane::Bode,
        Pane::Transfer,
        Pane::Tracer,
        Pane::Fgen,
        Pane::Serial,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Pane::Schematic => "Schematic",
            Pane::Palette => "Palette",
            Pane::Properties => "Properties",
            Pane::Checks => "Checks",
            Pane::Values => "Values",
            Pane::Scope => "Oscilloscope",
            Pane::Meter => "Multimeter",
            Pane::Logic => "Logic analyser",
            Pane::Bode => "Bode plotter",
            Pane::Transfer => "DC transfer",
            Pane::Tracer => "Curve tracer",
            Pane::Fgen => "Function generator",
            Pane::Serial => "Serial monitor",
        }
    }

    pub fn is_instrument(self) -> bool {
        Pane::INSTRUMENTS.contains(&self)
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
/// Share of its height the schematic keeps when an instrument opens below it.
const SCHEMATIC_ABOVE_INSTRUMENTS: f32 = 0.6;

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
        Pane::Properties => split_schematic(dock, Split::Right, SCHEMATIC_SHARE, pane),
        Pane::Checks | Pane::Values => {
            if !join(dock, &[Pane::Checks, Pane::Values], pane) {
                dock.main_surface_mut()
                    .split_below(NodeIndex::root(), UPPER_SHARE, vec![pane]);
            }
        }
        _ => {
            // Instruments share a tab strip under the schematic.
            if !join(dock, &Pane::INSTRUMENTS, pane) {
                split_schematic(dock, Split::Below, SCHEMATIC_ABOVE_INSTRUMENTS, pane);
            }
        }
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

/// Make the instrument tabs match which instruments are switched on.
pub fn reconcile(dock: &mut DockState<Pane>, wanted: &[(Pane, bool)]) {
    for &(pane, on) in wanted {
        match (on, is_open(dock, pane)) {
            (true, false) => open(dock, pane),
            (false, true) => close(dock, pane),
            _ => {}
        }
    }
}

/// Make a restored layout safe to use: instruments start switched off, so
/// their tabs are dropped; a panel appearing twice keeps its first tab; and
/// a layout with no schematic is replaced by the default.
pub fn sanitize(mut state: DockState<Pane>) -> DockState<Pane> {
    state.retain_tabs(|p| !p.is_instrument());
    let mut seen: Vec<Pane> = Vec::new();
    state.retain_tabs(|p| {
        if seen.contains(p) {
            false
        } else {
            seen.push(*p);
            true
        }
    });
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
        open(&mut dock, Pane::Scope);
        assert!(near(
            share_of(&dock, Pane::Scope),
            1.0 - SCHEMATIC_ABOVE_INSTRUMENTS
        ));
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
    fn instruments_share_a_strip_under_the_schematic() {
        let mut dock = default_layout();
        open(&mut dock, Pane::Scope);
        assert_ne!(leaf_of(&dock, Pane::Scope), leaf_of(&dock, Pane::Schematic));
        open(&mut dock, Pane::Meter);
        assert_eq!(leaf_of(&dock, Pane::Scope), leaf_of(&dock, Pane::Meter));
        // Opening twice does not duplicate, and does not move it.
        let before = leaf_of(&dock, Pane::Meter);
        open(&mut dock, Pane::Meter);
        assert_eq!(leaf_of(&dock, Pane::Meter), before);
        assert_eq!(
            panes(&dock).iter().filter(|p| **p == Pane::Meter).count(),
            1
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
        assert_eq!(panes(&dock).len(), 5);
    }

    #[test]
    fn reconcile_follows_the_switches() {
        let mut dock = default_layout();
        reconcile(
            &mut dock,
            &[
                (Pane::Scope, true),
                (Pane::Logic, true),
                (Pane::Meter, false),
            ],
        );
        assert!(is_open(&dock, Pane::Scope) && is_open(&dock, Pane::Logic));
        assert!(!is_open(&dock, Pane::Meter));
        reconcile(&mut dock, &[(Pane::Scope, false), (Pane::Logic, true)]);
        assert!(!is_open(&dock, Pane::Scope) && is_open(&dock, Pane::Logic));
        // Closing the last instrument leaves the core panels intact.
        reconcile(&mut dock, &[(Pane::Logic, false)]);
        assert_eq!(panes(&dock).len(), 5);
    }

    #[test]
    fn layout_survives_a_save_and_load() {
        let mut dock = default_layout();
        close(&mut dock, Pane::Palette);
        open(&mut dock, Pane::Scope);
        let text = ron::to_string(&dock).unwrap();
        let back: DockState<Pane> = ron::from_str(&text).unwrap();
        assert_eq!(panes(&back), panes(&dock));
        // On restore, instruments are dropped (their switches start off).
        let restored = sanitize(back);
        assert!(!is_open(&restored, Pane::Scope));
        assert!(!is_open(&restored, Pane::Palette));
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
