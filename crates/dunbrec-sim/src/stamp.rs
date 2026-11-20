//! Helpers that write device contributions ("stamps") into the MNA system.
//!
//! Unknown layout: node voltages first (node 1 → row 0, ground has no row),
//! then one row per branch current. KCL rows are written as "sum of currents
//! leaving the node through devices = 0", so an independent current pushed
//! *into* a node appears on the right-hand side with a positive sign.

use dunbrec_core::NodeId;

use crate::linalg::DenseMatrix;

/// Index of a branch-current unknown (voltage sources, inductors, current
/// sensors). The current flows from the branch's positive node through the
/// device to its negative node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BranchId(pub usize);

/// Number of rows used by node voltages for a circuit with `node_count`
/// nodes (including ground).
pub(crate) fn node_rows(node_count: usize) -> usize {
    node_count.saturating_sub(1)
}

pub struct Stamper<'a> {
    matrix: &'a mut DenseMatrix,
    rhs: &'a mut [f64],
    node_rows: usize,
    /// The solution guess the system is being linearised around (Newton
    /// iterate). Linear devices ignore it.
    iterate: &'a [f64],
}

impl<'a> Stamper<'a> {
    pub(crate) fn new(
        matrix: &'a mut DenseMatrix,
        rhs: &'a mut [f64],
        node_rows: usize,
        iterate: &'a [f64],
    ) -> Self {
        Self {
            matrix,
            rhs,
            node_rows,
            iterate,
        }
    }

    /// Voltage of `n` in the current Newton iterate.
    pub fn iterate_voltage(&self, n: NodeId) -> f64 {
        if n.is_ground() {
            0.0
        } else {
            self.iterate[n.0 - 1]
        }
    }

    /// A current leaving `node` into the device equal to
    /// `sum(coeff * V(other)) + constant`. Used for linearised nonlinear
    /// devices, where a terminal current depends on several voltages.
    pub fn current_row(&mut self, node: NodeId, coeffs: &[(NodeId, f64)], constant: f64) {
        let r = self.node(node);
        for &(other, g) in coeffs {
            let c = self.node(other);
            self.add(r, c, g);
        }
        self.add_rhs(r, -constant);
    }

    #[inline]
    fn node(&self, n: NodeId) -> Option<usize> {
        (!n.is_ground()).then(|| n.0 - 1)
    }

    #[inline]
    fn branch(&self, b: BranchId) -> usize {
        self.node_rows + b.0
    }

    #[inline]
    fn add(&mut self, r: Option<usize>, c: Option<usize>, v: f64) {
        if let (Some(r), Some(c)) = (r, c) {
            self.matrix.add(r, c, v);
        }
    }

    #[inline]
    fn add_rhs(&mut self, r: Option<usize>, v: f64) {
        if let Some(r) = r {
            self.rhs[r] += v;
        }
    }

    /// A conductance `g` (siemens) between nodes `a` and `b`.
    pub fn conductance(&mut self, a: NodeId, b: NodeId, g: f64) {
        let (ra, rb) = (self.node(a), self.node(b));
        self.add(ra, ra, g);
        self.add(rb, rb, g);
        self.add(ra, rb, -g);
        self.add(rb, ra, -g);
    }

    /// An independent current `i` flowing from node `from` through the device
    /// to node `to` (so it leaves `from` and is pushed into `to`).
    pub fn current(&mut self, from: NodeId, to: NodeId, i: f64) {
        let (rf, rt) = (self.node(from), self.node(to));
        self.add_rhs(rf, -i);
        self.add_rhs(rt, i);
    }

    /// A current `gm * (V(cp) - V(cn))` flowing from `p` through the device
    /// to `n`.
    pub fn transconductance(&mut self, p: NodeId, n: NodeId, cp: NodeId, cn: NodeId, gm: f64) {
        let (rp, rn, rcp, rcn) = (self.node(p), self.node(n), self.node(cp), self.node(cn));
        self.add(rp, rcp, gm);
        self.add(rp, rcn, -gm);
        self.add(rn, rcp, -gm);
        self.add(rn, rcn, gm);
    }

    /// Connect branch `k` between `p` and `n`: its current leaves `p` and
    /// enters `n` (KCL incidence), and its equation row starts with
    /// `V(p) - V(n)`. The caller completes the equation with
    /// [`branch_rhs`](Self::branch_rhs) and the `branch_*` coefficient methods.
    pub fn branch_incidence(&mut self, p: NodeId, n: NodeId, k: BranchId) {
        self.branch_kcl(p, n, k);
        let (rp, rn, rk) = (self.node(p), self.node(n), Some(self.branch(k)));
        self.add(rk, rp, 1.0);
        self.add(rk, rn, -1.0);
    }

    /// Only the KCL part of [`branch_incidence`](Self::branch_incidence):
    /// branch `k`'s current leaves `p` and enters `n`. The caller writes the
    /// whole branch equation itself.
    pub fn branch_kcl(&mut self, p: NodeId, n: NodeId, k: BranchId) {
        let (rp, rn, rk) = (self.node(p), self.node(n), Some(self.branch(k)));
        self.add(rp, rk, 1.0);
        self.add(rn, rk, -1.0);
    }

    /// Ideal voltage branch: `V(p) - V(n) = v`, with current through `k`.
    pub fn voltage_source(&mut self, p: NodeId, n: NodeId, k: BranchId, v: f64) {
        self.branch_incidence(p, n, k);
        self.branch_rhs(k, v);
    }

    /// Add `v` to the right-hand side of branch `k`'s equation.
    pub fn branch_rhs(&mut self, k: BranchId, v: f64) {
        let rk = Some(self.branch(k));
        self.add_rhs(rk, v);
    }

    /// Add `coeff * V(node)` to the left side of branch `k`'s equation.
    pub fn branch_voltage_coeff(&mut self, k: BranchId, node: NodeId, coeff: f64) {
        let (rk, rn) = (Some(self.branch(k)), self.node(node));
        self.add(rk, rn, coeff);
    }

    /// Add `coeff * I(other)` to the left side of branch `k`'s equation.
    pub fn branch_current_coeff(&mut self, k: BranchId, other: BranchId, coeff: f64) {
        let (rk, ro) = (Some(self.branch(k)), Some(self.branch(other)));
        self.add(rk, ro, coeff);
    }

    /// A current `gain * I(k)` flowing from `p` through the device to `n`.
    pub fn current_controlled_current(&mut self, p: NodeId, n: NodeId, k: BranchId, gain: f64) {
        let (rp, rn, rk) = (self.node(p), self.node(n), Some(self.branch(k)));
        self.add(rp, rk, gain);
        self.add(rn, rk, -gain);
    }
}

/// Read-only view of a solved system.
#[derive(Debug, Clone, Copy)]
pub struct SolutionView<'a> {
    pub(crate) x: &'a [f64],
    pub(crate) node_rows: usize,
}

impl SolutionView<'_> {
    pub fn voltage(&self, n: NodeId) -> f64 {
        if n.is_ground() { 0.0 } else { self.x[n.0 - 1] }
    }

    pub fn branch_current(&self, k: BranchId) -> f64 {
        self.x[self.node_rows + k.0]
    }
}
