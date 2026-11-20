//! Linear algebra for the MNA system `A x = b`.
//!
//! The engine talks to a [`LinearSolver`] so the backend can change without
//! touching device code. Today there is one implementation, a dense LU with
//! partial pivoting, which is fast for the circuit sizes Dunbrec targets
//! (tens to a few hundred unknowns). A sparse backend can be added behind the
//! same trait when larger circuits need it.

/// Row-major dense square matrix.
#[derive(Debug, Clone, PartialEq)]
pub struct DenseMatrix {
    n: usize,
    data: Vec<f64>,
}

impl DenseMatrix {
    pub fn zeros(n: usize) -> Self {
        Self {
            n,
            data: vec![0.0; n * n],
        }
    }

    pub fn size(&self) -> usize {
        self.n
    }

    pub fn clear(&mut self) {
        self.data.fill(0.0);
    }

    #[inline]
    pub fn get(&self, r: usize, c: usize) -> f64 {
        self.data[r * self.n + c]
    }

    #[inline]
    pub fn add(&mut self, r: usize, c: usize, v: f64) {
        self.data[r * self.n + c] += v;
    }

    fn row_mut(&mut self, r: usize) -> &mut [f64] {
        &mut self.data[r * self.n..(r + 1) * self.n]
    }

    fn swap_rows(&mut self, a: usize, b: usize) {
        if a != b {
            for c in 0..self.n {
                self.data.swap(a * self.n + c, b * self.n + c);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("matrix is singular at unknown {index}")]
pub struct Singular {
    /// Index of the unknown (column) for which no usable pivot was found.
    pub index: usize,
}

/// Solves `A x = b`. Implementations may keep factorisation state between
/// calls so repeated solves with the same sparsity pattern are cheap.
pub trait LinearSolver: Send {
    /// Solve in place: on success `rhs` holds `x`. `matrix` may be destroyed.
    fn solve(&mut self, matrix: &mut DenseMatrix, rhs: &mut [f64]) -> Result<(), Singular>;
}

/// Dense LU factorisation with partial (row) pivoting.
#[derive(Debug, Default)]
pub struct DenseLu {
    perm: Vec<usize>,
}

/// A pivot smaller than this fraction of the largest matrix entry is treated
/// as zero. MNA matrices legitimately span ~1e-12 (gmin, open switches) to
/// ~1e3 (milliohm resistors), so the threshold must be very small.
const PIVOT_RELATIVE_TOLERANCE: f64 = 1e-18;

impl LinearSolver for DenseLu {
    fn solve(&mut self, a: &mut DenseMatrix, b: &mut [f64]) -> Result<(), Singular> {
        let n = a.size();
        assert_eq!(b.len(), n, "rhs length must match matrix size");
        let max_abs = a.data.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
        let tiny = max_abs * PIVOT_RELATIVE_TOLERANCE;

        self.perm.clear();
        self.perm.extend(0..n);

        // Factorise A = P L U in place (L unit lower, stored below diagonal).
        for k in 0..n {
            let (p, pivot) = (k..n)
                .map(|r| (r, a.get(r, k).abs()))
                .fold(
                    (k, -1.0),
                    |best, cur| if cur.1 > best.1 { cur } else { best },
                );
            if pivot <= tiny || pivot == 0.0 {
                return Err(Singular { index: k });
            }
            a.swap_rows(k, p);
            b.swap(k, p);
            self.perm.swap(k, p);

            let akk = a.get(k, k);
            for r in k + 1..n {
                let factor = a.get(r, k) / akk;
                if factor == 0.0 {
                    continue;
                }
                a.data[r * n + k] = factor;
                // Row r -= factor * row k, for columns k+1..n.
                let (upper, lower) = a.data.split_at_mut(r * n);
                let row_k = &upper[k * n..k * n + n];
                let row_r = &mut lower[..n];
                for c in k + 1..n {
                    row_r[c] -= factor * row_k[c];
                }
                b[r] -= factor * b[k];
            }
        }

        // Back substitution with U.
        for k in (0..n).rev() {
            let row = a.row_mut(k);
            let mut sum = b[k];
            for c in k + 1..n {
                sum -= row[c] * b[c];
            }
            b[k] = sum / row[k];
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    fn matrix(rows: &[&[f64]]) -> DenseMatrix {
        let mut m = DenseMatrix::zeros(rows.len());
        for (r, row) in rows.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                m.add(r, c, *v);
            }
        }
        m
    }

    #[test]
    fn solves_with_pivoting() {
        // First pivot is zero, so row exchange is required.
        let mut a = matrix(&[&[0.0, 2.0, 1.0], &[1.0, 1.0, 0.0], &[2.0, 0.0, 3.0]]);
        // Compute b from a known x so the test is self-checking.
        let x = [1.0, 2.0, 3.0];
        let mut b = vec![0.0; 3];
        let orig = a.clone();
        for (r, br) in b.iter_mut().enumerate() {
            *br = (0..3).map(|c| orig.get(r, c) * x[c]).sum();
        }
        DenseLu::default().solve(&mut a, &mut b).unwrap();
        for i in 0..3 {
            assert_relative_eq!(b[i], x[i], epsilon = 1e-12);
        }
    }

    #[test]
    fn detects_singular() {
        let mut a = matrix(&[&[1.0, 2.0], &[2.0, 4.0]]);
        let mut b = vec![1.0, 2.0];
        assert_eq!(
            DenseLu::default().solve(&mut a, &mut b),
            Err(Singular { index: 1 })
        );
    }

    #[test]
    fn wide_dynamic_range() {
        // 1 TΩ in parallel-ish with 1 mΩ: entries 1e-12 and 1e3.
        let mut a = matrix(&[&[1e3 + 1e-12, -1e3], &[-1e3, 1e3 + 1e-12]]);
        let mut b = vec![1.0, 0.0];
        DenseLu::default().solve(&mut a, &mut b).unwrap();
        assert!(b.iter().all(|v| v.is_finite()));
    }
}
