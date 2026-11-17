//! Markets, as distinct from the account: what a perpetual market is doing
//! now and over a period. Public data only; nothing here knows an address.

pub mod candles;
pub mod feed;
pub mod fills;
pub mod history;
pub mod sort;
pub mod watchlist;
pub mod ws;

use std::fmt;

/// A perpetual market, by the venue's own coin name: `BTC` on the main dex,
/// `xyz:NVDA` on a builder-deployed one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MarketId(String);

impl MarketId {
    pub fn new(coin: impl Into<String>) -> Self {
        Self(coin.into())
    }

    /// From a dex name (empty for the main dex) and a ticker.
    pub fn from_parts(dex: &str, ticker: &str) -> Self {
        if dex.is_empty() {
            Self(ticker.to_string())
        } else {
            Self(format!("{dex}:{ticker}"))
        }
    }

    /// The venue's coin name, as it appears in queries and subscriptions.
    pub fn coin(&self) -> &str {
        &self.0
    }

    /// The builder dex's name; empty for the main dex.
    pub fn dex(&self) -> &str {
        self.0.split_once(':').map(|(dex, _)| dex).unwrap_or("")
    }

    /// The ticker without any dex prefix: `NVDA`.
    pub fn ticker(&self) -> &str {
        self.0.split_once(':').map(|(_, t)| t).unwrap_or(&self.0)
    }
}

impl fmt::Display for MarketId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The period a tile's change, range and volatility cover: a rolling window
/// ending now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MarketPeriod {
    Min3,
    Min5,
    Min15,
    Min30,
    Hour,
    #[default]
    Day,
    Week,
}

impl MarketPeriod {
    pub const ALL: [MarketPeriod; 7] = [
        MarketPeriod::Min3,
        MarketPeriod::Min5,
        MarketPeriod::Min15,
        MarketPeriod::Min30,
        MarketPeriod::Hour,
        MarketPeriod::Day,
        MarketPeriod::Week,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MarketPeriod::Min3 => "3m",
            MarketPeriod::Min5 => "5m",
            MarketPeriod::Min15 => "15m",
            MarketPeriod::Min30 => "30m",
            MarketPeriod::Hour => "1H",
            MarketPeriod::Day => "24H",
            MarketPeriod::Week => "7D",
        }
    }

    /// How long the window is, in milliseconds.
    pub fn millis(self) -> u64 {
        const HOUR: u64 = 3_600_000;
        const MINUTE: u64 = 60_000;
        match self {
            MarketPeriod::Min3 => 3 * MINUTE,
            MarketPeriod::Min5 => 5 * MINUTE,
            MarketPeriod::Min15 => 15 * MINUTE,
            MarketPeriod::Min30 => 30 * MINUTE,
            MarketPeriod::Hour => HOUR,
            MarketPeriod::Day => 24 * HOUR,
            MarketPeriod::Week => 7 * 24 * HOUR,
        }
    }

    /// How much time the chart shows: the period, and never less than an
    /// hour. A period of a few minutes is a handful of candles, too few to
    /// read, so its chart shows the hour it ends.
    pub fn chart_millis(self) -> u64 {
        self.millis().max(MarketPeriod::Hour.millis())
    }

    /// The candle interval the period is built from. From an hour up, each
    /// choice gives 60 to 100 candles in the window: enough for a volatility
    /// figure and a chart, and about the same cost to load whichever period
    /// is chosen. Shorter periods use the finest candles there are.
    pub fn interval(self) -> Interval {
        match self {
            MarketPeriod::Min3
            | MarketPeriod::Min5
            | MarketPeriod::Min15
            | MarketPeriod::Min30
            | MarketPeriod::Hour => Interval::M1,
            MarketPeriod::Day => Interval::M15,
            MarketPeriod::Week => Interval::H2,
        }
    }

    /// How many candles fill the window.
    pub fn candle_count(self) -> u64 {
        self.millis() / self.interval().millis()
    }
}

/// A candle interval, as the venue names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Interval {
    M1,
    M15,
    H2,
}

impl Interval {
    pub fn as_str(self) -> &'static str {
        match self {
            Interval::M1 => "1m",
            Interval::M15 => "15m",
            Interval::H2 => "2h",
        }
    }

    pub fn from_venue(text: &str) -> Option<Self> {
        match text {
            "1m" => Some(Interval::M1),
            "15m" => Some(Interval::M15),
            "2h" => Some(Interval::H2),
            _ => None,
        }
    }

    pub fn millis(self) -> u64 {
        match self {
            Interval::M1 => 60_000,
            Interval::M15 => 15 * 60_000,
            Interval::H2 => 2 * 3_600_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_main_dex_market_has_no_dex() {
        let btc = MarketId::new("BTC");
        assert_eq!(btc.coin(), "BTC");
        assert_eq!(btc.dex(), "");
        assert_eq!(btc.ticker(), "BTC");
        assert_eq!(MarketId::from_parts("", "BTC"), btc);
    }

    #[test]
    fn a_builder_dex_market_splits_into_dex_and_ticker() {
        let nvda = MarketId::new("xyz:NVDA");
        assert_eq!(nvda.dex(), "xyz");
        assert_eq!(nvda.ticker(), "NVDA");
        assert_eq!(MarketId::from_parts("xyz", "NVDA"), nvda);
        assert_eq!(nvda.to_string(), "xyz:NVDA");
    }

    #[test]
    fn each_period_has_its_interval_and_candle_count() {
        assert_eq!(MarketPeriod::Hour.interval().as_str(), "1m");
        assert_eq!(MarketPeriod::Hour.candle_count(), 60);
        assert_eq!(MarketPeriod::Day.interval().as_str(), "15m");
        assert_eq!(MarketPeriod::Day.candle_count(), 96);
        assert_eq!(MarketPeriod::Week.interval().as_str(), "2h");
        assert_eq!(MarketPeriod::Week.candle_count(), 84);
        assert_eq!(MarketPeriod::default(), MarketPeriod::Day);
    }

    #[test]
    fn intervals_round_trip_through_the_venues_names() {
        for interval in [Interval::M1, Interval::M15, Interval::H2] {
            assert_eq!(Interval::from_venue(interval.as_str()), Some(interval));
        }
        assert_eq!(Interval::from_venue("4h"), None);
    }
}
