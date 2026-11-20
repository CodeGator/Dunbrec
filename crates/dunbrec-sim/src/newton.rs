//! Newton-Raphson iteration and DC convergence aids.
//!
//! Nonlinear devices are linearised around the current guess; the linear
//! system is solved; the solution becomes the next guess. Iteration stops
//! when no unknown changes by more than its tolerance and no device had to
//! limit a junction voltage.
//!
//! If plain Newton fails at the DC operating point, two classic SPICE
//! fallbacks are tried in turn:
//! * **gmin stepping**: a conductance from every node to ground makes the
//!   problem easy; it is reduced decade by decade, each solve starting from
//!   the previous answer;
//! * **source stepping**: all sources are ramped up from zero, again
//!   reusing each answer as the next starting point.

use crate::circuit::Circuit;
use crate::device::{StampContext, Tolerances};
use crate::error::SimError;
use crate::linalg::{DenseLu, DenseMatrix};
use crate::stamp::node_rows;

/// Iteration limit for DC solves (SPICE ITL1).
pub const DC_MAX_ITERATIONS: usize = 100;
/// Iteration limit for one transient time point before the step is cut.
pub const TRANSIENT_MAX_ITERATIONS: usize = 30;

/// Scratch space reused across solves.
pub(crate) struct Workspace {
    matrix: DenseMatrix,
    x_new: Vec<f64>,
    solver: DenseLu,
}

impl Workspace {
    pub fn new(size: usize) -> Self {
        Self {
            matrix: DenseMatrix::zeros(size),
            x_new: vec![0.0; size],
            solver: DenseLu::default(),
        }
    }
}

/// Iterate from the guess in `x` until converged; on success `x` holds the
/// solution and the number of iterations is returned.
pub(crate) fn newton(
    circuit: &Circuit,
    ctx: &StampContext,
    x: &mut Vec<f64>,
    ws: &mut Workspace,
    max_iterations: usize,
    tol: &Tolerances,
) -> Result<usize, SimError> {
    let nonlinear = circuit.is_nonlinear();
    let rows = node_rows(circuit.node_count());
    let mut worst = 0;
    // Once the tolerances are met, one more iteration is taken: Newton
    // converges quadratically, so it shrinks the remaining error to almost
    // nothing for the cost of a single extra solve.
    let mut polishing = false;
    for iteration in 1..=max_iterations {
        circuit.solve_linearised(ctx, x, &mut ws.matrix, &mut ws.x_new, &mut ws.solver)?;
        if !nonlinear {
            std::mem::swap(x, &mut ws.x_new);
            return Ok(iteration);
        }
        let limited = circuit.devices.iter().any(|d| d.limited());
        let mut converged = !limited;
        let mut worst_excess = 0.0;
        for (i, (new, old)) in ws.x_new.iter().zip(x.iter()).enumerate() {
            let abs = if i < rows { tol.vntol } else { tol.abstol };
            let allowed = tol.reltol * new.abs().max(old.abs()) + abs;
            let excess = (new - old).abs() / allowed;
            if !excess.is_finite() || excess > 1.0 {
                converged = false;
            }
            if excess > worst_excess || !excess.is_finite() {
                worst_excess = excess;
                worst = i;
            }
        }
        std::mem::swap(x, &mut ws.x_new);
        if converged {
            if polishing {
                return Ok(iteration);
            }
            polishing = true;
        }
    }
    Err(SimError::NoConvergence {
        unknown: circuit.describe_unknown(worst),
    })
}

/// Find a DC operating point, falling back to gmin and source stepping.
pub(crate) fn dc_solve(
    circuit: &Circuit,
    initial_conditions: bool,
    ws: &mut Workspace,
    tol: &Tolerances,
) -> Result<Vec<f64>, SimError> {
    dc_solve_with(circuit, initial_conditions, ws, tol, DC_MAX_ITERATIONS).map(|(x, _)| x)
}

/// Which strategy found the DC solution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DcStrategy {
    Newton,
    GminStepping,
    SourceStepping,
}

pub(crate) fn dc_solve_with(
    circuit: &Circuit,
    initial_conditions: bool,
    ws: &mut Workspace,
    tol: &Tolerances,
    max_iterations: usize,
) -> Result<(Vec<f64>, DcStrategy), SimError> {
    let base = StampContext::dc(initial_conditions);
    let size = circuit.unknown_count();
    let reset = || circuit.devices.iter().for_each(|d| d.reset_iteration());

    reset();
    let mut x = vec![0.0; size];
    let first_error = match newton(circuit, &base, &mut x, ws, max_iterations, tol) {
        Ok(_) => return Ok((x, DcStrategy::Newton)),
        Err(e @ SimError::NoConvergence { .. }) => e,
        Err(other) => return Err(other),
    };

    // gmin stepping.
    reset();
    x.fill(0.0);
    let mut stepped = true;
    for exponent in 2..=12 {
        let ctx = StampContext {
            gmin: 10f64.powi(-exponent),
            ..base
        };
        if newton(circuit, &ctx, &mut x, ws, max_iterations, tol).is_err() {
            stepped = false;
            break;
        }
    }
    if stepped && newton(circuit, &base, &mut x, ws, max_iterations, tol).is_ok() {
        return Ok((x, DcStrategy::GminStepping));
    }

    // Source stepping.
    reset();
    x.fill(0.0);
    let mut good = x.clone();
    let (mut scale, mut step) = (0.0_f64, 0.1_f64);
    while scale < 1.0 {
        let next = (scale + step).min(1.0);
        let ctx = StampContext {
            source_scale: next,
            ..base
        };
        if newton(circuit, &ctx, &mut x, ws, max_iterations, tol).is_ok() {
            scale = next;
            good.copy_from_slice(&x);
            step = (step * 2.0).min(0.5);
        } else {
            x.copy_from_slice(&good);
            step /= 4.0;
            if step < 1e-4 {
                return Err(first_error);
            }
        }
    }
    Ok((x, DcStrategy::SourceStepping))
}
