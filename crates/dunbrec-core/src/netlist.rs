//! Turns a graphical [`Schematic`] into a [`Netlist`]: a list of elements and
//! the numbered nodes their pins connect to. This is the only view of the
//! circuit the simulator sees.
//!
//! Connectivity rules:
//! * a wire connects its two endpoints;
//! * pins, wire endpoints and labels connect to anything at the same point;
//! * a pin, wire endpoint or label on the interior of a wire joins that wire
//!   (T-junction); wires that only cross are not connected;
//! * labels with the same name are connected;
//! * every ground symbol pin is node 0.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::models::ModelKind;
use crate::schematic::{ComponentId, ComponentKind, Point, Polarity, Schematic};

/// Index of a node. Node 0 is always ground.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId(pub usize);

impl NodeId {
    pub const GROUND: NodeId = NodeId(0);

    pub fn is_ground(self) -> bool {
        self.0 == 0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    /// Net name: "0" for ground, the label name if labelled, else "N001" etc.
    pub name: String,
    /// True if the name came from a net label.
    pub labelled: bool,
}

/// One simulated element. Ground symbols do not produce elements.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Element {
    pub component: ComponentId,
    pub designator: String,
    pub kind: ComponentKind,
    /// Node of each pin, in [`ComponentKind::pins`] order.
    pub nodes: Vec<NodeId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Severity {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Diagnostic {
    /// The circuit has no ground reference.
    NoGround,
    /// A pin connects to nothing else.
    UnconnectedPin { component: ComponentId, pin: String },
    /// One net carries several different label names; the first is used.
    ConflictingLabels { names: Vec<String> },
    /// A component refers to a model that is not in the library.
    MissingModel {
        component: ComponentId,
        model: String,
    },
    /// A component refers to a model of the wrong type.
    WrongModelKind {
        component: ComponentId,
        model: String,
        expected: ModelKind,
        found: ModelKind,
    },
    /// Two components share a designator.
    DuplicateDesignator {
        designator: String,
        components: Vec<ComponentId>,
    },
}

impl Diagnostic {
    pub fn severity(&self) -> Severity {
        match self {
            Diagnostic::NoGround
            | Diagnostic::MissingModel { .. }
            | Diagnostic::WrongModelKind { .. } => Severity::Error,
            Diagnostic::UnconnectedPin { .. }
            | Diagnostic::ConflictingLabels { .. }
            | Diagnostic::DuplicateDesignator { .. } => Severity::Warning,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Netlist {
    /// All nodes; index 0 is ground.
    pub nodes: Vec<Node>,
    pub elements: Vec<Element>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Netlist {
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity() == Severity::Error)
    }

    pub fn node_by_name(&self, name: &str) -> Option<NodeId> {
        self.nodes.iter().position(|n| n.name == name).map(NodeId)
    }

    pub fn element(&self, designator: &str) -> Option<&Element> {
        self.elements.iter().find(|e| e.designator == designator)
    }
}

/// Union-find over connection points.
struct Dsu {
    parent: Vec<usize>,
}

impl Dsu {
    fn new() -> Self {
        Self { parent: Vec::new() }
    }
    fn make(&mut self) -> usize {
        self.parent.push(self.parent.len());
        self.parent.len() - 1
    }
    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[rb] = ra;
        }
    }
}

pub fn build_netlist(schematic: &Schematic) -> Netlist {
    let mut dsu = Dsu::new();
    // One DSU entry per distinct point that matters.
    let mut point_ids: HashMap<Point, usize> = HashMap::new();
    let mut id_of = |dsu: &mut Dsu, p: Point| *point_ids.entry(p).or_insert_with(|| dsu.make());

    // Wires join their endpoints.
    for w in &schematic.wires {
        let (a, b) = (id_of(&mut dsu, w.a), id_of(&mut dsu, w.b));
        dsu.union(a, b);
    }

    // Pins: remember the DSU entry of every pin.
    let mut pin_entries: Vec<Vec<usize>> = Vec::with_capacity(schematic.components.len());
    for c in &schematic.components {
        pin_entries.push(c.pin_positions().map(|(_, p)| id_of(&mut dsu, p)).collect());
    }

    // Labels.
    let label_entries: Vec<usize> = schematic
        .labels
        .iter()
        .map(|l| id_of(&mut dsu, l.position))
        .collect();

    // T-junctions: any point of interest on a wire's interior joins the wire.
    let points: Vec<(Point, usize)> = point_ids.iter().map(|(p, i)| (*p, *i)).collect();
    for w in &schematic.wires {
        let wire_entry = point_ids[&w.a];
        for &(p, entry) in &points {
            if w.contains_interior(p) {
                dsu.union(wire_entry, entry);
            }
        }
    }

    // Same-named labels.
    let mut first_label: HashMap<&str, usize> = HashMap::new();
    for (label, &entry) in schematic.labels.iter().zip(&label_entries) {
        match first_label.get(label.name.as_str()) {
            Some(&other) => dsu.union(other, entry),
            None => {
                first_label.insert(&label.name, entry);
            }
        }
    }

    // Ground pins all join together.
    let ground_root = {
        let mut ground_entries = schematic
            .components
            .iter()
            .zip(&pin_entries)
            .filter(|(c, _)| matches!(c.kind, ComponentKind::Ground))
            .flat_map(|(_, e)| e.iter().copied());
        let first = ground_entries.next();
        if let Some(first) = first {
            for e in ground_entries {
                dsu.union(first, e);
            }
        }
        first.map(|f| dsu.find(f))
    };

    let mut diagnostics = Vec::new();
    if ground_root.is_none() {
        diagnostics.push(Diagnostic::NoGround);
    }

    // Labels per net root, sorted for determinism.
    let mut labels_by_root: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for (label, &entry) in schematic.labels.iter().zip(&label_entries) {
        let names = labels_by_root.entry(dsu.find(entry)).or_default();
        if !names.contains(&label.name) {
            names.push(label.name.clone());
        }
    }
    for names in labels_by_root.values_mut() {
        names.sort();
        if names.len() > 1 {
            diagnostics.push(Diagnostic::ConflictingLabels {
                names: names.clone(),
            });
        }
    }

    // Assign node numbers in order of first appearance among component pins.
    let mut nodes = vec![Node {
        name: "0".into(),
        labelled: false,
    }];
    let mut node_of_root: HashMap<usize, NodeId> = HashMap::new();
    if let Some(g) = ground_root {
        node_of_root.insert(g, NodeId::GROUND);
    }
    let mut auto_index = 0;
    let mut pin_count_by_node: HashMap<NodeId, usize> = HashMap::new();
    let mut elements = Vec::new();

    for (c, entries) in schematic.components.iter().zip(&pin_entries) {
        let mut element_nodes = Vec::with_capacity(entries.len());
        for &entry in entries {
            let root = dsu.find(entry);
            let node = *node_of_root.entry(root).or_insert_with(|| {
                let id = NodeId(nodes.len());
                let (name, labelled) = match labels_by_root.get(&root) {
                    Some(names) => (names[0].clone(), true),
                    None => {
                        auto_index += 1;
                        (format!("N{auto_index:03}"), false)
                    }
                };
                nodes.push(Node { name, labelled });
                id
            });
            *pin_count_by_node.entry(node).or_default() += 1;
            element_nodes.push(node);
        }
        if !matches!(c.kind, ComponentKind::Ground) {
            elements.push(Element {
                component: c.id,
                designator: c.designator.clone(),
                kind: c.kind.clone(),
                nodes: element_nodes,
            });
        }
    }

    // Unconnected pins: a non-ground node touched by only one pin and not
    // labelled (a labelled stub is intentional, e.g. a probe point).
    for e in &elements {
        for (pin, node) in e.kind.pins().iter().zip(&e.nodes) {
            if !node.is_ground() && pin_count_by_node[node] == 1 && !nodes[node.0].labelled {
                diagnostics.push(Diagnostic::UnconnectedPin {
                    component: e.component,
                    pin: pin.name.to_string(),
                });
            }
        }
    }

    // Model references.
    for e in &elements {
        let Some(model_name) = e.kind.model_name() else {
            continue;
        };
        let expected = match &e.kind {
            ComponentKind::Diode { .. } => ModelKind::Diode,
            ComponentKind::Bjt {
                polarity: Polarity::N,
                ..
            } => ModelKind::Npn,
            ComponentKind::Bjt {
                polarity: Polarity::P,
                ..
            } => ModelKind::Pnp,
            ComponentKind::Mosfet {
                polarity: Polarity::N,
                ..
            } => ModelKind::Nmos,
            ComponentKind::Mosfet {
                polarity: Polarity::P,
                ..
            } => ModelKind::Pmos,
            _ => continue,
        };
        match schematic.models.get(model_name) {
            None => diagnostics.push(Diagnostic::MissingModel {
                component: e.component,
                model: model_name.to_string(),
            }),
            Some(m) if m.kind != expected => diagnostics.push(Diagnostic::WrongModelKind {
                component: e.component,
                model: model_name.to_string(),
                expected,
                found: m.kind,
            }),
            Some(_) => {}
        }
    }

    // Duplicate designators.
    let mut by_designator: BTreeMap<&str, Vec<ComponentId>> = BTreeMap::new();
    for e in &elements {
        if !e.designator.is_empty() {
            by_designator
                .entry(&e.designator)
                .or_default()
                .push(e.component);
        }
    }
    for (designator, components) in by_designator {
        if components.len() > 1 {
            diagnostics.push(Diagnostic::DuplicateDesignator {
                designator: designator.to_string(),
                components,
            });
        }
    }

    Netlist {
        nodes,
        elements,
        diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::DeviceModel;
    use crate::schematic::{Rotation, Waveform};

    fn res(r: f64) -> ComponentKind {
        ComponentKind::Resistor { resistance: r }
    }

    /// V1 (vertical, + at top) with two horizontal resistors to ground.
    ///
    /// ```text
    ///   (0,-2)---R1---(6,-2)---R2---(10,-2)
    ///     V1                            |
    ///   (0,2)------------GND--------(10,2)
    /// ```
    fn divider() -> Schematic {
        let mut s = Schematic::new();
        s.add_component(
            ComponentKind::VoltageSource {
                waveform: Waveform::Dc(10.0),
            },
            Point::new(0, 0),
        );
        s.add_component(res(1e3), Point::new(4, -2)); // pins (2,-2) (6,-2)
        s.add_component(res(1e3), Point::new(8, -2)); // pins (6,-2) (10,-2)
        s.add_component(ComponentKind::Ground, Point::new(5, 2));
        s.add_wire(Point::new(0, -2), Point::new(2, -2));
        s.add_wire(Point::new(10, -2), Point::new(10, 2));
        s.add_wire(Point::new(0, 2), Point::new(10, 2)); // ground at (5,2) is a T-junction
        s
    }

    #[test]
    fn voltage_divider_connectivity() {
        let n = build_netlist(&divider());
        assert!(n.diagnostics.is_empty(), "{:?}", n.diagnostics);
        assert_eq!(n.nodes.len(), 3); // ground, top, middle
        let v1 = n.element("V1").unwrap();
        let r1 = n.element("R1").unwrap();
        let r2 = n.element("R2").unwrap();
        assert_eq!(v1.nodes[1], NodeId::GROUND);
        assert_eq!(v1.nodes[0], r1.nodes[0]);
        assert_eq!(r1.nodes[1], r2.nodes[0]);
        assert_eq!(r2.nodes[1], NodeId::GROUND);
        assert_ne!(r1.nodes[0], r1.nodes[1]);
    }

    #[test]
    fn labels_name_and_join_nets() {
        let mut s = divider();
        s.add_label("VOUT", Point::new(6, -2));
        // A separate resistor tied to VOUT purely by label.
        s.add_component(res(10e3), Point::new(20, 0)); // pins (18,0) (22,0)
        s.add_label("VOUT", Point::new(18, 0));
        s.add_component(ComponentKind::Ground, Point::new(22, 0));
        let n = build_netlist(&s);
        assert!(n.diagnostics.is_empty(), "{:?}", n.diagnostics);
        let vout = n.node_by_name("VOUT").unwrap();
        assert_eq!(n.element("R1").unwrap().nodes[1], vout);
        assert_eq!(n.element("R3").unwrap().nodes[0], vout);
        assert_eq!(n.element("R3").unwrap().nodes[1], NodeId::GROUND);
    }

    #[test]
    fn crossing_wires_do_not_connect() {
        let mut s = Schematic::new();
        s.add_component(ComponentKind::Ground, Point::new(0, 10));
        s.add_component(res(1.0), Point::new(-7, 0)); // pins (-9,0) (-5,0)
        let r2 = s.add_component(res(1.0), Point::new(0, -7));
        s.component_mut(r2).unwrap().rotation = Rotation::R90; // pins (0,-9) (0,-5)
        // Horizontal wire from R1 and vertical wire from R2 to ground cross at (0,0).
        s.add_wire(Point::new(-5, 0), Point::new(5, 0));
        s.add_wire(Point::new(0, -5), Point::new(0, 10));
        let n = build_netlist(&s);
        let r1 = n.element("R1").unwrap();
        let r2 = n.element("R2").unwrap();
        assert_eq!(r2.nodes[1], NodeId::GROUND);
        assert_ne!(
            r1.nodes[1],
            NodeId::GROUND,
            "crossing wires must not connect"
        );
    }

    #[test]
    fn missing_ground_and_unconnected_pins() {
        let mut s = Schematic::new();
        s.add_component(res(1.0), Point::new(0, 0));
        let n = build_netlist(&s);
        assert!(n.has_errors());
        assert!(n.diagnostics.contains(&Diagnostic::NoGround));
        let unconnected = n
            .diagnostics
            .iter()
            .filter(|d| matches!(d, Diagnostic::UnconnectedPin { .. }))
            .count();
        assert_eq!(unconnected, 2);
    }

    #[test]
    fn model_checks() {
        let mut s = divider();
        s.add_component(
            ComponentKind::Bjt {
                polarity: Polarity::N,
                model: "Q1MOD".into(),
            },
            Point::new(30, 0),
        );
        s.add_component(
            ComponentKind::Diode { model: "DX".into() },
            Point::new(40, 0),
        );
        s.models.insert(DeviceModel::new("q1mod", ModelKind::Pnp));
        let n = build_netlist(&s);
        assert!(n.has_errors());
        assert!(n.diagnostics.iter().any(|d| matches!(
            d,
            Diagnostic::WrongModelKind {
                expected: ModelKind::Npn,
                found: ModelKind::Pnp,
                ..
            }
        )));
        assert!(
            n.diagnostics
                .iter()
                .any(|d| matches!(d, Diagnostic::MissingModel { model, .. } if model == "DX"))
        );
    }

    #[test]
    fn conflicting_labels_and_duplicates() {
        let mut s = divider();
        s.add_label("A", Point::new(6, -2));
        s.add_label("B", Point::new(6, -2));
        s.components[1].designator = "R2".into();
        let n = build_netlist(&s);
        assert!(!n.has_errors());
        assert!(n.diagnostics.contains(&Diagnostic::ConflictingLabels {
            names: vec!["A".into(), "B".into()]
        }));
        assert!(n.node_by_name("A").is_some());
        assert!(n
            .diagnostics
            .iter()
            .any(|d| matches!(d, Diagnostic::DuplicateDesignator { designator, .. } if designator == "R2")));
    }
}
