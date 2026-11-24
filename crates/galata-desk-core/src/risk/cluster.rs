//! Groups of markets that move together.
//!
//! Two markets are linked when their correlation is at or above the
//! threshold, and a cluster is everything linked to everything else through
//! some chain of links (single linkage). That matches how a book is exposed:
//! one shock carries along the chain.

use super::Matrix;

/// The thresholds offered.
pub const THRESHOLDS: [f64; 3] = [0.6, 0.7, 0.8];
pub const DEFAULT_THRESHOLD: f64 = 0.7;

/// A group of markets and what the book holds of them.
#[derive(Debug, Clone, PartialEq)]
pub struct Cluster {
    /// Column indices, in the order given.
    pub members: Vec<usize>,
    /// The mean correlation over member pairs; `None` for fewer than two.
    pub mean_correlation: Option<f64>,
    pub gross: f64,
    pub net: f64,
    /// Σ beta × notional over the members.
    pub beta_net: f64,
}

/// The clusters of two or more markets, largest gross first, and everything
/// in none of them as one group, if anything is.
pub fn clusters(
    corr: &Matrix,
    threshold: f64,
    w: &[f64],
    betas: &[f64],
) -> (Vec<Cluster>, Option<Cluster>) {
    let n = w.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    for i in 0..n {
        for j in i + 1..n {
            if corr.get(i, j) >= threshold {
                let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                parent[a] = b;
            }
        }
    }
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut root_of: Vec<Option<usize>> = vec![None; n];
    for i in 0..n {
        let r = find(&mut parent, i);
        match root_of[r] {
            Some(g) => groups[g].push(i),
            None => {
                root_of[r] = Some(groups.len());
                groups.push(vec![i]);
            }
        }
    }
    let make = |members: Vec<usize>| {
        let mut sum = 0.0;
        let mut pairs = 0usize;
        for (a, &i) in members.iter().enumerate() {
            for &j in &members[a + 1..] {
                sum += corr.get(i, j);
                pairs += 1;
            }
        }
        Cluster {
            mean_correlation: (pairs > 0).then(|| sum / pairs as f64),
            gross: members.iter().map(|&i| w[i].abs()).sum(),
            net: members.iter().map(|&i| w[i]).sum(),
            beta_net: members.iter().map(|&i| betas[i] * w[i]).sum(),
            members,
        }
    };
    let (multi, single): (Vec<_>, Vec<_>) = groups.into_iter().partition(|g| g.len() > 1);
    let mut multi: Vec<Cluster> = multi.into_iter().map(make).collect();
    multi.sort_by(|a, b| b.gross.total_cmp(&a.gross));
    let rest: Vec<usize> = single.into_iter().flatten().collect();
    let rest = (!rest.is_empty()).then(|| {
        let mut c = make(rest);
        // Uncorrelated markets have no shared correlation to report.
        c.mean_correlation = None;
        c
    });
    (multi, rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ETH, SOL, AVAX, SILVER.
    fn corr() -> Matrix {
        Matrix::from_rows(&[
            vec![1.0, 0.78, 0.65, 0.05],
            vec![0.78, 1.0, 0.80, 0.04],
            vec![0.65, 0.80, 1.0, 0.03],
            vec![0.05, 0.04, 0.03, 1.0],
        ])
    }

    #[test]
    fn linked_through_a_third_market() {
        let w = [50_000.0, 20_000.0, 8_000.0, 30_000.0];
        let betas = [1.25, 1.6, 1.4, 0.08];
        let (c, rest) = clusters(&corr(), 0.7, &w, &betas);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].members, vec![0, 1, 2]);
        assert_eq!(c[0].gross, 78_000.0);
        assert!((c[0].mean_correlation.unwrap() - (0.78 + 0.65 + 0.80) / 3.0).abs() < 1e-12);
        assert!((c[0].beta_net - (62_500.0 + 32_000.0 + 11_200.0)).abs() < 1e-9);
        assert_eq!(rest.unwrap().members, vec![3]);
    }

    #[test]
    fn raising_the_threshold_never_merges() {
        let w = [1.0, 1.0, 1.0, 1.0];
        let b = [1.0; 4];
        let sizes = |t| {
            let (c, rest) = clusters(&corr(), t, &w, &b);
            (
                c.iter().map(|c| c.members.len()).max().unwrap_or(1),
                rest.map_or(0, |r| r.members.len()),
            )
        };
        let (big6, rest6) = sizes(0.6);
        let (big8, rest8) = sizes(0.8);
        assert!(big8 <= big6);
        assert!(rest8 >= rest6);
        assert_eq!(sizes(0.8), (2, 2));
    }

    #[test]
    fn short_positions_count_in_gross_and_against_net() {
        let w = [10_000.0, -4_000.0, 0.0, 0.0];
        let (c, _) = clusters(&corr(), 0.7, &w, &[1.0, 1.0, 1.0, 1.0]);
        assert_eq!(c[0].gross, 14_000.0);
        assert_eq!(c[0].net, 6_000.0);
    }
}
