//! The account's history, as the History section lists it: trades, orders,
//! funding payments, and deposits and withdrawals.
//!
//! Each submodule turns the venue's rows into the rows a tab lists, with
//! the tab's totals. The period is applied to rows already held: narrowing
//! it never asks the venue for anything.

pub mod csv;
pub mod feed;
pub mod funding;
pub mod ledger;
pub mod loader;
pub mod orders;
pub mod trades;

const HOUR_MS: u64 = 3_600_000;
const DAY_MS: u64 = 24 * HOUR_MS;

/// How far back the section looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Period {
    Day,
    Week,
    #[default]
    Month,
    All,
}

impl Period {
    pub const ALL: [Period; 4] = [Period::Day, Period::Week, Period::Month, Period::All];

    pub fn label(self) -> &'static str {
        match self {
            Period::Day => "24H",
            Period::Week => "7D",
            Period::Month => "30D",
            Period::All => "All",
        }
    }

    /// The earliest time the period covers, at `now_ms`; `None` for All.
    pub fn since_ms(self, now_ms: u64) -> Option<u64> {
        let span = match self {
            Period::Day => DAY_MS,
            Period::Week => 7 * DAY_MS,
            Period::Month => 30 * DAY_MS,
            Period::All => return None,
        };
        Some(now_ms.saturating_sub(span))
    }

    /// Whether a row at `at_ms` falls within the period at `now_ms`.
    pub fn covers(self, at_ms: u64, now_ms: u64) -> bool {
        self.since_ms(now_ms).is_none_or(|since| at_ms >= since)
    }

    /// The rows of `rows` within the period, in their order.
    pub fn keep<'a, T>(
        self,
        rows: &'a [T],
        now_ms: u64,
        at_ms: impl Fn(&T) -> u64 + 'a,
    ) -> impl Iterator<Item = &'a T> + 'a {
        rows.iter().filter(move |r| self.covers(at_ms(r), now_ms))
    }
}

/// An address as a row shows it: `0x0104…703a`.
pub fn short_address(address: &str) -> String {
    if address.len() > 12 && address.is_char_boundary(6) {
        format!("{}\u{2026}{}", &address[..6], &address[address.len() - 4..])
    } else {
        address.to_string()
    }
}

/// A venue word in camel case as plain words: `reduceOnlyCanceled` reads
/// `Reduce only canceled`.
pub fn words(camel: &str) -> String {
    let mut out = String::new();
    for (i, c) in camel.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push(' ');
            out.extend(c.to_lowercase());
        } else if i == 0 {
            out.extend(c.to_uppercase());
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_795_000_000_000;

    #[test]
    fn each_period_reaches_back_its_span_and_all_keeps_everything() {
        assert!(Period::Day.covers(NOW - DAY_MS, NOW));
        assert!(!Period::Day.covers(NOW - DAY_MS - 1, NOW));
        assert!(Period::Week.covers(NOW - 7 * DAY_MS, NOW));
        assert!(!Period::Week.covers(NOW - 7 * DAY_MS - 1, NOW));
        assert!(Period::Month.covers(NOW - 30 * DAY_MS, NOW));
        assert!(!Period::Month.covers(NOW - 30 * DAY_MS - 1, NOW));
        assert!(Period::All.covers(0, NOW));
        assert_eq!(Period::All.since_ms(NOW), None);
        assert_eq!(Period::default(), Period::Month);
    }

    #[test]
    fn keeping_rows_within_a_period_keeps_their_order() {
        let rows = [NOW, NOW - 2 * DAY_MS, NOW - HOUR_MS, NOW - 40 * DAY_MS];
        let day: Vec<u64> = Period::Day.keep(&rows, NOW, |t| *t).copied().collect();
        assert_eq!(day, [NOW, NOW - HOUR_MS]);
        assert_eq!(Period::All.keep(&rows, NOW, |t| *t).count(), 4);
    }

    #[test]
    fn addresses_are_shortened_and_venue_words_spelled_out() {
        assert_eq!(
            short_address("0x010461c14e146ac35fe42271bdc1134ee31c703a"),
            "0x0104\u{2026}703a"
        );
        assert_eq!(short_address("0xab"), "0xab");
        assert_eq!(words("reduceOnlyCanceled"), "Reduce only canceled");
        assert_eq!(words("cStakingTransfer"), "C staking transfer");
    }
}
