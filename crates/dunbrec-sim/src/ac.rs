//! AC (small-signal frequency) analysis.
//!
//! The circuit is first solved for its DC operating point. Every device is
//! then replaced by its small-signal model at that point (a transistor
//! becomes its transconductances and junction capacitances) and the
//! resulting linear circuit is solved with complex numbers at each
//! frequency. One chosen source is driven with 1 V (or 1 A) at 0°; all
//! others are switched off, so node voltages are directly the transfer
//! function from that source.

use dunbrec_core::{ComponentKind, ModelLibrary, Netlist, NodeId};
use num_complex::Complex64;

use crate::circuit::{Circuit, FLOATING_SHUNT};
use crate::device::Tolerances;
use crate::error::SimError;
use crate::newton::{Workspace, dc_solve};
use crate::stamp::{BranchId, SolutionView, node_rows};

pub type C64 = Complex64;

/// What a device needs to write its small-signal equations.
pub struct AcContext<'a> {
    /// Angular frequency (rad/s).
    pub omega: f64,
    /// True for the one source driven with a unit signal.
    pub excited: bool,
    /// The DC operating point the devices are linearised around.
    pub op: SolutionView<'a>,
}

/// Dense complex matrix, row-major.
pub struct ComplexMatrix {
    n: usize,
    data: Vec<C64>,
}

impl ComplexMatrix {
    pub fn zeros(n: usize) -> Self {
        Self {
            n,
            data: vec![C64::new(0.0, 0.0); n * n],
        }
    }

    fn add(&mut self, r: usize, c: usize, v: C64) {
        self.data[r * self.n + c] += v;
    }

    /// Solve `A x = b` in place by LU with partial pivoting.
    #[allow(clippy::needless_range_loop)] // index form mirrors the maths
    fn solve(&mut self, b: &mut [C64]) -> Result<(), usize> {
        let n = self.n;
        let max = self.data.iter().fold(0.0_f64, |m, v| m.max(v.norm()));
        let tiny = max * 1e-18;
        for k in 0..n {
            let p = (k..n)
                .max_by(|&a, &b| {
                    self.data[a * n + k]
                        .norm()
                        .total_cmp(&self.data[b * n + k].norm())
                })
                .unwrap_or(k);
            let pivot = self.data[p * n + k];
            if pivot.norm() <= tiny || pivot.norm() == 0.0 {
                return Err(k);
            }
            if p != k {
                for c in 0..n {
                    self.data.swap(k * n + c, p * n + c);
                }
                b.swap(k, p);
            }
            for r in k + 1..n {
                let f = self.data[r * n + k] / pivot;
                if f == C64::new(0.0, 0.0) {
                    continue;
                }
                for c in k..n {
                    let v = self.data[k * n + c];
                    self.data[r * n + c] -= f * v;
                }
                let bk = b[k];
                b[r] -= f * bk;
            }
        }
        for k in (0..n).rev() {
            let mut sum = b[k];
            for c in k + 1..n {
                sum -= self.data[k * n + c] * b[c];
            }
            b[k] = sum / self.data[k * n + k];
        }
        Ok(())
    }
}

/// Writes small-signal equations; mirrors [`crate::stamp::Stamper`] with
/// complex admittances.
pub struct AcStamper<'a> {
    matrix: &'a mut ComplexMatrix,
    rhs: &'a mut [C64],
    node_rows: usize,
}

impl AcStamper<'_> {
    fn node(&self, n: NodeId) -> Option<usize> {
        (!n.is_ground()).then(|| n.0 - 1)
    }

    fn branch(&self, b: BranchId) -> usize {
        self.node_rows + b.0
    }

    fn add(&mut self, r: Option<usize>, c: Option<usize>, v: C64) {
        if let (Some(r), Some(c)) = (r, c) {
            self.matrix.add(r, c, v);
        }
    }

    fn add_rhs(&mut self, r: Option<usize>, v: C64) {
        if let Some(r) = r {
            self.rhs[r] += v;
        }
    }

    pub fn admittance(&mut self, a: NodeId, b: NodeId, y: C64) {
        let (ra, rb) = (self.node(a), self.node(b));
        self.add(ra, ra, y);
        self.add(rb, rb, y);
        self.add(ra, rb, -y);
        self.add(rb, ra, -y);
    }

    pub fn conductance(&mut self, a: NodeId, b: NodeId, g: f64) {
        self.admittance(a, b, C64::new(g, 0.0));
    }

    /// Capacitance `c` between `a` and `b` at angular frequency `omega`.
    pub fn capacitance(&mut self, a: NodeId, b: NodeId, c: f64, omega: f64) {
        self.admittance(a, b, C64::new(0.0, omega * c));
    }

    /// Independent current `i` from `from` through the device to `to`.
    pub fn current(&mut self, from: NodeId, to: NodeId, i: C64) {
        let (rf, rt) = (self.node(from), self.node(to));
        self.add_rhs(rf, -i);
        self.add_rhs(rt, i);
    }

    pub fn transconductance(&mut self, p: NodeId, n: NodeId, cp: NodeId, cn: NodeId, gm: f64) {
        let gm = C64::new(gm, 0.0);
        let (rp, rn, rcp, rcn) = (self.node(p), self.node(n), self.node(cp), self.node(cn));
        self.add(rp, rcp, gm);
        self.add(rp, rcn, -gm);
        self.add(rn, rcp, -gm);
        self.add(rn, rcn, gm);
    }

    pub fn branch_incidence(&mut self, p: NodeId, n: NodeId, k: BranchId) {
        let one = C64::new(1.0, 0.0);
        let (rp, rn, rk) = (self.node(p), self.node(n), Some(self.branch(k)));
        self.add(rp, rk, one);
        self.add(rn, rk, -one);
        self.add(rk, rp, one);
        self.add(rk, rn, -one);
    }

    pub fn voltage_source(&mut self, p: NodeId, n: NodeId, k: BranchId, v: C64) {
        self.branch_incidence(p, n, k);
        let rk = Some(self.branch(k));
        self.add_rhs(rk, v);
    }

    pub fn branch_voltage_coeff(&mut self, k: BranchId, node: NodeId, coeff: C64) {
        let (rk, rn) = (Some(self.branch(k)), self.node(node));
        self.add(rk, rn, coeff);
    }

    pub fn branch_current_coeff(&mut self, k: BranchId, other: BranchId, coeff: C64) {
        let (rk, ro) = (Some(self.branch(k)), Some(self.branch(other)));
        self.add(rk, ro, coeff);
    }

    pub fn current_controlled_current(&mut self, p: NodeId, n: NodeId, k: BranchId, gain: f64) {
        let g = C64::new(gain, 0.0);
        let (rp, rn, rk) = (self.node(p), self.node(n), Some(self.branch(k)));
        self.add(rp, rk, g);
        self.add(rn, rk, -g);
    }

    /// Current leaving `node` into the device = `sum(coeff · V(other))`.
    pub fn current_row(&mut self, node: NodeId, coeffs: &[(NodeId, f64)]) {
        let r = self.node(node);
        for &(other, g) in coeffs {
            let c = self.node(other);
            self.add(r, c, C64::new(g, 0.0));
        }
    }
}

/// Node voltages over frequency.
#[derive(Debug, Clone, PartialEq)]
pub struct AcResult {
    pub frequencies: Vec<f64>,
    pub node_names: Vec<String>,
    /// `voltages[frequency][node]`, indexed by [`NodeId`] (ground is 0).
    pub voltages: Vec<Vec<C64>>,
}

impl AcResult {
    pub fn node(&self, name: &str) -> Option<NodeId> {
        self.node_names.iter().position(|n| n == name).map(NodeId)
    }

    /// Complex response of node `n` at each frequency.
    pub fn response(&self, n: NodeId) -> Vec<C64> {
        self.voltages.iter().map(|v| v[n.0]).collect()
    }
}

/// `per_decade` logarithmically spaced points from `start` to `stop`.
pub fn log_frequencies(start: f64, stop: f64, per_decade: usize) -> Vec<f64> {
    let decades = (stop / start).log10();
    let n = ((decades * per_decade as f64).ceil() as usize).max(1);
    (0..=n)
        .map(|i| start * 10f64.powf(decades * i as f64 / n as f64))
        .collect()
}

/// Small-signal response to a unit AC signal on `source` (a voltage or
/// current source, by designator) at each of `frequencies` (Hz).
pub fn ac_analysis(
    netlist: &Netlist,
    models: &ModelLibrary,
    source: &str,
    frequencies: &[f64],
) -> Result<AcResult, SimError> {
    let excited = netlist
        .elements
        .iter()
        .position(|e| {
            e.designator == source
                && matches!(
                    e.kind,
                    ComponentKind::VoltageSource { .. } | ComponentKind::CurrentSource { .. }
                )
        })
        .ok_or_else(|| {
            SimError::InvalidOptions(format!("'{source}' is not a voltage or current source"))
        })?;
    if frequencies.iter().any(|f| !(f.is_finite() && *f > 0.0)) {
        return Err(SimError::InvalidOptions(
            "frequencies must be greater than zero".into(),
        ));
    }

    let circuit = Circuit::from_netlist(netlist, models)?;
    let size = circuit.unknown_count();
    let rows = node_rows(circuit.node_count());
    let op = dc_solve(
        &circuit,
        false,
        &mut Workspace::new(size),
        &Tolerances::default(),
    )?;

    let mut voltages = Vec::with_capacity(frequencies.len());
    for &f in frequencies {
        let omega = std::f64::consts::TAU * f;
        let mut matrix = ComplexMatrix::zeros(size);
        let mut x = vec![C64::new(0.0, 0.0); size];
        {
            let mut s = AcStamper {
                matrix: &mut matrix,
                rhs: &mut x,
                node_rows: rows,
            };
            for (i, d) in circuit.devices.iter().enumerate() {
                let ctx = AcContext {
                    omega,
                    excited: i == excited,
                    op: SolutionView {
                        x: &op,
                        node_rows: rows,
                    },
                };
                d.stamp_ac(&mut s, &ctx);
            }
            for &n in &circuit.shunted {
                s.conductance(n, NodeId::GROUND, FLOATING_SHUNT);
            }
        }
        matrix.solve(&mut x).map_err(|i| SimError::Singular {
            unknown: circuit.describe_unknown(i),
        })?;
        let mut v = vec![C64::new(0.0, 0.0); circuit.node_count()];
        v[1..].copy_from_slice(&x[..rows]);
        voltages.push(v);
    }
    Ok(AcResult {
        frequencies: frequencies.to_vec(),
        node_names: circuit.node_names.clone(),
        voltages,
    })
}
