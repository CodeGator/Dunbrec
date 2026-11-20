//! Nonlinear semiconductor devices: diode, BJT and MOSFET.
//!
//! Each device is linearised around the current Newton iterate: it computes
//! its terminal currents and their derivatives at the iterate's voltages and
//! stamps the tangent line (conductances plus a constant current). Junction
//! voltages are limited between iterations (SPICE's `pnjlim`/`fetlim`) so the
//! exponential curves cannot throw the iteration far off.
//!
//! Model parameters use SPICE names and defaults, at a fixed temperature of
//! 27 °C. Charge storage (junction capacitance, transit time) is not
//! modelled yet, so results are accurate at low and moderate frequencies.

use std::cell::Cell;

use dunbrec_core::{DeviceModel, NodeId};

use crate::device::{Device, StampContext};
use crate::stamp::{SolutionView, Stamper};

/// Thermal voltage kT/q at 27 °C.
pub const VT: f64 = 1.380_649e-23 * 300.15 / 1.602_176_634e-19;
/// Minimum conductance placed across every junction (SPICE GMIN).
pub const GMIN: f64 = 1e-12;
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

// ---------------------------------------------------------------- Diode ---

#[derive(Debug, Clone, PartialEq)]
pub struct DiodeParams {
    pub is: f64,
    pub n: f64,
    pub rs: f64,
    /// Reverse breakdown voltage (positive), `None` if not modelled.
    pub bv: Option<f64>,
    pub ibv: f64,
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
        }
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
    fn stamp(&self, s: &mut Stamper, _ctx: &StampContext) {
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
        s.conductance(self.anode_int, self.cathode, gd);
        s.current(self.anode_int, self.cathode, id - gd * vd);
    }
    fn dc_paths(&self) -> Vec<(NodeId, NodeId)> {
        vec![(self.anode, self.cathode)]
    }
    fn current(&self, sol: &SolutionView) -> f64 {
        self.eval(sol.voltage(self.anode_int) - sol.voltage(self.cathode))
            .0
    }
}

// ------------------------------------------------------------------ BJT ---

/// DC Gummel-Poon parameters (SPICE names and defaults).
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
}

impl BjtParams {
    pub fn from_model(m: &DeviceModel) -> Self {
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
        }
    }
}

/// Terminal currents (into collector and base) and their derivatives with
/// respect to Vbe and Vbc, in the NPN frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BjtOperatingPoint {
    pub ic: f64,
    pub ib: f64,
    pub dic_dvbe: f64,
    pub dic_dvbc: f64,
    pub dib_dvbe: f64,
    pub dib_dvbc: f64,
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
    let q1_den = 1.0 - vbc / p.vaf - vbe / p.var;
    let q1 = 1.0 / q1_den;
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
}

impl Device for Bjt {
    fn designator(&self) -> &str {
        &self.name
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
    fn stamp(&self, s: &mut Stamper, _ctx: &StampContext) {
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
    }
    fn dc_paths(&self) -> Vec<(NodeId, NodeId)> {
        vec![
            (self.base, self.emitter),
            (self.base, self.collector),
            (self.collector, self.emitter),
        ]
    }
    /// Collector current (into the collector pin).
    fn current(&self, sol: &SolutionView) -> f64 {
        let (vbe, vbc) = self.junctions(|n| sol.voltage(n));
        self.sign * bjt_eval(&self.p, vbe, vbc).ic
    }
}

// --------------------------------------------------------------- MOSFET ---

/// Level 1 (Shichman-Hodges) parameters. The source is tied to the body, so
/// there is no body effect. `W/L` defaults to 1, so `KP` is the device's
/// transconductance parameter β.
#[derive(Debug, Clone, PartialEq)]
pub struct MosfetParams {
    pub vto: f64,
    pub kp: f64,
    pub lambda: f64,
    pub rd: f64,
    pub rs: f64,
    pub w_over_l: f64,
}

impl MosfetParams {
    pub fn from_model(m: &DeviceModel) -> Self {
        Self {
            vto: m.get_or("VTO", 0.0),
            kp: m.get_or("KP", 2e-5),
            lambda: m.get_or("LAMBDA", 0.0),
            rd: m.get_or("RD", 0.0),
            rs: m.get_or("RS", 0.0),
            w_over_l: m.get_or("W", 1.0) / m.get_or("L", 1.0),
        }
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
}

impl Device for Mosfet {
    fn designator(&self) -> &str {
        &self.name
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
    fn stamp(&self, s: &mut Stamper, _ctx: &StampContext) {
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
    }
    fn dc_paths(&self) -> Vec<(NodeId, NodeId)> {
        vec![(self.drain, self.source)]
    }
    /// Drain current (into the drain pin).
    fn current(&self, sol: &SolutionView) -> f64 {
        let (vgs, vds) = self.terminal_voltages(|n| sol.voltage(n));
        self.sign * self.eval(vgs, vds).0
            + GMIN * (sol.voltage(self.d_int) - sol.voltage(self.s_int))
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

    #[test]
    fn diode_conductance_matches_derivative() {
        let d = Diode::new(
            "D1".into(),
            NodeId(1),
            NodeId(0),
            DiodeParams {
                is: 1e-14,
                n: 1.5,
                rs: 0.0,
                bv: Some(5.1),
                ibv: 1e-3,
            },
        );
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
                is: 1e-14,
                n: 1.0,
                rs: 0.0,
                bv: Some(5.1),
                ibv: 1e-3,
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
}
