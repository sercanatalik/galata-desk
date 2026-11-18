//! The order tiles are drawn in. Sorting is presentation only: the watchlist
//! and its own order are not changed by it.

use std::cmp::Ordering;

use rust_decimal::prelude::ToPrimitive;

use super::feed::Tile;

/// What the tiles are ordered by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SortBy {
    /// As the markets were added.
    #[default]
    Watchlist,
    /// Change over the period, largest rise first.
    Gainers,
    /// Change over the period, largest fall first.
    Losers,
    /// Volatility over the period, most first.
    Volatility,
    /// Hourly funding, highest first: where longs pay most.
    Funding,
    /// Ticker, A to Z.
    Name,
}

impl SortBy {
    pub const ALL: [SortBy; 6] = [
        SortBy::Watchlist,
        SortBy::Gainers,
        SortBy::Losers,
        SortBy::Volatility,
        SortBy::Funding,
        SortBy::Name,
    ];

    /// The short name, for the button that opens the menu.
    pub fn label(self) -> &'static str {
        match self {
            SortBy::Watchlist => "Watchlist",
            SortBy::Gainers => "Gainers",
            SortBy::Losers => "Losers",
            SortBy::Volatility => "Volatility",
            SortBy::Funding => "Funding",
            SortBy::Name => "Name",
        }
    }

    /// The name in the menu, which says which way the order runs.
    pub fn words(self) -> &'static str {
        match self {
            SortBy::Watchlist => "Watchlist order",
            SortBy::Gainers => "Change, gainers first",
            SortBy::Losers => "Change, losers first",
            SortBy::Volatility => "Volatility, most first",
            SortBy::Funding => "Funding, highest first",
            SortBy::Name => "Name, A to Z",
        }
    }

    /// Whether the order can change as figures arrive. Watchlist and name
    /// order never move.
    pub fn moves(self) -> bool {
        !matches!(self, SortBy::Watchlist | SortBy::Name)
    }

    /// The figure a tile is ordered by, larger first; `None` when it has not
    /// arrived, or the market is unavailable.
    fn key(self, tile: &Tile) -> Option<f64> {
        if tile.unavailable {
            return None;
        }
        match self {
            SortBy::Gainers => tile.stats.as_ref().map(|s| s.change),
            SortBy::Losers => tile.stats.as_ref().map(|s| -s.change),
            SortBy::Volatility => tile.stats.as_ref().and_then(|s| s.volatility),
            SortBy::Funding => tile.live.as_ref().and_then(|l| l.funding.to_f64()),
            SortBy::Watchlist | SortBy::Name => None,
        }
    }
}

/// The positions of `tiles`, which are in watchlist order, in the order to
/// draw them.
///
/// Tiles with the figure come first, largest first; those without it last.
/// The sort is stable, so equal figures, and the tiles without one, keep
/// their watchlist order.
pub fn order(tiles: &[Tile], by: SortBy) -> Vec<usize> {
    let mut positions: Vec<usize> = (0..tiles.len()).collect();
    match by {
        SortBy::Watchlist => {}
        SortBy::Name => positions.sort_by(|a, b| {
            let (a, b) = (&tiles[*a].market, &tiles[*b].market);
            a.ticker()
                .to_lowercase()
                .cmp(&b.ticker().to_lowercase())
                .then_with(|| a.dex().cmp(b.dex()))
        }),
        _ => positions.sort_by(|a, b| match (by.key(&tiles[*a]), by.key(&tiles[*b])) {
            (Some(x), Some(y)) => y.partial_cmp(&x).unwrap_or(Ordering::Equal),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }),
    }
    positions
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;

    use super::*;
    use crate::market::MarketId;
    use crate::market::candles::Stats;
    use crate::market::feed::Live;

    fn tile(coin: &str, change: Option<f64>, vol: Option<f64>, funding: Option<&str>) -> Tile {
        Tile {
            market: MarketId::new(coin),
            live: funding.map(|f| Live {
                last: Decimal::ONE,
                mark: Decimal::ONE,
                oracle: Decimal::ONE,
                premium: None,
                funding: f.parse().unwrap(),
                volume_usd: Decimal::ZERO,
                open_interest_usd: Decimal::ZERO,
            }),
            stats: change.map(|change| Stats {
                start: Decimal::ONE,
                change,
                low: Decimal::ONE,
                high: Decimal::ONE,
                marker: 0.5,
                volatility: vol,
                mean: None,
                sd: None,
            }),
            max_leverage: None,
            unavailable: false,
        }
    }

    fn names(tiles: &[Tile], by: SortBy) -> Vec<String> {
        order(tiles, by)
            .into_iter()
            .map(|i| tiles[i].market.to_string())
            .collect()
    }

    fn sample() -> Vec<Tile> {
        vec![
            tile("BTC", Some(-0.7), Some(2.9), Some("0.000008")),
            tile("SOL", Some(1.3), Some(4.2), Some("0.000013")),
            tile("xyz:NVDA", Some(-2.8), Some(4.9), Some("-0.000009")),
            tile("ETH", Some(0.2), Some(3.7), Some("0.000031")),
        ]
    }

    #[test]
    fn watchlist_order_is_the_order_given() {
        assert_eq!(
            names(&sample(), SortBy::Watchlist),
            ["BTC", "SOL", "xyz:NVDA", "ETH"]
        );
        assert_eq!(SortBy::default(), SortBy::Watchlist);
    }

    #[test]
    fn gainers_first_and_losers_first_are_each_others_reverse() {
        assert_eq!(
            names(&sample(), SortBy::Gainers),
            ["SOL", "ETH", "BTC", "xyz:NVDA"]
        );
        assert_eq!(
            names(&sample(), SortBy::Losers),
            ["xyz:NVDA", "BTC", "ETH", "SOL"]
        );
    }

    #[test]
    fn the_most_volatile_comes_first() {
        assert_eq!(
            names(&sample(), SortBy::Volatility),
            ["xyz:NVDA", "SOL", "ETH", "BTC"]
        );
    }

    #[test]
    fn funding_runs_from_longs_paying_most_to_shorts_paying_most() {
        assert_eq!(
            names(&sample(), SortBy::Funding),
            ["ETH", "SOL", "BTC", "xyz:NVDA"]
        );
    }

    #[test]
    fn names_are_sorted_by_ticker_whatever_the_dex_or_case() {
        let tiles = vec![
            tile("xyz:NVDA", None, None, None),
            tile("kPEPE", None, None, None),
            tile("BTC", None, None, None),
            tile("flx:NVDA", None, None, None),
        ];
        assert_eq!(
            names(&tiles, SortBy::Name),
            ["BTC", "kPEPE", "flx:NVDA", "xyz:NVDA"]
        );
    }

    #[test]
    fn a_market_without_the_figure_goes_last_in_watchlist_order() {
        let tiles = vec![
            tile("NEW1", None, None, Some("0.00001")),
            tile("BTC", Some(-0.7), Some(2.9), Some("0.000008")),
            tile("NEW2", None, None, None),
            tile("SOL", Some(1.3), None, Some("0.000013")),
        ];
        // No history: no change.
        assert_eq!(
            names(&tiles, SortBy::Gainers),
            ["SOL", "BTC", "NEW1", "NEW2"]
        );
        assert_eq!(
            names(&tiles, SortBy::Losers),
            ["BTC", "SOL", "NEW1", "NEW2"]
        );
        // Too few candles for a volatility, and no history at all.
        assert_eq!(
            names(&tiles, SortBy::Volatility),
            ["BTC", "NEW1", "NEW2", "SOL"]
        );
        // No live figures: no funding.
        assert_eq!(
            names(&tiles, SortBy::Funding),
            ["SOL", "NEW1", "BTC", "NEW2"]
        );
    }

    #[test]
    fn an_unavailable_market_goes_last_even_with_old_figures() {
        let mut tiles = sample();
        tiles[1].unavailable = true;
        assert_eq!(
            names(&tiles, SortBy::Gainers),
            ["ETH", "BTC", "xyz:NVDA", "SOL"]
        );
    }

    #[test]
    fn equal_figures_keep_their_watchlist_order() {
        let tiles = vec![
            tile("A", Some(1.0), Some(1.0), Some("0.00001")),
            tile("B", Some(2.0), Some(1.0), Some("0.00001")),
            tile("C", Some(1.0), Some(1.0), Some("0.00001")),
        ];
        assert_eq!(names(&tiles, SortBy::Gainers), ["B", "A", "C"]);
        assert_eq!(names(&tiles, SortBy::Volatility), ["A", "B", "C"]);
        assert_eq!(names(&tiles, SortBy::Funding), ["A", "B", "C"]);
    }

    #[test]
    fn only_the_figure_sorts_can_move() {
        assert!(!SortBy::Watchlist.moves());
        assert!(!SortBy::Name.moves());
        assert!(SortBy::Gainers.moves() && SortBy::Funding.moves());
    }
}
