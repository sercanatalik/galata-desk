//! Covariance of a table of returns, three ways.
//!
//! - Sample: every return weighs the same; unbiased (divides by T − 1).
//! - EWMA: RiskMetrics' recursion, `S = λS + (1 − λ) r rᵀ`, zero mean, so
//!   the latest returns weigh most. Seeded with the sample covariance of the
//!   first quarter of the window (at least ten returns).
//! - Ledoit-Wolf: the sample covariance pulled toward a target where every
//!   pair has the average correlation, by the closed-form intensity of
//!   Ledoit & Wolf, "Honey, I shrunk the sample covariance matrix" (2004).
//!   It steadies a matrix estimated from few returns per market.

use super::Matrix;

/// RiskMetrics' decay for one-day risk.
pub const EWMA_LAMBDA: f64 = 0.94;

/// Which covariance estimate the figures use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Estimator {
    #[default]
    Ewma,
    Sample,
    LedoitWolf,
}

impl Estimator {
    pub const ALL: [Estimator; 3] = [Estimator::Ewma, Estimator::Sample, Estimator::LedoitWolf];

    pub fn label(self) -> &'static str {
        match self {
            Estimator::Ewma => "EWMA λ0.94",
            Estimator::Sample => "Sample",
            Estimator::LedoitWolf => "Ledoit-Wolf",
        }
    }
}

/// The covariance of `r` (T rows × N columns) by `estimator`. `None` with
/// fewer than two returns.
pub fn estimate(r: &Matrix, estimator: Estimator) -> Option<Matrix> {
    match estimator {
        Estimator::Sample => sample(r),
        Estimator::Ewma => ewma(r, EWMA_LAMBDA),
        Estimator::LedoitWolf => ledoit_wolf(r).map(|(m, _)| m),
    }
}

fn means(r: &Matrix) -> Vec<f64> {
    let t = r.rows() as f64;
    (0..r.cols())
        .map(|j| (0..r.rows()).map(|i| r.get(i, j)).sum::<f64>() / t)
        .collect()
}

/// Mean-demeaned cross products of rows `0..rows`, divided by `div`.
fn cross(r: &Matrix, rows: usize, mean: &[f64], div: f64) -> Matrix {
    let n = r.cols();
    let mut s = Matrix::zeros(n, n);
    for a in 0..n {
        for b in a..n {
            let v = (0..rows)
                .map(|t| (r.get(t, a) - mean[a]) * (r.get(t, b) - mean[b]))
                .sum::<f64>()
                / div;
            s.set(a, b, v);
            s.set(b, a, v);
        }
    }
    s
}

pub fn sample(r: &Matrix) -> Option<Matrix> {
    let t = r.rows();
    if t < 2 {
        return None;
    }
    Some(cross(r, t, &means(r), (t - 1) as f64))
}

pub fn ewma(r: &Matrix, lambda: f64) -> Option<Matrix> {
    let t = r.rows();
    if t < 2 {
        return None;
    }
    let seed_rows = (t / 4).max(10).min(t).max(2);
    let seed = Matrix::from_rows(
        &(0..seed_rows)
            .map(|i| r.row(i).to_vec())
            .collect::<Vec<_>>(),
    );
    let mut s = sample(&seed)?;
    let n = r.cols();
    for i in seed_rows..t {
        let row = r.row(i);
        for a in 0..n {
            for b in 0..n {
                let v = lambda * s.get(a, b) + (1.0 - lambda) * row[a] * row[b];
                s.set(a, b, v);
            }
        }
    }
    Some(s)
}

/// The shrunk covariance and the intensity used, in [0, 1].
pub fn ledoit_wolf(r: &Matrix) -> Option<(Matrix, f64)> {
    let t = r.rows();
    let n = r.cols();
    if t < 2 {
        return None;
    }
    let tf = t as f64;
    let mean = means(r);
    let x: Vec<Vec<f64>> = (0..t)
        .map(|i| (0..n).map(|j| r.get(i, j) - mean[j]).collect())
        .collect();
    let s = cross(r, t, &mean, tf);
    let sd: Vec<f64> = (0..n).map(|i| s.get(i, i).max(0.0).sqrt()).collect();

    let mut sum_r = 0.0;
    let mut pairs = 0.0;
    for i in 0..n {
        for j in 0..n {
            if i != j && sd[i] > 0.0 && sd[j] > 0.0 {
                sum_r += s.get(i, j) / (sd[i] * sd[j]);
                pairs += 1.0;
            }
        }
    }
    let rbar = if pairs > 0.0 { sum_r / pairs } else { 0.0 };

    let mut f = Matrix::zeros(n, n);
    for i in 0..n {
        for j in 0..n {
            f.set(
                i,
                j,
                if i == j {
                    s.get(i, i)
                } else {
                    rbar * sd[i] * sd[j]
                },
            );
        }
    }

    // pi: the summed asymptotic variances of the sample covariances.
    let mut pi = 0.0;
    let mut pi_diag = 0.0;
    for i in 0..n {
        for j in 0..n {
            let p = x
                .iter()
                .map(|row| (row[i] * row[j] - s.get(i, j)).powi(2))
                .sum::<f64>()
                / tf;
            pi += p;
            if i == j {
                pi_diag += p;
            }
        }
    }
    // rho: the covariance of the target's entries with the sample's.
    let theta = |k: usize, i: usize, j: usize| {
        x.iter()
            .map(|row| (row[k] * row[k] - s.get(k, k)) * (row[i] * row[j] - s.get(i, j)))
            .sum::<f64>()
            / tf
    };
    let mut rho = pi_diag;
    for i in 0..n {
        for j in 0..n {
            if i != j && sd[i] > 0.0 && sd[j] > 0.0 {
                rho +=
                    rbar / 2.0 * (sd[j] / sd[i] * theta(i, i, j) + sd[i] / sd[j] * theta(j, i, j));
            }
        }
    }
    let mut gamma = 0.0;
    for i in 0..n {
        for j in 0..n {
            gamma += (f.get(i, j) - s.get(i, j)).powi(2);
        }
    }
    let delta = if gamma > 0.0 {
        ((pi - rho) / gamma / tf).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let mut out = Matrix::zeros(n, n);
    for i in 0..n {
        for j in 0..n {
            out.set(i, j, delta * f.get(i, j) + (1.0 - delta) * s.get(i, j));
        }
    }
    Some((out, delta))
}

/// Correlation from covariance. A market with no variance correlates 0 with
/// the others and 1 with itself.
pub fn correlation(cov: &Matrix) -> Matrix {
    let n = cov.rows();
    let sd: Vec<f64> = (0..n).map(|i| cov.get(i, i).max(0.0).sqrt()).collect();
    let mut c = Matrix::zeros(n, n);
    for i in 0..n {
        for j in 0..n {
            let v = if i == j {
                1.0
            } else if sd[i] > 0.0 && sd[j] > 0.0 {
                (cov.get(i, j) / (sd[i] * sd[j])).clamp(-1.0, 1.0)
            } else {
                0.0
            };
            c.set(i, j, v);
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic, roughly normal table: three markets that share a
    /// common driver, T rows.
    pub(crate) fn table(t: usize) -> Matrix {
        let mut seed = 12345u64;
        let mut next = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 11) as f64 / (1u64 << 53) as f64) - 0.5
        };
        let rows: Vec<Vec<f64>> = (0..t)
            .map(|_| {
                let common = next() + next() + next();
                vec![
                    0.02 * common + 0.01 * next(),
                    0.03 * common + 0.02 * next(),
                    0.01 * next(),
                ]
            })
            .collect();
        Matrix::from_rows(&rows)
    }

    fn symmetric(m: &Matrix) -> bool {
        (0..m.rows()).all(|i| (0..m.cols()).all(|j| (m.get(i, j) - m.get(j, i)).abs() < 1e-15))
    }

    #[test]
    fn sample_by_hand() {
        let r = Matrix::from_rows(&[vec![1.0, 2.0], vec![3.0, 6.0], vec![5.0, 4.0]]);
        let s = sample(&r).unwrap();
        // Means 3 and 4. Var a = (4+0+4)/2 = 4, var b = (4+4+0)/2 = 4,
        // cov = ((-2)(-2) + 0·2 + 2·0)/2 = 2.
        assert_eq!(s.get(0, 0), 4.0);
        assert_eq!(s.get(1, 1), 4.0);
        assert_eq!(s.get(0, 1), 2.0);
        assert!(symmetric(&s));
    }

    #[test]
    fn fewer_than_two_returns_give_nothing() {
        let r = Matrix::from_rows(&[vec![0.1, 0.2]]);
        assert!(sample(&r).is_none());
        assert!(ewma(&r, EWMA_LAMBDA).is_none());
        assert!(ledoit_wolf(&r).is_none());
    }

    #[test]
    fn every_estimate_is_symmetric_with_variances_on_the_diagonal() {
        let r = table(90);
        for e in Estimator::ALL {
            let m = estimate(&r, e).unwrap();
            assert!(symmetric(&m), "{e:?}");
            assert!((0..3).all(|i| m.get(i, i) > 0.0), "{e:?}");
        }
    }

    #[test]
    fn ewma_reacts_to_a_recent_shock_more_than_the_sample() {
        let mut rows: Vec<Vec<f64>> = (0..90)
            .map(|i| vec![if i % 2 == 0 { 0.01 } else { -0.01 }])
            .collect();
        rows.extend((0..5).map(|_| vec![0.08]));
        let r = Matrix::from_rows(&rows);
        let e = ewma(&r, EWMA_LAMBDA).unwrap().get(0, 0);
        let s = sample(&r).unwrap().get(0, 0);
        assert!(e > s, "ewma {e} sample {s}");
    }

    #[test]
    fn ledoit_wolf_intensity_is_a_fraction_and_shrinks_little_with_many_returns() {
        let (_, few) = ledoit_wolf(&table(12)).unwrap();
        let (_, many) = ledoit_wolf(&table(2000)).unwrap();
        assert!((0.0..=1.0).contains(&few));
        assert!((0.0..=1.0).contains(&many));
        assert!(many < few, "many {many} few {few}");
    }

    #[test]
    fn ledoit_wolf_keeps_the_variances() {
        let r = table(60);
        let (lw, _) = ledoit_wolf(&r).unwrap();
        let mean = means(&r);
        let s = cross(&r, r.rows(), &mean, r.rows() as f64);
        for i in 0..3 {
            assert!((lw.get(i, i) - s.get(i, i)).abs() < 1e-15);
        }
    }

    #[test]
    fn correlation_has_ones_on_the_diagonal_and_stays_in_range() {
        let c = correlation(&sample(&table(90)).unwrap());
        for i in 0..3 {
            assert_eq!(c.get(i, i), 1.0);
            for j in 0..3 {
                assert!((-1.0..=1.0).contains(&c.get(i, j)));
            }
        }
        assert!(
            c.get(0, 1) > 0.5,
            "the shared driver shows: {}",
            c.get(0, 1)
        );
    }
}
