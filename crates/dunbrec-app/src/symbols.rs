//! Schematic symbols as data: lines, circles, filled shapes and short texts
//! in the component's local grid coordinates (y grows downwards on screen).
//! Pin positions come from `dunbrec_core` (`ComponentKind::pins`); every
//! symbol has a lead ending exactly on each pin.

use dunbrec_core::{Component, ComponentKind, GateOp, Polarity, Rotation, Waveform};
use egui::{Pos2, Rect, pos2};

#[derive(Debug, Clone, PartialEq)]
pub enum Prim {
    /// Open polyline.
    Line(Vec<Pos2>),
    Circle {
        center: Pos2,
        radius: f32,
    },
    /// Filled convex polygon (arrow heads, diode triangles).
    Filled(Vec<Pos2>),
    /// Small fixed text such as "+" or "E", `size` in grid units.
    Text {
        pos: Pos2,
        text: &'static str,
        size: f32,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Symbol {
    pub prims: Vec<Prim>,
    /// Where the designator/value text goes, in local coordinates.
    pub label_anchor: Pos2,
}

fn line(points: &[(f32, f32)]) -> Prim {
    Prim::Line(points.iter().map(|&(x, y)| pos2(x, y)).collect())
}

fn filled(points: &[(f32, f32)]) -> Prim {
    Prim::Filled(points.iter().map(|&(x, y)| pos2(x, y)).collect())
}

fn text(x: f32, y: f32, text: &'static str, size: f32) -> Prim {
    Prim::Text {
        pos: pos2(x, y),
        text,
        size,
    }
}

/// Polyline approximating a circular arc (angles in radians, screen
/// orientation: positive angles turn clockwise because y points down).
fn arc(center: (f32, f32), radius: f32, from: f32, to: f32) -> Vec<Pos2> {
    const STEPS: usize = 10;
    (0..=STEPS)
        .map(|i| {
            let a = from + (to - from) * i as f32 / STEPS as f32;
            pos2(center.0 + radius * a.cos(), center.1 + radius * a.sin())
        })
        .collect()
}

fn resistor_body(prims: &mut Vec<Prim>) {
    prims.push(line(&[(-2.0, 0.0), (-1.2, 0.0)]));
    prims.push(line(&[(1.2, 0.0), (2.0, 0.0)]));
    let mut zig = vec![pos2(-1.2, 0.0)];
    for i in 0..6 {
        let x = -1.2 + 0.2 * (2 * i + 1) as f32;
        zig.push(pos2(x, if i % 2 == 0 { -0.45 } else { 0.45 }));
    }
    zig.push(pos2(1.2, 0.0));
    prims.push(Prim::Line(zig));
}

fn source_circle(prims: &mut Vec<Prim>) {
    prims.push(Prim::Circle {
        center: pos2(0.0, 0.0),
        radius: 1.2,
    });
    prims.push(line(&[(0.0, -2.0), (0.0, -1.2)]));
    prims.push(line(&[(0.0, 1.2), (0.0, 2.0)]));
}

/// Arrow along a straight line ending at `tip`.
fn arrow_head(from: (f32, f32), tip: (f32, f32), size: f32) -> Prim {
    let (dx, dy) = (tip.0 - from.0, tip.1 - from.1);
    let len = (dx * dx + dy * dy).sqrt().max(1e-6);
    let (ux, uy) = (dx / len, dy / len);
    let back = (tip.0 - ux * size, tip.1 - uy * size);
    let (px, py) = (-uy * size * 0.5, ux * size * 0.5);
    filled(&[tip, (back.0 + px, back.1 + py), (back.0 - px, back.1 - py)])
}

/// Symbol for a part; zener diodes are recognised by name (`Z…`) or as
/// library parts with a breakdown voltage. Use [`symbol_styled`] when the
/// schematic's own model is known.
pub fn symbol(kind: &ComponentKind) -> Symbol {
    let zener = match kind {
        ComponentKind::Diode { model } => {
            model.to_ascii_uppercase().starts_with('Z')
                || dunbrec_core::parts::standard_model(model)
                    .is_some_and(|m| m.params.contains_key("BV"))
        }
        _ => false,
    };
    symbol_styled(kind, zener)
}

/// Quadratic Bézier curve sampled as a polyline.
fn bezier(p0: (f32, f32), c: (f32, f32), p1: (f32, f32)) -> Vec<Pos2> {
    const STEPS: usize = 12;
    (0..=STEPS)
        .map(|i| {
            let t = i as f32 / STEPS as f32;
            let u = 1.0 - t;
            pos2(
                u * u * p0.0 + 2.0 * u * t * c.0 + t * t * p1.0,
                u * u * p0.1 + 2.0 * u * t * c.1 + t * t * p1.1,
            )
        })
        .collect()
}

/// Closed rectangle outline.
fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Prim {
    line(&[(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)])
}

/// Inversion bubble whose right edge touches `x_tip`.
fn bubble(x_tip: f32) -> Prim {
    Prim::Circle {
        center: pos2(x_tip - 0.25, 0.0),
        radius: 0.25,
    }
}

/// A 2-input gate in the distinctive-shape style.
fn gate_symbol(p: &mut Vec<Prim>, op: GateOp) {
    let inverted = matches!(op, GateOp::Nand | GateOp::Nor | GateOp::Xnor);
    let tip = 1.5;
    match op {
        GateOp::And | GateOp::Nand => {
            p.push(line(&[(0.0, -1.5), (-1.5, -1.5), (-1.5, 1.5), (0.0, 1.5)]));
            p.push(Prim::Line(arc(
                (0.0, 0.0),
                1.5,
                -std::f32::consts::FRAC_PI_2,
                std::f32::consts::FRAC_PI_2,
            )));
            p.push(line(&[(-3.0, -1.0), (-1.5, -1.0)]));
            p.push(line(&[(-3.0, 1.0), (-1.5, 1.0)]));
        }
        _ => {
            // OR family: concave back, curved sides meeting at the tip.
            p.push(Prim::Line(bezier((-1.5, -1.5), (-0.7, 0.0), (-1.5, 1.5))));
            p.push(Prim::Line(bezier((-1.5, -1.5), (0.6, -1.5), (tip, 0.0))));
            p.push(Prim::Line(bezier((-1.5, 1.5), (0.6, 1.5), (tip, 0.0))));
            if matches!(op, GateOp::Xor | GateOp::Xnor) {
                p.push(Prim::Line(bezier((-1.9, -1.5), (-1.1, 0.0), (-1.9, 1.5))));
            }
            // Inputs end on the back curve (x ≈ -1.1 at y = ±1).
            p.push(line(&[(-3.0, -1.0), (-1.13, -1.0)]));
            p.push(line(&[(-3.0, 1.0), (-1.13, 1.0)]));
        }
    }
    if inverted {
        p.push(bubble(tip + 0.5));
        p.push(line(&[(tip + 0.5, 0.0), (3.0, 0.0)]));
    } else {
        p.push(line(&[(tip, 0.0), (3.0, 0.0)]));
    }
}

/// Clock-input triangle on the left edge of a box at `x`, centred on `y`.
fn clock_mark(x: f32, y: f32) -> Prim {
    line(&[(x, y - 0.4), (x + 0.5, y), (x, y + 0.4)])
}

pub fn symbol_styled(kind: &ComponentKind, zener: bool) -> Symbol {
    let mut p = Vec::new();
    let mut label = pos2(0.0, -1.3);
    match kind {
        ComponentKind::Ground => {
            p.push(line(&[(0.0, 0.0), (0.0, 0.6)]));
            p.push(line(&[(-0.8, 0.6), (0.8, 0.6)]));
            p.push(line(&[(-0.5, 0.9), (0.5, 0.9)]));
            p.push(line(&[(-0.2, 1.2), (0.2, 1.2)]));
            label = pos2(1.0, 0.6);
        }
        ComponentKind::Resistor { .. } => resistor_body(&mut p),
        ComponentKind::Potentiometer { .. } => {
            resistor_body(&mut p);
            p.push(line(&[(0.0, -2.0), (0.0, -0.7)]));
            p.push(arrow_head((0.0, -2.0), (0.0, -0.5), 0.4));
            label = pos2(0.0, 1.3);
        }
        ComponentKind::Capacitor { .. } => {
            p.push(line(&[(-2.0, 0.0), (-0.25, 0.0)]));
            p.push(line(&[(0.25, 0.0), (2.0, 0.0)]));
            p.push(line(&[(-0.25, -0.9), (-0.25, 0.9)]));
            p.push(line(&[(0.25, -0.9), (0.25, 0.9)]));
        }
        ComponentKind::Inductor { .. } => {
            p.push(line(&[(-2.0, 0.0), (-1.2, 0.0)]));
            p.push(line(&[(1.2, 0.0), (2.0, 0.0)]));
            for i in 0..4 {
                let cx = -0.9 + 0.6 * i as f32;
                p.push(Prim::Line(arc(
                    (cx, 0.0),
                    0.3,
                    std::f32::consts::PI,
                    2.0 * std::f32::consts::PI,
                )));
            }
            label = pos2(0.0, -0.9);
        }
        ComponentKind::VoltageSource { waveform } => {
            source_circle(&mut p);
            match waveform {
                Waveform::Sine { .. } => {
                    let wave: Vec<Pos2> = (0..=16)
                        .map(|i| {
                            let x = -0.7 + 1.4 * i as f32 / 16.0;
                            pos2(x, -0.4 * (std::f32::consts::TAU * i as f32 / 16.0).sin())
                        })
                        .collect();
                    p.push(Prim::Line(wave));
                    p.push(text(0.0, -0.75, "+", 0.6));
                }
                Waveform::Pulse { .. } => {
                    p.push(line(&[
                        (-0.7, 0.3),
                        (-0.3, 0.3),
                        (-0.3, -0.3),
                        (0.3, -0.3),
                        (0.3, 0.3),
                        (0.7, 0.3),
                    ]));
                    p.push(text(0.0, -0.75, "+", 0.6));
                }
                _ => {
                    p.push(text(0.0, -0.5, "+", 0.9));
                    p.push(text(0.0, 0.5, "−", 0.9));
                }
            }
            label = pos2(1.5, 0.0);
        }
        ComponentKind::CurrentSource { .. } => {
            source_circle(&mut p);
            // Arrow from "in" (bottom) to "out" (top).
            p.push(line(&[(0.0, 0.7), (0.0, -0.3)]));
            p.push(arrow_head((0.0, 0.7), (0.0, -0.75), 0.5));
            label = pos2(1.5, 0.0);
        }
        ComponentKind::Diode { .. } => {
            p.push(line(&[(-2.0, 0.0), (-0.6, 0.0)]));
            p.push(line(&[(0.6, 0.0), (2.0, 0.0)]));
            p.push(filled(&[(-0.6, -0.6), (-0.6, 0.6), (0.6, 0.0)]));
            if zener {
                p.push(line(&[
                    (0.35, -0.75),
                    (0.6, -0.6),
                    (0.6, 0.6),
                    (0.85, 0.75),
                ]));
            } else {
                p.push(line(&[(0.6, -0.6), (0.6, 0.6)]));
            }
            label = pos2(0.0, -1.1);
        }
        ComponentKind::Bjt { polarity, .. } => {
            p.push(Prim::Circle {
                center: pos2(0.2, 0.0),
                radius: 1.3,
            });
            p.push(line(&[(-2.0, 0.0), (-0.5, 0.0)]));
            p.push(line(&[(-0.5, -0.8), (-0.5, 0.8)]));
            p.push(line(&[
                (-0.5, -0.35),
                (0.8, -1.0),
                (0.8, -2.0),
                (2.0, -2.0),
            ]));
            p.push(line(&[(-0.5, 0.35), (0.8, 1.0), (0.8, 2.0), (2.0, 2.0)]));
            p.push(match polarity {
                Polarity::N => arrow_head((-0.5, 0.35), (0.8, 1.0), 0.45),
                Polarity::P => arrow_head((0.8, 1.0), (-0.45, 0.38), 0.45),
            });
            label = pos2(-1.0, -1.3);
        }
        ComponentKind::Mosfet { polarity, .. } => {
            p.push(line(&[(-2.0, 0.0), (-0.8, 0.0)]));
            p.push(line(&[(-0.8, -0.8), (-0.8, 0.8)]));
            for (a, b) in [(-0.9, -0.4), (-0.25, 0.25), (0.4, 0.9)] {
                p.push(line(&[(-0.4, a), (-0.4, b)]));
            }
            p.push(line(&[
                (-0.4, -0.65),
                (0.8, -0.65),
                (0.8, -2.0),
                (2.0, -2.0),
            ]));
            p.push(line(&[(-0.4, 0.65), (0.8, 0.65), (0.8, 2.0), (2.0, 2.0)]));
            p.push(line(&[(-0.4, 0.0), (0.8, 0.0), (0.8, 0.65)]));
            p.push(match polarity {
                Polarity::N => arrow_head((0.8, 0.0), (-0.35, 0.0), 0.45),
                Polarity::P => arrow_head((-0.4, 0.0), (0.75, 0.0), 0.45),
            });
            label = pos2(-1.2, -1.3);
        }
        ComponentKind::Switch { closed, .. } => {
            p.push(line(&[(-2.0, 0.0), (-1.0, 0.0)]));
            p.push(line(&[(1.0, 0.0), (2.0, 0.0)]));
            p.push(Prim::Circle {
                center: pos2(-1.0, 0.0),
                radius: 0.12,
            });
            p.push(Prim::Circle {
                center: pos2(1.0, 0.0),
                radius: 0.12,
            });
            if *closed {
                p.push(line(&[(-1.0, 0.0), (1.0, -0.12)]));
            } else {
                p.push(line(&[(-1.0, 0.0), (0.85, -0.75)]));
            }
        }
        ComponentKind::Gate { op, .. } => {
            gate_symbol(&mut p, *op);
            label = pos2(0.0, -2.2);
        }
        ComponentKind::Inverter { .. } => {
            p.push(line(&[(-1.5, -1.2), (1.2, 0.0), (-1.5, 1.2), (-1.5, -1.2)]));
            p.push(bubble(1.7));
            p.push(line(&[(-3.0, 0.0), (-1.5, 0.0)]));
            p.push(line(&[(1.7, 0.0), (3.0, 0.0)]));
            label = pos2(0.0, -1.8);
        }
        ComponentKind::DFlipFlop { .. } => {
            p.push(rect(-2.0, -2.5, 2.0, 2.5));
            p.push(line(&[(-3.0, -1.0), (-2.0, -1.0)]));
            p.push(line(&[(-3.0, 1.0), (-2.0, 1.0)]));
            p.push(line(&[(2.0, -1.0), (3.0, -1.0)]));
            p.push(line(&[(2.0, 1.0), (3.0, 1.0)]));
            p.push(clock_mark(-2.0, 1.0));
            p.push(text(-1.4, -1.0, "D", 0.7));
            p.push(text(1.3, -1.0, "Q", 0.7));
            p.push(text(1.3, 1.0, "Q̅", 0.7));
            label = pos2(0.0, -3.1);
        }
        ComponentKind::Counter4 { .. } => {
            p.push(rect(-2.0, -4.0, 2.0, 4.0));
            p.push(line(&[(-3.0, 0.0), (-2.0, 0.0)]));
            p.push(clock_mark(-2.0, 0.0));
            for (y, name) in [(-3.0, "Q0"), (-1.0, "Q1"), (1.0, "Q2"), (3.0, "Q3")] {
                p.push(line(&[(2.0, y), (3.0, y)]));
                p.push(text(1.2, y, name, 0.6));
            }
            p.push(text(-0.6, -3.2, "CTR", 0.6));
            label = pos2(0.0, -4.6);
        }
        ComponentKind::Clock { .. } => {
            p.push(rect(-1.5, -1.0, 1.5, 1.0));
            p.push(line(&[
                (-1.1, 0.5),
                (-0.55, 0.5),
                (-0.55, -0.5),
                (0.0, -0.5),
                (0.0, 0.5),
                (0.55, 0.5),
                (0.55, -0.5),
                (1.1, -0.5),
            ]));
            p.push(line(&[(1.5, 0.0), (3.0, 0.0)]));
            label = pos2(0.0, -1.6);
        }
        ComponentKind::LogicSwitch { high } => {
            p.push(rect(-1.5, -1.0, 1.5, 1.0));
            p.push(text(0.0, 0.0, if *high { "1" } else { "0" }, 1.4));
            p.push(line(&[(1.5, 0.0), (3.0, 0.0)]));
            label = pos2(0.0, -1.6);
        }
        ComponentKind::LogicLed => {
            p.push(line(&[(-3.0, 0.0), (-1.0, 0.0)]));
            p.push(Prim::Circle {
                center: pos2(0.0, 0.0),
                radius: 1.0,
            });
            for (dx, dy) in [(0.9, -0.9), (1.3, -0.4)] {
                p.push(line(&[(dx, dy - 0.3), (dx + 0.5, dy - 0.8)]));
            }
            label = pos2(0.0, 1.6);
        }
        ComponentKind::Mcu { .. } => {
            p.push(rect(-4.0, -10.0, 4.0, 10.0));
            // Pin-1 notch.
            p.push(Prim::Line(arc(
                (0.0, -10.0),
                0.6,
                0.0,
                std::f32::consts::PI,
            )));
            for pin in kind.pins() {
                let (x, y) = (pin.offset.x as f32, pin.offset.y as f32);
                let (edge, tx) = if x < 0.0 { (-4.0, -3.0) } else { (4.0, 3.0) };
                p.push(line(&[(x, y), (edge, y)]));
                p.push(text(tx, y, pin.name, 0.6));
            }
            p.push(text(0.0, -0.5, "ATmega", 0.6));
            p.push(text(0.0, 0.5, "328P", 0.6));
            label = pos2(0.0, -10.9);
        }
        ComponentKind::OpAmp { .. } => {
            // Triangle pointing right; inverting input on top.
            p.push(line(&[(-2.0, -2.5), (2.0, 0.0), (-2.0, 2.5), (-2.0, -2.5)]));
            p.push(line(&[(-3.0, -1.0), (-2.0, -1.0)]));
            p.push(line(&[(-3.0, 1.0), (-2.0, 1.0)]));
            p.push(line(&[(2.0, 0.0), (3.0, 0.0)]));
            // Supply leads meet the slanted edges (y = ±1.25 at x = 0).
            p.push(line(&[(0.0, -3.0), (0.0, -1.25)]));
            p.push(line(&[(0.0, 3.0), (0.0, 1.25)]));
            p.push(text(-1.45, -1.0, "−", 0.8));
            p.push(text(-1.45, 1.0, "+", 0.8));
            p.push(text(0.45, -2.2, "V+", 0.5));
            p.push(text(0.45, 2.2, "V−", 0.5));
            label = pos2(2.1, -2.3);
        }
        ComponentKind::Timer555 => {
            p.push(rect(-3.0, -4.0, 3.0, 4.0));
            for pin in kind.pins() {
                let (x, y) = (pin.offset.x as f32, pin.offset.y as f32);
                // Lead from the pin to the box edge, name just inside.
                let (edge, name_at) = if x <= -5.0 {
                    ((-3.0, y), (-2.0, y))
                } else if x >= 5.0 {
                    ((3.0, y), (2.15, y))
                } else if y < 0.0 {
                    ((x, -4.0), (x, -3.45))
                } else {
                    ((x, 4.0), (x, 3.45))
                };
                p.push(line(&[(x, y), edge]));
                p.push(text(name_at.0, name_at.1, pin.name, 0.5));
            }
            p.push(text(0.2, 0.0, "555", 0.9));
            label = pos2(2.6, -5.0);
        }
        ComponentKind::Vcvs { .. }
        | ComponentKind::Vccs { .. }
        | ComponentKind::Ccvs { .. }
        | ComponentKind::Cccs { .. } => {
            let letter = match kind {
                ComponentKind::Vcvs { .. } => "E",
                ComponentKind::Vccs { .. } => "G",
                ComponentKind::Ccvs { .. } => "H",
                _ => "F",
            };
            p.push(line(&[
                (2.0, -1.0),
                (2.9, 0.0),
                (2.0, 1.0),
                (1.1, 0.0),
                (2.0, -1.0),
            ]));
            p.push(line(&[(2.0, -2.0), (2.0, -1.0)]));
            p.push(line(&[(2.0, 1.0), (2.0, 2.0)]));
            p.push(text(2.0, 0.0, letter, 0.7));
            p.push(line(&[(-2.0, -2.0), (-1.0, -2.0)]));
            p.push(line(&[(-2.0, 2.0), (-1.0, 2.0)]));
            p.push(text(-0.7, -2.0, "+", 0.6));
            p.push(text(-0.7, 2.0, "−", 0.6));
            p.push(text(2.45, -1.5, "+", 0.6));
            label = pos2(0.2, 0.0);
        }
    }
    Symbol {
        prims: p,
        label_anchor: label,
    }
}

/// Apply a component's mirror, rotation and position to a local point.
pub fn to_schematic(c: &Component, p: Pos2) -> Pos2 {
    let x = if c.mirrored { -p.x } else { p.x };
    let (x, y) = match c.rotation {
        Rotation::R0 => (x, p.y),
        Rotation::R90 => (-p.y, x),
        Rotation::R180 => (-x, -p.y),
        Rotation::R270 => (p.y, -x),
    };
    pos2(c.position.x as f32 + x, c.position.y as f32 + y)
}

/// Local bounding box of a symbol, including its pins.
pub fn local_bounds(kind: &ComponentKind) -> Rect {
    let mut r = Rect::NOTHING;
    for prim in symbol(kind).prims {
        match prim {
            Prim::Line(pts) | Prim::Filled(pts) => pts.iter().for_each(|p| r.extend_with(*p)),
            Prim::Circle { center, radius } => {
                r = r.union(Rect::from_center_size(
                    center,
                    egui::vec2(radius, radius) * 2.0,
                ));
            }
            Prim::Text { pos, size, .. } => {
                r = r.union(Rect::from_center_size(pos, egui::vec2(size, size)));
            }
        }
    }
    for pin in kind.pins() {
        r.extend_with(pos2(pin.offset.x as f32, pin.offset.y as f32));
    }
    r
}

/// Bounding box of a placed component in schematic coordinates.
pub fn bounds(c: &Component) -> Rect {
    let local = local_bounds(&c.kind);
    let corners = [
        local.left_top(),
        local.right_top(),
        local.left_bottom(),
        local.right_bottom(),
    ];
    Rect::from_points(&corners.map(|p| to_schematic(c, p)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::palette;
    use dunbrec_core::{ComponentId, Point};

    /// Every pin must coincide with the end of a drawn lead, or the wire
    /// would visibly stop short of the symbol.
    #[test]
    fn every_pin_has_a_lead() {
        for entry in palette() {
            let kind = (entry.make)();
            let ends: Vec<Pos2> = symbol(&kind)
                .prims
                .iter()
                .filter_map(|p| match p {
                    Prim::Line(pts) => Some([pts[0], pts[pts.len() - 1]]),
                    _ => None,
                })
                .flatten()
                .collect();
            for pin in kind.pins() {
                let target = pos2(pin.offset.x as f32, pin.offset.y as f32);
                assert!(
                    ends.iter().any(|e| e.distance(target) < 1e-4),
                    "{}: pin {} has no lead",
                    entry.name,
                    pin.name
                );
            }
        }
    }

    #[test]
    fn transform_matches_core_pin_positions() {
        for rotation in [Rotation::R0, Rotation::R90, Rotation::R180, Rotation::R270] {
            for mirrored in [false, true] {
                let c = Component {
                    id: ComponentId(0),
                    designator: "Q1".into(),
                    kind: ComponentKind::Bjt {
                        polarity: Polarity::N,
                        model: "Q".into(),
                    },
                    position: Point::new(5, -3),
                    rotation,
                    mirrored,
                };
                for (pin, core) in c.pin_positions() {
                    let ours = to_schematic(&c, pos2(pin.offset.x as f32, pin.offset.y as f32));
                    assert_eq!((ours.x as i32, ours.y as i32), (core.x, core.y));
                }
            }
        }
    }

    #[test]
    fn bounds_contain_pins() {
        for entry in palette() {
            let kind = (entry.make)();
            let b = local_bounds(&kind);
            for pin in kind.pins() {
                assert!(
                    b.expand(1e-4)
                        .contains(pos2(pin.offset.x as f32, pin.offset.y as f32))
                );
            }
        }
    }
}
