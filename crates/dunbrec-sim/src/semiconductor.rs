//! Nonlinear semiconductor devices: diode, BJT and MOSFET.
//!
//! Each device is linearised around the current Newton iterate: it computes
//! its terminal currents and their derivatives at the iterate's voltages and
//! stamps the tangent line (conductances plus a constant current). Junction
//! voltages are limited between iterations (SPICE's `pnjlim`/`fetlim`) so the
//! exponential curves cannot throw the iteration far off.
//!
//! In transient analysis the devices also store charge: depletion charge in
//! reverse-biased junctions (CJO/CJE/CJC/CBD) and diffusion charge from
//! minority carriers (TT/TF/TR). Each charge is integrated like a nonlinear
//! capacitor by a [`ChargeStore`]. In DC the charges play no part.
//!
//! Model parameters use SPICE names and defaults, at a fixed temperature of
//! 27 °C.

use std::cell::Cell;

use dunbrec_core::{DeviceModel, NodeId};

use crate::ac::{AcContext, AcStamper, C64};
use crate::device::{
    Device, History, Integration, Mode, RING_CURRENT_FLOOR, RingDetector, StampContext, Tolerances,
};
use crate::stamp::{SolutionView, Stamper};

/// Thermal voltage kT/q at 27 °C.
pub const VT: f64 = 1.380_649e-23 * 300.15 / 1.602_176_634e-19;
/// Minimum conductance placed across every junction (SPICE GMIN).
pub const GMIN: f64 = 1e-12;
/// Absolute charge tolerance for timestep control (SPICE CHGTOL).
pub const CHGTOL: f64 = 1e-14;
/// Changes smaller than this are not counted as limiting.
const LIMIT_EPS: f64 = 1e-12;

/// Voltage above which a junction is limited (SPICE `vcrit`).
pub fn vcrit(is: f64, nvt: f64) -> f64 {
    nvt * (nvt / (std::f64::consts::SQRT_2 * is)).ln()
}

/// Limit a forward junction voltage step (SPICE `DEVpnjlim`).
pub fn pnjlim(vnew: f64, vold: f64, nvt: f64, vcrit: f64) -> f64 {
    if vnew > vcrit && (vnew - vold).abs() > 2.0 * nvt {
        if vold > 0.0 {
            let arg = 1.0 + (vnew - vold) / nvt;
            if arg > 0.0 {
                vold + nvt * arg.ln()
            } else {
                vcrit
            }
        } else {
            nvt * (vnew / nvt).ln()
        }
    } else {
        vnew
    }
}

/// Limit a MOSFET gate-source voltage step (SPICE `DEVfetlim`).
pub fn fetlim(vnew: f64, vold: f64, vto: f64) -> f64 {
    let vtsthi = (2.0 * (vold - vto)).abs() + 2.0;
    let vtstlo = vtsthi / 2.0 + 2.0;
    let vtox = vto + 3.5;
    let delv = vnew - vold;
    if vold >= vto {
        if vold >= vtox {
            if delv <= 0.0 {
                // Turning off.
                if vnew >= vtox {
                    if -delv > vtstlo {
                        return vold - vtstlo;
                    }
                } else {
                    return vnew.max(vto + 2.0);
                }
            } else if delv >= vtsthi {
                return vold + vtsthi;
            }
        } else if delv <= 0.0 {
            return vnew.max(vto - 0.5);
        } else {
            return vnew.min(vto + 4.0);
        }
    } else if delv <= 0.0 {
        if -delv > vtsthi {
            return vold - vtsthi;
        }
    } else {
        let vtemp = vto + 0.5;
        if vnew <= vtemp {
            if delv > vtstlo {
                return vold + vtstlo;
            }
        } else {
            return vtemp;
        }
    }
    vnew
}

/// Limit a MOSFET drain-source voltage step (SPICE `DEVlimvds`).
pub fn limvds(vnew: f64, vold: f64) -> f64 {
    if vold >= 3.5 {
        if vnew > vold {
            vnew.min(3.0 * vold + 2.0)
        } else if vnew < 3.5 {
            vnew.max(2.0)
        } else {
            vnew
        }
    } else if vnew > vold {
        vnew.min(4.0)
    } else {
        vnew.max(-0.5)
    }
}

fn param_positive_or_inf(model: &DeviceModel, name: &str) -> f64 {
    let v = model.get_or(name, f64::INFINITY);
    if v > 0.0 { v } else { f64::INFINITY }
}

// ------------------------------------------------------- charge storage ---

/// Depletion (junction) capacitance: `C = CJ0 / (1 - v/VJ)^M`, continued
/// linearly above `FC·VJ` so it stays finite in forward bias (SPICE).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Depletion {
    pub cj0: f64,
    pub vj: f64,
    pub m: f64,
    pub fc: f64,
}

impl Depletion {
    pub fn is_zero(&self) -> bool {
        self.cj0 == 0.0
    }

    /// Charge and capacitance at junction voltage `v`.
    pub fn eval(&self, v: f64) -> (f64, f64) {
        let Depletion { cj0, vj, m, fc } = *self;
        if cj0 == 0.0 {
            return (0.0, 0.0);
        }
        let v_knee = fc * vj;
        if v < v_knee {
            let arg = 1.0 - v / vj;
            let c = cj0 * arg.powf(-m);
            let q = if (1.0 - m).abs() < 1e-9 {
                -cj0 * vj * arg.ln()
            } else {
                cj0 * vj / (1.0 - m) * (1.0 - arg.powf(1.0 - m))
            };
            (q, c)
        } else {
            // Charge at the knee plus the integral of the linear extension.
            let (q_knee, _) = Depletion { fc: 1.0, ..*self }.eval(v_knee);
            let f2 = (1.0 - fc).powf(1.0 + m);
            let f3 = 1.0 - fc * (1.0 + m);
            let c = cj0 / f2 * (f3 + m * v / vj);
            let q = q_knee
                + cj0 / f2 * (f3 * (v - v_knee) + m / (2.0 * vj) * (v * v - v_knee * v_knee));
            (q, c)
        }
    }
}

/// Integrates one nonlinear charge `q(v)` between two nodes across
/// transient steps. All values are in the device's own polarity frame.
#[derive(Debug, Clone, Default)]
pub struct ChargeStore {
    q_prev: f64,
    /// Current through the charge at the last accepted point.
    pub i_prev: f64,
    history: History,
    ring: RingDetector,
    pub ringing: bool,
}

impl ChargeStore {
    fn k(h: f64, method: Integration) -> (f64, bool) {
        match method {
            Integration::BackwardEuler => (1.0 / h, false),
            Integration::Trapezoidal => (2.0 / h, true),
        }
    }

    pub fn begin(&mut self, q: f64, time: f64) {
        self.q_prev = q;
        self.i_prev = 0.0;
        self.history.clear();
        self.history.push(time, q);
        self.ring.reset();
        self.ringing = false;
    }

    /// Companion model linearised at `v_lin` where the charge is `q` and
    /// the capacitance `c`: the current is `geq·v + ieq`.
    pub fn companion(&self, q: f64, c: f64, v_lin: f64, h: f64, method: Integration) -> (f64, f64) {
        let (k, trap) = Self::k(h, method);
        let i_lin = k * (q - self.q_prev) - if trap { self.i_prev } else { 0.0 };
        let geq = k * c;
        (geq, i_lin - geq * v_lin)
    }

    pub fn accept(&mut self, q: f64, time: f64, h: f64, method: Integration, restart: bool) {
        let (k, trap) = Self::k(h, method);
        self.i_prev = k * (q - self.q_prev) - if trap { self.i_prev } else { 0.0 };
        self.ringing = self.ring.push(self.i_prev, RING_CURRENT_FLOOR);
        if restart {
            self.history.clear();
            self.history.push(time - h, self.q_prev);
        }
        self.q_prev = q;
        self.history.push(time, q);
    }

    pub fn truncation_step(
        &self,
        q: f64,
        time: f64,
        h: f64,
        method: Integration,
        tol: &Tolerances,
    ) -> Option<f64> {
        let limit = tol.trtol * (tol.reltol * q.abs().max(self.q_prev.abs()) + CHGTOL);
        self.history.suggest_step(time, q, h, method, limit)
    }
}

/// Stamp a charge between `p` and `n` (current from `p` to `n`) during a
/// transient step. `sign` converts the device frame to real polarity.
fn stamp_charge(
    s: &mut Stamper,
    store: &ChargeStore,
    (p, n): (NodeId, NodeId),
    (q, c, v_lin): (f64, f64, f64),
    sign: f64,
    ctx: &StampContext,
) {
    if let Mode::Transient { h, method } = ctx.mode {
        let (geq, ieq) = store.companion(q, c, v_lin, h, method);
        s.conductance(p, n, geq);
        s.current(p, n, sign * ieq);
    }
}

fn transient_step(ctx: &StampContext) -> Option<(f64, Integration)> {
    match ctx.mode {
        Mode::Transient { h, method } => Some((h, method)),
        Mode::Dc { .. } => None,
    }
}

// ---------------------------------------------------------------- Diode ---

#[derive(Debug, Clone, PartialEq)]
pub struct DiodeParams {
    pub is: f64,
    pub n: f64,
    pub rs: f64,
    /// Reverse breakdown voltage (positive), `None` if not modelled.
    pub bv: Option<f64>,
    pub ibv: f64,
    pub depletion: Depletion,
    /// Transit time (diffusion charge = TT · current).
    pub tt: f64,
}

impl DiodeParams {
    pub fn from_model(m: &DeviceModel) -> Self {
        let bv = m.get_or("BV", f64::INFINITY);
        Self {
            is: m.get_or("IS", 1e-14),
            n: m.get_or("N", 1.0),
            rs: m.get_or("RS", 0.0),
            bv: (bv.is_finite() && bv > 0.0).then_some(bv),
            ibv: m.get_or("IBV", 1e-3),
            depletion: Depletion {
                // SPICE accepts both spellings.
                cj0: m.get_or("CJO", m.get_or("CJ0", 0.0)),
                vj: m.get_or("VJ", 1.0),
                m: m.get_or("M", 0.5),
                fc: m.get_or("FC", 0.5),
            },
            tt: m.get_or("TT", 0.0),
        }
    }

    pub fn simple(is: f64, n: f64) -> Self {
        Self {
            is,
            n,
            rs: 0.0,
            bv: None,
            ibv: 1e-3,
            depletion: Depletion {
                cj0: 0.0,
                vj: 1.0,
                m: 0.5,
                fc: 0.5,
            },
            tt: 0.0,
        }
    }

    fn has_charge(&self) -> bool {
        self.tt != 0.0 || !self.depletion.is_zero()
    }
}

#[derive(Debug)]
pub struct Diode {
    pub name: String,
    pub anode: NodeId,
    pub cathode: NodeId,
    /// Anode side of the junction: an internal node when RS > 0.
    anode_int: NodeId,
    p: DiodeParams,
    nvt: f64,
    vcrit: f64,
    /// Breakdown voltage adjusted so the current at -BV equals IBV.
    bv_eff: Option<f64>,
    v_last: Cell<Option<f64>>,
    limited: Cell<bool>,
    charge: ChargeStore,
}

impl Diode {
    pub fn new(name: String, anode: NodeId, cathode: NodeId, p: DiodeParams) -> Self {
        let nvt = p.n * VT;
        let bv_eff = p.bv.map(|bv| breakdown_voltage(bv, p.ibv, p.is, nvt));
        Self {
            name,
            anode,
            cathode,
            anode_int: anode,
            vcrit: vcrit(p.is, nvt),
            nvt,
            bv_eff,
            p,
            v_last: Cell::new(None),
            limited: Cell::new(false),
            charge: ChargeStore::default(),
        }
    }

    /// Junction current and conductance at junction voltage `vd`.
    pub fn eval(&self, vd: f64) -> (f64, f64) {
        let (is, nvt) = (self.p.is, self.nvt);
        let (id, gd) = if vd >= -3.0 * nvt {
            let e = (vd / nvt).exp();
            (is * (e - 1.0), is * e / nvt)
        } else if self.bv_eff.is_none_or(|bv| vd >= -bv) {
            // SPICE's smooth reverse-bias approximation.
            let arg = 3.0 * nvt / (vd * std::f64::consts::E);
            let arg = arg * arg * arg;
            (-is * (1.0 + arg), is * 3.0 * arg / vd)
        } else {
            let bv = self.bv_eff.unwrap_or(f64::INFINITY);
            let e = (-(bv + vd) / nvt).exp();
            (-is * e, is * e / nvt)
        };
        (id + GMIN * vd, gd + GMIN)
    }

    /// Stored charge and capacitance at junction voltage `vd`.
    pub fn charge(&self, vd: f64) -> (f64, f64) {
        let (id, gd) = self.eval(vd);
        let (qd, cd) = self.p.depletion.eval(vd);
        (self.p.tt * id + qd, self.p.tt * gd + cd)
    }

    fn junction(&self, sol: &SolutionView) -> f64 {
        sol.voltage(self.anode_int) - sol.voltage(self.cathode)
    }
}

/// SPICE's adjustment of BV so that the reverse current at -BV is IBV.
fn breakdown_voltage(bv: f64, ibv: f64, is: f64, nvt: f64) -> f64 {
    if ibv < is * bv / nvt {
        return bv;
    }
    let tol = 1e-3 * ibv;
    let mut xbv = bv - nvt * (1.0 + ibv / is).ln();
    for _ in 0..25 {
        xbv = bv - nvt * (ibv / is + 1.0 - xbv / nvt).ln();
        let xcbv = is * (((bv - xbv) / nvt).exp() - 1.0 + xbv / nvt);
        if (xcbv - ibv).abs() <= tol {
            break;
        }
    }
    xbv
}

impl Device for Diode {
    fn designator(&self) -> &str {
        &self.name
    }
    fn stamp_ac(&self, s: &mut AcStamper, ctx: &AcContext) {
        if self.p.rs > 0.0 {
            s.conductance(self.anode, self.anode_int, 1.0 / self.p.rs);
        }
        let vd = self.junction(&ctx.op);
        let (_, gd) = self.eval(vd);
        let (_, c) = self.charge(vd);
        s.admittance(self.anode_int, self.cathode, C64::new(gd, ctx.omega * c));
    }
    fn internal_nodes(&self) -> Vec<&'static str> {
        if self.p.rs > 0.0 {
            vec!["a"]
        } else {
            Vec::new()
        }
    }
    fn assign_internal_nodes(&mut self, first: NodeId) {
        self.anode_int = first;
    }
    fn is_nonlinear(&self) -> bool {
        true
    }
    fn limited(&self) -> bool {
        self.limited.get()
    }
    fn reset_iteration(&self) {
        self.v_last.set(None);
    }
    fn stamp(&self, s: &mut Stamper, ctx: &StampContext) {
        if self.p.rs > 0.0 {
            s.conductance(self.anode, self.anode_int, 1.0 / self.p.rs);
        }
        let raw = s.iterate_voltage(self.anode_int) - s.iterate_voltage(self.cathode);
        let vd = match self.v_last.get() {
            None => self.vcrit,
            Some(old) => pnjlim(raw, old, self.nvt, self.vcrit),
        };
        self.limited.set((vd - raw).abs() > LIMIT_EPS);
        self.v_last.set(Some(vd));
        let (id, gd) = self.eval(vd);
        let (a, k) = (self.anode_int, self.cathode);
        s.conductance(a, k, gd);
        s.current(a, k, id - gd * vd);
        if self.p.has_charge() {
            let (q, c) = self.charge(vd);
            stamp_charge(s, &self.charge, (a, k), (q, c, vd), 1.0, ctx);
        }
    }
    fn dc_paths(&self) -> Vec<(NodeId, NodeId)> {
        vec![(self.anode, self.cathode)]
    }
    /// Junction current plus charging current, into the anode.
    fn current(&self, sol: &SolutionView) -> f64 {
        self.eval(self.junction(sol)).0 + self.charge.i_prev
    }
    fn ringing(&self) -> bool {
        self.charge.ringing
    }
    fn begin_transient(&mut self, sol: &SolutionView, time: f64) {
        let q = self.charge(self.junction(sol)).0;
        self.charge.begin(q, time);
    }
    fn accept_step(&mut self, sol: &SolutionView, ctx: &StampContext, restart: bool) {
        if let Some((h, method)) = transient_step(ctx) {
            let q = self.charge(self.junction(sol)).0;
            self.charge.accept(q, ctx.time, h, method, restart);
        }
    }
    fn truncation_step(
        &self,
        sol: &SolutionView,
        ctx: &StampContext,
        tol: &Tolerances,
    ) -> Option<f64> {
        let (h, method) = transient_step(ctx)?;
        if !self.p.has_charge() {
            return None;
        }
        let q = self.charge(self.junction(sol)).0;
        self.charge.truncation_step(q, ctx.time, h, method, tol)
    }
}

// ------------------------------------------------------------------ BJT ---

/// Gummel-Poon parameters (SPICE names and defaults): DC terms plus
/// junction capacitances and transit times.
#[derive(Debug, Clone, PartialEq)]
pub struct BjtParams {
    pub is: f64,
    pub bf: f64,
    pub br: f64,
    pub nf: f64,
    pub nr: f64,
    pub vaf: f64,
    pub var: f64,
    pub ikf: f64,
    pub ikr: f64,
    pub ise: f64,
    pub ne: f64,
    pub isc: f64,
    pub nc: f64,
    pub rb: f64,
    pub rc: f64,
    pub re: f64,
    pub cje: Depletion,
    pub cjc: Depletion,
    /// Forward and reverse transit times.
    pub tf: f64,
    pub tr: f64,
}

impl BjtParams {
    pub fn from_model(m: &DeviceModel) -> Self {
        let fc = m.get_or("FC", 0.5);
        Self {
            is: m.get_or("IS", 1e-16),
            bf: m.get_or("BF", 100.0),
            br: m.get_or("BR", 1.0),
            nf: m.get_or("NF", 1.0),
            nr: m.get_or("NR", 1.0),
            vaf: param_positive_or_inf(m, "VAF"),
            var: param_positive_or_inf(m, "VAR"),
            ikf: param_positive_or_inf(m, "IKF"),
            ikr: param_positive_or_inf(m, "IKR"),
            ise: m.get_or("ISE", 0.0),
            ne: m.get_or("NE", 1.5),
            isc: m.get_or("ISC", 0.0),
            nc: m.get_or("NC", 2.0),
            rb: m.get_or("RB", 0.0),
            rc: m.get_or("RC", 0.0),
            re: m.get_or("RE", 0.0),
            cje: Depletion {
                cj0: m.get_or("CJE", 0.0),
                vj: m.get_or("VJE", 0.75),
                m: m.get_or("MJE", 0.33),
                fc,
            },
            cjc: Depletion {
                cj0: m.get_or("CJC", 0.0),
                vj: m.get_or("VJC", 0.75),
                m: m.get_or("MJC", 0.33),
                fc,
            },
            tf: m.get_or("TF", 0.0),
            tr: m.get_or("TR", 0.0),
        }
    }

    fn has_charge(&self) -> bool {
        self.tf != 0.0 || self.tr != 0.0 || !self.cje.is_zero() || !self.cjc.is_zero()
    }
}

/// Terminal currents (into collector and base) and their derivatives with
/// respect to Vbe and Vbc, in the NPN frame, plus the junction diode
/// currents the diffusion charges are built from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BjtOperatingPoint {
    pub ic: f64,
    pub ib: f64,
    pub dic_dvbe: f64,
    pub dic_dvbc: f64,
    pub dib_dvbe: f64,
    pub dib_dvbc: f64,
    /// Forward (B-E) and reverse (B-C) ideal diode currents and conductances.
    pub cbe: f64,
    pub gbe: f64,
    pub cbc: f64,
    pub gbc: f64,
}

/// Evaluate the DC Gummel-Poon equations.
pub fn bjt_eval(p: &BjtParams, vbe: f64, vbc: f64) -> BjtOperatingPoint {
    let exp_term = |v: f64, n: f64, is: f64| -> (f64, f64) {
        if is == 0.0 {
            return (0.0, 0.0);
        }
        let nvt = n * VT;
        let e = (v / nvt).exp();
        (is * (e - 1.0), is * e / nvt)
    };
    let (cbe, gbe) = exp_term(vbe, p.nf, p.is);
    let (cbc, gbc) = exp_term(vbc, p.nr, p.is);
    let (cben, gben) = exp_term(vbe, p.ne, p.ise);
    let (cbcn, gbcn) = exp_term(vbc, p.nc, p.isc);
    // GMIN across each junction.
    let (cbe, gbe) = (cbe + GMIN * vbe, gbe + GMIN);
    let (cbc, gbc) = (cbc + GMIN * vbc, gbc + GMIN);

    // Base charge: Early effect (q1) and high injection (q2).
    let q1 = 1.0 / (1.0 - vbc / p.vaf - vbe / p.var);
    let dq1_dvbe = q1 * q1 / p.var;
    let dq1_dvbc = q1 * q1 / p.vaf;
    let q2 = cbe / p.ikf + cbc / p.ikr;
    let dq2_dvbe = gbe / p.ikf;
    let dq2_dvbc = gbc / p.ikr;
    let sq = (1.0 + 4.0 * q2).max(0.0).sqrt();
    let qb = q1 * (1.0 + sq) / 2.0;
    let dqb_dq1 = (1.0 + sq) / 2.0;
    let dqb_dq2 = if sq > 0.0 { q1 / sq } else { 0.0 };
    let dqb_dvbe = dqb_dq1 * dq1_dvbe + dqb_dq2 * dq2_dvbe;
    let dqb_dvbc = dqb_dq1 * dq1_dvbc + dqb_dq2 * dq2_dvbc;

    // Transport current and its derivatives.
    let it = (cbe - cbc) / qb;
    let dit_dvbe = (gbe - it * dqb_dvbe) / qb;
    let dit_dvbc = (-gbc - it * dqb_dvbc) / qb;

    BjtOperatingPoint {
        ic: it - cbc / p.br - cbcn,
        ib: cbe / p.bf + cben + cbc / p.br + cbcn,
        dic_dvbe: dit_dvbe,
        dic_dvbc: dit_dvbc - gbc / p.br - gbcn,
        dib_dvbe: gbe / p.bf + gben,
        dib_dvbc: gbc / p.br + gbcn,
        cbe,
        gbe,
        cbc,
        gbc,
    }
}

#[derive(Debug)]
pub struct Bjt {
    pub name: String,
    pub collector: NodeId,
    pub base: NodeId,
    pub emitter: NodeId,
    /// +1 for NPN, -1 for PNP.
    pub sign: f64,
    p: BjtParams,
    c_int: NodeId,
    b_int: NodeId,
    e_int: NodeId,
    vcrit_be: f64,
    vcrit_bc: f64,
    /// Last (Vbe, Vbc) used, in the NPN frame.
    v_last: Cell<Option<(f64, f64)>>,
    limited: Cell<bool>,
    charge_be: ChargeStore,
    charge_bc: ChargeStore,
}

impl Bjt {
    pub fn new(
        name: String,
        [collector, base, emitter]: [NodeId; 3],
        npn: bool,
        p: BjtParams,
    ) -> Self {
        Self {
            name,
            collector,
            base,
            emitter,
            sign: if npn { 1.0 } else { -1.0 },
            c_int: collector,
            b_int: base,
            e_int: emitter,
            vcrit_be: vcrit(p.is, p.nf * VT),
            vcrit_bc: vcrit(p.is, p.nr * VT),
            p,
            v_last: Cell::new(None),
            limited: Cell::new(false),
            charge_be: ChargeStore::default(),
            charge_bc: ChargeStore::default(),
        }
    }

    fn resistors(&self) -> [(f64, NodeId, NodeId); 3] {
        [
            (self.p.rc, self.collector, self.c_int),
            (self.p.rb, self.base, self.b_int),
            (self.p.re, self.emitter, self.e_int),
        ]
    }

    /// (Vbe, Vbc) at the intrinsic terminals, in the NPN frame.
    fn junctions(&self, v: impl Fn(NodeId) -> f64) -> (f64, f64) {
        let vb = v(self.b_int);
        (
            self.sign * (vb - v(self.e_int)),
            self.sign * (vb - v(self.c_int)),
        )
    }

    /// Base-emitter and base-collector (charge, capacitance) pairs.
    fn charges(&self, op: &BjtOperatingPoint, vbe: f64, vbc: f64) -> [(f64, f64); 2] {
        let (qje, cje) = self.p.cje.eval(vbe);
        let (qjc, cjc) = self.p.cjc.eval(vbc);
        [
            (self.p.tf * op.cbe + qje, self.p.tf * op.gbe + cje),
            (self.p.tr * op.cbc + qjc, self.p.tr * op.gbc + cjc),
        ]
    }

    fn charges_at(&self, sol: &SolutionView) -> [f64; 2] {
        let (vbe, vbc) = self.junctions(|n| sol.voltage(n));
        let op = bjt_eval(&self.p, vbe, vbc);
        let [(qbe, _), (qbc, _)] = self.charges(&op, vbe, vbc);
        [qbe, qbc]
    }
}

impl Device for Bjt {
    fn designator(&self) -> &str {
        &self.name
    }
    fn stamp_ac(&self, s: &mut AcStamper, ctx: &AcContext) {
        for (r, a, b) in self.resistors() {
            if r > 0.0 {
                s.conductance(a, b, 1.0 / r);
            }
        }
        let (vbe, vbc) = self.junctions(|n| ctx.op.voltage(n));
        let op = bjt_eval(&self.p, vbe, vbc);
        let (c, b, e) = (self.c_int, self.b_int, self.e_int);
        let row = |g_be: f64, g_bc: f64| [(b, g_be + g_bc), (e, -g_be), (c, -g_bc)];
        s.current_row(c, &row(op.dic_dvbe, op.dic_dvbc));
        s.current_row(b, &row(op.dib_dvbe, op.dib_dvbc));
        let (ge, gc) = (op.dic_dvbe + op.dib_dvbe, op.dic_dvbc + op.dib_dvbc);
        s.current_row(e, &row(-ge, -gc));
        let [(_, cbe), (_, cbc)] = self.charges(&op, vbe, vbc);
        s.capacitance(b, e, cbe, ctx.omega);
        s.capacitance(b, c, cbc, ctx.omega);
    }
    fn internal_nodes(&self) -> Vec<&'static str> {
        let mut names = Vec::new();
        if self.p.rc > 0.0 {
            names.push("c");
        }
        if self.p.rb > 0.0 {
            names.push("b");
        }
        if self.p.re > 0.0 {
            names.push("e");
        }
        names
    }
    fn assign_internal_nodes(&mut self, first: NodeId) {
        let mut next = first.0;
        let mut take = || {
            next += 1;
            NodeId(next - 1)
        };
        if self.p.rc > 0.0 {
            self.c_int = take();
        }
        if self.p.rb > 0.0 {
            self.b_int = take();
        }
        if self.p.re > 0.0 {
            self.e_int = take();
        }
    }
    fn is_nonlinear(&self) -> bool {
        true
    }
    fn limited(&self) -> bool {
        self.limited.get()
    }
    fn reset_iteration(&self) {
        self.v_last.set(None);
    }
    fn stamp(&self, s: &mut Stamper, ctx: &StampContext) {
        for (r, a, b) in self.resistors() {
            if r > 0.0 {
                s.conductance(a, b, 1.0 / r);
            }
        }
        let (raw_be, raw_bc) = self.junctions(|n| s.iterate_voltage(n));
        let (vbe, vbc) = match self.v_last.get() {
            None => (self.vcrit_be, 0.0),
            Some((old_be, old_bc)) => (
                pnjlim(raw_be, old_be, self.p.nf * VT, self.vcrit_be),
                pnjlim(raw_bc, old_bc, self.p.nr * VT, self.vcrit_bc),
            ),
        };
        self.limited
            .set((vbe - raw_be).abs() > LIMIT_EPS || (vbc - raw_bc).abs() > LIMIT_EPS);
        self.v_last.set(Some((vbe, vbc)));

        let op = bjt_eval(&self.p, vbe, vbc);
        let t = self.sign;
        // Real terminal currents, linearised in the real Vbe = Vb - Ve and
        // Vbc = Vb - Vc. Derivatives are polarity-independent; the constant
        // terms flip sign for PNP.
        let (c, b, e) = (self.c_int, self.b_int, self.e_int);
        let ic0 = t * (op.ic - op.dic_dvbe * vbe - op.dic_dvbc * vbc);
        let ib0 = t * (op.ib - op.dib_dvbe * vbe - op.dib_dvbc * vbc);
        let row = |g_be: f64, g_bc: f64| [(b, g_be + g_bc), (e, -g_be), (c, -g_bc)];
        s.current_row(c, &row(op.dic_dvbe, op.dic_dvbc), ic0);
        s.current_row(b, &row(op.dib_dvbe, op.dib_dvbc), ib0);
        let (ge, gc) = (op.dic_dvbe + op.dib_dvbe, op.dic_dvbc + op.dib_dvbc);
        s.current_row(e, &row(-ge, -gc), -(ic0 + ib0));

        if self.p.has_charge() {
            let [(qbe, cbe), (qbc, cbc)] = self.charges(&op, vbe, vbc);
            stamp_charge(s, &self.charge_be, (b, e), (qbe, cbe, vbe), t, ctx);
            stamp_charge(s, &self.charge_bc, (b, c), (qbc, cbc, vbc), t, ctx);
        }
    }
    fn dc_paths(&self) -> Vec<(NodeId, NodeId)> {
        vec![
            (self.base, self.emitter),
            (self.base, self.collector),
            (self.collector, self.emitter),
        ]
    }
    /// Collector current (into the collector pin), including the current
    /// charging the base-collector junction.
    fn current(&self, sol: &SolutionView) -> f64 {
        let (vbe, vbc) = self.junctions(|n| sol.voltage(n));
        self.sign * (bjt_eval(&self.p, vbe, vbc).ic - self.charge_bc.i_prev)
    }
    fn ringing(&self) -> bool {
        self.charge_be.ringing || self.charge_bc.ringing
    }
    fn begin_transient(&mut self, sol: &SolutionView, time: f64) {
        let [qbe, qbc] = self.charges_at(sol);
        self.charge_be.begin(qbe, time);
        self.charge_bc.begin(qbc, time);
    }
    fn accept_step(&mut self, sol: &SolutionView, ctx: &StampContext, restart: bool) {
        if let Some((h, method)) = transient_step(ctx) {
            let [qbe, qbc] = self.charges_at(sol);
            self.charge_be.accept(qbe, ctx.time, h, method, restart);
            self.charge_bc.accept(qbc, ctx.time, h, method, restart);
        }
    }
    fn truncation_step(
        &self,
        sol: &SolutionView,
        ctx: &StampContext,
        tol: &Tolerances,
    ) -> Option<f64> {
        let (h, method) = transient_step(ctx)?;
        if !self.p.has_charge() {
            return None;
        }
        let [qbe, qbc] = self.charges_at(sol);
        let be = self
            .charge_be
            .truncation_step(qbe, ctx.time, h, method, tol);
        let bc = self
            .charge_bc
            .truncation_step(qbc, ctx.time, h, method, tol);
        [be, bc].into_iter().flatten().reduce(f64::min)
    }
}

// --------------------------------------------------------------- MOSFET ---

/// Level 1 (Shichman-Hodges) parameters. The source is tied to the body, so
/// there is no body effect. `W` and `L` default to 1, so `KP` is the
/// device's transconductance parameter β and `CGSO`/`CGDO` are the overlap
/// capacitances in farads.
#[derive(Debug, Clone, PartialEq)]
pub struct MosfetParams {
    pub vto: f64,
    pub kp: f64,
    pub lambda: f64,
    pub rd: f64,
    pub rs: f64,
    pub w_over_l: f64,
    /// Gate-source and gate-drain overlap capacitance (F).
    pub cgs: f64,
    pub cgd: f64,
    /// Bulk-drain junction (bulk is tied to the source).
    pub cbd: Depletion,
}

impl MosfetParams {
    pub fn from_model(m: &DeviceModel) -> Self {
        let w = m.get_or("W", 1.0);
        Self {
            vto: m.get_or("VTO", 0.0),
            kp: m.get_or("KP", 2e-5),
            lambda: m.get_or("LAMBDA", 0.0),
            rd: m.get_or("RD", 0.0),
            rs: m.get_or("RS", 0.0),
            w_over_l: w / m.get_or("L", 1.0),
            cgs: m.get_or("CGSO", 0.0) * w,
            cgd: m.get_or("CGDO", 0.0) * w,
            cbd: Depletion {
                cj0: m.get_or("CBD", 0.0),
                vj: m.get_or("PB", 0.8),
                m: m.get_or("MJ", 0.5),
                fc: m.get_or("FC", 0.5),
            },
        }
    }

    fn has_charge(&self) -> bool {
        self.cgs != 0.0 || self.cgd != 0.0 || !self.cbd.is_zero()
    }
}

/// Drain current and (gm, gds) for vds >= 0 in the NMOS frame, with `vto`
/// already in that frame.
pub fn mos1_eval(p: &MosfetParams, vto: f64, vgs: f64, vds: f64) -> (f64, f64, f64) {
    let beta = p.kp * p.w_over_l;
    let vgst = vgs - vto;
    if vgst <= 0.0 {
        return (0.0, 0.0, 0.0);
    }
    let clm = 1.0 + p.lambda * vds;
    if vds < vgst {
        // Linear (triode) region.
        let core = vgst * vds - 0.5 * vds * vds;
        (
            beta * core * clm,
            beta * vds * clm,
            beta * ((vgst - vds) * clm + core * p.lambda),
        )
    } else {
        // Saturation.
        let core = 0.5 * vgst * vgst;
        (beta * core * clm, beta * vgst * clm, beta * core * p.lambda)
    }
}

#[derive(Debug)]
pub struct Mosfet {
    pub name: String,
    pub drain: NodeId,
    pub gate: NodeId,
    pub source: NodeId,
    /// +1 for N-channel, -1 for P-channel.
    pub sign: f64,
    p: MosfetParams,
    d_int: NodeId,
    s_int: NodeId,
    /// Last (Vgs, Vds) used, in the NMOS frame.
    v_last: Cell<Option<(f64, f64)>>,
    limited: Cell<bool>,
    /// Gate-source, gate-drain and bulk-drain charges.
    charge_gs: ChargeStore,
    charge_gd: ChargeStore,
    charge_bd: ChargeStore,
}

impl Mosfet {
    pub fn new(
        name: String,
        [drain, gate, source]: [NodeId; 3],
        nmos: bool,
        p: MosfetParams,
    ) -> Self {
        Self {
            name,
            drain,
            gate,
            source,
            sign: if nmos { 1.0 } else { -1.0 },
            d_int: drain,
            s_int: source,
            p,
            v_last: Cell::new(None),
            limited: Cell::new(false),
            charge_gs: ChargeStore::default(),
            charge_gd: ChargeStore::default(),
            charge_bd: ChargeStore::default(),
        }
    }

    /// Threshold voltage in the NMOS frame.
    fn vto(&self) -> f64 {
        self.sign * self.p.vto
    }

    fn terminal_voltages(&self, v: impl Fn(NodeId) -> f64) -> (f64, f64) {
        let vs = v(self.s_int);
        (
            self.sign * (v(self.gate) - vs),
            self.sign * (v(self.d_int) - vs),
        )
    }

    /// Drain current and its derivatives w.r.t. (Vgs, Vds), NMOS frame,
    /// handling reverse operation (vds < 0) by swapping drain and source.
    fn eval(&self, vgs: f64, vds: f64) -> (f64, f64, f64) {
        if vds >= 0.0 {
            mos1_eval(&self.p, self.vto(), vgs, vds)
        } else {
            let (id, gm, gds) = mos1_eval(&self.p, self.vto(), vgs - vds, -vds);
            (-id, -gm, gm + gds)
        }
    }

    /// (charge, capacitance, voltage) for gate-source, gate-drain and
    /// bulk-drain, in the NMOS frame.
    fn charges(&self, vgs: f64, vds: f64) -> [(f64, f64, f64); 3] {
        let vgd = vgs - vds;
        let vbd = -vds;
        let (qbd, cbd) = self.p.cbd.eval(vbd);
        [
            (self.p.cgs * vgs, self.p.cgs, vgs),
            (self.p.cgd * vgd, self.p.cgd, vgd),
            (qbd, cbd, vbd),
        ]
    }

    fn stores(&self) -> [&ChargeStore; 3] {
        [&self.charge_gs, &self.charge_gd, &self.charge_bd]
    }

    fn charge_values(&self, sol: &SolutionView) -> [f64; 3] {
        let (vgs, vds) = self.terminal_voltages(|n| sol.voltage(n));
        self.charges(vgs, vds).map(|(q, _, _)| q)
    }
}

impl Device for Mosfet {
    fn designator(&self) -> &str {
        &self.name
    }
    fn stamp_ac(&self, s: &mut AcStamper, ctx: &AcContext) {
        if self.p.rd > 0.0 {
            s.conductance(self.drain, self.d_int, 1.0 / self.p.rd);
        }
        if self.p.rs > 0.0 {
            s.conductance(self.source, self.s_int, 1.0 / self.p.rs);
        }
        let (vgs, vds) = self.terminal_voltages(|n| ctx.op.voltage(n));
        let (_, g_gs, g_ds) = self.eval(vgs, vds);
        let (d, g, src) = (self.d_int, self.gate, self.s_int);
        s.current_row(d, &[(g, g_gs), (d, g_ds), (src, -g_gs - g_ds)]);
        s.current_row(src, &[(g, -g_gs), (d, -g_ds), (src, g_gs + g_ds)]);
        s.conductance(d, src, GMIN);
        let [(_, cgs, _), (_, cgd, _), (_, cbd, _)] = self.charges(vgs, vds);
        s.capacitance(g, src, cgs, ctx.omega);
        s.capacitance(g, d, cgd, ctx.omega);
        s.capacitance(src, d, cbd, ctx.omega);
    }
    fn internal_nodes(&self) -> Vec<&'static str> {
        let mut names = Vec::new();
        if self.p.rd > 0.0 {
            names.push("d");
        }
        if self.p.rs > 0.0 {
            names.push("s");
        }
        names
    }
    fn assign_internal_nodes(&mut self, first: NodeId) {
        let mut next = first.0;
        if self.p.rd > 0.0 {
            self.d_int = NodeId(next);
            next += 1;
        }
        if self.p.rs > 0.0 {
            self.s_int = NodeId(next);
        }
    }
    fn is_nonlinear(&self) -> bool {
        true
    }
    fn limited(&self) -> bool {
        self.limited.get()
    }
    fn reset_iteration(&self) {
        self.v_last.set(None);
    }
    fn stamp(&self, s: &mut Stamper, ctx: &StampContext) {
        if self.p.rd > 0.0 {
            s.conductance(self.drain, self.d_int, 1.0 / self.p.rd);
        }
        if self.p.rs > 0.0 {
            s.conductance(self.source, self.s_int, 1.0 / self.p.rs);
        }
        let (raw_gs, raw_ds) = self.terminal_voltages(|n| s.iterate_voltage(n));
        let vto = self.vto();
        let (vgs, vds) = match self.v_last.get() {
            None => (vto, 0.0),
            Some((old_gs, old_ds)) => {
                if old_ds >= 0.0 {
                    let vgs = fetlim(raw_gs, old_gs, vto);
                    let vds = limvds(vgs - (raw_gs - raw_ds), old_ds);
                    (vgs, vds)
                } else {
                    let vgd = fetlim(raw_gs - raw_ds, old_gs - old_ds, vto);
                    let vds = -limvds(-(raw_gs - vgd), -old_ds);
                    (vgd + vds, vds)
                }
            }
        };
        self.limited
            .set((vgs - raw_gs).abs() > LIMIT_EPS || (vds - raw_ds).abs() > LIMIT_EPS);
        self.v_last.set(Some((vgs, vds)));

        let (id, g_gs, g_ds) = self.eval(vgs, vds);
        let i0 = self.sign * (id - g_gs * vgs - g_ds * vds);
        let (d, g, src) = (self.d_int, self.gate, self.s_int);
        s.current_row(d, &[(g, g_gs), (d, g_ds), (src, -g_gs - g_ds)], i0);
        s.current_row(src, &[(g, -g_gs), (d, -g_ds), (src, g_gs + g_ds)], -i0);
        s.conductance(d, src, GMIN);

        if self.p.has_charge() {
            let pairs = [(g, src), (g, d), (src, d)];
            for ((store, nodes), qcv) in self
                .stores()
                .into_iter()
                .zip(pairs)
                .zip(self.charges(vgs, vds))
            {
                stamp_charge(s, store, nodes, qcv, self.sign, ctx);
            }
        }
    }
    fn dc_paths(&self) -> Vec<(NodeId, NodeId)> {
        vec![(self.drain, self.source)]
    }
    /// Drain current (into the drain pin), including currents charging the
    /// gate-drain and bulk-drain capacitances.
    fn current(&self, sol: &SolutionView) -> f64 {
        let (vgs, vds) = self.terminal_voltages(|n| sol.voltage(n));
        let charging = self.charge_gd.i_prev + self.charge_bd.i_prev;
        self.sign * (self.eval(vgs, vds).0 - charging)
            + GMIN * (sol.voltage(self.d_int) - sol.voltage(self.s_int))
    }
    fn ringing(&self) -> bool {
        self.stores().iter().any(|s| s.ringing)
    }
    fn begin_transient(&mut self, sol: &SolutionView, time: f64) {
        let qs = self.charge_values(sol);
        for (store, q) in [
            &mut self.charge_gs,
            &mut self.charge_gd,
            &mut self.charge_bd,
        ]
        .into_iter()
        .zip(qs)
        {
            store.begin(q, time);
        }
    }
    fn accept_step(&mut self, sol: &SolutionView, ctx: &StampContext, restart: bool) {
        if let Some((h, method)) = transient_step(ctx) {
            let qs = self.charge_values(sol);
            for (store, q) in [
                &mut self.charge_gs,
                &mut self.charge_gd,
                &mut self.charge_bd,
            ]
            .into_iter()
            .zip(qs)
            {
                store.accept(q, ctx.time, h, method, restart);
            }
        }
    }
    fn truncation_step(
        &self,
        sol: &SolutionView,
        ctx: &StampContext,
        tol: &Tolerances,
    ) -> Option<f64> {
        let (h, method) = transient_step(ctx)?;
        if !self.p.has_charge() {
            return None;
        }
        self.stores()
            .into_iter()
            .zip(self.charge_values(sol))
            .filter_map(|(store, q)| store.truncation_step(q, ctx.time, h, method, tol))
            .reduce(f64::min)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    fn finite_diff(f: impl Fn(f64) -> f64, x: f64) -> f64 {
        let h = 1e-7;
        (f(x + h) - f(x - h)) / (2.0 * h)
    }

    fn no_caps() -> Depletion {
        Depletion {
            cj0: 0.0,
            vj: 0.75,
            m: 0.33,
            fc: 0.5,
        }
    }

    #[test]
    fn bjt_derivatives_match_finite_differences() {
        let p = BjtParams {
            is: 6.734e-15,
            bf: 416.4,
            br: 0.7371,
            nf: 1.0,
            nr: 1.0,
            vaf: 74.03,
            var: 20.0,
            ikf: 0.06678,
            ikr: 0.5,
            ise: 6.734e-15,
            ne: 1.259,
            isc: 1e-14,
            nc: 2.0,
            rb: 0.0,
            rc: 0.0,
            re: 0.0,
            cje: no_caps(),
            cjc: no_caps(),
            tf: 0.0,
            tr: 0.0,
        };
        for (vbe, vbc) in [(0.65, -5.0), (0.75, 0.3), (0.5, 0.6), (0.7, -0.1)] {
            let op = bjt_eval(&p, vbe, vbc);
            let rel = |a: f64, b: f64| (a - b).abs() / b.abs().max(1e-9);
            assert!(rel(op.dic_dvbe, finite_diff(|v| bjt_eval(&p, v, vbc).ic, vbe)) < 1e-4);
            assert!(rel(op.dic_dvbc, finite_diff(|v| bjt_eval(&p, vbe, v).ic, vbc)) < 1e-4);
            assert!(rel(op.dib_dvbe, finite_diff(|v| bjt_eval(&p, v, vbc).ib, vbe)) < 1e-4);
            assert!(rel(op.dib_dvbc, finite_diff(|v| bjt_eval(&p, vbe, v).ib, vbc)) < 1e-4);
        }
    }

    #[test]
    fn mosfet_derivatives_match_finite_differences() {
        let p = MosfetParams {
            vto: 1.5,
            kp: 0.1,
            lambda: 0.02,
            rd: 0.0,
            rs: 0.0,
            w_over_l: 1.0,
            cgs: 0.0,
            cgd: 0.0,
            cbd: no_caps(),
        };
        let m = Mosfet::new("M1".into(), [NodeId(1), NodeId(2), NodeId(0)], true, p);
        for (vgs, vds) in [(3.0, 0.5), (3.0, 5.0), (2.0, -0.3), (4.0, -2.0)] {
            let (_, gm, gds) = m.eval(vgs, vds);
            assert_relative_eq!(
                gm,
                finite_diff(|v| m.eval(v, vds).0, vgs),
                max_relative = 1e-5
            );
            assert_relative_eq!(
                gds,
                finite_diff(|v| m.eval(vgs, v).0, vds),
                max_relative = 1e-5
            );
        }
    }

    fn zener() -> Diode {
        Diode::new(
            "D1".into(),
            NodeId(1),
            NodeId(0),
            DiodeParams {
                bv: Some(5.1),
                tt: 5e-9,
                depletion: Depletion {
                    cj0: 4e-12,
                    vj: 0.7,
                    m: 0.4,
                    fc: 0.5,
                },
                ..DiodeParams::simple(1e-14, 1.5)
            },
        )
    }

    #[test]
    fn diode_conductance_matches_derivative() {
        let d = zener();
        for vd in [0.7, 0.1, -0.05, -1.0, -5.0, -5.2] {
            let (_, gd) = d.eval(vd);
            assert_relative_eq!(gd, finite_diff(|v| d.eval(v).0, vd), max_relative = 1e-4);
        }
    }

    #[test]
    fn breakdown_current_at_bv_is_ibv() {
        let d = Diode::new(
            "D1".into(),
            NodeId(1),
            NodeId(0),
            DiodeParams {
                bv: Some(5.1),
                ..DiodeParams::simple(1e-14, 1.0)
            },
        );
        let (i, _) = d.eval(-5.1);
        assert_relative_eq!(i, -1e-3, max_relative = 1e-2);
    }

    #[test]
    fn pnjlim_limits_large_forward_steps() {
        let nvt = VT;
        let vc = vcrit(1e-14, nvt);
        let limited = pnjlim(5.0, 0.6, nvt, vc);
        assert!(limited < 0.8 && limited > 0.6, "{limited}");
        assert_eq!(pnjlim(0.3, 0.29, nvt, vc), 0.3);
    }

    #[test]
    fn depletion_capacitance_is_derivative_of_charge() {
        for dep in [
            Depletion {
                cj0: 1e-12,
                vj: 0.75,
                m: 0.33,
                fc: 0.5,
            },
            Depletion {
                cj0: 2e-12,
                vj: 1.0,
                m: 0.5,
                fc: 0.5,
            },
            Depletion {
                cj0: 1e-12,
                vj: 0.8,
                m: 1.0,
                fc: 0.5,
            },
        ] {
            // Reverse bias, just below the knee, above it, and well forward.
            for v in [-10.0, -1.0, 0.0, 0.3, 0.37, 0.45, 0.9] {
                let (_, c) = dep.eval(v);
                let dq = finite_diff(|x| dep.eval(x).0, v);
                assert_relative_eq!(c, dq, max_relative = 1e-5);
            }
            // Charge and capacitance are continuous at the knee.
            let knee = dep.fc * dep.vj;
            let (below, above) = (dep.eval(knee - 1e-9), dep.eval(knee + 1e-9));
            assert_relative_eq!(below.0, above.0, max_relative = 1e-6);
            assert_relative_eq!(below.1, above.1, max_relative = 1e-6);
        }
        // Zero-bias value is CJ0, and capacitance falls with reverse bias.
        let dep = Depletion {
            cj0: 1e-12,
            vj: 0.75,
            m: 0.33,
            fc: 0.5,
        };
        assert_relative_eq!(dep.eval(0.0).1, 1e-12);
        assert!(dep.eval(-5.0).1 < dep.eval(-1.0).1);
    }

    #[test]
    fn diode_capacitance_is_derivative_of_charge() {
        let d = zener();
        for vd in [-3.0, -0.5, 0.2, 0.6, 0.7] {
            let (_, c) = d.charge(vd);
            assert_relative_eq!(c, finite_diff(|v| d.charge(v).0, vd), max_relative = 1e-4);
        }
    }
}
