//! What a market is, in words: the company or thing it references, where
//! that is listed, and what kind of market it is. Read from the venue's
//! annotation, whose description is the only place the company's name is
//! given.

use crate::venue::PerpAnnotation;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct About {
    /// The company the market references: `Anthropic, PBC`. `None` for a
    /// commodity, an index, or a description in a form not recognised.
    pub name: Option<String>,
    /// Where the referenced security trades: `Nasdaq: NVDA`. For a market
    /// not yet listed, where it is expected to: `Nasdaq: ANT (expected)`.
    pub listing: Option<String>,
    /// `Stock`, `Pre-IPO`, `Commodity`, `Index`, `Crypto`, or the venue's
    /// own word for a kind this build does not know.
    pub category: Option<String>,
    /// The name the venue's app shows, when it differs from the ticker.
    pub display_name: Option<String>,
    /// The venue's description, whole.
    pub description: Option<String>,
}

impl About {
    /// `ticker` is the market's name on its dex, without the dex: a display
    /// name equal to it is not repeated.
    pub fn of(annotation: &PerpAnnotation, ticker: &str) -> Self {
        let description = annotation
            .description
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty());
        About {
            name: description.and_then(company),
            listing: description.and_then(listing),
            category: annotation.category.as_deref().map(category_words),
            display_name: annotation
                .display_name
                .clone()
                .filter(|d| !d.is_empty() && !d.eq_ignore_ascii_case(ticker)),
            description: description.map(str::to_string),
        }
    }

    /// A main-dex market: the venue says nothing about it, but every one of
    /// them is a crypto asset.
    pub fn main_dex() -> Self {
        About {
            category: Some("Crypto".into()),
            ..About::default()
        }
    }

    /// The name, or failing that the category: what fits on a tile.
    pub fn headline(&self) -> Option<&str> {
        self.name.as_deref().or(self.category.as_deref())
    }
}

/// A page of a deployer's documentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocLink {
    pub label: &'static str,
    pub url: &'static str,
}

const fn link(label: &'static str, url: &'static str) -> DocLink {
    DocLink { label, url }
}

/// Where a market's deployer explains it: its specification and how its
/// oracle price is made. The venue does not say, so this is kept by hand;
/// a dex not listed here has no links. `pre_ipo` picks the pre-IPO
/// specifications where a deployer keeps them apart.
pub fn docs(dex: &str, pre_ipo: bool) -> Vec<DocLink> {
    match dex {
        "" => vec![link(
            "Hyperliquid oracle",
            "https://hyperliquid.gitbook.io/hyperliquid-docs/hypercore/oracle",
        )],
        "xyz" => vec![
            if pre_ipo {
                link(
                    "Pre-IPO specifications",
                    "https://docs.trade.xyz/perpetuals/specifications-and-schedules/pre-ipo-specification-index",
                )
            } else {
                link(
                    "Specifications",
                    "https://docs.trade.xyz/perpetuals/specifications-and-schedules/specification-index",
                )
            },
            link(
                "Oracle price",
                "https://docs.trade.xyz/perpetuals/mechanics/oracle-price",
            ),
            link("trade[XYZ] docs", "https://docs.trade.xyz"),
        ],
        "flx" => vec![link("Felix perps docs", "https://usefelix.gitbook.io/perps")],
        _ => vec![],
    }
    .into_iter()
    .chain((!dex.is_empty()).then(|| {
        link(
            "Builder-deployed perps (HIP-3)",
            "https://hyperliquid.gitbook.io/hyperliquid-docs/hyperliquid-improvement-proposals-hips/hip-3-builder-deployed-perpetuals",
        )
    }))
    .collect()
}

fn category_words(category: &str) -> String {
    match category {
        "stocks" => "Stock".into(),
        "preipo" => "Pre-IPO".into(),
        "commodities" => "Commodity".into(),
        "indices" => "Index".into(),
        "crypto" => "Crypto".into(),
        "fx" => "FX".into(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        }
    }
}

/// What follows a company's name in the venue's descriptions.
const AFTER_NAME: [&str; 7] = [
    " Class ",
    " common stock",
    " common share",
    " ordinary share",
    " ADS",
    " ADR",
    " American Depositary",
];

/// The company in "X references 1 share of NVIDIA Corporation common stock"
/// or "X references 1 SK hynix Inc. ADS".
fn company(description: &str) -> Option<String> {
    let (_, rest) = description.split_once(" references 1 ")?;
    let rest = rest.strip_prefix("share of ").unwrap_or(rest);
    // The first sentence only: a later one may mention a share class too.
    let sentence = rest.split(". ").next().unwrap_or(rest);
    let end = AFTER_NAME
        .iter()
        .filter_map(|marker| rest.find(marker))
        .filter(|&at| at <= sentence.len() + 1)
        .min()?;
    let name = rest[..end].trim();
    (!name.is_empty() && name.len() <= 80).then(|| name.to_string())
}

/// The first "(Exchange: TICKER)" in the description.
fn listing(description: &str) -> Option<String> {
    let mut rest = description;
    while let Some(open) = rest.find('(') {
        let after = &rest[open + 1..];
        let close = after.find(')')?;
        let inside = after[..close].trim();
        // "(Nasdaq: SKHY), representing …" and "(expected Nasdaq: ANT)".
        let (expected, inside) = match inside.strip_prefix("expected ") {
            Some(i) => (true, i),
            None => (false, inside),
        };
        if let Some((exchange, symbol)) = inside.split_once(": ")
            && !exchange.is_empty()
            && !exchange.contains(' ')
            && !symbol.is_empty()
            && !symbol.contains(' ')
        {
            return Some(if expected {
                format!("{exchange}: {symbol} (expected)")
            } else {
                format!("{exchange}: {symbol}")
            });
        }
        rest = &after[close..];
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn about(category: &str, display: &str, description: &str, ticker: &str) -> About {
        About::of(
            &PerpAnnotation {
                category: Some(category.into()),
                description: Some(description.into()),
                display_name: Some(display.into()),
                keywords: vec![],
            },
            ticker,
        )
    }

    #[test]
    fn a_pre_ipo_market_names_its_company_and_where_it_is_expected_to_list() {
        let a = about(
            "preipo",
            "ANT",
            "ANT references 1 share of Anthropic, PBC Class A common stock (expected Nasdaq: ANT), quoted in USD. ANT is a pre-IPO market: the referenced security has not yet listed, and upon listing the market is intended to convert to a standard perpetual. Anthropic develops the Claude family of AI models.",
            "ANT",
        );
        assert_eq!(a.name.as_deref(), Some("Anthropic, PBC"));
        assert_eq!(a.listing.as_deref(), Some("Nasdaq: ANT (expected)"));
        assert_eq!(a.category.as_deref(), Some("Pre-IPO"));
        assert_eq!(a.display_name, None);
    }

    #[test]
    fn a_stock_names_its_company_and_exchange() {
        let a = about(
            "stocks",
            "NVDA",
            "NVDA references 1 share of NVIDIA Corporation common stock (Nasdaq: NVDA), quoted in USD. NVIDIA designs accelerated-computing hardware, software, and platforms.",
            "NVDA",
        );
        assert_eq!(a.name.as_deref(), Some("NVIDIA Corporation"));
        assert_eq!(a.listing.as_deref(), Some("Nasdaq: NVDA"));
        assert_eq!(a.headline(), Some("NVIDIA Corporation"));
    }

    #[test]
    fn a_depositary_share_names_its_company() {
        let a = about(
            "stocks",
            "SKHY",
            "SKHY references 1 SK hynix Inc. ADS (Nasdaq: SKHY), representing one-tenth of 1 common share (KRX: 000660), quoted in USD.",
            "SKHY",
        );
        assert_eq!(a.name.as_deref(), Some("SK hynix Inc."));
        assert_eq!(a.listing.as_deref(), Some("Nasdaq: SKHY"));
        let baba = about(
            "stocks",
            "BABA",
            "BABA references 1 Alibaba Group Holding Limited ADS (NYSE: BABA), representing 8 ordinary shares (HKEX: 9988), quoted in USD.",
            "BABA",
        );
        assert_eq!(baba.name.as_deref(), Some("Alibaba Group Holding Limited"));
        assert_eq!(baba.listing.as_deref(), Some("NYSE: BABA"));
        let skhx = about(
            "stocks",
            "SKHYNIX",
            "SKHYNIX references 1 SK hynix Inc. common share (KRX: 000660). The oracle converts the KRW share price to USD.",
            "SKHX",
        );
        assert_eq!(skhx.name.as_deref(), Some("SK hynix Inc."));
        assert_eq!(skhx.display_name.as_deref(), Some("SKHYNIX"));
    }

    #[test]
    fn a_commodity_or_index_has_no_company_and_falls_back_to_its_kind() {
        let silver = about(
            "commodities",
            "SILVER",
            "SILVER references the USD spot price of 1 troy ounce of silver (XAG/USD).",
            "SILVER",
        );
        assert_eq!(silver.name, None);
        assert_eq!(silver.listing, None);
        assert_eq!(silver.headline(), Some("Commodity"));
        let index = about(
            "indices",
            "XYZ100",
            "XYZ100 tracks a modified market-capitalization-weighted index of 100 of the largest companies.",
            "XYZ100",
        );
        assert_eq!(index.name, None);
        assert_eq!(index.headline(), Some("Index"));
        let oil = about(
            "commodities",
            "WTIOIL",
            "WTIOIL references the USD price of 1 barrel of WTI Light Sweet Crude Oil.",
            "CL",
        );
        assert_eq!(oil.name, None);
        assert_eq!(oil.display_name.as_deref(), Some("WTIOIL"));
    }

    #[test]
    fn each_dex_links_its_own_docs_and_builder_dexes_link_hip3() {
        let xyz = docs("xyz", true);
        assert_eq!(xyz[0].label, "Pre-IPO specifications");
        assert!(xyz.iter().any(|l| l.label == "Oracle price"));
        assert_eq!(docs("xyz", false)[0].label, "Specifications");
        assert_eq!(docs("", false).len(), 1);
        // A dex with no docs known still links what HIP-3 is.
        let unknown = docs("io", false);
        assert_eq!(unknown.len(), 1);
        assert!(unknown[0].url.contains("hip-3"));
    }

    #[test]
    fn an_empty_annotation_says_nothing() {
        let a = About::of(&PerpAnnotation::default(), "X");
        assert_eq!(a, About::default());
        assert_eq!(About::main_dex().headline(), Some("Crypto"));
    }
}
