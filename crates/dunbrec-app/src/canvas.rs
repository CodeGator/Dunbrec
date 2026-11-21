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

enum Drag {
    /// Moving the selection; `last` is the grid point of the previous frame.
    Move { last: Point, started: bool },
    /// Rubber-band selection from a screen position.
    Rubber { start: Pos2 },
    /// Dragging a wire corner or free end, which is now at `at`.
    Vertex { at: Point, started: bool },
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
    let unused = netlist.unused_io_nets();
    let mut out: Vec<TagPlace> = best
        .into_values()
        .map(|(_, place)| place)
        .filter(|place| !place.node.is_ground() && !unused.contains(&place.node))
        .collect();
    out.sort_by_key(|place| place.node);
    out
}

pub struct Canvas {
    pub camera: Camera,
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
}

impl Default for Canvas {
    fn default() -> Self {
        Self {
            camera: Camera::default(),
            tool: Tool::Select,
            selection: BTreeSet::new(),
            wire_start: None,
            drag: None,
            label_request: None,
            probe_request: None,
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

/// Move the selected items by `d`. Unselected wire ends attached to moved
/// pins, wire ends or labels follow, so connections are kept.
pub fn move_items(s: &mut Schematic, items: &BTreeSet<Item>, d: Point) {
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
            if moving.contains(&w.a) {
                w.a = w.a + d;
            }
            if moving.contains(&w.b) {
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
    s.components
        .iter()
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

/// Move the point `from` to `to`: every wire end there, and every net label
/// there, so what was connected stays connected.
fn move_vertex(s: &mut Schematic, from: Point, to: Point) {
    for w in &mut s.wires {
        if w.a == from {
            w.a = to;
        }
        if w.b == from {
            w.b = to;
        }
    }
    for l in &mut s.labels {
        if l.position == from {
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
        ComponentKind::Ground => String::new(),
        ComponentKind::Resistor { resistance } => format_value(*resistance, "Ω"),
        ComponentKind::Capacitor { capacitance, .. } => format_value(*capacitance, "F"),
        ComponentKind::Inductor { inductance, .. } => format_value(*inductance, "H"),
        ComponentKind::VoltageSource { waveform } => wave(waveform, "V"),
        ComponentKind::CurrentSource { waveform } => wave(waveform, "A"),
        ComponentKind::Diode { model }
        | ComponentKind::Bjt { model, .. }
        | ComponentKind::Mosfet { model, .. } => model.clone(),
        ComponentKind::Switch { closed, .. } => (if *closed { "closed" } else { "open" }).into(),
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
        ComponentKind::Mcu { firmware, .. } => firmware_label(firmware),
        ComponentKind::OpAmp { part } => part.clone(),
        ComponentKind::Timer555 => "NE555".into(),
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

// ------------------------------------------------------- interaction ---

impl Canvas {
    pub fn cancel(&mut self) {
        self.wire_start = None;
        self.drag = None;
        self.tool = Tool::Select;
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

    /// Show the canvas, handle input, and edit `doc`.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        doc: &mut Document,
        netlist: &Netlist,
        annotations: &[Annotation],
        markers: &[Marker],
        lit: &std::collections::HashSet<ComponentId>,
    ) {
        let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let rect = response.rect;
        let origin = rect.min;
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

        // ---- navigation: zoom with the wheel, pan with middle drag ----
        if response.hovered() {
            let (scroll, zoom) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
            if let Some(p) = pointer {
                let factor = zoom * (scroll.y * 0.0025).exp();
                if (factor - 1.0).abs() > 1e-4 {
                    self.camera.zoom_at(origin, p, factor);
                }
            }
        }
        if response.dragged_by(PointerButton::Middle) {
            self.camera.offset += response.drag_delta();
        }

        // ---- keyboard ----
        if !ui.ctx().wants_keyboard_input() {
            self.keyboard(ui, doc);
        }

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
                    doc.edit(|s| add_part(s, kind.clone(), at, rotation, mirrored));
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
                    Some(WirePress::Grab(_)) => {
                        // Carry the press into the Select tool: it selects
                        // the part, and a drag moves it.
                        self.tool = Tool::Select;
                        self.select_tool(&response, doc, origin, grid_pointer, snapped);
                    }
                    Some(WirePress::Ignore) => {}
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

        // Dropped from the palette: place it and start wiring.
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

        for w in &s.wires {
            let color = if self.selection.contains(&Item::Wire(w.id)) {
                colors.selected
            } else {
                colors.wire
            };
            painter.line_segment(
                [p.screen(to_pos(w.a)), p.screen(to_pos(w.b))],
                Stroke::new(width, color),
            );
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
            if matches!(c.kind, ComponentKind::LogicLed) && lit.contains(&c.id) {
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
                    painter.circle_stroke(
                        p.screen(to_pos(at)),
                        4.0,
                        Stroke::new(1.0_f32, colors.label),
                    );
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
            return;
        }
        if self.selection.is_empty() {
            return;
        }
        if rotate || mirror {
            let items = self.selection.clone();
            doc.edit(|s| transform_components(s, &items, mirror));
        }
        if delete {
            let items = std::mem::take(&mut self.selection);
            doc.edit(|s| delete_items(s, &items));
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
            self.drag = Some(Drag::Vertex { at, started: false });
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
                        started: false,
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
            && let (Some(Drag::Move { last, started }), Some(now)) = (&mut self.drag, snapped)
        {
            let d = now - *last;
            if d != Point::default() {
                if !*started {
                    doc.checkpoint();
                    *started = true;
                }
                move_items(doc.schematic_mut(), &self.selection, d);
                *last = now;
            }
        }
        if response.dragged_by(PointerButton::Primary)
            && let (Some(Drag::Vertex { at, started }), Some(now)) = (&mut self.drag, snapped)
            && now != *at
        {
            if !*started {
                doc.checkpoint();
                *started = true;
            }
            move_vertex(doc.schematic_mut(), *at, now);
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
            let pin = if shift {
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
                Some(ComponentKind::Switch { .. } | ComponentKind::LogicSwitch { .. })
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

    /// A part dropped from the palette at `at`: place it, keep it selected
    /// (so R, X and the properties panel apply to it) and start wiring.
    fn drop_part(&mut self, doc: &mut Document, kind: ComponentKind, at: Point) {
        let id = doc.edit(|s| add_part(s, kind, at, Rotation::R0, false));
        self.selection = BTreeSet::from([Item::Component(id)]);
        self.wire_start = None;
        self.tool = Tool::Wire;
    }

    fn wire_tool(&mut self, response: &egui::Response, doc: &mut Document, snapped: Option<Point>) {
        if response.clicked_by(PointerButton::Secondary) {
            if self.wire_start.is_some() {
                self.wire_start = None;
            } else {
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
        move_vertex(&mut s, Point::new(6, 0), Point::new(6, 2));
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
    fn dropping_a_part_places_selects_and_starts_wiring() {
        let mut canvas = Canvas::default();
        let mut doc = Document::default();
        let resistor = ComponentKind::Resistor { resistance: 1e3 };
        canvas.drop_part(&mut doc, resistor.clone(), Point::new(4, 6));

        let s = doc.schematic();
        assert_eq!(s.components.len(), 1);
        let c = &s.components[0];
        assert_eq!((c.kind.clone(), c.position), (resistor, Point::new(4, 6)));
        assert_eq!(canvas.selection, BTreeSet::from([Item::Component(c.id)]));
        assert_eq!(canvas.tool, Tool::Wire);
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
        move_items(&mut s, &items, Point::new(0, -3));
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
