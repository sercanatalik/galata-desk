//! Which markets are watched, and the rules for changing that.

use super::MarketId;
use crate::model::PositionRow;

/// The most markets a watchlist holds.
pub const LIMIT: usize = 40;

/// Why a market was not added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AddError {
    #[error("that market is already watched")]
    AlreadyWatched,
    #[error("the watchlist is full; remove a market first")]
    Full,
}

/// The watched markets, in the order they were added, and which of them is
/// selected: the one the market panel shows.
///
/// While the list holds any market exactly one is selected.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Watchlist {
    markets: Vec<MarketId>,
    selected: Option<MarketId>,
}

impl Watchlist {
    /// From a stored list. Duplicates are dropped and the list is cut at the
    /// limit, so a hand-edited file cannot break the rules.
    pub fn from_markets(markets: impl IntoIterator<Item = MarketId>) -> Self {
        let mut list = Self::default();
        for market in markets {
            let _ = list.add(market);
        }
        list
    }

    /// From a stored list and a stored selection. A selection that is not in
    /// the list, or absent, gives way to the first market.
    pub fn with_selection(
        markets: impl IntoIterator<Item = MarketId>,
        selected: Option<MarketId>,
    ) -> Self {
        let mut list = Self::from_markets(markets);
        if let Some(selected) = selected {
            list.select(&selected);
        }
        list
    }

    /// The market the panel shows. `None` only when nothing is watched.
    pub fn selected(&self) -> Option<&MarketId> {
        self.selected.as_ref()
    }

    /// Select a watched market. Returns whether it is watched; selecting one
    /// that is not changes nothing.
    pub fn select(&mut self, market: &MarketId) -> bool {
        let watched = self.contains(market);
        if watched {
            self.selected = Some(market.clone());
        }
        watched
    }

    /// The list for someone who has never chosen one: the markets they hold
    /// positions in, largest position first; with no position, four majors.
    pub fn default_for(positions: &[PositionRow]) -> Self {
        if positions.is_empty() {
            return Self::from_markets(["BTC", "ETH", "SOL", "HYPE"].map(MarketId::new));
        }
        let mut held: Vec<&PositionRow> = positions.iter().collect();
        held.sort_by_key(|p| std::cmp::Reverse(p.value));
        Self::from_markets(held.iter().map(|p| MarketId::from_parts(&p.dex, &p.market)))
    }

    pub fn markets(&self) -> &[MarketId] {
        &self.markets
    }

    pub fn contains(&self, market: &MarketId) -> bool {
        self.markets.contains(market)
    }

    pub fn is_full(&self) -> bool {
        self.markets.len() >= LIMIT
    }

    pub fn is_empty(&self) -> bool {
        self.markets.is_empty()
    }

    /// Add a market at the end.
    pub fn add(&mut self, market: MarketId) -> Result<(), AddError> {
        if self.contains(&market) {
            return Err(AddError::AlreadyWatched);
        }
        if self.is_full() {
            return Err(AddError::Full);
        }
        // The selection stays where it is, unless there was nothing to select.
        if self.selected.is_none() {
            self.selected = Some(market.clone());
        }
        self.markets.push(market);
        Ok(())
    }

    /// Remove a market. Returns whether it was watched.
    ///
    /// Removing the selected market selects the one that followed it, or the
    /// one before it when it was last; removing the only market leaves
    /// nothing selected.
    pub fn remove(&mut self, market: &MarketId) -> bool {
        let Some(at) = self.markets.iter().position(|m| m == market) else {
            return false;
        };
        self.markets.remove(at);
        if self.selected.as_ref() == Some(market) {
            self.selected = self
                .markets
                .get(at)
                .or_else(|| self.markets.last())
                .cloned();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::venue::MarginMode;
    use rust_decimal::Decimal;

    fn id(coin: &str) -> MarketId {
        MarketId::new(coin)
    }

    fn position(market: &str, dex: &str, value: i64) -> PositionRow {
        PositionRow {
            asset: None,
            market: market.into(),
            dex: dex.into(),
            leverage: 10,
            mode: MarginMode::Cross,
            size: Decimal::ONE,
            value: Decimal::from(value),
            entry: Decimal::ONE,
            mark: None,
            pnl: Decimal::ZERO,
            roe: Decimal::ZERO,
            liquidation: None,
            margin: Decimal::ZERO,
            funding: Decimal::ZERO,
        }
    }

    #[test]
    fn markets_keep_the_order_they_were_added() {
        let mut list = Watchlist::default();
        list.add(id("ETH")).unwrap();
        list.add(id("xyz:NVDA")).unwrap();
        list.add(id("BTC")).unwrap();
        assert_eq!(list.markets(), [id("ETH"), id("xyz:NVDA"), id("BTC")]);
    }

    #[test]
    fn a_market_is_not_added_twice() {
        let mut list = Watchlist::default();
        list.add(id("ETH")).unwrap();
        assert_eq!(list.add(id("ETH")), Err(AddError::AlreadyWatched));
        assert_eq!(list.markets().len(), 1);
    }

    #[test]
    fn the_same_ticker_on_two_dexes_is_two_markets() {
        let mut list = Watchlist::default();
        list.add(id("BTC")).unwrap();
        list.add(id("hyna:BTC")).unwrap();
        assert_eq!(list.markets().len(), 2);
    }

    #[test]
    fn the_watchlist_is_full_at_forty() {
        let mut list = Watchlist::default();
        for i in 0..LIMIT {
            list.add(id(&format!("M{i}"))).unwrap();
        }
        assert!(list.is_full());
        assert_eq!(list.add(id("ONE-MORE")), Err(AddError::Full));
        // Removing one makes room again.
        assert!(list.remove(&id("M0")));
        list.add(id("ONE-MORE")).unwrap();
    }

    #[test]
    fn removing_a_market_that_is_not_watched_changes_nothing() {
        let mut list = Watchlist::from_markets([id("ETH")]);
        assert!(!list.remove(&id("BTC")));
        assert!(list.remove(&id("ETH")));
        assert!(list.is_empty());
    }

    #[test]
    fn the_default_is_what_the_account_holds_largest_first() {
        let positions = [
            position("BTC", "", 10_000),
            position("ETH", "", 25_000),
            position("NVDA", "xyz", 19_500),
            position("SILVER", "xyz", 10_100),
        ];
        let list = Watchlist::default_for(&positions);
        assert_eq!(
            list.markets(),
            [id("ETH"), id("xyz:NVDA"), id("xyz:SILVER"), id("BTC")]
        );
    }

    #[test]
    fn with_no_position_the_default_is_four_majors() {
        let list = Watchlist::default_for(&[]);
        assert_eq!(
            list.markets(),
            [id("BTC"), id("ETH"), id("SOL"), id("HYPE")]
        );
    }

    #[test]
    fn the_first_market_is_selected_by_default() {
        let list = Watchlist::from_markets([id("ETH"), id("xyz:NVDA"), id("BTC")]);
        assert_eq!(list.selected(), Some(&id("ETH")));
        assert_eq!(Watchlist::default().selected(), None);
    }

    #[test]
    fn adding_a_market_leaves_the_selection_alone() {
        let mut list = Watchlist::from_markets([id("ETH"), id("xyz:NVDA")]);
        list.select(&id("xyz:NVDA"));
        list.add(id("xyz:TSLA")).unwrap();
        assert_eq!(list.selected(), Some(&id("xyz:NVDA")));
    }

    #[test]
    fn a_market_added_to_an_empty_list_is_selected() {
        let mut list = Watchlist::default();
        list.add(id("SOL")).unwrap();
        assert_eq!(list.selected(), Some(&id("SOL")));
    }

    #[test]
    fn only_a_watched_market_can_be_selected() {
        let mut list = Watchlist::from_markets([id("ETH"), id("BTC")]);
        assert!(list.select(&id("BTC")));
        assert_eq!(list.selected(), Some(&id("BTC")));
        assert!(!list.select(&id("DOGE")));
        assert_eq!(list.selected(), Some(&id("BTC")));
    }

    #[test]
    fn removing_the_selected_market_selects_the_one_after_it() {
        let mut list = Watchlist::from_markets([id("ETH"), id("xyz:NVDA"), id("xyz:SILVER")]);
        list.select(&id("xyz:NVDA"));
        list.remove(&id("xyz:NVDA"));
        assert_eq!(list.selected(), Some(&id("xyz:SILVER")));
    }

    #[test]
    fn removing_the_selected_market_at_the_end_selects_the_one_before_it() {
        let mut list = Watchlist::from_markets([id("ETH"), id("xyz:NVDA"), id("xyz:SILVER")]);
        list.select(&id("xyz:SILVER"));
        list.remove(&id("xyz:SILVER"));
        assert_eq!(list.selected(), Some(&id("xyz:NVDA")));
    }

    #[test]
    fn removing_another_market_leaves_the_selection_alone() {
        let mut list = Watchlist::from_markets([id("ETH"), id("xyz:NVDA"), id("xyz:SILVER")]);
        list.select(&id("xyz:NVDA"));
        list.remove(&id("ETH"));
        assert_eq!(list.selected(), Some(&id("xyz:NVDA")));
    }

    #[test]
    fn removing_the_only_market_leaves_nothing_selected() {
        let mut list = Watchlist::from_markets([id("ETH")]);
        list.remove(&id("ETH"));
        assert_eq!(list.selected(), None);
        assert!(list.is_empty());
    }

    #[test]
    fn a_stored_selection_is_kept_and_a_stale_one_gives_way_to_the_first() {
        let markets = || [id("ETH"), id("xyz:NVDA"), id("BTC")];
        let kept = Watchlist::with_selection(markets(), Some(id("BTC")));
        assert_eq!(kept.selected(), Some(&id("BTC")));
        let stale = Watchlist::with_selection(markets(), Some(id("DOGE")));
        assert_eq!(stale.selected(), Some(&id("ETH")));
        let none = Watchlist::with_selection(markets(), None);
        assert_eq!(none.selected(), Some(&id("ETH")));
    }

    #[test]
    fn a_stored_list_cannot_break_the_rules() {
        let stored = (0..50).map(|i| id(&format!("M{}", i % 45)));
        let list = Watchlist::from_markets(stored);
        assert_eq!(list.markets().len(), LIMIT);
    }
}
