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

use dunbrec_core::{Component, ComponentKind, ModelLibrary, Point, Rotation, Schematic};

/// One part to place.
#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    pub designator: String,
    pub kind: ComponentKind,
    /// Net name of each pin, in [`ComponentKind::pins`] order. `"0"` (or
    /// `"gnd"`, any case) is ground, and [`UNJOINED`] a pin left alone.
    pub nets: Vec<String>,
    /// Where the source drawing put the part, if it had one.
    pub position: Option<Point>,
    pub rotation: Rotation,
}

/// In place of a net name: the pin is joined to nothing.
pub const UNJOINED: &str = "-";
/// A part with more pins than this is large: it is given its own room, and
/// its stubs lead away from the side of the part each pin is on.
pub const MANY_PINS: usize = 8;

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
            if !is_ground(net) && net != UNJOINED {
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

/// The way a stub leads from the pin of `c` whose offset in the part is
/// `offset`, as the part is turned.
pub fn pin_direction(c: &Component, offset: Point) -> Point {
    let pins = c.kind.pins();
    let turn = |p: Point| c.transform(p) - c.position;
    if pins.len() <= MANY_PINS {
        return outward(turn(offset));
    }
    // A large part has rows of pins: those down a side lead sideways, even
    // the ones further from the middle than the side is.
    let left = pins.iter().map(|p| p.offset.x).min().unwrap_or(0);
    let right = pins.iter().map(|p| p.offset.x).max().unwrap_or(0);
    turn(if offset.x == left {
        Point::new(-1, 0)
    } else if offset.x == right {
        Point::new(1, 0)
    } else {
        Point::new(0, offset.y.signum())
    })
}

/// Where each column (or row) of the grid goes, given the room each part in
/// `slots` needs in that direction and which column (or row) it is in.
fn spread(slots: &[(usize, i32)], spacing: i32) -> Vec<i32> {
    let count = slots.iter().map(|s| s.0 + 1).max().unwrap_or(0);
    let mut room = vec![spacing; count];
    for &(at, needed) in slots {
        room[at] = room[at].max(needed);
    }
    let mut places = Vec::with_capacity(count);
    let mut place = 0;
    for (i, &r) in room.iter().enumerate() {
        if i > 0 {
            place += (room[i - 1] + r) / 2;
        }
        places.push(place);
    }
    places
}

/// The room a part needs, across and down.
fn room(kind: &ComponentKind) -> (i32, i32) {
    let pins = kind.pins();
    if pins.len() <= MANY_PINS {
        return (SPACING_X, SPACING_Y);
    }
    let span = |of: fn(Point) -> i32| {
        let all = pins.iter().map(|p| of(p.offset));
        all.clone().max().unwrap_or(0) - all.min().unwrap_or(0)
    };
    // The stubs at both ends, and the labels beyond them.
    (span(|p| p.x) + 2 * STUB + 12, span(|p| p.y) + 2 * STUB + 6)
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

    let order = connection_order(parts);
    let rooms = || {
        order
            .iter()
            .enumerate()
            .map(|(slot, &i)| (slot % columns, slot / columns, room(&parts[i].kind)))
    };
    let across: Vec<(usize, i32)> = rooms().map(|(col, _, r)| (col, r.0)).collect();
    let down: Vec<(usize, i32)> = rooms().map(|(_, row, r)| (row, r.1)).collect();
    let (xs, ys) = (spread(&across, SPACING_X), spread(&down, SPACING_Y));

    for (slot, &i) in order.iter().enumerate() {
        let part = &parts[i];
        let position = part
            .position
            .unwrap_or_else(|| Point::new(xs[slot % columns], ys[slot / columns]));
        let id = s.add_component_named(part.kind.clone(), part.designator.clone(), position);
        let c = s.component_mut(id).expect("just added");
        c.rotation = part.rotation;
        let pins: Vec<(Point, Point)> = c
            .pin_positions()
            .map(|(def, at)| (at, pin_direction(c, def.offset)))
            .collect();

        for ((at, dir), net) in pins.into_iter().zip(&part.nets) {
            if net == UNJOINED {
                continue;
            }
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
    fn large_parts_get_room_and_stubs_that_join_nothing_else() {
        use dunbrec_core::{CommonPin, LedColour};
        let colour = LedColour::Red;
        let common = CommonPin::Cathode;
        let kinds = [
            ComponentKind::Mcu {
                firmware: String::new(),
                clock: 16e6,
            },
            ComponentKind::LedBar { colour },
            ComponentKind::SevenSegment { colour, common },
            ComponentKind::FourDigit { colour, common },
            ComponentKind::Cd4066 { r_on: 100.0 },
        ];
        // Every pin on a net of its own, so any two that touched would show,
        // but for the first pin of each part, which is left alone.
        let parts: Vec<Part> = kinds
            .iter()
            .enumerate()
            .map(|(i, kind)| Part {
                designator: format!("P{i}"),
                kind: kind.clone(),
                nets: (0..kind.pins().len())
                    .map(|n| match n {
                        0 => UNJOINED.to_string(),
                        _ => format!("n{i}_{n}"),
                    })
                    .collect(),
                position: None,
                rotation: Rotation::R0,
            })
            .collect();
        let s = layout(&parts, ModelLibrary::new());
        let n = build_netlist(&s);
        for p in &parts {
            let e = n.element(&p.designator).unwrap();
            for (pin, (net, node)) in p.nets.iter().zip(&e.nodes).enumerate() {
                let node = &n.nodes[node.0];
                if net == UNJOINED {
                    assert!(!node.labelled, "{} pin {pin}", p.designator);
                } else {
                    assert_eq!(&node.name, net, "{} pin {pin}", p.designator);
                }
            }
        }
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
