//! Value at risk, four ways, and each position's share of it.
//!
//! - Parametric (delta-normal): `z · √(wᵀΣw)`.
//! - Cornish-Fisher: the normal quantile corrected for the skewness and
//!   excess kurtosis of the portfolio's own returns.
//! - Historical: the empirical loss quantile of the portfolio's returns
//!   replayed at today's positions.
//! - Expected shortfall: the mean of the losses at or beyond that quantile.
//!
//! Marginal VaR is `z (Σw)ᵢ / σₚ`, and component VaR is the position times
//! its marginal. By Euler's theorem the components sum to parametric VaR.

use super::Matrix;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Confidence {
    P95,
    #[default]
    P99,
}

impl Confidence {
    pub const ALL: [Confidence; 2] = [Confidence::P95, Confidence::P99];

    /// The standard normal quantile.
    pub fn z(self) -> f64 {
        match self {
            Confidence::P95 => 1.644_853_626_951_472_2,
            Confidence::P99 => 2.326_347_874_040_841,
        }
    }

    pub fn level(self) -> f64 {
        match self {
            Confidence::P95 => 0.95,
            Confidence::P99 => 0.99,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Confidence::P95 => "95%",
            Confidence::P99 => "99%",
        }
    }
}

/// The portfolio's standard deviation in USD: `√(wᵀΣw)`.
pub fn sigma(cov: &Matrix, w: &[f64]) -> f64 {
    cov.quad(w).max(0.0).sqrt()
}

pub fn parametric(cov: &Matrix, w: &[f64], confidence: Confidence) -> f64 {
    confidence.z() * sigma(cov, w)
}

/// One position's share of parametric VaR.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Contribution {
    /// VaR added per extra dollar of the position.
    pub marginal: f64,
    /// The position times its marginal. Negative when it hedges the book.
    pub component: f64,
}

pub fn contributions(cov: &Matrix, w: &[f64], confidence: Confidence) -> Vec<Contribution> {
    let sp = sigma(cov, w);
    let sw = cov.mul_vec(w);
    w.iter()
        .zip(sw)
        .map(|(wi, s)| {
            let marginal = if sp > 0.0 {
                confidence.z() * s / sp
            } else {
                0.0
            };
            Contribution {
                marginal,
                component: wi * marginal,
            }
        })
        .collect()
}

/// The portfolio's USD return for each row of returns: `w · rₜ`.
pub fn pnl(returns: &Matrix, w: &[f64]) -> Vec<f64> {
    (0..returns.rows())
        .map(|t| returns.row(t).iter().zip(w).map(|(r, wi)| r * wi).sum())
        .collect()
}

/// Sample skewness and excess kurtosis (population moments). Zeros when the
/// series has no spread.
pub fn moments(x: &[f64]) -> (f64, f64) {
    let n = x.len() as f64;
    if n < 2.0 {
        return (0.0, 0.0);
    }
    let mean = x.iter().sum::<f64>() / n;
    let m2 = x.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;
    if m2 <= 0.0 {
        return (0.0, 0.0);
    }
    let m3 = x.iter().map(|v| (v - mean).powi(3)).sum::<f64>() / n;
    let m4 = x.iter().map(|v| (v - mean).powi(4)).sum::<f64>() / n;
    (m3 / m2.powf(1.5), m4 / (m2 * m2) - 3.0)
}

/// The Cornish-Fisher loss multiplier at `confidence` for returns with skew
/// `s` and excess kurtosis `k`, and whether it fell back to the normal `z`.
/// The expansion is only a quantile where it is increasing; where it is not,
/// or where it would give no loss at all, the normal quantile is used.
pub fn cornish_fisher(confidence: Confidence, s: f64, k: f64) -> (f64, bool) {
    let z = -confidence.z();
    let q = z + (z * z - 1.0) * s / 6.0 + (z.powi(3) - 3.0 * z) * k / 24.0
        - (2.0 * z.powi(3) - 5.0 * z) * s * s / 36.0;
    let slope =
        1.0 + z * s / 3.0 + (3.0 * z * z - 3.0) * k / 24.0 - (6.0 * z * z - 5.0) * s * s / 36.0;
    if slope <= 0.0 || q >= 0.0 {
        (confidence.z(), true)
    } else {
        (-q, false)
    }
}

/// The empirical tail of a series of USD returns.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tail {
    /// The loss at the confidence quantile, positive.
    pub var: f64,
    /// The mean of the losses at or beyond it.
    pub shortfall: f64,
    /// How many returns are in the tail.
    pub count: usize,
}

pub fn historical(pnl: &[f64], confidence: Confidence) -> Option<Tail> {
    if pnl.is_empty() {
        return None;
    }
    let mut losses: Vec<f64> = pnl.iter().map(|p| -p).collect();
    losses.sort_by(|a, b| b.total_cmp(a));
    let count = (((1.0 - confidence.level()) * losses.len() as f64) - 1e-9)
        .ceil()
        .max(1.0) as usize;
    let tail = &losses[..count];
    Some(Tail {
        var: tail[count - 1],
        shortfall: tail.iter().sum::<f64>() / count as f64,
        count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cov3() -> Matrix {
        // Vols 2%, 3%, 1%; correlations 0.8, 0.2, 0.1.
        let v = [0.02, 0.03, 0.01];
        let rho = [[1.0, 0.8, 0.2], [0.8, 1.0, 0.1], [0.2, 0.1, 1.0]];
        let rows: Vec<Vec<f64>> = (0..3)
            .map(|i| (0..3).map(|j| rho[i][j] * v[i] * v[j]).collect())
            .collect();
        Matrix::from_rows(&rows)
    }

    #[test]
    fn confidence_scales_parametric_var_by_the_quantile_ratio() {
        let w = [10_000.0, 5_000.0, -3_000.0];
        let v95 = parametric(&cov3(), &w, Confidence::P95);
        let v99 = parametric(&cov3(), &w, Confidence::P99);
        assert!((v99 / v95 - 2.326_347_874 / 1.644_853_627).abs() < 1e-9);
    }

    #[test]
    fn components_sum_to_var() {
        let w = [10_000.0, 5_000.0, -3_000.0];
        let total = parametric(&cov3(), &w, Confidence::P99);
        let sum: f64 = contributions(&cov3(), &w, Confidence::P99)
            .iter()
            .map(|c| c.component)
            .sum();
        assert!(((sum - total) / total).abs() < 1e-9, "{sum} vs {total}");
    }

    #[test]
    fn a_hedging_short_has_a_negative_component() {
        // Long the first market, short the highly correlated second.
        let w = [10_000.0, -4_000.0, 0.0];
        let c = contributions(&cov3(), &w, Confidence::P95);
        assert!(c[1].component < 0.0, "{c:?}");
        assert!(c[0].component > 0.0);
    }

    #[test]
    fn fat_left_tails_widen_cornish_fisher_var() {
        let (z, fell_back) = cornish_fisher(Confidence::P99, -0.5, 4.0);
        assert!(!fell_back);
        assert!(z > Confidence::P99.z(), "{z}");
        // Normal moments give the normal quantile.
        let (z0, _) = cornish_fisher(Confidence::P99, 0.0, 0.0);
        assert!((z0 - Confidence::P99.z()).abs() < 1e-12);
    }

    #[test]
    fn an_expansion_that_is_not_increasing_falls_back() {
        let (z, fell_back) = cornish_fisher(Confidence::P99, 4.0, 0.0);
        assert!(fell_back);
        assert_eq!(z, Confidence::P99.z());
    }

    #[test]
    fn historical_tail_by_hand() {
        // Twenty returns: losses 1..=20 dollars.
        let pnl: Vec<f64> = (1..=20).map(|i| -(i as f64)).collect();
        let t95 = historical(&pnl, Confidence::P95).unwrap();
        // 5% of 20 is one return: the worst.
        assert_eq!(t95.count, 1);
        assert_eq!(t95.var, 20.0);
        assert_eq!(t95.shortfall, 20.0);
        let pnl40: Vec<f64> = (1..=40).map(|i| -(i as f64)).collect();
        let t = historical(&pnl40, Confidence::P95).unwrap();
        assert_eq!(t.count, 2);
        assert_eq!(t.var, 39.0);
        assert_eq!(t.shortfall, 39.5);
        assert!(t.shortfall >= t.var);
    }

    #[test]
    fn moments_of_a_symmetric_series() {
        let (s, k) = moments(&[-1.0, 1.0, -1.0, 1.0]);
        assert!(s.abs() < 1e-12);
        assert!((k - (-2.0)).abs() < 1e-12);
    }
}
