//! Turns a list of parts and their nets into a drawable [`Schematic`].
//!
//! Imported netlists have no drawing (SPICE) or a drawing whose symbols do
//! not match Dunbrec's (KiCad). Parts are placed either where the source
//! put them or on a grid, ordered so connected parts sit near each other.
//! Rather than routing wires, each pin gets a short stub ending in a net
//! label named after its net; same-named labels are connected, so the
//! circuit is exactly the imported one and can be tidied by hand. Ground
//! pins get a ground symbol instead of a label.

use std::collections::{HashMap, HashSet, VecDeque};

use dunbrec_core::{ComponentKind, ModelLibrary, Point, Rotation, Schematic};

/// One part to place.
#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    pub designator: String,
    pub kind: ComponentKind,
    /// Net name of each pin, in [`ComponentKind::pins`] order. `"0"` (or
    /// `"gnd"`, any case) is ground.
    pub nets: Vec<String>,
    /// Where the source drawing put the part, if it had one.
    pub position: Option<Point>,
    pub rotation: Rotation,
}

/// Grid spacing between automatically placed parts.
const SPACING_X: i32 = 14;
const SPACING_Y: i32 = 12;
/// Length of the wire stub from a pin to its label.
pub const STUB: i32 = 2;

pub fn is_ground(net: &str) -> bool {
    net == "0" || net.eq_ignore_ascii_case("gnd")
}

/// Order parts breadth-first through shared (non-ground) nets, so that
/// connected parts end up next to each other.
fn connection_order(parts: &[Part]) -> Vec<usize> {
    let mut by_net: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, p) in parts.iter().enumerate() {
        for net in &p.nets {
            if !is_ground(net) {
                by_net.entry(net).or_default().push(i);
            }
        }
    }
    let mut seen = HashSet::new();
    let mut order = Vec::with_capacity(parts.len());
    for start in 0..parts.len() {
        if !seen.insert(start) {
            continue;
        }
        let mut queue = VecDeque::from([start]);
        while let Some(i) = queue.pop_front() {
            order.push(i);
            for net in &parts[i].nets {
                for &j in by_net.get(net.as_str()).into_iter().flatten() {
                    if seen.insert(j) {
                        queue.push_back(j);
                    }
                }
            }
        }
    }
    order
}

/// Unit direction pointing away from the part at a pin with this offset.
pub fn outward(offset: Point) -> Point {
    if offset.x.abs() >= offset.y.abs() {
        Point::new(offset.x.signum(), 0)
    } else {
        Point::new(0, offset.y.signum())
    }
}

/// Rotation that points a ground symbol (drawn downwards) along `dir`.
pub fn ground_rotation(dir: Point) -> Rotation {
    match (dir.x, dir.y) {
        (-1, 0) => Rotation::R90,
        (0, -1) => Rotation::R180,
        (1, 0) => Rotation::R270,
        _ => Rotation::R0,
    }
}

pub fn layout(parts: &[Part], models: ModelLibrary) -> Schematic {
    let mut s = Schematic::new();
    s.models = models;
    let columns = ((parts.len() as f64 * 1.5).sqrt().ceil() as usize).max(1);

    for (slot, &i) in connection_order(parts).iter().enumerate() {
        let part = &parts[i];
        let position = part.position.unwrap_or_else(|| {
            let (col, row) = (slot % columns, slot / columns);
            Point::new(col as i32 * SPACING_X, row as i32 * SPACING_Y)
        });
        let id = s.add_component_named(part.kind.clone(), part.designator.clone(), position);
        let c = s.component_mut(id).expect("just added");
        c.rotation = part.rotation;
        let pins: Vec<(Point, Point)> = c
            .pin_positions()
            .map(|(def, at)| (at, outward(part.rotation.apply(def.offset))))
            .collect();

        for ((at, dir), net) in pins.into_iter().zip(&part.nets) {
            let end = Point::new(at.x + dir.x * STUB, at.y + dir.y * STUB);
            s.add_wire(at, end);
            if is_ground(net) {
                let g = s.add_component(ComponentKind::Ground, end);
                s.component_mut(g).expect("just added").rotation = ground_rotation(dir);
            } else {
                s.add_label(net.clone(), end);
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use dunbrec_core::{Waveform, build_netlist};

    fn part(des: &str, kind: ComponentKind, nets: &[&str]) -> Part {
        Part {
            designator: des.into(),
            kind,
            nets: nets.iter().map(|n| n.to_string()).collect(),
            position: None,
            rotation: Rotation::R0,
        }
    }

    #[test]
    fn layout_preserves_connectivity() {
        let parts = vec![
            part(
                "V1",
                ComponentKind::VoltageSource {
                    waveform: Waveform::Dc(10.0),
                },
                &["in", "0"],
            ),
            part(
                "R1",
                ComponentKind::Resistor { resistance: 1e3 },
                &["in", "out"],
            ),
            part(
                "R2",
                ComponentKind::Resistor { resistance: 1e3 },
                &["out", "0"],
            ),
            part(
                "R3",
                ComponentKind::Resistor { resistance: 2e3 },
                &["out", "in"],
            ),
        ];
        let s = layout(&parts, ModelLibrary::new());
        let n = build_netlist(&s);
        assert!(n.diagnostics.is_empty(), "{:?}", n.diagnostics);
        for p in &parts {
            let e = n.element(&p.designator).unwrap();
            let names: Vec<&str> = e
                .nodes
                .iter()
                .map(|id| n.nodes[id.0].name.as_str())
                .collect();
            let expected: Vec<&str> = p.nets.iter().map(|x| x.as_str()).collect();
            assert_eq!(names, expected, "{}", p.designator);
        }
        // Parts do not overlap.
        let positions: HashSet<Point> = s
            .components
            .iter()
            .filter(|c| !matches!(c.kind, ComponentKind::Ground))
            .map(|c| c.position)
            .collect();
        assert_eq!(positions.len(), 4);
    }

    #[test]
    fn connected_parts_are_placed_together() {
        // Two separate circuits interleaved in the input.
        let r = |v| ComponentKind::Resistor { resistance: v };
        let parts = vec![
            part("RA1", r(1.0), &["a", "b"]),
            part("RB1", r(1.0), &["x", "y"]),
            part("RA2", r(1.0), &["b", "c"]),
            part("RB2", r(1.0), &["y", "z"]),
        ];
        let order = connection_order(&parts);
        assert_eq!(order, vec![0, 2, 1, 3]);
    }
}
