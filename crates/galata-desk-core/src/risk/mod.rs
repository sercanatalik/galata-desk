//! Portfolio risk: how much the open positions could lose in a day and where
//! that risk sits.
//!
//! Everything here is arithmetic on candles and positions already fetched.
//! Statistics are `f64`, like [`crate::market::candles::volatility`]: they are
//! statistics, not money.

pub mod beta;
pub mod cluster;
pub mod cov;
pub mod feed;
mod matrix;
pub mod report;
pub mod returns;
pub mod var;

pub use cov::Estimator;
pub use matrix::Matrix;
pub use report::{RiskPosition, RiskReport, RiskSettings, SeriesState};
pub use var::Confidence;

use crate::market::{Interval, MarketPeriod};

/// One day, in milliseconds: the horizon every figure is stated for.
pub const DAY_MS: u64 = 24 * 3_600_000;

/// A lookback the risk figures are estimated over. Each uses the candles
/// Markets already loads for the same period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum RiskPeriod {
    Month,
    #[default]
    Quarter,
    HalfYear,
    Year,
}

impl RiskPeriod {
    pub const ALL: [RiskPeriod; 4] = [
        RiskPeriod::Month,
        RiskPeriod::Quarter,
        RiskPeriod::HalfYear,
        RiskPeriod::Year,
    ];

    pub fn market_period(self) -> MarketPeriod {
        match self {
            RiskPeriod::Month => MarketPeriod::Month,
            RiskPeriod::Quarter => MarketPeriod::Quarter,
            RiskPeriod::HalfYear => MarketPeriod::HalfYear,
            RiskPeriod::Year => MarketPeriod::Year,
        }
    }

    pub fn label(self) -> &'static str {
        self.market_period().label()
    }

    pub fn millis(self) -> u64 {
        self.market_period().millis()
    }

    /// 8h, 1d, 1d and 3d.
    pub fn interval(self) -> Interval {
        self.market_period().interval()
    }

    /// How many candle intervals make a day: 3 for 8h candles, ⅓ for 3d.
    pub fn per_day(self) -> f64 {
        DAY_MS as f64 / self.interval().millis() as f64
    }

    /// Whether one-day figures are scaled from another interval by √time.
    pub fn scaled(self) -> bool {
        self.interval().millis() != DAY_MS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn periods_use_the_candles_markets_loads() {
        assert_eq!(RiskPeriod::Month.interval(), Interval::H8);
        assert_eq!(RiskPeriod::Quarter.interval(), Interval::D1);
        assert_eq!(RiskPeriod::HalfYear.interval(), Interval::D1);
        assert_eq!(RiskPeriod::Year.interval(), Interval::D3);
        assert_eq!(RiskPeriod::Month.per_day(), 3.0);
        assert!(RiskPeriod::Month.scaled());
        assert!(!RiskPeriod::Quarter.scaled());
        assert!(RiskPeriod::Year.scaled());
        assert_eq!(RiskPeriod::Year.label(), "1Y");
    }
}
