//! Exposure to one benchmark market.
//!
//! A position's beta is `cov(position, benchmark) ÷ var(benchmark)`; its
//! beta-equivalent is beta times its signed notional: how much of the
//! benchmark it behaves like. R² says how much of its variance the benchmark
//! explains, and the residual is what a benchmark hedge leaves behind.

use super::Matrix;
use crate::market::MarketId;

/// Below this R² a position is marked as poorly explained by the benchmark.
pub const LOW_R2: f64 = 0.25;

/// One position against the benchmark. Volatilities are one-day, in USD.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BetaRow {
    pub beta: f64,
    pub r2: f64,
    /// Beta times the signed notional, in USD.
    pub equivalent: f64,
    /// The same in benchmark units at the benchmark's mark.
    pub equivalent_units: f64,
    /// `|equivalent| × σ(benchmark)`.
    pub systematic: f64,
    /// `|notional| × σ(position) × √(1 − R²)`.
    pub residual: f64,
}

impl BetaRow {
    pub fn low_r2(&self) -> bool {
        self.r2 < LOW_R2
    }

    /// The benchmark trade that offsets this position's beta, in units.
    pub fn hedge_units(&self) -> f64 {
        -self.equivalent_units
    }
}

/// Every position against benchmark column `bench` of `cov` (one-day).
pub fn rows(cov: &Matrix, w: &[f64], bench: usize, bench_mark: f64) -> Vec<BetaRow> {
    let var_b = cov.get(bench, bench);
    let sd_b = var_b.max(0.0).sqrt();
    (0..w.len())
        .map(|i| {
            let var_i = cov.get(i, i).max(0.0);
            let c = cov.get(i, bench);
            let beta = if var_b > 0.0 { c / var_b } else { 0.0 };
            let r2 = if var_b > 0.0 && var_i > 0.0 {
                (c * c / (var_i * var_b)).min(1.0)
            } else {
                0.0
            };
            let equivalent = beta * w[i];
            BetaRow {
                beta,
                r2,
                equivalent,
                equivalent_units: if bench_mark > 0.0 {
                    equivalent / bench_mark
                } else {
                    0.0
                },
                systematic: equivalent.abs() * sd_b,
                residual: w[i].abs() * var_i.sqrt() * (1.0 - r2).max(0.0).sqrt(),
            }
        })
        .collect()
}

/// The book's exposure, raw and beta-adjusted.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Exposure {
    /// Σ |notional|.
    pub gross: f64,
    /// Σ notional.
    pub net: f64,
    /// Σ |beta × notional|.
    pub beta_gross: f64,
    /// Σ beta × notional.
    pub beta_net: f64,
    pub beta_gross_units: f64,
    pub beta_net_units: f64,
}

impl Exposure {
    /// The benchmark trade, in units, that makes the book beta-neutral.
    pub fn hedge_units(&self) -> f64 {
        -self.beta_net_units
    }
}

pub fn exposure(w: &[f64], rows: &[BetaRow], bench_mark: f64) -> Exposure {
    let mut e = Exposure::default();
    for (wi, r) in w.iter().zip(rows) {
        e.gross += wi.abs();
        e.net += wi;
        e.beta_gross += r.equivalent.abs();
        e.beta_net += r.equivalent;
    }
    if bench_mark > 0.0 {
        e.beta_gross_units = e.beta_gross / bench_mark;
        e.beta_net_units = e.beta_net / bench_mark;
    }
    e
}

/// Which held market is the benchmark: the stored choice while it is held,
/// else BTC when held, else the largest position by absolute notional.
pub fn default_benchmark(held: &[(MarketId, f64)], stored: Option<&MarketId>) -> Option<usize> {
    let find = |m: &MarketId| held.iter().position(|(h, _)| h == m);
    stored
        .and_then(find)
        .or_else(|| find(&MarketId::new("BTC")))
        .or_else(|| {
            held.iter()
                .enumerate()
                .max_by(|a, b| a.1.1.abs().total_cmp(&b.1.1.abs()))
                .map(|(i, _)| i)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-day covariance from vols and the correlation of each market with
    /// the first; other pairs correlate through it.
    fn cov(vols: &[f64], rho_b: &[f64]) -> Matrix {
        let n = vols.len();
        let rows: Vec<Vec<f64>> = (0..n)
            .map(|i| {
                (0..n)
                    .map(|j| {
                        let rho = if i == j {
                            1.0
                        } else if i == 0 {
                            rho_b[j]
                        } else if j == 0 {
                            rho_b[i]
                        } else {
                            rho_b[i] * rho_b[j]
                        };
                        rho * vols[i] * vols[j]
                    })
                    .collect()
            })
            .collect();
        Matrix::from_rows(&rows)
    }

    #[test]
    fn silver_in_btc_terms() {
        // BTC 2.8% a day; silver 1.8% with correlation chosen for beta 0.08.
        let rho = 0.08 * 0.028 / 0.018;
        let c = cov(&[0.028, 0.018], &[1.0, rho]);
        let r = rows(&c, &[40_000.0, 30_000.0], 0, 98_000.0);
        assert!((r[1].beta - 0.08).abs() < 1e-12);
        assert!((r[1].equivalent - 2_400.0).abs() < 1e-6);
        assert!((r[1].equivalent_units - 0.024_489_8).abs() < 1e-6);
        assert!((r[1].hedge_units() + 0.024_489_8).abs() < 1e-6);
        assert!(r[1].low_r2());
        // Nearly all its risk is residual.
        assert!(r[1].residual > 0.99 * 30_000.0 * 0.018);
    }

    #[test]
    fn the_benchmark_against_itself() {
        let c = cov(&[0.028, 0.036], &[1.0, 0.8]);
        let r = rows(&c, &[40_000.0, 50_000.0], 0, 98_000.0);
        assert!((r[0].beta - 1.0).abs() < 1e-12);
        assert!((r[0].r2 - 1.0).abs() < 1e-12);
        assert!(r[0].residual.abs() < 1e-6);
    }

    #[test]
    fn a_long_short_book() {
        let rows = [
            BetaRow {
                beta: 1.25,
                r2: 0.7,
                equivalent: 62_500.0,
                equivalent_units: 0.0,
                systematic: 0.0,
                residual: 0.0,
            },
            BetaRow {
                beta: 1.40,
                r2: 0.5,
                equivalent: -21_000.0,
                equivalent_units: 0.0,
                systematic: 0.0,
                residual: 0.0,
            },
        ];
        let e = exposure(&[50_000.0, -15_000.0], &rows, 98_000.0);
        assert_eq!(e.gross, 65_000.0);
        assert_eq!(e.net, 35_000.0);
        assert_eq!(e.beta_gross, 83_500.0);
        assert_eq!(e.beta_net, 41_500.0);
        assert!((e.hedge_units() + 41_500.0 / 98_000.0).abs() < 1e-12);
    }

    #[test]
    fn the_default_benchmark() {
        let held = vec![
            (MarketId::new("ETH"), 50_000.0),
            (MarketId::new("BTC"), 40_000.0),
            (MarketId::new("DOGE"), -90_000.0),
        ];
        assert_eq!(default_benchmark(&held, None), Some(1));
        let eth = MarketId::new("ETH");
        assert_eq!(default_benchmark(&held, Some(&eth)), Some(0));
        // A stored choice no longer held falls back without being lost.
        let sol = MarketId::new("SOL");
        assert_eq!(default_benchmark(&held, Some(&sol)), Some(1));
        let no_btc = vec![
            (MarketId::new("ETH"), 50_000.0),
            (MarketId::new("DOGE"), -90_000.0),
        ];
        assert_eq!(default_benchmark(&no_btc, None), Some(1));
        assert_eq!(default_benchmark(&[], None), None);
    }
}
