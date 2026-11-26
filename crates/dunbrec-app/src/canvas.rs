//! The schematic canvas: drawing, hit testing, and the editing tools.

use std::collections::{BTreeSet, HashMap, HashSet};

use dunbrec_core::{
    ComponentId, ComponentKind, Diagnostic, LabelId, Netlist, NodeId, Point, Rotation, Schematic,
    Waveform, WireId, units::format_value,
};
use egui::{
    Align2, Color32, FontId, Key, PointerButton, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui,
    Vec2, pos2, vec2,
};

use crate::document::Document;
use crate::leds::{self, Lights};
use crate::library::ensure_model;
use crate::symbols::{self, Prim};

/// Something on the schematic that can be selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Item {
    Component(ComponentId),
    Wire(WireId),
    Label(LabelId),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tool {
    Select,
    Place {
        kind: ComponentKind,
        rotation: Rotation,
        mirrored: bool,
    },
    Wire,
    Label,
    /// Click a wire or pin (voltage) or a part (current) to attach the
    /// instrument input chosen in the instrument windows.
    Probe,
}

/// Maps schematic grid coordinates to screen pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    /// Screen position of the grid origin, relative to the canvas corner.
    pub offset: Vec2,
    /// Pixels per grid unit.
    pub scale: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            offset: vec2(300.0, 250.0),
            scale: 14.0,
        }
    }
}

impl Camera {
    pub const MIN_SCALE: f32 = 3.0;
    pub const MAX_SCALE: f32 = 80.0;

    pub fn to_screen(self, origin: Pos2, p: Pos2) -> Pos2 {
        origin + self.offset + p.to_vec2() * self.scale
    }

    pub fn to_grid(self, origin: Pos2, screen: Pos2) -> Pos2 {
        ((screen - origin - self.offset) / self.scale).to_pos2()
    }

    /// Zoom by `factor`, keeping the grid point under `anchor` fixed.
    pub fn zoom_at(&mut self, origin: Pos2, anchor: Pos2, factor: f32) {
        let before = self.to_grid(origin, anchor);
        self.scale = (self.scale * factor).clamp(Self::MIN_SCALE, Self::MAX_SCALE);
        let after = self.to_screen(origin, before);
        self.offset += anchor - after;
    }
}

pub fn snap(p: Pos2) -> Point {
    Point::new(p.x.round() as i32, p.y.round() as i32)
}

fn to_pos(p: Point) -> Pos2 {
    pos2(p.x as f32, p.y as f32)
}

/// What a drag takes along besides what it was begun on: whatever was joined
/// to it at that moment. Nothing it passes over later is picked up.
#[derive(Debug, Default)]
struct Joined {
    /// Wire ends, each as the wire and whether it is the wire's `a` end.
    ends: Vec<(WireId, bool)>,
    /// Net labels.
    labels: Vec<LabelId>,
}

enum Drag {
    /// Moving the selection; `last` is the grid point of the previous frame,
    /// and `joined` is set by the first step.
    Move { last: Point, joined: Option<Joined> },
    /// Rubber-band selection from a screen position.
    Rubber { start: Pos2 },
    /// Dragging a wire corner or free end, which is now at `at`.
    Vertex { at: Point, joined: Option<Joined> },
}

/// Text placed on the canvas by an analysis, e.g. node voltages.
#[derive(Debug, Clone, PartialEq)]
pub struct Annotation {
    pub place: TagPlace,
    pub text: String,
}

/// Where a node's value tag goes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TagPlace {
    pub node: NodeId,
    /// Grid position (may fall between grid points, e.g. a wire midpoint).
    pub at: Pos2,
    /// On a vertical wire the tag goes beside it rather than above it.
    pub vertical: bool,
    /// Use the other side (left / below), because a net label already
    /// occupies the usual side.
    pub flip: bool,
}

/// A coloured flag on the canvas showing where an instrument is connected.
#[derive(Debug, Clone, PartialEq)]
pub struct Marker {
    pub at: Point,
    pub text: String,
    pub color: Color32,
}

/// The node at each pin and wire end, and the node of each wire.
pub struct NodeMap {
    pub points: HashMap<Point, NodeId>,
    pub wires: Vec<Option<NodeId>>,
}

impl NodeMap {
    pub fn new(s: &Schematic, netlist: &Netlist) -> Self {
        let (points, wires) = spread_nodes(s, netlist);
        Self { points, wires }
    }

    /// Node at grid point `p`: a pin, a wire end, a point along a wire, or
    /// a net label.
    pub fn node_at(&self, s: &Schematic, netlist: &Netlist, p: Point) -> Option<NodeId> {
        if let Some(n) = self.points.get(&p) {
            return Some(*n);
        }
        let on_wire = s
            .wires
            .iter()
            .zip(&self.wires)
            .find(|(w, _)| w.contains_interior(p))
            .and_then(|(_, n)| *n);
        on_wire.or_else(|| {
            s.labels
                .iter()
                .find(|l| l.position == p)
                .and_then(|l| netlist.node_by_name(&l.name))
        })
    }
}

/// Seed node numbers at pin positions, then spread them along wires.
fn spread_nodes(s: &Schematic, netlist: &Netlist) -> (HashMap<Point, NodeId>, Vec<Option<NodeId>>) {
    let mut node_at: HashMap<Point, NodeId> = HashMap::new();
    for e in &netlist.elements {
        if let Some(c) = s.component(e.component) {
            for ((_, at), n) in c.pin_positions().zip(&e.nodes) {
                node_at.insert(at, *n);
            }
        }
    }
    let mut wire_node: Vec<Option<NodeId>> = vec![None; s.wires.len()];
    loop {
        let mut changed = false;
        for (i, w) in s.wires.iter().enumerate() {
            if wire_node[i].is_some() {
                continue;
            }
            let found = node_at
                .iter()
                .find(|(p, _)| **p == w.a || **p == w.b || w.contains_interior(**p))
                .map(|(_, n)| *n);
            if let Some(n) = found {
                wire_node[i] = Some(n);
                node_at.entry(w.a).or_insert(n);
                node_at.entry(w.b).or_insert(n);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    (node_at, wire_node)
}

/// Where to show each node's value: the midpoint of the node's longest
/// wire, or failing that (a pin connected straight to a pin) a pin.
pub fn node_tag_positions(s: &Schematic, netlist: &Netlist) -> Vec<TagPlace> {
    let (node_at, wire_node) = spread_nodes(s, netlist);

    // Longest wire per node, remembering whether a label sits on it.
    let mut best: HashMap<NodeId, (f32, TagPlace)> = HashMap::new();
    for (w, n) in s.wires.iter().zip(&wire_node) {
        let Some(node) = *n else { continue };
        let (a, b) = (to_pos(w.a), to_pos(w.b));
        let len = a.distance(b);
        if best.get(&node).is_some_and(|(l, _)| *l >= len) {
            continue;
        }
        let labelled = s
            .labels
            .iter()
            .any(|l| l.position == w.a || l.position == w.b || w.contains_interior(l.position));
        let place = TagPlace {
            node,
            at: a.lerp(b, 0.5),
            vertical: (b.x - a.x).abs() < (b.y - a.y).abs(),
            flip: labelled,
        };
        best.insert(node, (len, place));
    }
    for (p, n) in &node_at {
        best.entry(*n).or_insert((
            0.0,
            TagPlace {
                node: *n,
                at: to_pos(*p),
                vertical: false,
                flip: false,
            },
        ));
    }
    let unused = netlist.unused_nets();
    let mut out: Vec<TagPlace> = best
        .into_values()
        .map(|(_, place)| place)
        .filter(|place| !place.node.is_ground() && !unused.contains(&place.node))
        .collect();
    out.sort_by_key(|place| place.node);
    out
}

/// A scroll bar's thumb being dragged.
#[derive(Clone, Copy)]
struct ScrollDrag {
    axis: usize,
    /// How far along the thumb it was taken hold of, in points.
    grab: f32,
    /// What the bars covered when the drag began; kept for the whole drag so
    /// that the thumb stays under the pointer.
    extent: Rect,
}

/// A tool in use only while its key is held.
struct Borrowed {
    /// What to go back to when the key is let go.
    from: Tool,
    /// The tool the key gives.
    to: Tool,
}

pub struct Canvas {
    pub camera: Camera,
    scrolling: Option<ScrollDrag>,
    pub tool: Tool,
    pub selection: BTreeSet<Item>,
    wire_start: Option<Point>,
    drag: Option<Drag>,
    /// Point where the user clicked with the label tool; the app asks for a
    /// name and then calls [`Canvas::take_label_request`].
    label_request: Option<Point>,
    /// Internal clipboard, and how many times it has been pasted (each
    /// paste lands a little further along so copies do not stack).
    pub clipboard: Option<Clipboard>,
    pub paste_count: i32,
    /// Probe tool click: the grid point and the part under it, if any.
    probe_request: Option<(Point, Option<ComponentId>)>,
    /// The push button being held down. Pressing flips it from its resting
    /// state and releasing flips it back, without an undo step.
    held_button: Option<ComponentId>,
    /// Set while Ctrl or Alt lends another tool.
    borrowed: Option<Borrowed>,
    /// Set until the drawing has been put in the middle of a new view: the
    /// size of the view it was last centred in.
    centring: Option<Vec2>,
}

impl Default for Canvas {
    fn default() -> Self {
        Self {
            camera: Camera::default(),
            scrolling: None,
            tool: Tool::Select,
            selection: BTreeSet::new(),
            wire_start: None,
            drag: None,
            label_request: None,
            probe_request: None,
            held_button: None,
            borrowed: None,
            centring: Some(Vec2::ZERO),
            clipboard: None,
            paste_count: 0,
        }
    }
}

// ------------------------------------------------------------ colours ---

struct Palette {
    background: Color32,
    grid: Color32,
    wire: Color32,
    symbol: Color32,
    selected: Color32,
    ghost: Color32,
    text: Color32,
    label: Color32,
    error: Color32,
    annotation_bg: Color32,
    annotation_fg: Color32,
}

impl Palette {
    fn for_ui(ui: &Ui) -> Self {
        if ui.visuals().dark_mode {
            Self {
                background: Color32::from_rgb(24, 26, 30),
                grid: Color32::from_rgb(60, 64, 72),
                wire: Color32::from_rgb(90, 200, 120),
                symbol: Color32::from_rgb(220, 220, 225),
                selected: Color32::from_rgb(255, 190, 60),
                ghost: Color32::from_rgba_unmultiplied(220, 220, 225, 110),
                text: Color32::from_rgb(170, 175, 185),
                label: Color32::from_rgb(120, 180, 255),
                error: Color32::from_rgb(240, 80, 80),
                annotation_bg: Color32::from_rgba_unmultiplied(40, 60, 110, 220),
                annotation_fg: Color32::WHITE,
            }
        } else {
            Self {
                background: Color32::from_rgb(250, 250, 247),
                grid: Color32::from_rgb(205, 205, 200),
                wire: Color32::from_rgb(20, 120, 50),
                symbol: Color32::from_rgb(30, 30, 35),
                selected: Color32::from_rgb(220, 120, 0),
                ghost: Color32::from_rgba_unmultiplied(30, 30, 35, 90),
                text: Color32::from_rgb(80, 80, 90),
                label: Color32::from_rgb(20, 80, 200),
                error: Color32::from_rgb(210, 30, 30),
                annotation_bg: Color32::from_rgba_unmultiplied(225, 235, 255, 235),
                annotation_fg: Color32::from_rgb(20, 40, 100),
            }
        }
    }
}

// --------------------------------------------------------- geometry ---

/// Distance from `p` to the segment `a`–`b`.
pub fn segment_distance(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let len2 = ab.length_sq();
    if len2 == 0.0 {
        return p.distance(a);
    }
    let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
    p.distance(a + ab * t)
}

/// Approximate extent of a net label (in grid units).
/// Width of the schematic's scroll bars.
const SCROLLBAR: f32 = 14.0;
/// The shortest a scroll bar's thumb gets, so it can be taken hold of.
const MIN_THUMB: f32 = 28.0;

/// The area the drawing takes up, in grid units; `None` if there is nothing.
pub fn drawing_bounds(s: &Schematic) -> Option<Rect> {
    let parts = s.components.iter().map(symbols::bounds);
    let wires = s
        .wires
        .iter()
        .map(|w| Rect::from_two_pos(to_pos(w.a), to_pos(w.b)));
    let labels = s.labels.iter().map(|l| label_rect(l.position, &l.name));
    parts.chain(wires).chain(labels).reduce(Rect::union)
}

/// What the scroll bars cover, in grid units: the drawing with half a screen
/// of room around it, so that there is always space to add to it at the edges,
/// and whatever is in view now.
fn scroll_extent(drawing: Option<Rect>, visible: Rect) -> Rect {
    let drawing = drawing.unwrap_or_else(|| Rect::from_min_size(visible.center(), Vec2::ZERO));
    drawing.expand2(visible.size() / 2.0).union(visible)
}

fn label_rect(position: Point, name: &str) -> Rect {
    let p = to_pos(position);
    Rect::from_min_max(
        pos2(p.x, p.y - 1.1),
        pos2(p.x + 0.5 + 0.55 * name.chars().count() as f32, p.y + 0.1),
    )
}

/// Net-label names whose connections should be shown: the label under the
/// pointer (`hovered`) and any selected labels.
fn focused_label_names<'a>(
    s: &'a Schematic,
    hovered: Option<Item>,
    selection: &BTreeSet<Item>,
) -> HashSet<&'a str> {
    s.labels
        .iter()
        .filter(|l| hovered == Some(Item::Label(l.id)) || selection.contains(&Item::Label(l.id)))
        .map(|l| l.name.as_str())
        .collect()
}

/// Indices of the wires on the nets those labels name.
fn wires_of_labelled_nets(
    s: &Schematic,
    netlist: &Netlist,
    names: &HashSet<&str>,
) -> HashSet<usize> {
    if names.is_empty() {
        return HashSet::new();
    }
    let nodes: HashSet<NodeId> = names
        .iter()
        .filter_map(|n| netlist.node_by_name(n))
        .collect();
    let (_, wire_node) = spread_nodes(s, netlist);
    wire_node
        .iter()
        .enumerate()
        .filter(|(_, n)| n.is_some_and(|n| nodes.contains(&n)))
        .map(|(i, _)| i)
        .collect()
}

/// Topmost item at grid position `p`.
pub fn hit_test(s: &Schematic, p: Pos2) -> Option<Item> {
    // Labels and wires are thin, so test them before component boxes.
    for l in s.labels.iter().rev() {
        if label_rect(l.position, &l.name).contains(p) {
            return Some(Item::Label(l.id));
        }
    }
    // A junction sits on the wires it joins, so it is tested before them.
    for c in s.components.iter().rev() {
        if matches!(c.kind, ComponentKind::Junction) && to_pos(c.position).distance(p) < 0.35 {
            return Some(Item::Component(c.id));
        }
    }
    for w in s.wires.iter().rev() {
        if segment_distance(p, to_pos(w.a), to_pos(w.b)) < 0.35 {
            return Some(Item::Wire(w.id));
        }
    }
    for c in s.components.iter().rev() {
        if symbols::bounds(c).expand(0.2).contains(p) {
            return Some(Item::Component(c.id));
        }
    }
    None
}

/// Items lying entirely inside `rect` (grid units).
pub fn items_in_rect(s: &Schematic, rect: Rect) -> Vec<Item> {
    let mut items = Vec::new();
    for c in &s.components {
        if rect.contains_rect(symbols::bounds(c)) {
            items.push(Item::Component(c.id));
        }
    }
    for w in &s.wires {
        if rect.contains(to_pos(w.a)) && rect.contains(to_pos(w.b)) {
            items.push(Item::Wire(w.id));
        }
    }
    for l in &s.labels {
        if rect.contains(to_pos(l.position)) {
            items.push(Item::Label(l.id));
        }
    }
    items
}

/// Points where three or more connections meet (drawn as dots).
pub fn junctions(s: &Schematic) -> Vec<Point> {
    let mut count: HashMap<Point, usize> = HashMap::new();
    for w in &s.wires {
        *count.entry(w.a).or_default() += 1;
        *count.entry(w.b).or_default() += 1;
    }
    for c in &s.components {
        for (_, p) in c.pin_positions() {
            *count.entry(p).or_default() += 1;
        }
    }
    let points: Vec<Point> = count.keys().copied().collect();
    for p in points {
        let through = s.wires.iter().filter(|w| w.contains_interior(p)).count();
        *count.get_mut(&p).expect("present") += 2 * through;
    }
    let mut dots: Vec<Point> = count
        .into_iter()
        .filter(|&(_, n)| n >= 3)
        .map(|(p, _)| p)
        .collect();
    dots.sort();
    dots
}

/// Radius of the hop drawn where a wire passes over another (grid units).
const HOP_RADIUS: f32 = 0.4;

/// Where a horizontal wire crosses a vertical one without being connected
/// to it: the horizontal wire and the crossing point. The wire is drawn
/// with a small hop there. A crossing is connected, and gets no hop, if a
/// pin (a junction part, say), a wire end or a label sits on it.
pub fn hops(s: &Schematic) -> Vec<(WireId, Point)> {
    let touched = |p: Point| {
        s.wires.iter().any(|w| w.a == p || w.b == p)
            || s.components
                .iter()
                .any(|c| c.pin_positions().any(|(_, q)| q == p))
            || s.labels.iter().any(|l| l.position == p)
    };
    let mut out = Vec::new();
    for h in s.wires.iter().filter(|w| w.a.y == w.b.y && w.a.x != w.b.x) {
        for v in s.wires.iter().filter(|w| w.a.x == w.b.x && w.a.y != w.b.y) {
            let p = Point::new(v.a.x, h.a.y);
            if h.contains_interior(p) && v.contains_interior(p) && !touched(p) {
                out.push((h.id, p));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// True if `p` is a connection point: a pin, a wire end, or on a wire.
fn is_connection_point(s: &Schematic, p: Point) -> bool {
    s.wires
        .iter()
        .any(|w| w.a == p || w.b == p || w.contains_interior(p))
        || s.components
            .iter()
            .any(|c| c.pin_positions().any(|(_, q)| q == p))
}

/// Orthogonal route from `a` to `b`: horizontal first, then vertical.
pub fn route(a: Point, b: Point) -> Vec<(Point, Point)> {
    let corner = Point::new(b.x, a.y);
    [(a, corner), (corner, b)]
        .into_iter()
        .filter(|(p, q)| p != q)
        .collect()
}

/// The unselected wire ends attached to the selected pins, wire ends and
/// labels: those that follow when the selection moves, so that connections are
/// kept. To be asked once, before the move begins.
fn joined_to(s: &Schematic, items: &BTreeSet<Item>) -> Joined {
    let mut moving: Vec<Point> = Vec::new();
    for item in items {
        match *item {
            Item::Component(id) => {
                if let Some(c) = s.component(id) {
                    moving.extend(c.pin_positions().map(|(_, p)| p));
                }
            }
            Item::Wire(id) => {
                if let Some(w) = s.wires.iter().find(|w| w.id == id) {
                    moving.extend([w.a, w.b]);
                }
            }
            Item::Label(id) => {
                if let Some(l) = s.labels.iter().find(|l| l.id == id) {
                    moving.push(l.position);
                }
            }
        }
    }
    let mut ends = Vec::new();
    for w in &s.wires {
        // A selected wire moves as a whole.
        if !items.contains(&Item::Wire(w.id)) {
            for (is_a, p) in [(true, w.a), (false, w.b)] {
                if moving.contains(&p) {
                    ends.push((w.id, is_a));
                }
            }
        }
    }
    Joined {
        ends,
        labels: Vec::new(),
    }
}

/// Move the selected items by `d`, and the wire ends `joined` to them.
fn move_items(s: &mut Schematic, items: &BTreeSet<Item>, joined: &Joined, d: Point) {
    for c in &mut s.components {
        if items.contains(&Item::Component(c.id)) {
            c.position = c.position + d;
        }
    }
    for l in &mut s.labels {
        if items.contains(&Item::Label(l.id)) {
            l.position = l.position + d;
        }
    }
    for w in &mut s.wires {
        if items.contains(&Item::Wire(w.id)) {
            w.a = w.a + d;
            w.b = w.b + d;
        } else {
            if joined.ends.contains(&(w.id, true)) {
                w.a = w.a + d;
            }
            if joined.ends.contains(&(w.id, false)) {
                w.b = w.b + d;
            }
        }
    }
}

/// Copied parts, wires and labels, with the models the parts use.
#[derive(Debug, Clone, PartialEq)]
pub struct Clipboard {
    components: Vec<dunbrec_core::Component>,
    wires: Vec<dunbrec_core::Wire>,
    labels: Vec<dunbrec_core::NetLabel>,
    models: Vec<dunbrec_core::DeviceModel>,
}

/// Copy the selected items, or `None` if nothing is selected.
pub fn copy_items(s: &Schematic, items: &BTreeSet<Item>) -> Option<Clipboard> {
    let components: Vec<_> = s
        .components
        .iter()
        .filter(|c| items.contains(&Item::Component(c.id)))
        .cloned()
        .collect();
    let wires: Vec<_> = s
        .wires
        .iter()
        .filter(|w| items.contains(&Item::Wire(w.id)))
        .cloned()
        .collect();
    let labels: Vec<_> = s
        .labels
        .iter()
        .filter(|l| items.contains(&Item::Label(l.id)))
        .cloned()
        .collect();
    if components.is_empty() && wires.is_empty() && labels.is_empty() {
        return None;
    }
    let models = components
        .iter()
        .filter_map(|c| c.kind.model_name())
        .filter_map(|m| s.models.get(m).cloned())
        .collect();
    Some(Clipboard {
        components,
        wires,
        labels,
        models,
    })
}

/// Paste `clip` moved by `offset`; returns the new items. Clashing
/// designators are renumbered; models missing here are added.
pub fn paste(s: &mut Schematic, clip: &Clipboard, offset: Point) -> BTreeSet<Item> {
    let mut pasted = BTreeSet::new();
    for m in &clip.models {
        if s.models.get(&m.name).is_none() {
            s.models.insert(m.clone());
        }
    }
    for c in &clip.components {
        let prefix = c.kind.designator_prefix();
        let taken = s
            .components
            .iter()
            .any(|o| !o.designator.is_empty() && o.designator == c.designator);
        let designator = if taken {
            s.next_designator(prefix)
        } else {
            c.designator.clone()
        };
        let id = s.add_component_named(c.kind.clone(), designator, c.position + offset);
        let new = s.component_mut(id).expect("just added");
        new.rotation = c.rotation;
        new.mirrored = c.mirrored;
        pasted.insert(Item::Component(id));
    }
    for w in &clip.wires {
        pasted.insert(Item::Wire(s.add_wire(w.a + offset, w.b + offset)));
    }
    for l in &clip.labels {
        pasted.insert(Item::Label(
            s.add_label(l.name.clone(), l.position + offset),
        ));
    }
    pasted
}

/// Flip a switch between open and closed, or a logic switch between low
/// and high.
pub fn toggle_switch(s: &mut Schematic, id: ComponentId) {
    if let Some(c) = s.component_mut(id) {
        match &mut c.kind {
            ComponentKind::Switch { closed, .. } => *closed = !*closed,
            ComponentKind::MultiSwitch { thrown, .. } => *thrown = !*thrown,
            ComponentKind::LogicSwitch { high } => *high = !*high,
            _ => {}
        }
    }
}

pub fn delete_items(s: &mut Schematic, items: &BTreeSet<Item>) {
    s.components
        .retain(|c| !items.contains(&Item::Component(c.id)));
    s.wires.retain(|w| !items.contains(&Item::Wire(w.id)));
    s.labels.retain(|l| !items.contains(&Item::Label(l.id)));
}

/// Drag-and-drop payload: a part dragged from the palette onto the canvas.
pub struct PartDrag {
    pub kind: ComponentKind,
}

/// Add a part to the schematic (with its device model, if it needs one).
fn add_part(
    s: &mut Schematic,
    kind: ComponentKind,
    at: Point,
    rotation: Rotation,
    mirrored: bool,
) -> ComponentId {
    ensure_model(s, &kind);
    let id = s.add_component(kind, at);
    let c = s.component_mut(id).expect("just added");
    c.rotation = rotation;
    c.mirrored = mirrored;
    id
}

/// What a press does in the wire tool.
#[derive(Debug, PartialEq, Eq)]
enum WirePress {
    /// Switch to Select and select this part: a wire cannot start in the
    /// middle of a part.
    Grab(ComponentId),
    /// A click inside a part's body while a wire is being drawn does
    /// nothing: the part has no connection there.
    Ignore,
    /// Start, bend or end a wire.
    Wire,
}

/// Classify a press at grid position `p` in the wire tool. Only a part's body
/// (not one of its pins) counts; a pin, a wire, a label or empty canvas all
/// mean wiring.
fn classify_wire_press(wire_in_progress: bool, s: &Schematic, p: Pos2) -> WirePress {
    let body = match hit_test(s, p) {
        Some(Item::Component(id)) => s
            .component(id)
            .filter(|c| {
                let at = snap(p);
                !c.pin_positions().any(|(_, pin)| pin == at)
            })
            .map(|c| c.id),
        _ => None,
    };
    match body {
        Some(id) if !wire_in_progress => WirePress::Grab(id),
        Some(_) => WirePress::Ignore,
        None => WirePress::Wire,
    }
}

/// The pin a click at grid position `p` lands on: the click snaps to a pin
/// of some part. A wire already ending there does not matter.
fn pin_at(s: &Schematic, p: Pos2) -> Option<Point> {
    let at = snap(p);
    // A junction's dot is something to select, not a pin to wire from.
    s.components
        .iter()
        .filter(|c| !matches!(c.kind, ComponentKind::Junction))
        .any(|c| c.pin_positions().any(|(_, pin)| pin == at))
        .then_some(at)
}

/// Wire corners and free ends that can be dragged: every wire end that is
/// not on a part's pin (those belong to the part). Sorted, without repeats.
fn draggable_vertices(s: &Schematic) -> Vec<Point> {
    let pins: HashSet<Point> = s
        .components
        .iter()
        .flat_map(|c| c.pin_positions().map(|(_, p)| p))
        .collect();
    let mut ends: Vec<Point> = s
        .wires
        .iter()
        .flat_map(|w| [w.a, w.b])
        .filter(|p| !pins.contains(p))
        .collect();
    ends.sort();
    ends.dedup();
    ends
}

/// The draggable wire end nearest to grid position `p`, if within `radius`
/// grid units.
fn vertex_near(s: &Schematic, p: Pos2, radius: f32) -> Option<Point> {
    draggable_vertices(s)
        .into_iter()
        .map(|v| (v, to_pos(v).distance(p)))
        .filter(|(_, d)| *d <= radius)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(v, _)| v)
}

/// Every wire end and net label at the point `at`: what dragging that point
/// takes along, so that what is connected there stays connected. To be asked
/// once, before the drag begins.
fn joined_at(s: &Schematic, at: Point) -> Joined {
    let ends = s
        .wires
        .iter()
        .flat_map(|w| [(w.id, true, w.a), (w.id, false, w.b)])
        .filter(|(.., p)| *p == at)
        .map(|(id, is_a, _)| (id, is_a))
        .collect();
    let labels = s
        .labels
        .iter()
        .filter(|l| l.position == at)
        .map(|l| l.id)
        .collect();
    Joined { ends, labels }
}

/// Put the wire ends and labels of `joined` at `to`.
fn move_vertex(s: &mut Schematic, joined: &Joined, to: Point) {
    for w in &mut s.wires {
        if joined.ends.contains(&(w.id, true)) {
            w.a = to;
        }
        if joined.ends.contains(&(w.id, false)) {
            w.b = to;
        }
    }
    for l in &mut s.labels {
        if joined.labels.contains(&l.id) {
            l.position = to;
        }
    }
}

/// How close (grid units) a press must be to a wire corner to grab it.
const CORNER_GRAB: f32 = 0.5;
/// How close (grid units) the pointer must be for a corner's handle to show.
const CORNER_SHOW: f32 = 0.8;

/// What a right-click on component `id` rotates: the whole selection if the
/// part is in it, otherwise just that part.
fn rotation_targets(selection: &BTreeSet<Item>, id: ComponentId) -> BTreeSet<Item> {
    if selection.contains(&Item::Component(id)) {
        selection.clone()
    } else {
        BTreeSet::from([Item::Component(id)])
    }
}

/// Rotate (or mirror) selected components in place.
pub fn transform_components(s: &mut Schematic, items: &BTreeSet<Item>, mirror: bool) {
    for c in &mut s.components {
        if items.contains(&Item::Component(c.id)) {
            if mirror {
                c.mirrored = !c.mirrored;
            } else {
                c.rotation = c.rotation.rotated_ccw();
            }
        }
    }
}

/// Short description of a component's value for the canvas.
pub fn value_text(kind: &ComponentKind) -> String {
    let wave = |w: &Waveform, unit: &str| match w {
        Waveform::Dc(v) => format_value(*v, unit),
        Waveform::Sine {
            amplitude,
            frequency,
            ..
        } => format!(
            "SIN {} {}",
            format_value(*amplitude, unit),
            format_value(*frequency, "Hz")
        ),
        Waveform::Pulse { v1, v2, period, .. } => format!(
            "PULSE {}…{} {}",
            format_value(*v1, ""),
            format_value(*v2, unit),
            format_value(*period, "s")
        ),
        Waveform::Pwl(points) => format!("PWL ({} points)", points.len()),
    };
    match kind {
        ComponentKind::Ground | ComponentKind::Junction => String::new(),
        ComponentKind::Resistor { resistance } => format_value(*resistance, "Ω"),
        ComponentKind::Capacitor { capacitance, .. } => format_value(*capacitance, "F"),
        ComponentKind::Inductor { inductance, .. } => format_value(*inductance, "H"),
        ComponentKind::VoltageSource { waveform } => wave(waveform, "V"),
        ComponentKind::CurrentSource { waveform } => wave(waveform, "A"),
        ComponentKind::Diode { model }
        | ComponentKind::Bjt { model, .. }
        | ComponentKind::Mosfet { model, .. } => model.clone(),
        ComponentKind::Switch { closed, .. } => (if *closed { "closed" } else { "open" }).into(),
        ComponentKind::MultiSwitch { form, thrown, .. } => {
            format!(
                "{} {}",
                form.name(),
                if *thrown { "thrown" } else { "rest" }
            )
        }
        ComponentKind::Potentiometer { resistance, wiper } => {
            format!("{} @{:.0}%", format_value(*resistance, "Ω"), wiper * 100.0)
        }
        ComponentKind::Vcvs { gain } | ComponentKind::Cccs { gain } => format!("×{gain}"),
        ComponentKind::Vccs { transconductance } => format_value(*transconductance, "S"),
        ComponentKind::Ccvs { transresistance } => format_value(*transresistance, "Ω"),
        ComponentKind::Gate { op, .. } => op.name().into(),
        ComponentKind::Inverter { .. } => "NOT".into(),
        ComponentKind::DFlipFlop { .. }
        | ComponentKind::Counter4 { .. }
        | ComponentKind::LogicLed => String::new(),
        ComponentKind::Clock { frequency, .. } => format_value(*frequency, "Hz"),
        ComponentKind::LogicSwitch { high } => (if *high { "high" } else { "low" }).into(),
        ComponentKind::Led { colour } | ComponentKind::LedBar { colour } => colour.name().into(),
        ComponentKind::SevenSegment { colour, common }
        | ComponentKind::FourDigit { colour, common } => {
            format!("{}, {}", colour.name(), common.name())
        }
        ComponentKind::Mcu { firmware, .. } => firmware_label(firmware),
        ComponentKind::OpAmp { part } => part.clone(),
        ComponentKind::Timer555 => "NE555".into(),
        ComponentKind::Cd4066 { .. } => "CD4066B".into(),
    }
}

/// Short name of a firmware reference: the built-in program or file name.
pub fn firmware_label(firmware: &str) -> String {
    let firmware = firmware.trim();
    if let Some(name) = firmware.strip_prefix(dunbrec_mcu::firmware::BUILTIN_PREFIX) {
        return name.to_string();
    }
    std::path::Path::new(firmware)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "no firmware".into())
}

// ----------------------------------------------------------- drawing ---

struct Painter<'a> {
    painter: &'a egui::Painter,
    camera: Camera,
    origin: Pos2,
}

impl Painter<'_> {
    fn screen(&self, p: Pos2) -> Pos2 {
        self.camera.to_screen(self.origin, p)
    }

    fn stroke_width(&self) -> f32 {
        (self.camera.scale * 0.12).clamp(1.0, 3.0)
    }

    /// The outline of a display's LED `segment` on screen.
    fn segment_outline(&self, c: &dunbrec_core::Component, segment: usize, grow: f32) -> Vec<Pos2> {
        symbols::segment_area(&c.kind, segment, grow, c.mirrored)
            .into_iter()
            .map(|p| self.screen(symbols::to_schematic(c, p)))
            .collect()
    }

    /// A display's colours, drawn behind its symbol: each segment pale when
    /// off, and filled with a glow around it as bright as its current.
    fn display_light(&self, c: &dunbrec_core::Component, light: Color32, currents: &[f64]) {
        let block = |segment: usize, grow: f32, colour: Color32| {
            let corners = self.segment_outline(c, segment, grow);
            self.painter
                .add(Shape::convex_polygon(corners, colour, Stroke::NONE));
        };
        let segments = c.kind.led_segments().map_or(0, |s| s.1.len());
        let brightness: Vec<f32> = (0..segments)
            .map(|k| leds::brightness(currents.get(k).copied().unwrap_or(0.0)))
            .collect();
        // Every glow first, so that none lies over a neighbouring segment.
        for (k, &b) in brightness.iter().enumerate() {
            if b > 0.0 {
                block(k, 0.35, light.gamma_multiply(0.35 * b));
            }
        }
        for (k, &b) in brightness.iter().enumerate() {
            block(k, 0.0, light.gamma_multiply(0.22 + 0.78 * b));
        }
    }

    /// An LED's colour, drawn behind its symbol: a pale filling when it is
    /// off, and a filling with a glow around it as bright as `brightness`.
    fn led_light(&self, c: &dunbrec_core::Component, light: Color32, brightness: f32) {
        let at = |x: f32, y: f32| self.screen(symbols::to_schematic(c, pos2(x, y)));
        if brightness > 0.0 {
            let centre = at(0.0, 0.0);
            for (radius, strength) in [(2.1, 0.25), (1.5, 0.45)] {
                self.painter.circle_filled(
                    centre,
                    self.camera.scale * radius,
                    light.gamma_multiply(strength * brightness),
                );
            }
        }
        let body: Vec<Pos2> = symbols::LED_BODY[..3]
            .iter()
            .map(|&(x, y)| at(x, y))
            .collect();
        let fill = light.gamma_multiply(0.22 + 0.78 * brightness);
        self.painter
            .add(Shape::convex_polygon(body, fill, Stroke::NONE));
    }

    fn component(
        &self,
        c: &dunbrec_core::Component,
        models: &dunbrec_core::ModelLibrary,
        color: Color32,
        text_color: Option<Color32>,
    ) {
        // A diode whose model has a breakdown voltage is drawn as a zener.
        let sym = match &c.kind {
            ComponentKind::Diode { model } => match models.get(model) {
                Some(m) => symbols::symbol_styled(&c.kind, m.params.contains_key("BV")),
                None => symbols::symbol(&c.kind),
            },
            _ => symbols::symbol(&c.kind),
        };
        let stroke = Stroke::new(self.stroke_width(), color);
        let map = |p: Pos2| self.screen(symbols::to_schematic(c, p));
        for prim in &sym.prims {
            match prim {
                Prim::Line(pts) => {
                    self.painter
                        .add(Shape::line(pts.iter().map(|p| map(*p)).collect(), stroke));
                }
                Prim::Filled(pts) => {
                    self.painter.add(Shape::convex_polygon(
                        pts.iter().map(|p| map(*p)).collect(),
                        color,
                        Stroke::NONE,
                    ));
                }
                Prim::Circle { center, radius } => {
                    self.painter
                        .circle_stroke(map(*center), radius * self.camera.scale, stroke);
                }
                Prim::Text { pos, text, size } => {
                    self.painter.text(
                        map(*pos),
                        Align2::CENTER_CENTER,
                        *text,
                        FontId::proportional(size * self.camera.scale),
                        color,
                    );
                }
            }
        }
        // A digit's segments are outlined here, not in its symbol, so that
        // mirroring the part does not mirror the digit.
        if c.kind.digit_count() > 0 {
            for segment in 0..c.kind.led_segments().map_or(0, |s| s.1.len()) {
                let outline = self.segment_outline(c, segment, 0.0);
                self.painter.add(Shape::closed_line(outline, stroke));
            }
        }
        if let Some(text_color) = text_color
            && self.camera.scale >= 6.0
        {
            let anchor = map(sym.label_anchor);
            // Align the text away from the symbol, whatever the rotation.
            let away = anchor - map(symbols::local_bounds(&c.kind).center());
            let align = if away.x.abs() >= away.y.abs() {
                if away.x >= 0.0 {
                    Align2::LEFT_CENTER
                } else {
                    Align2::RIGHT_CENTER
                }
            } else if away.y >= 0.0 {
                Align2::CENTER_TOP
            } else {
                Align2::CENTER_BOTTOM
            };
            let mut lines = Vec::new();
            if !c.designator.is_empty() {
                lines.push(c.designator.clone());
            }
            let value = value_text(&c.kind);
            if !value.is_empty() {
                lines.push(value);
            }
            self.painter.text(
                anchor,
                align,
                lines.join("\n"),
                FontId::proportional((self.camera.scale * 0.75).clamp(9.0, 16.0)),
                text_color,
            );
        }
    }
}

/// What the Label tool shows at the point `at` that a click would name: a
/// dot there, and a tag reading "abc" tied to it. The tag stands above and to
/// the right, clear of the mouse pointer.
fn label_cursor(painter: &egui::Painter, at: Pos2, fill: Color32, ink: Color32) {
    let text = painter.layout_no_wrap("abc".into(), FontId::proportional(15.0), ink);
    let half = text.size().y / 2.0 + 3.0;
    // The tag's pointed end, and how far along its straight sides start.
    let tip = at + vec2(8.0, -half - 5.0);
    let (left, right) = (tip.x + half, tip.x + half + text.size().x + 8.0);
    painter.line_segment([at, tip], Stroke::new(2.0_f32, fill));
    painter.circle_filled(at, 4.0, fill);
    painter.add(egui::Shape::convex_polygon(
        vec![
            tip,
            pos2(left, tip.y - half),
            pos2(right, tip.y - half),
            pos2(right, tip.y + half),
            pos2(left, tip.y + half),
        ],
        fill,
        Stroke::NONE,
    ));
    // The hole its string goes through.
    painter.circle_filled(tip + vec2(half * 0.65, 0.0), 2.5, ink);
    let corner = pos2(left + 3.0, tip.y - text.size().y / 2.0);
    painter.galley(corner, text, ink);
}

/// The warning over an LED carrying more than it should, ending above `at`.
/// `which` says which LED of a display it is.
fn too_much_current(painter: &egui::Painter, at: Pos2, which: &str, current: f64) {
    let text = format!("⚠ {which}{} - too much", format_value(current, "A"));
    let text = painter.layout_no_wrap(text, FontId::proportional(14.0), Color32::WHITE);
    let corner = at - vec2(text.size().x / 2.0, text.size().y + 3.0);
    let behind = Rect::from_min_size(corner, text.size()).expand(3.0);
    painter.rect_filled(behind, 3.0, Color32::from_rgb(190, 20, 20));
    painter.galley(corner, text, Color32::WHITE);
}

// ------------------------------------------------------- interaction ---

impl Canvas {
    pub fn cancel(&mut self) {
        self.wire_start = None;
        self.drag = None;
        self.borrowed = None;
        self.tool = Tool::Select;
    }

    /// Holding Ctrl wires and holding Alt selects, for as long as the key is
    /// down; then the tool in use before comes back. Both together, or a
    /// window without the keyboard, ask for neither. A drag is finished with
    /// the tool it began with.
    ///
    /// True if the tool changed, or will as soon as a drag ends.
    fn hold_tool(&mut self, modifiers: egui::Modifiers, focused: bool) -> bool {
        let wanted = match (modifiers.command, modifiers.alt) {
            _ if !focused => None,
            (true, false) => Some(Tool::Wire),
            (false, true) => Some(Tool::Select),
            _ => None,
        };
        let mut changed = false;
        if let Some(Borrowed { from, to }) = &self.borrowed {
            if self.tool != *to {
                // Another tool was chosen meanwhile; that choice stands.
                self.borrowed = None;
            } else if wanted.as_ref() != Some(to) {
                if self.drag.is_some() {
                    return true;
                }
                // A wire not finished with the borrowed tool is given up.
                if *to == Tool::Wire {
                    self.wire_start = None;
                }
                self.tool = from.clone();
                self.borrowed = None;
                changed = true;
            }
        }
        if self.borrowed.is_none()
            && self.drag.is_none()
            && let Some(to) = wanted
            && self.tool != to
        {
            let from = std::mem::replace(&mut self.tool, to.clone());
            self.borrowed = Some(Borrowed { from, to });
            changed = true;
        }
        changed
    }

    /// When a circuit is first shown, put its drawing in the middle of the
    /// view, at the zoom it has. The panels can take a frame or two to settle,
    /// so this goes on until the view is the same size twice; after that the
    /// view is the user's. True if another frame is wanted.
    fn centre_drawing(&mut self, view: Vec2, s: &Schematic) -> bool {
        let Some(last) = self.centring else {
            return false;
        };
        if view.x <= 0.0 || view.y <= 0.0 {
            return false;
        }
        let Some(drawing) = drawing_bounds(s) else {
            // Nothing to centre, and what is added later stays where it is put.
            self.centring = None;
            return false;
        };
        let offset = view / 2.0 - drawing.center().to_vec2() * self.camera.scale;
        // Whole points, so that lines stay sharp.
        self.camera.offset = offset.round();
        self.centring = (view != last).then_some(view);
        self.centring.is_some()
    }

    pub fn take_label_request(&mut self) -> Option<Point> {
        self.label_request.take()
    }

    pub fn take_probe_request(&mut self) -> Option<(Point, Option<ComponentId>)> {
        self.probe_request.take()
    }

    pub fn has_label_request(&self) -> bool {
        self.label_request.is_some()
    }

    /// Draw the scroll bars of the drawing shown in `view`, in the strips of
    /// `area` beside and below it, and move the view as they are used.
    fn scroll_bars(&mut self, ui: &mut Ui, area: Rect, view: Rect, s: &Schematic) {
        let corner = Rect::from_min_max(view.max, area.max);
        let tracks = [
            Rect::from_min_max(
                pos2(view.left(), view.bottom()),
                pos2(view.right(), area.bottom()),
            ),
            Rect::from_min_max(
                pos2(view.right(), view.top()),
                pos2(area.right(), view.bottom()),
            ),
        ];
        ui.painter()
            .rect_filled(corner, 0.0, ui.visuals().extreme_bg_color);
        for (axis, track) in tracks.into_iter().enumerate() {
            let visible = Rect::from_min_max(
                self.camera.to_grid(view.min, view.min),
                self.camera.to_grid(view.min, view.max),
            );
            let held = self.scrolling.filter(|d| d.axis == axis);
            let extent =
                held.map_or_else(|| scroll_extent(drawing_bounds(s), visible), |d| d.extent);
            // Along this axis, in grid units.
            let (start, length) = (extent.min[axis], extent.size()[axis]);
            let (from, shown) = (visible.min[axis], visible.size()[axis]);
            let room = (length - shown).max(0.0);
            // And on the track, in points.
            let along = track.size()[axis];
            let size = (along * shown / length.max(f32::EPSILON))
                .max(MIN_THUMB)
                .min(along);
            let travel = along - size;
            let part = if room > 0.0 {
                ((from - start) / room).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let mut thumb = track;
            thumb.min[axis] = track.min[axis] + part * travel;
            thumb.max[axis] = thumb.min[axis] + size;

            let id = ui.id().with(("schematic scroll", axis));
            let response = ui.interact(track, id, Sense::click_and_drag());
            if response.drag_started_by(PointerButton::Primary)
                && let Some(press) = ui.input(|i| i.pointer.press_origin())
                && thumb.contains(press)
            {
                self.scrolling = Some(ScrollDrag {
                    axis,
                    grab: press[axis] - thumb.min[axis],
                    extent,
                });
            }
            let mut to = None;
            if let Some(held) = self.scrolling.filter(|d| d.axis == axis)
                && let Some(pointer) = response.interact_pointer_pos()
                && travel > 0.0
            {
                let part = (pointer[axis] - held.grab - track.min[axis]) / travel;
                to = Some(start + part.clamp(0.0, 1.0) * room);
            } else if response.clicked()
                && let Some(click) = response.interact_pointer_pos()
                && !thumb.contains(click)
            {
                // A click beside the thumb moves most of a screen that way.
                let step = if click[axis] < thumb.min[axis] {
                    -shown
                } else {
                    shown
                };
                to = Some((from + 0.9 * step).clamp(start, start + room));
            }
            if let Some(to) = to {
                self.camera.offset[axis] = -to * self.camera.scale;
            }
            if response.drag_stopped() {
                self.scrolling = None;
            }

            let visuals = ui.visuals();
            let strength = if response.dragged() {
                0.85
            } else if response.hovered() {
                0.65
            } else {
                0.45
            };
            let color = visuals.text_color().gamma_multiply(strength);
            ui.painter()
                .rect_filled(track, 0.0, visuals.extreme_bg_color);
            ui.painter().rect_filled(thumb.shrink(2.0), 4.0, color);
        }
    }

    /// Show the canvas, handle input, and edit `doc`.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        doc: &mut Document,
        netlist: &Netlist,
        annotations: &[Annotation],
        markers: &[Marker],
        lights: &Lights,
    ) {
        // The drawing, with a scroll bar to its right and one below.
        let (area, _) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
        let rect = Rect::from_min_max(area.min, (area.max - Vec2::splat(SCROLLBAR)).max(area.min));
        let response = ui.interact(rect, ui.id().with("schematic"), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let origin = rect.min;
        if self.centre_drawing(rect.size(), doc.schematic()) {
            ui.ctx().request_repaint();
        }
        // First, so that what is drawn is where the bars have taken it.
        self.scroll_bars(ui, area, rect, doc.schematic());
        let colors = Palette::for_ui(ui);
        let pointer = response.hover_pos().or(response.interact_pointer_pos());
        let grid_pointer = pointer.map(|p| self.camera.to_grid(origin, p));
        let snapped = grid_pointer.map(snap);
        // A part being dragged in from the palette. No widget counts as
        // hovered during a drag, so read the pointer directly.
        let part_drag = response.dnd_hover_payload::<PartDrag>();
        let drag_at = part_drag
            .as_ref()
            .and_then(|_| ui.input(|i| i.pointer.latest_pos()))
            .map(|p| snap(self.camera.to_grid(origin, p)));

        // ---- navigation: the wheel scrolls (sideways with Shift) and zooms
        // with Ctrl, as does a pinch; the middle button drags the drawing ----
        if response.hovered() {
            let (scroll, zoom) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
            if let Some(p) = pointer
                && (zoom - 1.0).abs() > 1e-4
            {
                self.camera.zoom_at(origin, p, zoom);
            }
            self.camera.offset += scroll;
        }
        if response.dragged_by(PointerButton::Middle) {
            self.camera.offset += response.drag_delta();
        }

        // ---- keyboard ----
        let (modifiers, focused) = ui.input(|i| (i.modifiers, i.focused));
        if self.hold_tool(modifiers, focused) {
            // The palette and the bottom bar show the tool too.
            ui.ctx().request_repaint();
        }
        if !ui.ctx().wants_keyboard_input() {
            self.keyboard(ui, doc);
        }

        self.push_buttons(&response, doc, grid_pointer);

        // ---- tools ----
        match self.tool.clone() {
            Tool::Select => self.select_tool(&response, doc, origin, grid_pointer, snapped),
            Tool::Place {
                kind,
                rotation,
                mirrored,
            } => {
                if response.clicked()
                    && let Some(at) = snapped
                {
                    self.place(doc, kind.clone(), at, rotation, mirrored);
                }
                if response.clicked_by(PointerButton::Secondary) {
                    self.cancel();
                }
            }
            Tool::Wire => {
                let press = if response.drag_started_by(PointerButton::Primary) {
                    ui.input(|i| i.pointer.press_origin())
                        .map(|p| self.camera.to_grid(origin, p))
                } else if response.clicked() {
                    grid_pointer
                } else {
                    None
                };
                let kind = press
                    .map(|p| classify_wire_press(self.wire_start.is_some(), doc.schematic(), p));
                match kind {
                    Some(WirePress::Grab(_)) if self.borrowed.is_none() => {
                        // Carry the press into the Select tool: it selects
                        // the part, and a drag moves it.
                        self.tool = Tool::Select;
                        self.select_tool(&response, doc, origin, grid_pointer, snapped);
                    }
                    Some(WirePress::Grab(_) | WirePress::Ignore) => {}
                    _ => self.wire_tool(&response, doc, snapped),
                }
            }
            Tool::Label => {
                if response.clicked() {
                    self.label_request = snapped;
                }
                if response.clicked_by(PointerButton::Secondary) {
                    self.cancel();
                }
            }
            Tool::Probe => {
                if response.clicked()
                    && let (Some(at), Some(g)) = (snapped, grid_pointer)
                {
                    let part = match hit_test(doc.schematic(), g) {
                        Some(Item::Component(id)) => Some(id),
                        _ => None,
                    };
                    self.probe_request = Some((at, part));
                }
                if response.clicked_by(PointerButton::Secondary) {
                    self.cancel();
                }
            }
        }

        // Dropped from the palette: place it and carry on placing.
        if let Some(drag) = response.dnd_release_payload::<PartDrag>()
            && let Some(at) = drag_at
        {
            self.drop_part(doc, drag.kind.clone(), at);
        }

        // ---- drawing ----
        let p = Painter {
            painter: &painter,
            camera: self.camera,
            origin,
        };
        painter.rect_filled(rect, 0.0, colors.background);
        self.draw_grid(&painter, rect, &colors);
        let s = doc.schematic();
        let width = p.stroke_width();

        // Hovering over or selecting a net label lights up every label of
        // that name and the wires of its net: same-named labels are joined
        // even though no wire is drawn between them.
        let hovered = grid_pointer
            .filter(|_| response.hovered())
            .and_then(|g| hit_test(s, g))
            .filter(|item| matches!(item, Item::Label(_)));
        let focus = focused_label_names(s, hovered, &self.selection);
        let glow = colors.label.gamma_multiply(0.25);
        for i in wires_of_labelled_nets(s, netlist, &focus) {
            let w = &s.wires[i];
            painter.line_segment(
                [p.screen(to_pos(w.a)), p.screen(to_pos(w.b))],
                Stroke::new(width * 5.0, glow),
            );
        }

        let hops = hops(s);
        for w in &s.wires {
            let color = if self.selection.contains(&Item::Wire(w.id)) {
                colors.selected
            } else {
                colors.wire
            };
            let stroke = Stroke::new(width, color);
            let mut over: Vec<f32> = hops
                .iter()
                .filter(|(id, _)| *id == w.id)
                .map(|(_, at)| at.x as f32)
                .collect();
            if over.is_empty() {
                painter.line_segment([p.screen(to_pos(w.a)), p.screen(to_pos(w.b))], stroke);
                continue;
            }
            // Break the wire into pieces joined by small arcs over each
            // wire it crosses.
            let y = w.a.y as f32;
            let dir = if w.b.x >= w.a.x { 1.0 } else { -1.0 };
            over.sort_by(|a, b| (a * dir).total_cmp(&(b * dir)));
            let mut from = w.a.x as f32;
            for at in over {
                painter.line_segment(
                    [
                        p.screen(pos2(from, y)),
                        p.screen(pos2(at - dir * HOP_RADIUS, y)),
                    ],
                    stroke,
                );
                let arc: Vec<Pos2> = (0..=12)
                    .map(|i| {
                        let t = std::f32::consts::PI * i as f32 / 12.0;
                        p.screen(pos2(
                            at - dir * HOP_RADIUS * t.cos(),
                            y - HOP_RADIUS * t.sin(),
                        ))
                    })
                    .collect();
                painter.add(Shape::line(arc, stroke));
                from = at + dir * HOP_RADIUS;
            }
            painter.line_segment([p.screen(pos2(from, y)), p.screen(to_pos(w.b))], stroke);
        }
        for j in junctions(s) {
            painter.circle_filled(
                p.screen(to_pos(j)),
                (self.camera.scale * 0.25).max(2.5),
                colors.wire,
            );
        }
        if self.tool == Tool::Select {
            self.draw_corner_handles(
                ui,
                &p,
                &response,
                grid_pointer,
                s,
                [colors.selected, colors.ghost],
            );
        }
        for c in &s.components {
            let selected = self.selection.contains(&Item::Component(c.id));
            let color = if selected {
                colors.selected
            } else {
                colors.symbol
            };
            let currents = lights.leds.get(&c.id).map_or(&[][..], Vec::as_slice);
            if let ComponentKind::Led { colour } = c.kind {
                let current = currents.first().copied().unwrap_or(0.0);
                p.led_light(c, leds::light(colour), leds::brightness(current));
            }
            if let Some((colour, _)) = c.kind.led_segments() {
                p.display_light(c, leds::light(colour), currents);
            }
            p.component(
                c,
                &s.models,
                color,
                Some(if selected {
                    colors.selected
                } else {
                    colors.text
                }),
            );
            if matches!(c.kind, ComponentKind::LogicLed) && lights.logic.contains(&c.id) {
                let centre = p.screen(symbols::to_schematic(c, pos2(0.0, 0.0)));
                painter.circle_filled(
                    centre,
                    self.camera.scale * 1.6,
                    Color32::from_rgba_unmultiplied(255, 60, 40, 60),
                );
                painter.circle_filled(
                    centre,
                    self.camera.scale * 0.85,
                    Color32::from_rgb(255, 70, 50),
                );
            }
        }
        for l in &s.labels {
            let selected = self.selection.contains(&Item::Label(l.id));
            let color = if selected {
                colors.selected
            } else {
                colors.label
            };
            let at = p.screen(to_pos(l.position));
            if focus.contains(l.name.as_str()) {
                let r = label_rect(l.position, &l.name);
                let r = Rect::from_min_max(p.screen(r.min), p.screen(r.max));
                painter.rect_filled(r, 3.0, glow);
                painter.rect_stroke(
                    r,
                    3.0,
                    Stroke::new(1.0_f32, colors.label),
                    egui::StrokeKind::Outside,
                );
            }
            painter.circle_filled(at, (self.camera.scale * 0.15).max(2.0), color);
            painter.text(
                at + vec2(self.camera.scale * 0.3, -self.camera.scale * 0.2),
                Align2::LEFT_BOTTOM,
                &l.name,
                FontId::monospace((self.camera.scale * 0.85).clamp(9.0, 18.0)),
                color,
            );
        }
        // Unconnected pins.
        for d in &netlist.diagnostics {
            if let Diagnostic::UnconnectedPin { component, pin } = d
                && let Some(c) = s.component(*component)
                && let Some((_, at)) = c.pin_positions().find(|(def, _)| def.name == pin)
            {
                painter.circle_stroke(
                    p.screen(to_pos(at)),
                    (self.camera.scale * 0.3).max(3.0),
                    Stroke::new(1.5_f32, colors.error),
                );
            }
        }
        for a in annotations {
            let font = FontId::monospace(12.0);
            let galley = painter.layout_no_wrap(a.text.clone(), font, colors.annotation_fg);
            let size = galley.size();
            let point = p.screen(a.place.at);
            let at = match (a.place.vertical, a.place.flip) {
                (true, false) => point + vec2(6.0, -size.y / 2.0),
                (true, true) => point + vec2(-6.0 - size.x, -size.y / 2.0),
                (false, false) => point + vec2(-size.x / 2.0, -6.0 - size.y),
                (false, true) => point + vec2(-size.x / 2.0, 6.0),
            };
            let box_rect = Rect::from_min_size(at, galley.size()).expand(3.0);
            painter.rect_filled(box_rect, 3.0, colors.annotation_bg);
            painter.galley(at, galley, colors.annotation_fg);
        }

        // Over everything else drawn: the LEDs with too much current. A
        // display is warned about once, for its worst LED.
        for c in &s.components {
            let currents = lights.leds.get(&c.id).into_iter().flatten().copied();
            let worst = currents.enumerate().max_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((segment, current)) = worst
                && current > leds::MOST_CURRENT
            {
                let display = c.kind.led_segments().is_some();
                let centre = p.screen(symbols::to_schematic(c, pos2(0.0, 0.0)));
                // Clear of the part, however it is turned.
                let rise = if display {
                    c.position.y as f32 - symbols::bounds(c).top() + 0.3
                } else {
                    2.6
                };
                let above = centre - vec2(0.0, self.camera.scale * rise);
                let name = c.kind.segment_name(segment);
                let which = match c.kind.digit_count() {
                    1 => format!("segment {name}: "),
                    // The name starts with the digit's number.
                    4 => format!("digit {}, segment {}: ", &name[..1], &name[1..]),
                    _ if display => format!("LED {name}: "),
                    _ => String::new(),
                };
                too_much_current(&painter, above, &which, current);
            }
        }

        // ---- tool previews ----
        match &self.tool {
            Tool::Place {
                kind,
                rotation,
                mirrored,
            } => {
                if let Some(at) = snapped {
                    let ghost = dunbrec_core::Component {
                        id: ComponentId(u32::MAX),
                        designator: String::new(),
                        kind: kind.clone(),
                        position: at,
                        rotation: *rotation,
                        mirrored: *mirrored,
                    };
                    p.component(&ghost, &doc.schematic().models, colors.ghost, None);
                }
            }
            Tool::Wire => {
                if let Some(at) = snapped {
                    let cursor = p.screen(to_pos(at));
                    painter.circle_stroke(cursor, 4.0, Stroke::new(1.0_f32, colors.wire));
                    if let Some(start) = self.wire_start {
                        for (a, b) in route(start, at) {
                            painter.line_segment(
                                [p.screen(to_pos(a)), p.screen(to_pos(b))],
                                Stroke::new(width, colors.ghost),
                            );
                        }
                    }
                }
            }
            Tool::Label => {
                if let Some(at) = snapped {
                    let at = p.screen(to_pos(at));
                    label_cursor(&painter, at, colors.label, colors.background);
                }
            }
            Tool::Probe => {
                if let Some(at) = snapped {
                    let c = p.screen(to_pos(at));
                    let stroke = Stroke::new(1.5_f32, colors.selected);
                    painter.circle_stroke(c, 5.0, stroke);
                    painter.line_segment([c + vec2(4.0, -4.0), c + vec2(14.0, -14.0)], stroke);
                }
            }
            Tool::Select => {}
        }
        if let (Some(drag), Some(at)) = (&part_drag, drag_at) {
            let ghost = dunbrec_core::Component {
                id: ComponentId(u32::MAX),
                designator: String::new(),
                kind: drag.kind.clone(),
                position: at,
                rotation: Rotation::R0,
                mirrored: false,
            };
            p.component(&ghost, &doc.schematic().models, colors.ghost, None);
        }
        for m in markers {
            let at = p.screen(to_pos(m.at));
            let tip = at + vec2(-10.0, -18.0);
            painter.line_segment([at, tip], Stroke::new(2.0_f32, m.color));
            painter.circle_filled(at, 3.5, m.color);
            let galley =
                painter.layout_no_wrap(m.text.clone(), FontId::proportional(11.0), Color32::BLACK);
            let r = Align2::RIGHT_BOTTOM
                .anchor_size(tip + vec2(2.0, 0.0), galley.size())
                .expand(2.0);
            painter.rect_filled(r, 2.0, m.color);
            painter.galley(r.min + vec2(2.0, 2.0), galley, Color32::BLACK);
        }
        if let (Some(Drag::Rubber { start }), Some(now)) = (&self.drag, pointer) {
            let r = Rect::from_two_pos(*start, now);
            painter.rect_filled(r, 0.0, colors.selected.gamma_multiply(0.12));
            painter.rect_stroke(
                r,
                0.0,
                Stroke::new(1.0_f32, colors.selected),
                StrokeKind::Inside,
            );
        }
    }

    fn draw_grid(&self, painter: &egui::Painter, rect: Rect, colors: &Palette) {
        let step = if self.camera.scale >= 8.0 { 1 } else { 5 };
        let spacing = self.camera.scale * step as f32;
        if spacing < 6.0 {
            return;
        }
        let top_left = snap(self.camera.to_grid(rect.min, rect.min));
        let bottom_right = snap(self.camera.to_grid(rect.min, rect.max));
        let radius = if self.camera.scale >= 20.0 { 1.3 } else { 1.0 };
        let first = |v: i32| v - v.rem_euclid(step);
        let mut y = first(top_left.y);
        while y <= bottom_right.y {
            let mut x = first(top_left.x);
            while x <= bottom_right.x {
                let major = x.rem_euclid(5) == 0 && y.rem_euclid(5) == 0;
                let r = if major { radius + 0.4 } else { radius };
                painter.circle_filled(
                    self.camera.to_screen(rect.min, pos2(x as f32, y as f32)),
                    r,
                    colors.grid,
                );
                x += step;
            }
            y += step;
        }
    }

    fn keyboard(&mut self, ui: &Ui, doc: &mut Document) {
        let (escape, rotate, mirror, delete) = ui.input(|i| {
            (
                i.key_pressed(Key::Escape),
                i.key_pressed(Key::R) && !i.modifiers.command,
                i.key_pressed(Key::X) && !i.modifiers.command,
                i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace),
            )
        });
        if escape {
            if self.wire_start.is_some() {
                self.wire_start = None;
            } else if self.tool != Tool::Select {
                self.cancel();
            } else {
                self.selection.clear();
            }
        }
        // While placing, R and X turn the part about to be placed; otherwise
        // they turn what is selected.
        if let Tool::Place {
            rotation, mirrored, ..
        } = &mut self.tool
        {
            if rotate {
                *rotation = rotation.rotated_ccw();
            }
            if mirror {
                *mirrored = !*mirrored;
            }
        } else if (rotate || mirror) && !self.selection.is_empty() {
            let items = self.selection.clone();
            doc.edit(|s| transform_components(s, &items, mirror));
        }
        if delete && !self.selection.is_empty() {
            let items = std::mem::take(&mut self.selection);
            doc.edit(|s| delete_items(s, &items));
        }
    }

    /// Push buttons act while the mouse button is held on them (Select tool
    /// only). Starting to drag the part, or letting go anywhere, lets the
    /// button spring back.
    fn push_buttons(
        &mut self,
        response: &egui::Response,
        doc: &mut Document,
        grid_pointer: Option<Pos2>,
    ) {
        if let Some(id) = self.held_button {
            let down = response.ctx.input(|i| i.pointer.primary_down());
            let dragging = response.drag_started_by(PointerButton::Primary);
            if !down || dragging || self.tool != Tool::Select {
                self.held_button = None;
                if doc.schematic().component(id).is_some() {
                    toggle_switch(doc.schematic_mut(), id);
                }
            }
        }
        if self.tool == Tool::Select
            && self.held_button.is_none()
            && response.hovered()
            && response.ctx.input(|i| i.pointer.primary_pressed())
            && let Some(Item::Component(id)) =
                grid_pointer.and_then(|p| hit_test(doc.schematic(), p))
            && matches!(
                doc.schematic().component(id).map(|c| &c.kind),
                Some(ComponentKind::Switch {
                    momentary: true,
                    ..
                })
            )
        {
            toggle_switch(doc.schematic_mut(), id);
            self.held_button = Some(id);
        }
    }

    fn select_tool(
        &mut self,
        response: &egui::Response,
        doc: &mut Document,
        origin: Pos2,
        grid_pointer: Option<Pos2>,
        snapped: Option<Point>,
    ) {
        let shift = response.ctx.input(|i| i.modifiers.shift);
        // Right-click rotates the part under the pointer, once per click. If
        // it is part of the selection, the whole selection turns, as with R.
        if response.clicked_by(PointerButton::Secondary)
            && let Some(Item::Component(id)) =
                grid_pointer.and_then(|p| hit_test(doc.schematic(), p))
        {
            let items = rotation_targets(&self.selection, id);
            doc.edit(|s| transform_components(s, &items, false));
        }
        let corner_press = if response.drag_started_by(PointerButton::Primary) {
            response
                .ctx
                .input(|i| i.pointer.press_origin())
                .map(|p| self.camera.to_grid(origin, p))
                .and_then(|p| vertex_near(doc.schematic(), p, CORNER_GRAB))
        } else {
            None
        };
        if let Some(at) = corner_press {
            self.drag = Some(Drag::Vertex { at, joined: None });
        } else if response.drag_started_by(PointerButton::Primary) {
            let press = response
                .ctx
                .input(|i| i.pointer.press_origin())
                .map(|p| self.camera.to_grid(origin, p));
            let hit = press.and_then(|p| hit_test(doc.schematic(), p));
            match hit {
                Some(item) => {
                    if !self.selection.contains(&item) {
                        if !shift {
                            self.selection.clear();
                        }
                        self.selection.insert(item);
                    }
                    self.drag = press.map(|p| Drag::Move {
                        last: snap(p),
                        joined: None,
                    });
                }
                None => {
                    if !shift {
                        self.selection.clear();
                    }
                    let start = response.ctx.input(|i| i.pointer.press_origin());
                    self.drag = start.map(|start| Drag::Rubber { start });
                }
            }
        }
        if response.dragged_by(PointerButton::Primary)
            && let (Some(Drag::Move { last, joined }), Some(now)) = (&mut self.drag, snapped)
        {
            let d = now - *last;
            if d != Point::default() {
                let joined = joined.get_or_insert_with(|| {
                    doc.checkpoint();
                    joined_to(doc.schematic(), &self.selection)
                });
                move_items(doc.schematic_mut(), &self.selection, joined, d);
                *last = now;
            }
        }
        if response.dragged_by(PointerButton::Primary)
            && let (Some(Drag::Vertex { at, joined }), Some(now)) = (&mut self.drag, snapped)
            && now != *at
        {
            let joined = joined.get_or_insert_with(|| {
                doc.checkpoint();
                joined_at(doc.schematic(), *at)
            });
            move_vertex(doc.schematic_mut(), joined, now);
            *at = now;
        }
        if response.drag_stopped() {
            if let Some(Drag::Rubber { start }) = self.drag.take()
                && let Some(end) = response.interact_pointer_pos().or(response.hover_pos())
            {
                let r = Rect::from_two_pos(
                    self.camera.to_grid(origin, start),
                    self.camera.to_grid(origin, end),
                );
                self.selection.extend(items_in_rect(doc.schematic(), r));
            }
            self.drag = None;
        }
        if response.clicked() {
            // A plain click on a pin starts a wire from it; dragging from a
            // pin still moves the part.
            let pin = if shift || self.borrowed.is_some() {
                None
            } else {
                grid_pointer.and_then(|p| pin_at(doc.schematic(), p))
            };
            if let Some(at) = pin {
                self.tool = Tool::Wire;
                self.wire_start = Some(at);
            } else {
                match grid_pointer.and_then(|p| hit_test(doc.schematic(), p)) {
                    Some(item) => {
                        if shift {
                            if !self.selection.remove(&item) {
                                self.selection.insert(item);
                            }
                        } else {
                            self.selection.clear();
                            self.selection.insert(item);
                        }
                    }
                    None if !shift => self.selection.clear(),
                    None => {}
                }
            }
        }
        if response.double_clicked()
            && let Some(Item::Component(id)) =
                grid_pointer.and_then(|p| hit_test(doc.schematic(), p))
            && matches!(
                doc.schematic().component(id).map(|c| &c.kind),
                Some(
                    ComponentKind::Switch {
                        momentary: false,
                        ..
                    } | ComponentKind::MultiSwitch { .. }
                        | ComponentKind::LogicSwitch { .. }
                )
            )
        {
            doc.edit(|s| toggle_switch(s, id));
        }
    }

    /// Handles on wire corners and free ends (the Select tool): on the one
    /// the pointer is near, on the ends of selected wires, and on the one
    /// being dragged. While dragging, guide lines through the far end of each
    /// attached wire show where it would be straight.
    fn draw_corner_handles(
        &self,
        ui: &Ui,
        p: &Painter,
        response: &egui::Response,
        grid_pointer: Option<Pos2>,
        s: &Schematic,
        [accent, guide]: [Color32; 2],
    ) {
        let dragged = match &self.drag {
            Some(Drag::Vertex { at, .. }) => Some(*at),
            _ => None,
        };
        let size = (self.camera.scale * 0.3).max(4.0);
        let mut hovering = false;
        for v in draggable_vertices(s) {
            let near = response.hovered()
                && self.drag.is_none()
                && grid_pointer.is_some_and(|g| to_pos(v).distance(g) < CORNER_SHOW);
            let on_selected = s
                .wires
                .iter()
                .any(|w| (w.a == v || w.b == v) && self.selection.contains(&Item::Wire(w.id)));
            let active = dragged == Some(v);
            if !(near || on_selected || active) {
                continue;
            }
            hovering |= near;
            let square = Rect::from_center_size(p.screen(to_pos(v)), vec2(size * 2.0, size * 2.0));
            if active {
                p.painter.rect_filled(square, 1.0, accent);
            }
            p.painter.rect_stroke(
                square,
                1.0,
                Stroke::new(1.5_f32, accent),
                StrokeKind::Middle,
            );
        }
        if dragged.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if hovering {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        if let Some(at) = dragged {
            let area = response.rect;
            for w in &s.wires {
                let other = if w.a == at {
                    w.b
                } else if w.b == at {
                    w.a
                } else {
                    continue;
                };
                let o = p.screen(to_pos(other));
                let lines = [
                    (
                        other.y == at.y,
                        pos2(area.left(), o.y),
                        pos2(area.right(), o.y),
                    ),
                    (
                        other.x == at.x,
                        pos2(o.x, area.top()),
                        pos2(o.x, area.bottom()),
                    ),
                ];
                for (straight, a, b) in lines {
                    let stroke = if straight {
                        Stroke::new(1.5_f32, accent)
                    } else {
                        Stroke::new(1.0_f32, guide.gamma_multiply(0.6))
                    };
                    p.painter.line_segment([a, b], stroke);
                }
            }
        }
    }

    /// Add a part at `at`. It becomes the selection, so that the properties
    /// panel and Del apply to the one just placed.
    fn place(
        &mut self,
        doc: &mut Document,
        kind: ComponentKind,
        at: Point,
        rotation: Rotation,
        mirrored: bool,
    ) {
        let id = doc.edit(|s| add_part(s, kind, at, rotation, mirrored));
        self.selection = BTreeSet::from([Item::Component(id)]);
    }

    /// A part dropped from the palette at `at`: place it, and carry on placing
    /// more of the same until another tool or part is chosen.
    fn drop_part(&mut self, doc: &mut Document, kind: ComponentKind, at: Point) {
        self.place(doc, kind.clone(), at, Rotation::R0, false);
        self.wire_start = None;
        self.tool = Tool::Place {
            kind,
            rotation: Rotation::R0,
            mirrored: false,
        };
    }

    fn wire_tool(&mut self, response: &egui::Response, doc: &mut Document, snapped: Option<Point>) {
        if response.clicked_by(PointerButton::Secondary) {
            if self.wire_start.is_some() {
                self.wire_start = None;
            } else if self.borrowed.is_none() {
                self.cancel();
            }
            return;
        }
        if !(response.clicked() || response.double_clicked()) {
            return;
        }
        let Some(at) = snapped else { return };
        match self.wire_start {
            None => self.wire_start = Some(at),
            Some(start) if start == at => {
                // Clicking twice on the same point finishes the wire.
                self.wire_start = None;
            }
            Some(start) => {
                let ends_on_connection = is_connection_point(doc.schematic(), at);
                doc.edit(|s| {
                    for (a, b) in route(start, at) {
                        s.add_wire(a, b);
                    }
                });
                self.wire_start = if ends_on_connection || response.double_clicked() {
                    None
                } else {
                    Some(at)
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::examples::voltage_divider;

    /// A canvas filling a headless window, driven by made-up events.
    struct Rig {
        ctx: egui::Context,
        canvas: Canvas,
        doc: Document,
        netlist: Netlist,
        time: f64,
        /// What the last frame drew.
        drawn: Vec<egui::epaint::ClippedShape>,
        lights: Lights,
    }

    impl Rig {
        const WINDOW: Vec2 = vec2(800.0, 600.0);

        fn new(schematic: Schematic) -> Self {
            let netlist = dunbrec_core::build_netlist(&schematic);
            let mut rig = Rig {
                ctx: egui::Context::default(),
                canvas: Canvas::default(),
                doc: Document::new(schematic, None),
                netlist,
                time: 0.0,
                drawn: Vec::new(),
                lights: Lights::default(),
            };
            rig.settle();
            rig
        }

        fn frame(&mut self, modifiers: egui::Modifiers, events: Vec<egui::Event>) {
            self.time += 0.1;
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Self::WINDOW)),
                time: Some(self.time),
                modifiers,
                events,
                ..Default::default()
            };
            let (canvas, doc, netlist) = (&mut self.canvas, &mut self.doc, &self.netlist);
            let lights = &self.lights;
            let out = self.ctx.run(input, |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| {
                        canvas.show(ui, doc, netlist, &[], &[], lights);
                    });
            });
            self.drawn = out.shapes;
        }

        fn settle(&mut self) {
            for _ in 0..30 {
                self.frame(egui::Modifiers::NONE, vec![]);
            }
        }

        /// Turn the wheel (towards the user is negative) over the drawing.
        fn wheel(&mut self, lines: f32, modifiers: egui::Modifiers) {
            let at = pos2(300.0, 300.0);
            self.frame(modifiers, vec![egui::Event::PointerMoved(at)]);
            self.frame(
                modifiers,
                vec![egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: vec2(0.0, lines),
                    modifiers,
                }],
            );
            for _ in 0..30 {
                self.frame(modifiers, vec![]);
            }
        }

        fn button(&mut self, at: Pos2, pressed: bool) {
            self.frame(
                egui::Modifiers::NONE,
                vec![egui::Event::PointerButton {
                    pos: at,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
        }

        fn drag(&mut self, from: Pos2, to: Pos2) {
            let moved = |p| vec![egui::Event::PointerMoved(p)];
            self.frame(egui::Modifiers::NONE, moved(from));
            self.button(from, true);
            for i in 1..=10 {
                let at = from + (to - from) * (i as f32 / 10.0);
                self.frame(egui::Modifiers::NONE, moved(at));
            }
            self.button(to, false);
            self.settle();
        }

        /// The part of the schematic in view, in grid units.
        fn visible(&self) -> Rect {
            let c = self.canvas.camera;
            let size = Self::WINDOW - Vec2::splat(SCROLLBAR);
            Rect::from_min_max(
                c.to_grid(Pos2::ZERO, Pos2::ZERO),
                c.to_grid(Pos2::ZERO, Pos2::ZERO + size),
            )
        }
    }

    #[test]
    fn the_wheel_scrolls_and_zooms_with_ctrl() {
        let mut rig = Rig::new(voltage_divider());
        let start = rig.canvas.camera;
        rig.wheel(-2.0, egui::Modifiers::NONE);
        let down = rig.canvas.camera;
        assert_eq!(down.scale, start.scale);
        assert!(down.offset.y < start.offset.y, "further down the drawing");
        assert_eq!(down.offset.x, start.offset.x);
        rig.wheel(-2.0, egui::Modifiers::SHIFT);
        let right = rig.canvas.camera;
        assert!(right.offset.x < down.offset.x, "further to the right");
        assert_eq!((right.scale, right.offset.y), (down.scale, down.offset.y));
        // Zooming keeps what is under the pointer where it is.
        let ctrl = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        let at = pos2(300.0, 300.0);
        let under = right.to_grid(Pos2::ZERO, at);
        rig.wheel(2.0, ctrl);
        let zoomed = rig.canvas.camera;
        assert!(zoomed.scale > right.scale);
        assert!(zoomed.to_grid(Pos2::ZERO, at).distance(under) < 0.01);
    }

    #[test]
    fn the_scroll_bars_move_the_view_over_the_whole_drawing() {
        let mut s = Schematic::new();
        // A drawing far wider and taller than the window.
        s.add_wire(Point::new(0, 0), Point::new(200, 0));
        s.add_wire(Point::new(0, 0), Point::new(0, 150));
        let mut rig = Rig::new(s);
        let drawing = drawing_bounds(rig.doc.schematic()).unwrap();
        let start = rig.visible();
        let extent = scroll_extent(Some(drawing), start);
        assert!(extent.contains_rect(drawing.expand(1.0)));
        // The bottom bar's thumb, dragged to the far end, shows the right
        // edge of the drawing; up and down is left alone.
        let along = rig.canvas.camera;
        let width = Rig::WINDOW.x - SCROLLBAR;
        let thumb_x = (start.left() - extent.left()) / extent.width() * width + 10.0;
        let y = Rig::WINDOW.y - SCROLLBAR / 2.0;
        rig.drag(pos2(thumb_x, y), pos2(Rig::WINDOW.x - 1.0, y));
        let end = rig.visible();
        assert!(end.right() >= drawing.right(), "{end:?}");
        assert_eq!(rig.canvas.camera.offset.y, along.offset.y);
        assert_eq!(rig.canvas.camera.scale, along.scale);
        assert!(rig.canvas.scrolling.is_none(), "let go");
        // A click above the right bar's thumb pages up, one below it down.
        let x = Rig::WINDOW.x - SCROLLBAR / 2.0;
        let before = rig.visible().top();
        let bottom = pos2(x, Rig::WINDOW.y - SCROLLBAR - 2.0);
        rig.frame(
            egui::Modifiers::NONE,
            vec![egui::Event::PointerMoved(bottom)],
        );
        rig.button(bottom, true);
        rig.button(bottom, false);
        rig.settle();
        let paged = rig.visible().top();
        assert!(paged > before, "{before} then {paged}");
        let top = pos2(x, 2.0);
        rig.frame(egui::Modifiers::NONE, vec![egui::Event::PointerMoved(top)]);
        rig.button(top, true);
        rig.button(top, false);
        rig.settle();
        assert!(rig.visible().top() < paged);
        // The bars edit nothing.
        assert!(!rig.doc.is_dirty() && rig.canvas.selection.is_empty());
    }

    #[test]
    fn an_empty_schematic_has_nothing_to_scroll_to() {
        assert_eq!(drawing_bounds(&Schematic::new()), None);
        let visible = Rect::from_min_size(pos2(-3.0, 2.0), vec2(40.0, 30.0));
        assert_eq!(scroll_extent(None, visible), visible);
        let rig = Rig::new(Schematic::new());
        assert!(rig.canvas.scrolling.is_none());
    }

    #[test]
    fn unconnected_crossings_get_a_hop_and_connected_ones_do_not() {
        let mut s = Schematic::new();
        let h = s.add_wire(Point::new(0, 0), Point::new(10, 0));
        s.add_wire(Point::new(5, -4), Point::new(5, 4));
        s.add_wire(Point::new(8, -4), Point::new(8, 4));
        // Two vertical wires cross the horizontal one: two hops.
        assert_eq!(hops(&s), vec![(h, Point::new(5, 0)), (h, Point::new(8, 0))]);
        // A junction part on the first crossing connects it: no hop there.
        s.add_component(ComponentKind::Junction, Point::new(5, 0));
        assert_eq!(hops(&s), vec![(h, Point::new(8, 0))]);
        // A wire ending on the second one (a four-way meeting) connects it too.
        s.add_wire(Point::new(8, 0), Point::new(8, 2));
        assert!(hops(&s).is_empty());
    }

    #[test]
    fn a_junction_on_a_wire_can_be_clicked() {
        let mut s = Schematic::new();
        let w = s.add_wire(Point::new(0, 0), Point::new(10, 0));
        let j = s.add_component(ComponentKind::Junction, Point::new(5, 0));
        assert_eq!(hit_test(&s, pos2(5.1, 0.1)), Some(Item::Component(j)));
        assert_eq!(hit_test(&s, pos2(2.0, 0.0)), Some(Item::Wire(w)));
        // Clicking it selects it rather than starting a wire.
        assert_eq!(pin_at(&s, pos2(5.0, 0.0)), None);
    }

    #[test]
    fn wires_that_only_meet_at_ends_or_in_a_t_have_no_hop() {
        let mut s = Schematic::new();
        s.add_wire(Point::new(0, 0), Point::new(10, 0));
        s.add_wire(Point::new(5, 0), Point::new(5, 4)); // a T
        s.add_wire(Point::new(10, 0), Point::new(10, 4)); // a corner
        assert!(hops(&s).is_empty());
    }

    #[test]
    fn toggling_flips_every_kind_of_switch() {
        use crate::library::palette;
        let mut s = Schematic::new();
        let ids: Vec<_> = palette()
            .iter()
            .filter(|e| e.group == "Switches")
            .map(|e| s.add_component((e.make)(), Point::new(0, 0)))
            .collect();
        assert_eq!(ids.len(), 6);
        let labels = |s: &Schematic| -> Vec<String> {
            s.components.iter().map(|c| value_text(&c.kind)).collect()
        };
        let before = labels(&s);
        for &id in &ids {
            toggle_switch(&mut s, id);
        }
        let after = labels(&s);
        assert!(
            before.iter().zip(&after).all(|(a, b)| a != b),
            "{before:?} {after:?}"
        );
        for &id in &ids {
            toggle_switch(&mut s, id);
        }
        assert_eq!(labels(&s), before);
    }

    #[test]
    fn corners_are_found_and_moved_with_what_is_attached() {
        // A resistor at the origin with a bent wire from its right pin: a
        // corner at (6, 0) and a free end at (6, 4), and a label on the corner.
        let mut s = Schematic::new();
        s.add_component(
            ComponentKind::Resistor { resistance: 1e3 },
            Point::new(0, 0),
        );
        s.add_wire(Point::new(2, 0), Point::new(6, 0));
        s.add_wire(Point::new(6, 0), Point::new(6, 4));
        s.add_label("N", Point::new(6, 0));

        // The wire end on the pin belongs to the part; the others can move.
        assert_eq!(
            draggable_vertices(&s),
            vec![Point::new(6, 0), Point::new(6, 4)]
        );
        assert_eq!(vertex_near(&s, pos2(6.2, 0.3), 0.5), Some(Point::new(6, 0)));
        assert_eq!(vertex_near(&s, pos2(2.0, 0.0), 0.5), None, "a pin");
        assert_eq!(vertex_near(&s, pos2(4.0, 0.0), 0.5), None, "along a wire");
        assert_eq!(vertex_near(&s, pos2(6.0, 2.0), 0.5), None, "mid-segment");

        // Moving the corner takes both wire ends and the label with it, and
        // leaves the pin end where it was.
        let joined = joined_at(&s, Point::new(6, 0));
        move_vertex(&mut s, &joined, Point::new(6, 2));
        assert_eq!(
            (s.wires[0].a, s.wires[0].b),
            (Point::new(2, 0), Point::new(6, 2))
        );
        assert_eq!(
            (s.wires[1].a, s.wires[1].b),
            (Point::new(6, 2), Point::new(6, 4))
        );
        assert_eq!(s.labels[0].position, Point::new(6, 2));
    }

    #[test]
    fn wire_presses_are_classified() {
        // A resistor at the origin: pins at (-2, 0) and (2, 0), body between.
        let mut s = Schematic::new();
        let id = s.add_component(
            ComponentKind::Resistor { resistance: 1e3 },
            Point::new(0, 0),
        );
        let press = |in_progress, x: f32, y: f32, s: &Schematic| {
            classify_wire_press(in_progress, s, pos2(x, y))
        };
        // Idle: the body grabs the part, including a click that snaps there.
        assert_eq!(press(false, 0.0, 0.0, &s), WirePress::Grab(id));
        assert_eq!(press(false, 0.9, 0.2, &s), WirePress::Grab(id));
        // A pin, a click that snaps to a pin, and empty canvas all wire.
        assert_eq!(press(false, 2.0, 0.0, &s), WirePress::Wire);
        assert_eq!(press(false, -1.8, 0.2, &s), WirePress::Wire);
        assert_eq!(press(false, 10.0, 10.0, &s), WirePress::Wire);
        // Mid-wire: a click inside the body is ignored; a pin still ends it.
        assert_eq!(press(true, 0.0, 0.0, &s), WirePress::Ignore);
        assert_eq!(press(true, 2.0, 0.0, &s), WirePress::Wire);
        assert_eq!(press(true, 10.0, 10.0, &s), WirePress::Wire);
        // A wire in front of the body is hit first, so it means wiring.
        s.add_wire(Point::new(-1, 0), Point::new(1, 0));
        assert_eq!(press(false, 0.0, 0.0, &s), WirePress::Wire);
        assert_eq!(press(true, 0.0, 0.0, &s), WirePress::Wire);
    }

    #[test]
    fn a_click_on_a_pin_is_found_even_with_a_wire_on_it() {
        let mut s = Schematic::new();
        s.add_component(
            ComponentKind::Resistor { resistance: 1e3 },
            Point::new(0, 0),
        );
        // On the pin, and near enough to snap to it.
        assert_eq!(pin_at(&s, pos2(2.0, 0.0)), Some(Point::new(2, 0)));
        assert_eq!(pin_at(&s, pos2(-1.8, 0.3)), Some(Point::new(-2, 0)));
        // The body, the lead's middle and empty canvas are not pins.
        assert_eq!(pin_at(&s, pos2(0.0, 0.0)), None);
        assert_eq!(pin_at(&s, pos2(1.0, 0.0)), None);
        assert_eq!(pin_at(&s, pos2(10.0, 10.0)), None);
        // A wire ending on the pin does not hide it.
        s.add_wire(Point::new(2, 0), Point::new(6, 0));
        assert_eq!(pin_at(&s, pos2(2.0, 0.0)), Some(Point::new(2, 0)));
    }

    #[test]
    fn dropping_a_part_places_selects_and_goes_on_placing() {
        let mut canvas = Canvas::default();
        let mut doc = Document::default();
        let resistor = ComponentKind::Resistor { resistance: 1e3 };
        canvas.drop_part(&mut doc, resistor.clone(), Point::new(4, 6));

        let s = doc.schematic();
        assert_eq!(s.components.len(), 1);
        let c = &s.components[0];
        assert_eq!((c.kind.clone(), c.position), (resistor, Point::new(4, 6)));
        assert_eq!(canvas.selection, BTreeSet::from([Item::Component(c.id)]));
        let placing = Tool::Place {
            kind: ComponentKind::Resistor { resistance: 1e3 },
            rotation: Rotation::R0,
            mirrored: false,
        };
        assert_eq!(canvas.tool, placing);
        assert!(canvas.wire_start.is_none());

        // One undo step removes it.
        assert!(doc.undo());
        assert!(doc.schematic().components.is_empty());

        // A part that needs a model brings it along.
        let diode = ComponentKind::Diode {
            model: crate::library::GENERIC_DIODE.into(),
        };
        canvas.drop_part(&mut doc, diode, Point::new(0, 0));
        assert!(
            doc.schematic()
                .models
                .get(crate::library::GENERIC_DIODE)
                .is_some()
        );
    }

    /// Press `key` on a canvas.
    fn press_key(canvas: &mut Canvas, doc: &mut Document, key: Key) {
        let input = egui::RawInput {
            events: vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
            ..Default::default()
        };
        let _ = egui::Context::default().run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| canvas.keyboard(ui, doc));
        });
    }

    #[test]
    fn while_placing_r_turns_the_next_part_and_del_removes_the_last() {
        let mut canvas = Canvas::default();
        let mut doc = Document::default();
        let kind = ComponentKind::Resistor { resistance: 1e3 };
        canvas.drop_part(&mut doc, kind.clone(), Point::new(0, 0));
        canvas.place(&mut doc, kind, Point::new(0, 10), Rotation::R0, false);
        let ids: Vec<ComponentId> = doc.schematic().components.iter().map(|c| c.id).collect();
        // The one just placed is the selected one.
        assert_eq!(canvas.selection, BTreeSet::from([Item::Component(ids[1])]));

        press_key(&mut canvas, &mut doc, Key::R);
        assert!(matches!(
            canvas.tool,
            Tool::Place { rotation, .. } if rotation == Rotation::R0.rotated_ccw()
        ));
        let turned = |doc: &Document| {
            doc.schematic()
                .components
                .iter()
                .any(|c| c.rotation != Rotation::R0)
        };
        assert!(!turned(&doc), "the placed parts stay as they are");

        press_key(&mut canvas, &mut doc, Key::Delete);
        assert_eq!(doc.schematic().components.len(), 1);
        assert_eq!(doc.schematic().components[0].id, ids[0]);
        assert!(matches!(canvas.tool, Tool::Place { .. }), "still placing");

        // Esc leaves.
        press_key(&mut canvas, &mut doc, Key::Escape);
        assert_eq!(canvas.tool, Tool::Select);
    }

    #[test]
    fn only_the_label_tool_carries_a_tag_clear_of_the_mouse_pointer() {
        let mut rig = Rig::new(Schematic::new());
        let mouse = pos2(300.0, 300.0);
        // Where the tag's writing is drawn, if it is.
        let tag = |rig: &mut Rig, tool: Tool| {
            rig.canvas.tool = tool;
            for _ in 0..3 {
                rig.frame(
                    egui::Modifiers::NONE,
                    vec![egui::Event::PointerMoved(mouse)],
                );
            }
            rig.drawn.iter().find_map(|c| match &c.shape {
                egui::Shape::Text(t) if t.galley.text() == "abc" => {
                    Some(Rect::from_min_size(t.pos, t.galley.size()))
                }
                _ => None,
            })
        };
        let writing = tag(&mut rig, Tool::Label).expect("the tag is drawn");
        // Right of the point and above it; the arrow hangs below the mouse.
        assert!(writing.left() > mouse.x, "{writing:?}");
        assert!(writing.bottom() < mouse.y, "{writing:?}");
        assert!(writing.left() - mouse.x < 60.0 && mouse.y - writing.top() < 60.0);
        for tool in [Tool::Select, Tool::Wire, Tool::Probe] {
            assert_eq!(tag(&mut rig, tool.clone()), None, "{tool:?}");
        }
    }

    const CTRL: egui::Modifiers = egui::Modifiers {
        alt: false,
        ctrl: true,
        shift: false,
        mac_cmd: false,
        command: true,
    };
    const ALT: egui::Modifiers = egui::Modifiers::ALT;
    const NO_KEY: egui::Modifiers = egui::Modifiers::NONE;

    fn resistor_tool() -> Tool {
        Tool::Place {
            kind: ComponentKind::Resistor { resistance: 1e3 },
            rotation: Rotation::R0.rotated_ccw(),
            mirrored: true,
        }
    }

    impl Rig {
        /// Where the grid point `x`, `y` is drawn.
        fn at(&self, x: i32, y: i32) -> Pos2 {
            self.canvas
                .camera
                .to_screen(Pos2::ZERO, to_pos(Point::new(x, y)))
        }

        fn hold(&mut self, keys: egui::Modifiers) {
            self.frame(keys, vec![]);
        }

        /// Move to `at` and click there with `keys` held.
        fn click_holding(&mut self, keys: egui::Modifiers, at: Pos2) {
            self.frame(keys, vec![egui::Event::PointerMoved(at)]);
            for pressed in [true, false] {
                self.press(keys, at, PointerButton::Primary, pressed);
            }
            // Long enough that the next click is not a double click.
            for _ in 0..10 {
                self.frame(keys, vec![]);
            }
        }

        fn press(&mut self, keys: egui::Modifiers, at: Pos2, button: PointerButton, pressed: bool) {
            self.frame(
                keys,
                vec![egui::Event::PointerButton {
                    pos: at,
                    button,
                    pressed,
                    modifiers: keys,
                }],
            );
        }
    }

    #[test]
    fn holding_ctrl_wires_and_letting_go_drops_an_unfinished_wire() {
        let mut rig = Rig::new(Schematic::new());
        rig.canvas.tool = resistor_tool();
        rig.hold(CTRL);
        assert_eq!(rig.canvas.tool, Tool::Wire);
        let (a, b) = (rig.at(4, 4), rig.at(10, 4));
        rig.click_holding(CTRL, a);
        rig.click_holding(CTRL, b);
        let wires = rig.doc.schematic().wires.len();
        assert!(wires > 0, "the first piece is drawn");
        assert!(rig.canvas.wire_start.is_some(), "and another is under way");
        assert!(rig.doc.schematic().components.is_empty());
        rig.hold(NO_KEY);
        // Just as it was, turned and mirrored.
        assert_eq!(rig.canvas.tool, resistor_tool());
        assert!(rig.canvas.wire_start.is_none());
        assert_eq!(rig.doc.schematic().wires.len(), wires);
    }

    #[test]
    fn holding_alt_selects_and_the_wire_carries_on_afterwards() {
        let mut s = Schematic::new();
        let id = s.add_component(
            ComponentKind::Resistor { resistance: 1e3 },
            Point::new(10, 10),
        );
        let mut rig = Rig::new(s);
        rig.canvas.tool = Tool::Wire;
        rig.canvas.wire_start = Some(Point::new(2, 2));
        // A click on a pin selects here; it does not start a wire.
        rig.hold(ALT);
        assert_eq!(rig.canvas.tool, Tool::Select);
        let pin = rig.at(12, 10);
        rig.click_holding(ALT, pin);
        assert_eq!(rig.canvas.tool, Tool::Select);
        assert_eq!(rig.canvas.selection, BTreeSet::from([Item::Component(id)]));

        // A drag still going when Alt is let go is finished first.
        let (from, to) = (rig.at(10, 10), rig.at(14, 16));
        rig.frame(ALT, vec![egui::Event::PointerMoved(from)]);
        rig.press(ALT, from, PointerButton::Primary, true);
        for i in 1..=10 {
            let keys = if i < 5 { ALT } else { NO_KEY };
            let at = from + (to - from) * (i as f32 / 10.0);
            rig.frame(keys, vec![egui::Event::PointerMoved(at)]);
            assert_eq!(rig.canvas.tool, Tool::Select, "step {i}");
        }
        rig.press(NO_KEY, to, PointerButton::Primary, false);
        rig.settle();
        let moved = rig.doc.schematic().component(id).unwrap().position;
        assert_eq!(moved, Point::new(14, 16));
        assert_eq!(rig.canvas.tool, Tool::Wire);
        assert_eq!(rig.canvas.wire_start, Some(Point::new(2, 2)));
        assert!(rig.doc.schematic().wires.is_empty());
    }

    #[test]
    fn a_borrowed_wire_tool_stays_a_wire_tool() {
        let mut s = Schematic::new();
        s.add_component(
            ComponentKind::Resistor { resistance: 1e3 },
            Point::new(10, 10),
        );
        let mut rig = Rig::new(s);
        rig.canvas.tool = Tool::Label;
        // Neither a part's body nor a right-click leaves it.
        let body = rig.at(10, 10);
        rig.click_holding(CTRL, body);
        assert_eq!(rig.canvas.tool, Tool::Wire);
        for pressed in [true, false] {
            rig.press(CTRL, body, PointerButton::Secondary, pressed);
        }
        rig.hold(CTRL);
        assert_eq!(rig.canvas.tool, Tool::Wire);
        rig.hold(NO_KEY);
        assert_eq!(rig.canvas.tool, Tool::Label);
    }

    #[test]
    fn which_keys_borrow_a_tool() {
        let mut canvas = Canvas {
            tool: Tool::Probe,
            ..Canvas::default()
        };
        let both = egui::Modifiers { alt: true, ..CTRL };
        // Both keys, as AltGr sends, and Shift ask for nothing.
        for keys in [NO_KEY, both, egui::Modifiers::SHIFT] {
            assert!(!canvas.hold_tool(keys, true));
            assert_eq!(canvas.tool, Tool::Probe);
        }
        // From one key to the other, and then both.
        for (keys, tool) in [(CTRL, Tool::Wire), (ALT, Tool::Select), (both, Tool::Probe)] {
            assert!(canvas.hold_tool(keys, true));
            assert_eq!(canvas.tool, tool);
        }
        // Leaving the window lets go, whatever the keys seem to be.
        canvas.hold_tool(ALT, true);
        canvas.hold_tool(ALT, false);
        assert_eq!(canvas.tool, Tool::Probe);
        // A tool chosen while a key is down is kept.
        canvas.hold_tool(CTRL, true);
        canvas.tool = Tool::Label;
        canvas.hold_tool(NO_KEY, true);
        assert_eq!(canvas.tool, Tool::Label);
        // Already the tool the key gives: nothing to go back to.
        canvas.tool = Tool::Wire;
        assert!(!canvas.hold_tool(CTRL, true));
        assert!(!canvas.hold_tool(NO_KEY, true));
        assert_eq!(canvas.tool, Tool::Wire);
    }

    /// Drag from one screen point to another one grid step at a time, so
    /// that everything on the way is passed over.
    fn drag_slowly(rig: &mut Rig, from: Point, to: Point) {
        let (a, b) = (rig.at(from.x, from.y), rig.at(to.x, to.y));
        let steps = (to.x - from.x).abs().max((to.y - from.y).abs()) * 2;
        rig.frame(NO_KEY, vec![egui::Event::PointerMoved(a)]);
        rig.press(NO_KEY, a, PointerButton::Primary, true);
        for i in 1..=steps {
            let at = a + (b - a) * (i as f32 / steps as f32);
            rig.frame(NO_KEY, vec![egui::Event::PointerMoved(at)]);
        }
        rig.press(NO_KEY, b, PointerButton::Primary, false);
        rig.settle();
    }

    #[test]
    fn a_dragged_part_does_not_pick_up_wires_it_passes() {
        let mut s = Schematic::new();
        let id = s.add_component(
            ComponentKind::Resistor { resistance: 1e3 },
            Point::new(6, 6),
        );
        // One wire joined to its left pin, and two it will pass over: its
        // pins cross their ends on the way.
        s.add_wire(Point::new(4, 6), Point::new(4, 2));
        s.add_wire(Point::new(14, 6), Point::new(14, 2));
        s.add_wire(Point::new(18, 6), Point::new(22, 6));
        let mut rig = Rig::new(s);
        drag_slowly(&mut rig, Point::new(6, 6), Point::new(26, 6));
        let s = rig.doc.schematic();
        assert_eq!(s.component(id).unwrap().position, Point::new(26, 6));
        let ends: Vec<(Point, Point)> = s.wires.iter().map(|w| (w.a, w.b)).collect();
        assert_eq!(
            ends,
            [
                (Point::new(24, 6), Point::new(4, 2)),
                (Point::new(14, 6), Point::new(14, 2)),
                (Point::new(18, 6), Point::new(22, 6)),
            ]
        );
        // One undo step puts it all back.
        assert!(rig.doc.undo());
        assert_eq!(rig.doc.schematic().wires[0].a, Point::new(4, 6));
    }

    #[test]
    fn a_dragged_wire_end_does_not_pick_up_what_it_passes() {
        let mut s = Schematic::new();
        s.add_wire(Point::new(4, 4), Point::new(4, 8));
        s.add_wire(Point::new(10, 4), Point::new(10, 8));
        s.add_label("OTHER", Point::new(10, 4));
        let mut rig = Rig::new(s);
        drag_slowly(&mut rig, Point::new(4, 4), Point::new(16, 4));
        let s = rig.doc.schematic();
        assert_eq!(s.wires[0].a, Point::new(16, 4));
        assert_eq!(s.wires[1].a, Point::new(10, 4));
        assert_eq!(s.labels[0].position, Point::new(10, 4));
    }

    #[test]
    fn a_circuit_is_first_shown_in_the_middle_of_the_view() {
        // Far from where an uncentred view would look, and large or small.
        for far in [Point::new(300, 200), Point::new(-150, -90)] {
            let mut s = Schematic::new();
            s.add_component(ComponentKind::Resistor { resistance: 1e3 }, far);
            s.add_wire(far + Point::new(2, 0), far + Point::new(12, 8));
            let drawing = drawing_bounds(&s).unwrap();
            let mut rig = Rig::new(s);
            let view = Rig::WINDOW - Vec2::splat(SCROLLBAR);
            let at = rig.canvas.camera.to_screen(Pos2::ZERO, drawing.center());
            assert!(at.distance((view / 2.0).to_pos2()) <= 1.0, "{at:?}");
            assert_eq!(rig.canvas.camera.scale, Camera::default().scale);
            // From then on the view stays where the user takes it.
            rig.wheel(-2.0, egui::Modifiers::NONE);
            let scrolled = rig.canvas.camera.offset;
            rig.settle();
            assert_eq!(rig.canvas.camera.offset, scrolled);
        }
    }

    #[test]
    fn an_empty_circuit_keeps_its_view_when_the_first_part_arrives() {
        let mut rig = Rig::new(Schematic::new());
        let start = rig.canvas.camera.offset;
        assert_eq!(start, Camera::default().offset);
        rig.canvas.place(
            &mut rig.doc,
            ComponentKind::Resistor { resistance: 1e3 },
            Point::new(30, 20),
            Rotation::R0,
            false,
        );
        rig.settle();
        assert_eq!(rig.canvas.camera.offset, start);
    }

    #[test]
    fn an_led_glows_with_its_current_and_warns_of_too_much() {
        let mut s = Schematic::new();
        let id = s.add_component(
            ComponentKind::Led {
                colour: dunbrec_core::LedColour::Green,
            },
            Point::new(10, 10),
        );
        let mut rig = Rig::new(s);
        let green = leds::light(dunbrec_core::LedColour::Green);
        // How solidly its triangle is filled, how many glows surround it, and
        // the warning if there is one.
        let seen = |rig: &mut Rig, current: Option<f64>| {
            rig.lights.leds = current.into_iter().map(|i| (id, vec![i])).collect();
            rig.hold(NO_KEY);
            let greenish = |c: Color32| c.a() > 0 && c.g() > c.r() && c.g() > c.b();
            let mut fill = None;
            let mut glows = 0;
            let mut warning = None;
            for shape in &rig.drawn {
                match &shape.shape {
                    egui::Shape::Path(p) if p.points.len() == 3 && greenish(p.fill) => {
                        fill = Some(p.fill.a());
                    }
                    egui::Shape::Circle(c) if greenish(c.fill) && c.fill.a() < green.a() => {
                        glows += usize::from(c.radius > rig.canvas.camera.scale);
                    }
                    egui::Shape::Text(t) if t.galley.text().contains("too much") => {
                        warning = Some(t.galley.text().to_string());
                    }
                    _ => {}
                }
            }
            (fill.expect("the triangle is filled"), glows, warning)
        };
        let (off, glows, warning) = seen(&mut rig, None);
        assert_eq!((glows, warning), (0, None));
        let (faint, glows, _) = seen(&mut rig, Some(0.001));
        assert_eq!(glows, 2);
        let (full, glows, warning) = seen(&mut rig, Some(0.020));
        assert_eq!((glows, warning), (2, None));
        assert!(off < faint && faint < full, "{off} {faint} {full}");
        let (.., warning) = seen(&mut rig, Some(0.045));
        assert_eq!(warning.as_deref(), Some("⚠ 45 mA - too much"));
    }

    #[test]
    fn a_bar_display_lights_each_bar_by_itself_and_names_the_worst() {
        let mut s = Schematic::new();
        let colour = dunbrec_core::LedColour::Green;
        let id = s.add_component(ComponentKind::LedBar { colour }, Point::new(10, 10));
        let mut rig = Rig::new(s);
        let green = leds::light(colour);
        // How solidly each bar is filled, top first, how many glows there
        // are, and the warning if there is one.
        let seen = |rig: &mut Rig, currents: Vec<f64>| {
            rig.lights.leds = [(id, currents)].into_iter().collect();
            rig.hold(NO_KEY);
            let greenish = |c: Color32| c.a() > 0 && c.g() > c.r() && c.g() > c.b();
            let mut blocks = Vec::new();
            let mut warning = None;
            for shape in &rig.drawn {
                match &shape.shape {
                    egui::Shape::Path(p) if p.points.len() == 4 && greenish(p.fill) => {
                        blocks.push(p.fill.a());
                    }
                    egui::Shape::Text(t) if t.galley.text().contains("too much") => {
                        warning = Some(t.galley.text().to_string());
                    }
                    _ => {}
                }
            }
            // The glows are drawn first, then the ten bars.
            let glows = blocks.len() - 10;
            (blocks.split_off(glows), glows, warning)
        };
        let (off, glows, warning) = seen(&mut rig, Vec::new());
        assert_eq!((glows, warning), (0, None));
        assert!(off.iter().all(|&a| a < green.a() / 2), "{off:?}");

        let mut currents = vec![0.0; 10];
        (currents[1], currents[6]) = (0.002, 0.020);
        let (bars, glows, warning) = seen(&mut rig, currents.clone());
        assert_eq!((glows, warning), (2, None));
        assert!(off[0] == bars[0] && bars[0] < bars[1] && bars[1] < bars[6]);

        (currents[2], currents[8]) = (0.035, 0.050);
        let (.., warning) = seen(&mut rig, currents);
        assert_eq!(warning.as_deref(), Some("⚠ LED 9: 50 mA - too much"));
    }

    #[test]
    fn a_digit_lights_by_segment_and_reads_the_same_when_mirrored() {
        use dunbrec_core::{CommonPin, LedColour};
        let mut s = Schematic::new();
        let (colour, common) = (LedColour::Green, CommonPin::Cathode);
        let kind = ComponentKind::SevenSegment { colour, common };
        let id = s.add_component(kind, Point::new(10, 10));
        let mut rig = Rig::new(s);
        let green = leds::light(colour);
        // Where the brightly lit shapes are, and the warning if there is one.
        let seen = |rig: &mut Rig, currents: Vec<f64>| {
            rig.lights.leds = [(id, currents)].into_iter().collect();
            rig.hold(NO_KEY);
            let mut lit = Vec::new();
            let mut warning = None;
            for shape in &rig.drawn {
                match &shape.shape {
                    egui::Shape::Path(p) if p.fill == green => {
                        lit.push(p.visual_bounding_rect().center());
                    }
                    egui::Shape::Text(t) if t.galley.text().contains("too much") => {
                        warning = Some(t.galley.text().to_string());
                    }
                    _ => {}
                }
            }
            (lit, warning)
        };
        assert_eq!(seen(&mut rig, Vec::new()), (Vec::new(), None));
        // Segment b, upper right, and the decimal point, low and further right.
        let mut currents = vec![0.0; 8];
        (currents[1], currents[7]) = (0.020, 0.020);
        let (lit, warning) = seen(&mut rig, currents.clone());
        assert_eq!(warning, None);
        let [b, dp] = lit[..] else {
            panic!("{lit:?}");
        };
        assert!(b.x < dp.x && b.y < dp.y, "{b} {dp}");

        rig.doc
            .edit(|s| s.component_mut(id).unwrap().mirrored = true);
        let (mirrored, _) = seen(&mut rig, currents.clone());
        assert!((mirrored[0] - b).length() < 0.5 && (mirrored[1] - dp).length() < 0.5);

        currents[6] = 0.045;
        let (_, warning) = seen(&mut rig, currents);
        assert_eq!(warning.as_deref(), Some("⚠ segment g: 45 mA - too much"));
    }

    #[test]
    fn four_digits_stand_in_a_row_and_the_warning_says_which() {
        use dunbrec_core::{CommonPin, LedColour};
        let mut s = Schematic::new();
        let (colour, common) = (LedColour::Green, CommonPin::Anode);
        let kind = ComponentKind::FourDigit { colour, common };
        let id = s.add_component(kind, Point::new(20, 12));
        let mut rig = Rig::new(s);
        let green = leds::light(colour);
        // Segment a of each digit, fully lit, and too much through one.
        let mut currents = vec![0.0; 32];
        for digit in 0..4 {
            currents[digit * 8] = 0.020;
        }
        currents[2 * 8 + 6] = 0.045;
        rig.lights.leds = [(id, currents)].into_iter().collect();
        rig.hold(NO_KEY);
        let (mut lit, mut warning) = (Vec::new(), None);
        for shape in &rig.drawn {
            match &shape.shape {
                egui::Shape::Path(p) if p.fill == green => {
                    lit.push(p.visual_bounding_rect().center());
                }
                egui::Shape::Text(t) if t.galley.text().contains("too much") => {
                    warning = Some(t.galley.text().to_string());
                }
                _ => {}
            }
        }
        // Four top segments from left to right, then digit 3's middle one.
        assert_eq!(lit.len(), 5, "{lit:?}");
        let tops = [lit[0], lit[1], lit[2], lit[4]];
        assert!(tops.windows(2).all(|w| w[0].x < w[1].x && w[0].y == w[1].y));
        assert!(lit[3].y > tops[2].y && (lit[3].x - tops[2].x).abs() < 0.5);
        assert_eq!(
            warning.as_deref(),
            Some("⚠ digit 3, segment g: 45 mA - too much")
        );
    }

    #[test]
    fn right_click_rotates_the_part_or_the_selection_it_belongs_to() {
        let mut s = voltage_divider();
        let ids: Vec<ComponentId> = s.components.iter().map(|c| c.id).collect();
        let (a, b) = (ids[0], ids[1]);
        let turns = |s: &Schematic, id| s.component(id).unwrap().rotation;
        let before = (turns(&s, a), turns(&s, b));

        // Not selected: only that part turns, one quarter-turn.
        let items = rotation_targets(&BTreeSet::new(), a);
        transform_components(&mut s, &items, false);
        assert_eq!(turns(&s, a), before.0.rotated_ccw());
        assert_eq!(turns(&s, b), before.1);

        // In a selection: the whole selection turns.
        let selection = BTreeSet::from([Item::Component(a), Item::Component(b)]);
        let items = rotation_targets(&selection, a);
        transform_components(&mut s, &items, false);
        assert_eq!(turns(&s, a), before.0.rotated_ccw().rotated_ccw());
        assert_eq!(turns(&s, b), before.1.rotated_ccw());

        // Selected elsewhere: the part under the pointer turns alone.
        let items = rotation_targets(&BTreeSet::from([Item::Component(b)]), a);
        assert_eq!(items, BTreeSet::from([Item::Component(a)]));
    }

    #[test]
    fn focusing_a_label_finds_its_whole_net() {
        // In the op-amp example "+15V" names a stub on the op-amp's V+ pin
        // and a separate stub on the supply: both light up, nothing else.
        let s = crate::examples::opamp_inverting();
        let netlist = dunbrec_core::build_netlist(&s);
        let label = s.labels.iter().find(|l| l.name == "+15V").unwrap();
        let focus = focused_label_names(&s, Some(Item::Label(label.id)), &BTreeSet::new());
        assert_eq!(focus, HashSet::from(["+15V"]));
        let wires = wires_of_labelled_nets(&s, &netlist, &focus);
        let ends: HashSet<(Point, Point)> = wires
            .iter()
            .map(|&i| (s.wires[i].a, s.wires[i].b))
            .collect();
        assert_eq!(
            ends,
            HashSet::from([
                (Point::new(0, -3), Point::new(0, -4)),
                (Point::new(12, -8), Point::new(12, -10)),
            ])
        );
        // Nothing focused, nothing lit.
        let none = focused_label_names(&s, None, &BTreeSet::new());
        assert!(wires_of_labelled_nets(&s, &netlist, &none).is_empty());
    }

    #[test]
    fn camera_round_trip_and_zoom_anchor() {
        let mut cam = Camera::default();
        let origin = pos2(10.0, 20.0);
        let g = pos2(3.5, -2.0);
        let s = cam.to_screen(origin, g);
        assert!(cam.to_grid(origin, s).distance(g) < 1e-4);
        cam.zoom_at(origin, s, 2.0);
        assert!(
            cam.to_grid(origin, s).distance(g) < 1e-3,
            "anchor must stay put"
        );
        cam.zoom_at(origin, s, 1e6);
        assert_eq!(cam.scale, Camera::MAX_SCALE);
    }

    #[test]
    fn routes_are_orthogonal() {
        let r = route(Point::new(0, 0), Point::new(3, 4));
        assert_eq!(
            r,
            vec![
                (Point::new(0, 0), Point::new(3, 0)),
                (Point::new(3, 0), Point::new(3, 4))
            ]
        );
        assert_eq!(route(Point::new(0, 0), Point::new(0, 4)).len(), 1);
        assert!(route(Point::new(1, 1), Point::new(1, 1)).is_empty());
    }

    #[test]
    fn divider_junctions_and_hits() {
        let s = voltage_divider();
        // Ground on the bottom wire and the label are T-junctions; the
        // bottom-right corner joins only a wire end and a pin (no dot).
        let dots = junctions(&s);
        assert!(dots.contains(&Point::new(4, 2)), "{dots:?}");
        assert!(!dots.contains(&Point::new(8, 2)), "{dots:?}");

        let r1 = s.components[1].id;
        assert_eq!(hit_test(&s, pos2(4.0, -2.0)), Some(Item::Component(r1)));
        assert!(matches!(hit_test(&s, pos2(1.0, -2.0)), Some(Item::Wire(_))));
        assert_eq!(hit_test(&s, pos2(40.0, 40.0)), None);
    }

    #[test]
    fn moving_a_component_drags_attached_wire_ends() {
        let mut s = voltage_divider();
        let r1 = s.components[1].id;
        let items: BTreeSet<Item> = [Item::Component(r1)].into();
        let joined = joined_to(&s, &items);
        move_items(&mut s, &items, &joined, Point::new(0, -3));
        // Wire from V1 (0,-2) to R1's left pin (2,-2) now ends at (2,-5).
        assert!(
            s.wires
                .iter()
                .any(|w| w.a == Point::new(0, -2) && w.b == Point::new(2, -5))
        );
        // Connectivity is unchanged, so the netlist is still clean.
        assert!(dunbrec_core::build_netlist(&s).diagnostics.is_empty());
    }

    #[test]
    fn node_tags_sit_on_wires() {
        let s = voltage_divider();
        let netlist = dunbrec_core::build_netlist(&s);
        let tags = node_tag_positions(&s, &netlist);
        // Two non-ground nodes: the supply node and VOUT.
        assert_eq!(tags.len(), 2);
        let vout = netlist.node_by_name("VOUT").unwrap();
        let tag = tags.iter().find(|t| t.node == vout).unwrap();
        // VOUT's only wire runs (6,-2)–(8,-2): its midpoint is (7,-2). The
        // VOUT label sits on that wire, so the tag moves below it.
        assert_eq!(tag.at, pos2(7.0, -2.0));
        assert!(!tag.vertical);
        assert!(tag.flip);
        let other = tags.iter().find(|t| t.node != vout).unwrap();
        assert!(!other.flip);
    }

    #[test]
    fn node_map_finds_nodes_anywhere_on_a_net() {
        let s = voltage_divider();
        let netlist = dunbrec_core::build_netlist(&s);
        let map = NodeMap::new(&s, &netlist);
        let vout = netlist.node_by_name("VOUT").unwrap();
        // R1's right pin, the wire end at R2, and a point along the wire.
        for p in [Point::new(6, -2), Point::new(8, -2), Point::new(7, -2)] {
            assert_eq!(map.node_at(&s, &netlist, p), Some(vout), "{p:?}");
        }
        // Along the bottom wire: ground.
        assert_eq!(
            map.node_at(&s, &netlist, Point::new(2, 2)),
            Some(NodeId::GROUND)
        );
        assert_eq!(map.node_at(&s, &netlist, Point::new(30, 30)), None);
    }

    #[test]
    fn copy_and_paste_renumbers_and_keeps_wiring() {
        let mut s = voltage_divider();
        let all: BTreeSet<Item> =
            items_in_rect(&s, Rect::from_min_max(pos2(-5.0, -10.0), pos2(20.0, 10.0)))
                .into_iter()
                .collect();
        let clip = copy_items(&s, &all).unwrap();
        let pasted = paste(&mut s, &clip, Point::new(0, 20));
        assert_eq!(pasted.len(), all.len());
        let n = dunbrec_core::build_netlist(&s);
        assert!(n.diagnostics.is_empty(), "{:?}", n.diagnostics);
        assert_eq!(n.elements.len(), 6);
        let mut names: Vec<&str> = n.elements.iter().map(|e| e.designator.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["R1", "R2", "R3", "R4", "V1", "V2"]);
        assert!(copy_items(&s, &BTreeSet::new()).is_none());
    }

    #[test]
    fn rubber_band_and_delete() {
        let mut s = voltage_divider();
        let all = items_in_rect(&s, Rect::from_min_max(pos2(-5.0, -10.0), pos2(20.0, 10.0)));
        assert_eq!(
            all.len(),
            s.components.len() + s.wires.len() + s.labels.len()
        );
        let set: BTreeSet<Item> = all.into_iter().collect();
        delete_items(&mut s, &set);
        assert!(s.components.is_empty() && s.wires.is_empty() && s.labels.is_empty());
    }
}
