//! What the venue calls a market when it is acted on: a number, and how
//! finely a size in it can be written.
//!
//! Queries name a market by its coin (`BTC`, `xyz:NVDA`, `@107`); actions
//! name it by an asset number. The number is not in any answer: it is the
//! market's place in the venue's lists.
//!
//! - main-dex perpetual: its index in the dex's markets
//! - builder-dex perpetual: `100000 + 10000 * dex index + index`, the dex
//!   index being the dex's place in `perpDexs` (the main dex is 0)
//! - spot pair: `10000 +` the pair's own index

use crate::venue::{PerpMeta, SpotMeta};

/// A market, as an action needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Asset {
    pub number: u32,
    /// Decimal places a size may have.
    pub sz_decimals: u32,
    pub spot: bool,
}

impl Asset {
    /// Decimal places a price may have. The venue allows six in all for a
    /// perpetual and eight for spot, shared between price and size.
    pub fn price_decimals(&self) -> u32 {
        let total = if self.spot { 8 } else { 6 };
        total - self.sz_decimals.min(total)
    }
}

/// The perpetual `coin` on `dex`. `dexes` is the venue's list of dexes, main
/// dex first; `meta` is that dex's markets. `None` when the dex or the market
/// is not listed.
pub fn perp(dexes: &[String], dex: &str, meta: &PerpMeta, coin: &str) -> Option<Asset> {
    let dex_index = dexes.iter().position(|d| d == dex)? as u32;
    let index = meta.universe.iter().position(|m| m.name == coin)? as u32;
    let offset = if dex_index == 0 {
        0
    } else {
        100_000 + 10_000 * dex_index
    };
    Some(Asset {
        number: offset + index,
        sz_decimals: meta.universe[index as usize].sz_decimals,
        spot: false,
    })
}

/// The spot pair the venue calls `coin` (`PURR/USDC`, `@107`). Sizes are in
/// the base token.
pub fn spot(meta: &SpotMeta, coin: &str) -> Option<Asset> {
    let pair = meta.universe.iter().find(|p| p.name == coin)?;
    let base = meta.tokens.iter().find(|t| t.index == pair.tokens[0])?;
    Some(Asset {
        number: 10_000 + pair.index,
        sz_decimals: base.sz_decimals,
        spot: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::venue::decode;

    fn fixture<T: serde::de::DeserializeOwned>(query: &'static str, file: &str) -> T {
        let path = format!("{}/tests/fixtures/{file}.json", env!("CARGO_MANIFEST_DIR"));
        decode(query, &std::fs::read(path).unwrap()).unwrap()
    }

    fn dexes() -> Vec<String> {
        vec![String::new(), "xyz".into(), "flx".into()]
    }

    #[test]
    fn a_main_dex_market_is_its_place_in_the_list() {
        let meta: PerpMeta = fixture("meta", "meta");
        // The fixture lists BTC, ETH, SOL, HYPE.
        let btc = perp(&dexes(), "", &meta, "BTC").unwrap();
        assert_eq!((btc.number, btc.sz_decimals, btc.spot), (0, 5, false));
        assert_eq!(perp(&dexes(), "", &meta, "SOL").unwrap().number, 2);
    }

    #[test]
    fn a_builder_dex_market_is_offset_by_its_dex() {
        let meta: PerpMeta = fixture("meta", "meta_xyz");
        // `xyz` is the first builder dex: its markets start at 110000.
        assert_eq!(
            perp(&dexes(), "xyz", &meta, "xyz:NVDA").unwrap().number,
            110_000
        );
        assert_eq!(
            perp(&dexes(), "xyz", &meta, "xyz:SILVER").unwrap().number,
            110_002
        );
        // The second builder dex starts ten thousand later.
        assert_eq!(
            perp(&dexes(), "flx", &meta, "xyz:NVDA").unwrap().number,
            120_000
        );
    }

    #[test]
    fn a_spot_pair_is_numbered_by_its_own_index_and_sized_in_its_base_token() {
        let meta: SpotMeta = fixture("spotMeta", "spotMeta");
        let purr = spot(&meta, "PURR/USDC").unwrap();
        assert_eq!(
            (purr.number, purr.sz_decimals, purr.spot),
            (10_000, 0, true)
        );
        // `@107` is pair 107 whatever its place in the list.
        assert_eq!(spot(&meta, "@107").unwrap().number, 10_107);
    }

    #[test]
    fn an_unknown_market_or_dex_has_no_number() {
        let meta: PerpMeta = fixture("meta", "meta");
        assert_eq!(perp(&dexes(), "", &meta, "NOPE"), None);
        assert_eq!(perp(&dexes(), "nowhere", &meta, "BTC"), None);
        let spot_meta: SpotMeta = fixture("spotMeta", "spotMeta");
        assert_eq!(spot(&spot_meta, "BTC"), None);
    }

    #[test]
    fn price_places_are_what_size_places_leave() {
        let asset = |sz_decimals, spot| Asset {
            number: 0,
            sz_decimals,
            spot,
        };
        assert_eq!(asset(5, false).price_decimals(), 1);
        assert_eq!(asset(0, false).price_decimals(), 6);
        assert_eq!(asset(2, true).price_decimals(), 6);
        // Never negative.
        assert_eq!(asset(9, false).price_decimals(), 0);
    }
}
