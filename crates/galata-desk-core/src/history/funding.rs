//! The Funding payments tab: totals by market, and every payment.

use std::collections::HashMap;

use rust_decimal::Decimal;

use crate::venue::UserFundingRow;

/// One payment, or for an older stretch a day's payments summed.
#[derive(Debug, Clone, PartialEq)]
pub struct Payment {
    /// The hour paid, or the day for a summed row.
    pub time_ms: u64,
    pub coin: String,
    pub long: bool,
    /// The position's size, unsigned.
    pub size: Decimal,
    /// Positive when received.
    pub usdc: Decimal,
    /// The hourly rate, or the day's average for a summed row.
    pub rate: Decimal,
    /// How many hourly payments the row holds.
    pub payments: u32,
}

impl Payment {
    pub fn of(row: &UserFundingRow) -> Self {
        Payment {
            time_ms: row.time,
            coin: row.delta.coin.clone(),
            long: row.delta.szi >= Decimal::ZERO,
            size: row.delta.szi.abs(),
            usdc: row.delta.usdc,
            rate: row.delta.funding_rate,
            payments: row.delta.n_samples.max(1),
        }
    }
}

/// Every payment held, newest first.
pub fn payments(rows: &[UserFundingRow]) -> Vec<Payment> {
    let mut out: Vec<Payment> = rows.iter().map(Payment::of).collect();
    out.sort_by(|a, b| b.time_ms.cmp(&a.time_ms).then(a.coin.cmp(&b.coin)));
    out
}

/// One market's funding over the period.
#[derive(Debug, Clone, PartialEq)]
pub struct MarketFunding {
    pub coin: String,
    /// The side of the latest payment.
    pub long: bool,
    /// The position's size now; `None` when it is no longer held.
    pub size_now: Option<Decimal>,
    pub payments: u32,
    /// The sum of payments made: negative or zero.
    pub paid: Decimal,
    /// The sum of payments received: positive or zero.
    pub received: Decimal,
    pub net: Decimal,
    /// The hourly rate averaged over every payment.
    pub mean_rate: Decimal,
}

/// Payments summed by market, the largest net cost first. `held` gives the
/// signed size now of each coin the account holds.
pub fn by_market<'a>(
    payments: impl IntoIterator<Item = &'a Payment>,
    held: &HashMap<String, Decimal>,
) -> Vec<MarketFunding> {
    let mut by: HashMap<&str, (MarketFunding, u64, Decimal)> = HashMap::new();
    for p in payments {
        let (m, latest, rate_sum) = by.entry(p.coin.as_str()).or_insert_with(|| {
            (
                MarketFunding {
                    coin: p.coin.clone(),
                    long: p.long,
                    size_now: held.get(&p.coin).map(|s| s.abs()).filter(|s| !s.is_zero()),
                    payments: 0,
                    paid: Decimal::ZERO,
                    received: Decimal::ZERO,
                    net: Decimal::ZERO,
                    mean_rate: Decimal::ZERO,
                },
                0,
                Decimal::ZERO,
            )
        });
        m.payments += p.payments;
        if p.usdc < Decimal::ZERO {
            m.paid += p.usdc;
        } else {
            m.received += p.usdc;
        }
        m.net += p.usdc;
        *rate_sum += p.rate * Decimal::from(p.payments);
        if p.time_ms >= *latest {
            *latest = p.time_ms;
            m.long = p.long;
        }
    }
    let mut out: Vec<MarketFunding> = by
        .into_values()
        .map(|(mut m, _, rate_sum)| {
            if m.payments > 0 {
                m.mean_rate = rate_sum / Decimal::from(m.payments);
            }
            m
        })
        .collect();
    out.sort_by(|a, b| a.net.cmp(&b.net).then(a.coin.cmp(&b.coin)));
    out
}

/// Net funding over the payments listed, and its two parts.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FundingTotals {
    pub payments: u32,
    pub markets: usize,
    pub paid: Decimal,
    pub received: Decimal,
    pub net: Decimal,
}

pub fn totals<'a>(payments: impl IntoIterator<Item = &'a Payment>) -> FundingTotals {
    let mut coins: Vec<&str> = Vec::new();
    let mut t = FundingTotals::default();
    for p in payments {
        t.payments += p.payments;
        if p.usdc < Decimal::ZERO {
            t.paid += p.usdc;
        } else {
            t.received += p.usdc;
        }
        t.net += p.usdc;
        if !coins.contains(&p.coin.as_str()) {
            coins.push(&p.coin);
        }
    }
    t.markets = coins.len();
    t
}

/// The Funding payments tab's filters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FundingFilter {
    pub coin: Option<String>,
    /// `Some(true)` for longs only, `Some(false)` for shorts only.
    pub long: Option<bool>,
}

impl FundingFilter {
    pub fn admits(&self, p: &Payment) -> bool {
        self.coin.as_ref().is_none_or(|c| *c == p.coin) && self.long.is_none_or(|l| l == p.long)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn pay(at: u64, coin: &str, szi: &str, usdc: &str, rate: &str, n: u32) -> Payment {
        Payment {
            time_ms: at,
            coin: coin.into(),
            long: d(szi) >= Decimal::ZERO,
            size: d(szi).abs(),
            usdc: d(usdc),
            rate: d(rate),
            payments: n,
        }
    }

    #[test]
    fn a_long_that_paid_shows_its_payments_paid_and_net() {
        // SILVER paid funding 720 times in 30 days: here as 30 summed days.
        let rows: Vec<Payment> = (0..30)
            .map(|i| {
                pay(
                    i,
                    "xyz:SILVER",
                    "212.54",
                    if i == 0 { "-1.95" } else { "-1.94" },
                    "0.000021",
                    24,
                )
            })
            .collect();
        let held: HashMap<String, Decimal> = [("xyz:SILVER".to_string(), d("212.54"))].into();
        let m = &by_market(&rows, &held)[0];
        assert_eq!(m.payments, 720);
        assert_eq!(m.paid, d("-58.21"));
        assert_eq!(m.received, Decimal::ZERO);
        assert_eq!(m.net, d("-58.21"));
        assert_eq!(m.size_now, Some(d("212.54")));
        assert_eq!(m.mean_rate, d("0.000021"));
        assert!(m.long);
    }

    #[test]
    fn a_market_no_longer_held_has_no_size_now() {
        let rows = [pay(1, "BTC", "-0.5", "0.2", "0.00001", 1)];
        let m = &by_market(&rows, &HashMap::new())[0];
        assert_eq!(m.size_now, None);
        assert!(!m.long);
        assert_eq!(m.received, d("0.2"));
    }

    #[test]
    fn the_largest_net_cost_comes_first_and_the_side_is_the_latest() {
        let rows = [
            pay(1, "A", "1", "-1", "0.00001", 1),
            pay(2, "B", "1", "-5", "0.00001", 1),
            pay(3, "C", "1", "2", "-0.00001", 1),
            pay(4, "A", "-1", "0.5", "0.00001", 1),
        ];
        let by = by_market(&rows, &HashMap::new());
        let order: Vec<&str> = by.iter().map(|m| m.coin.as_str()).collect();
        assert_eq!(order, ["B", "A", "C"]);
        assert!(!by[1].long);
    }

    #[test]
    fn net_funding_is_paid_plus_received() {
        let rows = [
            pay(1, "A", "1", "-201.50", "0", 1),
            pay(2, "B", "1", "17.28", "0", 1),
        ];
        let t = totals(&rows);
        assert_eq!(t.paid, d("-201.50"));
        assert_eq!(t.received, d("17.28"));
        assert_eq!(t.net, d("-184.22"));
        assert_eq!(t.markets, 2);
    }

    #[test]
    fn a_received_payment_and_filters() {
        let rows = [
            pay(1, "xyz:COPPER", "871.62", "0.0436", "-0.000009", 1),
            pay(2, "xyz:ANT", "-139.65", "-0.0161", "-0.000001", 1),
        ];
        let listed = payments(&[]);
        assert!(listed.is_empty());
        let shorts = FundingFilter {
            long: Some(false),
            ..Default::default()
        };
        let kept: Vec<&Payment> = rows.iter().filter(|p| shorts.admits(p)).collect();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].coin, "xyz:ANT");
        assert!(rows[0].usdc > Decimal::ZERO);
    }
}
