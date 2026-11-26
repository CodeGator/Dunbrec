//! Draws a new circuit the way a person would: supply rails along the top,
//! ground symbols at the bottom, parts in columns, and real wires between
//! them.
//!
//! Parts are put in three rows. Parts hanging from a supply rail go in the
//! top row, parts standing on ground in the bottom row, and everything else
//! (transistors, coupling capacitors, ICs, sources) in the middle row. A
//! transistor gets the part on its collector above it and the part on its
//! emitter below it, so a stage reads top to bottom; a resistor from the
//! supply and one to ground that share a net (a divider) form one column.
//! Columns follow the signal from the input to the output.
//!
//! Wires are then found net by net with a router that only runs at right
//! angles, never through a part, and only crosses another net's wire
//! square-on (wires that cross do not connect). Every net also gets a
//! label, so it keeps its name.
//!
//! [`tidy`] gives up (`None`) when it cannot place or route something; the
//! caller is expected to check the netlist of what it returns and to fall
//! back to [`crate::layout`].

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};

use dunbrec_core::{ComponentKind, ModelLibrary, Point, Rotation, Schematic};

use crate::layout::{Part, STUB, ground_rotation, is_ground, outward};

/// Clear columns between neighbouring columns.
const GAP: i32 = 6;
/// Rows of the drawing: the first supply rail, and the middle of each part row.
const RAIL_Y: i32 = 0;
const TOP_Y: i32 = 6;
const MID_Y: i32 = 16;
const BOT_Y: i32 = 24;
/// Rows between supply rails.
const RAIL_STEP: i32 = 3;
/// Cost of a bend, and of crossing another wire, in grid steps.
const TURN_COST: i32 = 3;
const CROSS_COST: i32 = 8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    /// Two pins, one on a supply rail and one on ground.
    Source,
    /// Two pins, one on a supply rail and one on a signal net.
    Top,
    /// Two pins, one on ground and one on a signal net.
    Bottom,
    /// Two pins, both on signal nets.
    Across,
    /// A transistor.
    Active,
    Other,
}

/// How a part is turned.
#[derive(Clone, Copy, Debug)]
struct Turn {
    rotation: Rotation,
    mirrored: bool,
}

impl Turn {
    const NONE: Turn = Turn {
        rotation: Rotation::R0,
        mirrored: false,
    };
}

fn offsets(kind: &ComponentKind, t: Turn) -> Vec<Point> {
    kind.pins()
        .iter()
        .map(|p| {
            let m = if t.mirrored {
                Point::new(-p.offset.x, p.offset.y)
            } else {
                p.offset
            };
            t.rotation.apply(m)
        })
        .collect()
}

const ROTATIONS: [Rotation; 4] = [Rotation::R0, Rotation::R90, Rotation::R270, Rotation::R180];

/// A turn that puts pin `top` of a two-pin part straight above the other.
fn vertical_with_top(kind: &ComponentKind, top: usize) -> Option<Turn> {
    ROTATIONS.iter().find_map(|&rotation| {
        let t = Turn {
            rotation,
            mirrored: false,
        };
        let o = offsets(kind, t);
        (o[top].x == o[1 - top].x && o[top].y < o[1 - top].y).then_some(t)
    })
}

/// A turn that puts pin 0 of a two-pin part to the left of pin 1.
fn horizontal(kind: &ComponentKind) -> Option<Turn> {
    ROTATIONS.iter().find_map(|&rotation| {
        let t = Turn {
            rotation,
            mirrored: false,
        };
        let o = offsets(kind, t);
        (o[0].y == o[1].y && o[0].x < o[1].x).then_some(t)
    })
}

fn is_rail_name(net: &str) -> bool {
    matches!(
        net.to_ascii_uppercase().as_str(),
        "VCC" | "VDD" | "VBUS" | "V+" | "VBAT" | "VPLUS"
    )
}

/// One column of the drawing: up to a part in each row.
#[derive(Default, Clone, Copy)]
struct Column {
    top: Option<usize>,
    mid: Option<usize>,
    bot: Option<usize>,
}

/// Where a part ended up.
struct Placed {
    part: usize,
    at: Point,
    turn: Turn,
}

/// What is on a grid cell: wire runs and pins of one net.
struct Own {
    net: usize,
    h: bool,
    v: bool,
    node: bool,
}

struct Grid {
    occ: HashMap<Point, Vec<Own>>,
    blocked: HashSet<Point>,
    lo: Point,
    hi: Point,
}

impl Grid {
    fn mark(&mut self, p: Point, net: usize, h: bool, v: bool, node: bool) {
        let e = self.occ.entry(p).or_default();
        if let Some(o) = e.iter_mut().find(|o| o.net == net) {
            o.h |= h;
            o.v |= v;
            o.node |= node;
        } else {
            e.push(Own { net, h, v, node });
        }
    }

    /// Mark the straight run `a`..`b`; its two ends are nodes.
    fn mark_run(&mut self, a: Point, b: Point, net: usize) {
        let d = Point::new((b.x - a.x).signum(), (b.y - a.y).signum());
        let horizontal = d.y == 0;
        let mut p = a;
        loop {
            self.mark(p, net, horizontal, !horizontal, p == a || p == b);
            if p == b {
                break;
            }
            p = p + d;
        }
    }

    fn foreign(&self, p: Point, me: usize) -> bool {
        self.occ
            .get(&p)
            .is_some_and(|v| v.iter().any(|o| o.net != me))
    }

    fn inside(&self, p: Point) -> bool {
        p.x >= self.lo.x && p.x <= self.hi.x && p.y >= self.lo.y && p.y <= self.hi.y
    }
}

const DIRS: [Point; 4] = [
    Point::new(1, 0),
    Point::new(-1, 0),
    Point::new(0, 1),
    Point::new(0, -1),
];

/// The cheapest right-angled path from `from` to a cell of `tree`, as the
/// cells along it. It never enters a part, never ends or bends on another
/// net's wire, and crosses one only straight across.
fn route(g: &Grid, me: usize, tree: &HashSet<Point>, from: Point) -> Option<Vec<Point>> {
    type State = (Point, usize);
    const NONE: usize = 4;
    let mut best: HashMap<State, i32> = HashMap::new();
    let mut prev: HashMap<State, State> = HashMap::new();
    let mut heap = BinaryHeap::new();
    best.insert((from, NONE), 0);
    heap.push(Reverse((0, from.x, from.y, NONE)));
    while let Some(Reverse((cost, x, y, din))) = heap.pop() {
        let c = Point::new(x, y);
        let st = (c, din);
        if best.get(&st).is_some_and(|&b| b < cost) {
            continue;
        }
        let at_foreign = g.foreign(c, me);
        if c != from && tree.contains(&c) && !at_foreign {
            let mut path = vec![c];
            let mut cur = st;
            while let Some(&p) = prev.get(&cur) {
                path.push(p.0);
                cur = p;
            }
            path.reverse();
            return Some(path);
        }
        for (k, d) in DIRS.iter().enumerate() {
            if din != NONE && (k == din ^ 1 || (at_foreign && k != din)) {
                continue;
            }
            let n = c + *d;
            if !g.inside(n) || g.blocked.contains(&n) {
                continue;
            }
            let mut extra = 0;
            if let Some(owners) = g.occ.get(&n) {
                let others: Vec<&Own> = owners.iter().filter(|o| o.net != me).collect();
                if !others.is_empty() {
                    // Only straight across a single wire of one other net.
                    if others.len() != 1 || owners.len() != 1 || others[0].node {
                        continue;
                    }
                    let o = others[0];
                    let across = if d.y == 0 { o.v && !o.h } else { o.h && !o.v };
                    if !across {
                        continue;
                    }
                    extra = CROSS_COST;
                }
            }
            let turn = if din != NONE && k != din {
                TURN_COST
            } else {
                0
            };
            let nc = cost + 1 + extra + turn;
            let ns = (n, k);
            if best.get(&ns).is_none_or(|&b| nc < b) {
                best.insert(ns, nc);
                prev.insert(ns, st);
                heap.push(Reverse((nc, n.x, n.y, k)));
            }
        }
    }
    None
}

/// The straight runs of a path of adjacent cells.
fn runs(path: &[Point]) -> Vec<(Point, Point)> {
    let mut out = Vec::new();
    let mut start = path[0];
    for i in 1..path.len() {
        let straight = i + 1 < path.len()
            && (path[i].x - path[i - 1].x, path[i].y - path[i - 1].y)
                == (path[i + 1].x - path[i].x, path[i + 1].y - path[i].y);
        if !straight {
            out.push((start, path[i]));
            start = path[i];
        }
    }
    out
}

fn cells_of(a: Point, b: Point) -> Vec<Point> {
    let d = Point::new((b.x - a.x).signum(), (b.y - a.y).signum());
    let mut out = vec![a];
    let mut p = a;
    while p != b {
        p = p + d;
        out.push(p);
    }
    out
}

/// The cells a part's body fills, which wires must keep out of.
fn body_cells(pins: &[Point]) -> Vec<Point> {
    if pins.len() < 2 {
        return Vec::new();
    }
    let x0 = pins.iter().map(|p| p.x).min().unwrap_or(0);
    let x1 = pins.iter().map(|p| p.x).max().unwrap_or(0);
    let y0 = pins.iter().map(|p| p.y).min().unwrap_or(0);
    let y1 = pins.iter().map(|p| p.y).max().unwrap_or(0);
    let span = |lo: i32, hi: i32| {
        if hi > lo {
            lo + 1..=hi - 1
        } else {
            lo - 1..=lo + 1
        }
    };
    let mut out = Vec::new();
    for x in span(x0, x1) {
        for y in span(y0, y1) {
            out.push(Point::new(x, y));
        }
    }
    for (i, a) in pins.iter().enumerate() {
        for b in &pins[i + 1..] {
            if a.x == b.x {
                for y in a.y.min(b.y) + 1..a.y.max(b.y) {
                    out.push(Point::new(a.x, y));
                }
            }
            if a.y == b.y {
                for x in a.x.min(b.x) + 1..a.x.max(b.x) {
                    out.push(Point::new(x, a.y));
                }
            }
        }
    }
    out.retain(|c| !pins.contains(c));
    out
}

/// Draw `parts` with wires, or `None` if they cannot be laid out that way.
pub fn tidy(parts: &[Part], models: ModelLibrary) -> Option<Schematic> {
    if parts.is_empty() {
        return None;
    }
    let n = parts.len();

    // ---- what each net is ----
    let mut rails: Vec<String> = Vec::new();
    for p in parts {
        if matches!(p.kind, ComponentKind::VoltageSource { .. })
            && p.nets.len() == 2
            && !is_ground(&p.nets[0])
            && is_ground(&p.nets[1])
            && !rails.contains(&p.nets[0])
        {
            rails.push(p.nets[0].clone());
        }
        for net in &p.nets {
            if is_rail_name(net) && !rails.contains(net) {
                rails.push(net.clone());
            }
        }
    }
    let rail = |net: &str| rails.iter().any(|r| r == net);
    let signal = |net: &str| !is_ground(net) && !rail(net);

    // ---- what each part is, and how it is turned ----
    let mut class = Vec::with_capacity(n);
    let mut turn = Vec::with_capacity(n);
    for p in parts {
        let (c, t) = match (p.kind.pins().len(), &p.kind) {
            (2, kind) => {
                let (a, b) = (p.nets[0].as_str(), p.nets[1].as_str());
                if a == b {
                    return None;
                }
                let (c, top) = if (rail(a) && is_ground(b)) || (is_ground(a) && rail(b)) {
                    (Class::Source, Some(usize::from(!rail(a))))
                } else if (rail(a) && signal(b)) || (signal(a) && rail(b)) {
                    (Class::Top, Some(usize::from(!rail(a))))
                } else if (is_ground(a) && signal(b)) || (signal(a) && is_ground(b)) {
                    (Class::Bottom, Some(usize::from(is_ground(a))))
                } else {
                    (Class::Across, None)
                };
                let t = match top {
                    Some(top) => vertical_with_top(kind, top)?,
                    None => horizontal(kind)?,
                };
                (c, t)
            }
            (3, ComponentKind::Bjt { .. } | ComponentKind::Mosfet { .. }) => {
                // With its emitter on the supply (a PNP), flip it over.
                let t = if rail(&p.nets[2]) {
                    Turn {
                        rotation: Rotation::R180,
                        mirrored: true,
                    }
                } else {
                    Turn::NONE
                };
                (Class::Active, t)
            }
            _ => (Class::Other, Turn::NONE),
        };
        class.push(c);
        turn.push(t);
    }
    let pin_offsets: Vec<Vec<Point>> = (0..n).map(|i| offsets(&parts[i].kind, turn[i])).collect();

    // ---- the order the parts are met along the signal ----
    let mut by_net: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, p) in parts.iter().enumerate() {
        for net in p.nets.iter().filter(|x| signal(x)) {
            by_net.entry(net).or_default().push(i);
        }
    }
    let input = (0..n).find(|&i| {
        matches!(class[i], Class::Across | Class::Active | Class::Other)
            && parts[i]
                .nets
                .iter()
                .any(|net| signal(net) && by_net[net.as_str()].len() == 1)
    });
    let mut order = Vec::with_capacity(n);
    let mut seen = vec![false; n];
    for root in input.into_iter().chain(0..n) {
        if std::mem::replace(&mut seen[root], true) {
            continue;
        }
        let mut queue = VecDeque::from([root]);
        while let Some(i) = queue.pop_front() {
            order.push(i);
            for net in parts[i].nets.iter().filter(|x| signal(x)) {
                for &j in &by_net[net.as_str()] {
                    if !std::mem::replace(&mut seen[j], true) {
                        queue.push_back(j);
                    }
                }
            }
        }
    }

    // ---- columns ----
    let mut used = vec![false; n];
    let mut columns: Vec<Column> = Vec::new();
    for i in 0..n {
        if class[i] == Class::Source {
            used[i] = true;
            columns.push(Column {
                mid: Some(i),
                ..Column::default()
            });
        }
    }
    let partner = |used: &[bool], want: Class, net: &str| -> Option<usize> {
        (0..n).find(|&j| !used[j] && class[j] == want && parts[j].nets.iter().any(|x| x == net))
    };
    // Transistors first: the part on the collector goes above, on the emitter below.
    let mut active_columns: HashMap<usize, Column> = HashMap::new();
    for &i in &order {
        if class[i] != Class::Active || used[i] {
            continue;
        }
        used[i] = true;
        let flipped = rail(&parts[i].nets[2]);
        let (above, below) = if flipped {
            (None, Some(parts[i].nets[0].as_str()))
        } else {
            (
                Some(parts[i].nets[0].as_str()),
                Some(parts[i].nets[2].as_str()),
            )
        };
        let mut col = Column {
            mid: Some(i),
            ..Column::default()
        };
        if let Some(net) = above.filter(|x| signal(x)) {
            col.top = partner(&used, Class::Top, net);
            if let Some(j) = col.top {
                used[j] = true;
            }
        }
        if let Some(net) = below.filter(|x| signal(x)) {
            col.bot = partner(&used, Class::Bottom, net);
            if let Some(j) = col.bot {
                used[j] = true;
            }
        }
        active_columns.insert(i, col);
    }
    for &i in &order {
        if let Some(col) = active_columns.remove(&i) {
            columns.push(col);
            continue;
        }
        if used[i] {
            continue;
        }
        used[i] = true;
        match class[i] {
            Class::Top | Class::Bottom => {
                let (mine, other) = if class[i] == Class::Top {
                    (Class::Top, Class::Bottom)
                } else {
                    (Class::Bottom, Class::Top)
                };
                let mate = parts[i]
                    .nets
                    .iter()
                    .filter(|x| signal(x))
                    .find_map(|net| partner(&used, other, net));
                let mut col = Column::default();
                for (slot, k) in [(mine, Some(i)), (other, mate)] {
                    if slot == Class::Top {
                        col.top = k;
                    } else {
                        col.bot = k;
                    }
                }
                if let Some(j) = mate {
                    used[j] = true;
                }
                columns.push(col);
            }
            _ => columns.push(Column {
                mid: Some(i),
                ..Column::default()
            }),
        }
    }

    // ---- positions ----
    let anchor = |i: usize| {
        if class[i] == Class::Active {
            pin_offsets[i][0].x
        } else {
            0
        }
    };
    let mut placed: Vec<Placed> = Vec::new();
    let mut spine = 0;
    let mut right_prev: Option<i32> = None;
    for col in &columns {
        let members: Vec<(usize, i32)> = [(col.top, TOP_Y), (col.mid, MID_Y), (col.bot, BOT_Y)]
            .into_iter()
            .filter_map(|(k, y)| k.map(|i| (i, y)))
            .collect();
        let mut left = 2;
        let mut right = 2;
        for &(i, _) in &members {
            for o in &pin_offsets[i] {
                left = left.max(anchor(i) - o.x);
                right = right.max(o.x - anchor(i));
            }
        }
        spine = match right_prev {
            None => left,
            Some(r) => spine + r + GAP + left,
        };
        right_prev = Some(right);
        for &(i, y) in &members {
            placed.push(Placed {
                part: i,
                at: Point::new(spine - anchor(i), y),
                turn: turn[i],
            });
        }
    }

    // ---- terminals, ground symbols, rails ----
    let mut net_ids: HashMap<String, usize> = HashMap::new();
    let mut net_names: Vec<String> = Vec::new();
    let mut terms: Vec<Vec<Point>> = Vec::new();
    // (pin, direction away from the part) for every ground pin.
    let mut grounds: Vec<(Point, Point)> = Vec::new();
    for pl in &placed {
        let p = &parts[pl.part];
        for (k, net) in p.nets.iter().enumerate() {
            let off = pin_offsets[pl.part][k];
            let at = pl.at + off;
            if is_ground(net) {
                grounds.push((at, outward(off)));
                continue;
            }
            let id = *net_ids.entry(net.clone()).or_insert_with(|| {
                net_names.push(net.clone());
                terms.push(Vec::new());
                net_names.len() - 1
            });
            if !terms[id].contains(&at) {
                terms[id].push(at);
            }
        }
    }
    let signal_nets = net_names.len();

    let all_pins: Vec<Point> = terms
        .iter()
        .flatten()
        .copied()
        .chain(grounds.iter().map(|g| g.0))
        .collect();
    let rail_row = |net: &str| {
        let k = rails.iter().position(|r| r == net).unwrap_or(0) as i32;
        RAIL_Y - k * RAIL_STEP
    };
    let top_y = rails.len() as i32 * RAIL_STEP;
    let mut grid = Grid {
        occ: HashMap::new(),
        blocked: HashSet::new(),
        lo: Point::new(all_pins.iter().map(|p| p.x).min()? - 8, RAIL_Y - top_y - 4),
        hi: Point::new(
            all_pins.iter().map(|p| p.x).max()? + 8,
            all_pins.iter().map(|p| p.y).max()? + 8,
        ),
    };
    for pl in &placed {
        let pins: Vec<Point> = pin_offsets[pl.part].iter().map(|o| pl.at + *o).collect();
        grid.blocked.extend(body_cells(&pins));
    }
    for (id, pins) in terms.iter().enumerate() {
        for &p in pins {
            grid.mark(p, id, false, false, true);
        }
    }
    // Each ground pin: a stub and a ground symbol, which no wire may touch.
    let mut stubs: Vec<(Point, Point, Point)> = Vec::new();
    for (k, &(at, dir)) in grounds.iter().enumerate() {
        let id = signal_nets + k;
        let end = Point::new(at.x + dir.x * STUB, at.y + dir.y * STUB);
        grid.mark_run(at, end, id);
        let side = Point::new(dir.y.abs(), dir.x.abs());
        for a in 1..=2 {
            for s in -1..=1 {
                grid.blocked.insert(Point::new(
                    end.x + dir.x * a + side.x * s,
                    end.y + dir.y * a + side.y * s,
                ));
            }
        }
        stubs.push((at, end, dir));
    }

    // ---- wires ----
    let mut wires: Vec<(Point, Point)> = Vec::new();
    let mut net_runs: Vec<Vec<(Point, Point)>> = vec![Vec::new(); signal_nets];
    let mut labels: Vec<(String, Point)> = Vec::new();
    let mut order_nets: Vec<usize> = (0..signal_nets).collect();
    let extent = |id: usize| {
        let t = &terms[id];
        let w = t.iter().map(|p| p.x).max().unwrap_or(0) - t.iter().map(|p| p.x).min().unwrap_or(0);
        let h = t.iter().map(|p| p.y).max().unwrap_or(0) - t.iter().map(|p| p.y).min().unwrap_or(0);
        w + h
    };
    order_nets.sort_by_key(|&id| (!rail(&net_names[id]), extent(id), id));
    let mut trees: Vec<HashSet<Point>> = vec![HashSet::new(); signal_nets];
    for &id in &order_nets {
        let name = &net_names[id];
        let mut tree: HashSet<Point> = HashSet::new();
        let mut pending: Vec<Point> = terms[id].clone();
        if rail(name) {
            let y = rail_row(name);
            let x0 = terms[id].iter().map(|p| p.x).min()? - 2;
            let x1 = terms[id].iter().map(|p| p.x).max()?;
            let (a, b) = (Point::new(x0, y), Point::new(x1, y));
            grid.mark_run(a, b, id);
            tree.extend(cells_of(a, b));
            wires.push((a, b));
            net_runs[id].push((a, b));
            labels.push((name.clone(), a));
        } else {
            tree.insert(pending.remove(0));
        }
        while !pending.is_empty() {
            pending.retain(|p| !tree.contains(p));
            if pending.is_empty() {
                break;
            }
            let gap = |p: &Point| {
                tree.iter()
                    .map(|q| (p.x - q.x).abs() + (p.y - q.y).abs())
                    .min()
                    .unwrap_or(0)
            };
            pending.sort_by_key(gap);
            let mut done = None;
            for (k, &p) in pending.iter().enumerate() {
                if let Some(path) = route(&grid, id, &tree, p) {
                    done = Some((k, path));
                    break;
                }
            }
            let (k, path) = done?;
            pending.remove(k);
            for (a, b) in runs(&path) {
                grid.mark_run(a, b, id);
                tree.extend(cells_of(a, b));
                wires.push((a, b));
                net_runs[id].push((a, b));
            }
        }
        trees[id] = tree;
    }

    // ---- a label on every net, so it keeps its name ----
    for id in 0..signal_nets {
        let name = &net_names[id];
        if rail(name) {
            continue;
        }
        let first = terms[id][0];
        if terms[id].len() == 1 {
            // A lone pin: a short stub out of the part, ending in the label.
            let part = placed
                .iter()
                .find(|pl| pin_offsets[pl.part].iter().any(|o| pl.at + *o == first));
            let dir = part
                .map(|pl| outward(first - pl.at))
                .unwrap_or(Point::new(1, 0));
            let mid = Point::new(first.x + dir.x, first.y + dir.y);
            let end = Point::new(first.x + dir.x * STUB, first.y + dir.y * STUB);
            let free = |p: Point| {
                grid.inside(p) && !grid.blocked.contains(&p) && !grid.occ.contains_key(&p)
            };
            if free(mid) && free(end) {
                grid.mark_run(first, end, id);
                wires.push((first, end));
                labels.push((name.clone(), end));
            } else {
                labels.push((name.clone(), first));
            }
            continue;
        }
        // Otherwise on the middle of the longest wire, if nothing else is there.
        let mut runs_here = net_runs[id].clone();
        runs_here.sort_by_key(|&(a, b)| Reverse((a.x - b.x).abs() + (a.y - b.y).abs()));
        let spot = runs_here.iter().find_map(|&(a, b)| {
            let cells = cells_of(a, b);
            let mid = cells[cells.len() / 2];
            let alone = grid
                .occ
                .get(&mid)
                .is_some_and(|v| v.len() == 1 && v[0].net == id && !v[0].node);
            alone.then_some(mid)
        });
        labels.push((name.clone(), spot.unwrap_or(first)));
    }

    // ---- build the drawing, moved so it starts at the origin ----
    let mut points: Vec<Point> = placed.iter().map(|pl| pl.at).collect();
    points.extend(wires.iter().flat_map(|w| [w.0, w.1]));
    points.extend(labels.iter().map(|l| l.1));
    points.extend(stubs.iter().flat_map(|s| [s.0, s.1]));
    let (min_x, min_y) = (
        points.iter().map(|p| p.x).min()?,
        points.iter().map(|p| p.y).min()?,
    );
    let shift = |p: Point| Point::new(p.x - min_x, p.y - min_y);

    let mut s = Schematic::new();
    s.models = models;
    for pl in &placed {
        let p = &parts[pl.part];
        let id = s.add_component_named(p.kind.clone(), p.designator.clone(), shift(pl.at));
        let c = s.component_mut(id)?;
        c.rotation = pl.turn.rotation;
        c.mirrored = pl.turn.mirrored;
    }
    for &(a, b) in &wires {
        s.add_wire(shift(a), shift(b));
    }
    for &(at, end, dir) in &stubs {
        s.add_wire(shift(at), shift(end));
        let g = s.add_component(ComponentKind::Ground, shift(end));
        s.component_mut(g)?.rotation = ground_rotation(dir);
    }
    for (name, at) in &labels {
        s.add_label(name.clone(), shift(*at));
    }
    Some(s)
}
