//! Max drawdown: the one statistic on the screen the venue does not report.

use rust_decimal::Decimal;

/// The largest fall in PNL from an earlier peak, divided by the account value
/// at that peak, as a fraction (`0.0623` is 6.23%).
///
/// `pnl` and `account_value` are the venue's two histories for one scope and
/// period, which it samples at the same instants; they are paired by position.
/// `None` when there are fewer than two points to compare. A peak where the
/// account was worth nothing cannot be a denominator and is passed over.
pub fn max_drawdown(pnl: &[(u64, Decimal)], account_value: &[(u64, Decimal)]) -> Option<Decimal> {
    let n = pnl.len().min(account_value.len());
    if n < 2 {
        return None;
    }
    let mut worst = Decimal::ZERO;
    let mut peak = pnl[0].1;
    let mut peak_value = account_value[0].1;
    for i in 1..n {
        let now = pnl[i].1;
        if now > peak {
            peak = now;
            peak_value = account_value[i].1;
        } else if peak_value > Decimal::ZERO {
            let fall = (peak - now) / peak_value;
            if fall > worst {
                worst = fall;
            }
        }
    }
    Some(worst)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn series(values: &[i64]) -> Vec<(u64, Decimal)> {
        values
            .iter()
            .enumerate()
            .map(|(i, v)| (i as u64, Decimal::from(*v)))
            .collect()
    }

    #[test]
    fn the_specs_example_is_six_point_two_three_percent() {
        // PNL peaks at 100 with the account worth 10,000, then falls to -523.
        let pnl = series(&[0, 100, -200, -523, -400]);
        let value = series(&[9_900, 10_000, 9_700, 9_377, 9_500]);
        let dd = max_drawdown(&pnl, &value).unwrap();
        assert_eq!((dd * Decimal::from(100)).round_dp(2).to_string(), "6.23");
    }

    #[test]
    fn pnl_that_only_rises_has_no_drawdown() {
        let pnl = series(&[0, 5, 5, 40, 90]);
        let value = series(&[1_000, 1_005, 1_005, 1_040, 1_090]);
        assert_eq!(max_drawdown(&pnl, &value), Some(Decimal::ZERO));
    }

    #[test]
    fn fewer_than_two_points_is_no_answer() {
        assert_eq!(max_drawdown(&series(&[0]), &series(&[1_000])), None);
        assert_eq!(max_drawdown(&[], &[]), None);
    }

    #[test]
    fn the_deepest_of_several_falls_wins() {
        let pnl = series(&[0, 100, 50, 300, 0]);
        let value = series(&[1_000, 1_100, 1_050, 1_300, 1_000]);
        // 50/1100 = 4.5%, then 300/1300 = 23.1%.
        let dd = max_drawdown(&pnl, &value).unwrap();
        assert_eq!((dd * Decimal::from(100)).round_dp(1).to_string(), "23.1");
    }

    #[test]
    fn a_worthless_peak_is_passed_over() {
        let pnl = series(&[0, -10, -20]);
        let value = series(&[0, 0, 0]);
        assert_eq!(max_drawdown(&pnl, &value), Some(Decimal::ZERO));
    }
}
