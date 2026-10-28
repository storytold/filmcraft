//! Small, bounded numerical kernels shared by temporal factorisation and tracking.
//!
//! The temporal path deliberately avoids a dependency on a general-purpose linear algebra
//! package.  Both routines below operate on fixed-size arrays, validate every arithmetic result,
//! and use a fixed upper bound on their iterations.

const MAX_DIM: usize = 50;
const PIVOT_EPSILON: f64 = 1.0e-12;
const EIGEN_EPSILON: f64 = 1.0e-13;

/// Eigen-decompose a finite real symmetric matrix.
///
/// The returned eigenvalues are ordered from largest to smallest and each eigenvector is a
/// *column* of the returned matrix.  Jacobi rotations are especially suitable here: the matrix
/// dimensions used by moving factorisation are at most fifty, and the method has no dynamic
/// workspaces or convergence-dependent allocations.
pub(super) fn symmetric_eigen<const N: usize>(matrix: [[f64; N]; N]) -> Option<([f64; N], [[f64; N]; N])> {
    if N > MAX_DIM {
        return None;
    }
    if matrix.iter().flatten().any(|value| !value.is_finite()) {
        return None;
    }

    let mut a = [[0.0f64; N]; N];
    // Accept the tiny asymmetry introduced by accumulated sums, while making the Jacobi update
    // exactly symmetric.  A grossly asymmetric input is not a covariance matrix; averaging it
    // here is safer than allowing one triangle to silently disagree with the other.
    for row in 0..N {
        for col in 0..N {
            let value = (matrix[row][col] + matrix[col][row]) * 0.5;
            if !value.is_finite() {
                return None;
            }
            a[row][col] = value;
        }
    }

    let mut vectors = [[0.0f64; N]; N];
    for i in 0..N {
        vectors[i][i] = 1.0;
    }

    if N > 1 {
        // A Jacobi sweep visits every pair once.  The multiplier is a fixed bound (rather than a
        // tolerance-driven while loop) so hostile inputs cannot make analysis unbounded.
        let max_sweeps = 16usize.saturating_mul(N).max(16);
        let mut converged = false;
        for _ in 0..max_sweeps {
            let mut largest = 0.0f64;
            let mut scale = 0.0f64;
            for i in 0..N {
                scale = scale.max(a[i][i].abs());
                for j in (i + 1)..N {
                    largest = largest.max(a[i][j].abs());
                }
            }
            if !largest.is_finite() || !scale.is_finite() {
                return None;
            }
            if largest <= EIGEN_EPSILON * scale.max(1.0) {
                converged = true;
                break;
            }

            for p in 0..N {
                for q in (p + 1)..N {
                    let apq = a[p][q];
                    if !apq.is_finite() {
                        return None;
                    }
                    // Recheck the pair because earlier rotations in this sweep may have made it
                    // negligible.  This also avoids dividing by a denormal value.
                    if apq.abs() <= EIGEN_EPSILON * scale.max(1.0) {
                        continue;
                    }
                    let app = a[p][p];
                    let aqq = a[q][q];
                    let theta = (aqq - app) / (2.0 * apq);
                    let theta_squared = theta * theta;
                    if !theta.is_finite() || !theta_squared.is_finite() {
                        return None;
                    }
                    let sign = if theta < 0.0 { -1.0 } else { 1.0 };
                    let t = sign / (theta.abs() + (1.0 + theta_squared).sqrt());
                    let c = 1.0 / (1.0 + t * t).sqrt();
                    let s = t * c;
                    if !c.is_finite() || !s.is_finite() {
                        return None;
                    }

                    // Apply A <- Gᵀ A G.  Updating both triangles from the same old pair keeps
                    // round-off from making the next sweep non-symmetric.
                    for k in 0..N {
                        if k == p || k == q {
                            continue;
                        }
                        let akp = a[k][p];
                        let akq = a[k][q];
                        let new_kp = c * akp - s * akq;
                        let new_kq = s * akp + c * akq;
                        if !new_kp.is_finite() || !new_kq.is_finite() {
                            return None;
                        }
                        a[k][p] = new_kp;
                        a[p][k] = new_kp;
                        a[k][q] = new_kq;
                        a[q][k] = new_kq;
                    }
                    let new_app = c * c * app - 2.0 * s * c * apq + s * s * aqq;
                    let new_aqq = s * s * app + 2.0 * s * c * apq + c * c * aqq;
                    if !new_app.is_finite() || !new_aqq.is_finite() {
                        return None;
                    }
                    a[p][p] = new_app;
                    a[q][q] = new_aqq;
                    a[p][q] = 0.0;
                    a[q][p] = 0.0;

                    for k in 0..N {
                        let vkp = vectors[k][p];
                        let vkq = vectors[k][q];
                        let new_vkp = c * vkp - s * vkq;
                        let new_vkq = s * vkp + c * vkq;
                        if !new_vkp.is_finite() || !new_vkq.is_finite() {
                            return None;
                        }
                        vectors[k][p] = new_vkp;
                        vectors[k][q] = new_vkq;
                    }
                }
            }
        }
        if !converged {
            let residual = (0..N).flat_map(|i| ((i + 1)..N).map(move |j| a[i][j].abs())).fold(0.0, f64::max);
            let diagonal = (0..N).map(|i| a[i][i].abs()).fold(0.0, f64::max);
            if !residual.is_finite() || residual > EIGEN_EPSILON * diagonal.max(1.0) {
                return None;
            }
        }
    }

    let mut values = [0.0f64; N];
    for i in 0..N {
        values[i] = a[i][i];
    }

    // Sort eigenpairs together.  N is bounded by fifty, so selection sort is predictable and
    // avoids a temporary vector.  Equal values keep their Jacobi order for deterministic output.
    for i in 0..N {
        let mut best = i;
        for j in (i + 1)..N {
            if values[j] > values[best] {
                best = j;
            }
        }
        if best != i {
            values.swap(i, best);
            for row in 0..N {
                vectors[row].swap(i, best);
            }
        }
    }
    canonicalise_eigenvectors(&values, &mut vectors)?;
    Some((values, vectors))
}

fn canonicalise_eigenvectors<const N: usize>(values: &[f64; N], vectors: &mut [[f64; N]; N]) -> Option<()> {
    for col in 0..N {
        let mut norm = 0.0;
        let mut pivot = 0usize;
        let mut pivot_abs = 0.0;
        for row in 0..N {
            let value = vectors[row][col];
            if !value.is_finite() {
                return None;
            }
            norm += value * value;
            if !norm.is_finite() {
                return None;
            }
            if value.abs() > pivot_abs {
                pivot_abs = value.abs();
                pivot = row;
            }
        }
        if !norm.is_finite() || (N > 0 && norm <= f64::MIN_POSITIVE) {
            return None;
        }
        let inv_norm = 1.0 / norm.sqrt();
        if !inv_norm.is_finite() {
            return None;
        }
        for row in 0..N {
            vectors[row][col] *= inv_norm;
        }
        if N > 0 && vectors[pivot][col] < 0.0 {
            for row in 0..N {
                vectors[row][col] = -vectors[row][col];
            }
        }
    }
    if values.iter().any(|value| !value.is_finite()) {
        return None;
    }
    Some(())
}

/// Solve a finite square linear system with partial pivoting.
///
/// Rows are scaled before elimination, making the same bounded kernel useful for the covariance
/// systems in Eq. 7/Eq. 8 even when image coordinates and track weights have very different
/// magnitudes.  Singular and numerically indeterminate systems return `None` rather than
/// producing an unbounded coefficient.
pub(super) fn solve<const N: usize>(matrix: [[f64; N]; N], rhs: [f64; N]) -> Option<[f64; N]> {
    if N > MAX_DIM || matrix.iter().flatten().any(|value| !value.is_finite()) || rhs.iter().any(|value| !value.is_finite()) {
        return None;
    }
    if N == 0 {
        return Some([0.0; N]);
    }

    let mut a = matrix;
    let mut b = rhs;
    for row in 0..N {
        let scale = a[row].iter().map(|value| value.abs()).fold(0.0, f64::max);
        if !scale.is_finite() || scale <= f64::MIN_POSITIVE {
            return None;
        }
        for col in 0..N {
            a[row][col] /= scale;
        }
        b[row] /= scale;
        if !a[row].iter().all(|value| value.is_finite()) || !b[row].is_finite() {
            return None;
        }
    }

    for col in 0..N {
        let mut pivot = col;
        let mut magnitude = a[col][col].abs();
        for row in (col + 1)..N {
            let candidate = a[row][col].abs();
            if candidate > magnitude {
                magnitude = candidate;
                pivot = row;
            }
        }
        if !magnitude.is_finite() || magnitude <= PIVOT_EPSILON {
            return None;
        }
        if pivot != col {
            a.swap(pivot, col);
            b.swap(pivot, col);
        }
        let diagonal = a[col][col];
        for row in (col + 1)..N {
            let factor = a[row][col] / diagonal;
            if !factor.is_finite() {
                return None;
            }
            a[row][col] = 0.0;
            for k in (col + 1)..N {
                let value = a[row][k] - factor * a[col][k];
                if !value.is_finite() {
                    return None;
                }
                a[row][k] = value;
            }
            b[row] -= factor * b[col];
            if !b[row].is_finite() {
                return None;
            }
        }
    }

    let mut result = [0.0f64; N];
    for row in (0..N).rev() {
        let mut value = b[row];
        for col in (row + 1)..N {
            value -= a[row][col] * result[col];
        }
        let diagonal = a[row][row];
        if !value.is_finite() || !diagonal.is_finite() || diagonal.abs() <= PIVOT_EPSILON {
            return None;
        }
        result[row] = value / diagonal;
        if !result[row].is_finite() {
            return None;
        }
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::{solve, symmetric_eigen};

    #[test]
    fn symmetric_eigen_returns_columns_and_descending_values() {
        let (values, vectors) = symmetric_eigen([[3.0, 1.0], [1.0, 3.0]]).expect("finite symmetric matrix");
        assert!(values[0] >= values[1]);
        assert!((values[0] - 4.0).abs() < 1.0e-9);
        assert!(vectors[0][0].hypot(vectors[1][0]) > 0.99);
        let first = [vectors[0][0], vectors[1][0]];
        let second = [vectors[0][1], vectors[1][1]];
        assert!(first[0] * first[1] >= 0.0);
        assert!((first[0] * second[0] + first[1] * second[1]).abs() < 1.0e-9);
    }

    #[test]
    fn solve_scales_rows_and_rejects_singular_or_nonfinite_systems() {
        let result = solve([[1.0e-9, 2.0e-9], [3.0, 4.0]], [3.0e-9, 7.0]).expect("well-conditioned system");
        assert!((result[0] - 1.0).abs() < 1.0e-8);
        assert!((result[1] - 1.0).abs() < 1.0e-8);
        assert!(solve([[1.0, 2.0], [2.0, 4.0]], [1.0, 2.0]).is_none());
        assert!(solve([[f64::NAN, 0.0], [0.0, 1.0]], [0.0, 1.0]).is_none());
        assert!(symmetric_eigen([[f64::INFINITY, 0.0], [0.0, 1.0]]).is_none());
    }
}
