//! How numbers read on the screen. One place, so a figure looks the same
//! wherever it appears.
//!
//! A gain carries `+` and a loss carries `-`, always: the sign is what tells
//! them apart for a reader who cannot rely on the colours.

use rust_decimal::{Decimal, RoundingStrategy};

const DASH: &str = "\u{2014}";

fn round(value: Decimal, places: u32) -> Decimal {
    value.round_dp_with_strategy(places, RoundingStrategy::MidpointAwayFromZero)
}

/// Thousands separators on the integer part of an unsigned decimal string.
fn group(unsigned: &str) -> String {
    let (int, frac) = match unsigned.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (unsigned, None),
    };
    let mut out = String::with_capacity(unsigned.len() + int.len() / 3);
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if let Some(frac) = frac {
        out.push('.');
        out.push_str(frac);
    }
    out
}

fn fixed(value: Decimal, places: u32) -> String {
    format!("{:.*}", places as usize, round(value.abs(), places))
}

/// `$65,004.73`, `-$246.25`. Zero and gains carry no sign.
pub fn usd(value: Decimal) -> String {
    let body = group(&fixed(value, 2));
    if round(value, 2) < Decimal::ZERO {
        format!("-${body}")
    } else {
        format!("${body}")
    }
}

/// `+$106.06`, `-$466.20`, `$0.00`.
pub fn usd_signed(value: Decimal) -> String {
    let rounded = round(value, 2);
    let body = group(&fixed(value, 2));
    if rounded > Decimal::ZERO {
        format!("+${body}")
    } else if rounded < Decimal::ZERO {
        format!("-${body}")
    } else {
        format!("${body}")
    }
}

/// A large amount, abbreviated: `$38k`, `$1.2M`, `$950`.
pub fn usd_compact(value: Decimal) -> String {
    let sign = if value < Decimal::ZERO { "-" } else { "" };
    let abs = value.abs();
    let (scaled, unit) = if abs >= Decimal::from(1_000_000_000u64) {
        (abs / Decimal::from(1_000_000_000u64), "B")
    } else if abs >= Decimal::from(1_000_000u64) {
        (abs / Decimal::from(1_000_000u64), "M")
    } else if abs >= Decimal::from(1_000u64) {
        (abs / Decimal::from(1_000u64), "k")
    } else {
        return format!("{sign}${}", round(abs, 0));
    };
    // One decimal below ten units (`$1.2k`), none from there up (`$38k`).
    let places = if scaled < Decimal::from(10) { 1 } else { 0 };
    let text = round(scaled, places).normalize().to_string();
    format!("{sign}${text}{unit}")
}

/// A fraction as a signed percentage: `0.212` is `+21.2%`.
pub fn pct_signed(fraction: Decimal, places: u32) -> String {
    let pct = round(fraction * Decimal::from(100), places);
    let body = format!("{:.*}", places as usize, pct.abs());
    if pct > Decimal::ZERO {
        format!("+{body}%")
    } else if pct < Decimal::ZERO {
        format!("-{body}%")
    } else {
        format!("{body}%")
    }
}

/// `-$466.20 (-23.3%)`: an unrealised PNL with its return on equity.
pub fn pnl_with_roe(pnl: Decimal, roe: Decimal) -> String {
    format!("{} ({})", usd_signed(pnl), pct_signed(roe, 1))
}

/// A fee rate as a percentage with four decimals: `0.00045` is `0.0450%`.
pub fn fee_pct(rate: Decimal) -> String {
    format!("{:.4}%", round(rate * Decimal::from(100), 4))
}

/// `0.0450% / 0.0150%`: taker, then maker.
pub fn fee_pair(taker: Decimal, maker: Decimal) -> String {
    format!("{} / {}", fee_pct(taker), fee_pct(maker))
}

/// A drawdown fraction: `0.0623` is `6.23%`. `None` is a dash.
pub fn drawdown(fraction: Option<Decimal>) -> String {
    match fraction {
        Some(f) => format!("{:.2}%", round(f * Decimal::from(100), 2)),
        None => DASH.into(),
    }
}

/// A price at the venue's own precision, grouped: `64,891`, `1,832.2`, `17.308`.
pub fn price(value: Decimal) -> String {
    let text = group(&value.abs().to_string());
    if value < Decimal::ZERO {
        format!("-{text}")
    } else {
        text
    }
}

/// A price the venue did not give.
pub fn price_or(value: Option<Decimal>, absent: &str) -> String {
    value.map(price).unwrap_or_else(|| absent.into())
}

/// A derived price, which has no precision of its own, rounded to the number
/// of decimals `like` carries.
pub fn price_like(value: Decimal, like: Decimal) -> String {
    price(round(value, like.scale()))
}

/// A size at the venue's precision with its asset: `13.6429 ETH`, `-0.15 BTC`.
pub fn size(value: Decimal, symbol: &str) -> String {
    format!("{} {symbol}", amount(value))
}

/// A quantity at the venue's precision, grouped and signed only when negative.
///
/// The venue sometimes sends a size with binary-float noise in it
/// (`1467709.6000000001`); nothing it lists is quoted past eight decimals, so
/// rounding there removes the noise and nothing else.
pub fn amount(value: Decimal) -> String {
    let text = group(&round(value.abs(), 8).normalize().to_string());
    if value < Decimal::ZERO {
        format!("-{text}")
    } else {
        text
    }
}

/// A USD value that may be unknown.
pub fn usd_or_dash(value: Option<Decimal>) -> String {
    value.map(usd).unwrap_or_else(|| DASH.into())
}

/// A fraction held as a float, as a signed percentage: `0.0271` is `+2.71%`.
pub fn pct_signed_f(fraction: f64, places: usize) -> String {
    let pct = fraction * 100.0;
    let body = format!("{:.*}", places, pct.abs());
    // Round first: a move too small to show carries no sign.
    let zero = body.chars().all(|c| c == '0' || c == '.');
    if zero {
        format!("{body}%")
    } else if pct > 0.0 {
        format!("+{body}%")
    } else {
        format!("-{body}%")
    }
}

/// A period's volatility: `0.019` is `1.9%`. `None` is a dash.
pub fn volatility(fraction: Option<f64>) -> String {
    match fraction {
        Some(f) => format!("{:.1}%", f * 100.0),
        None => DASH.into(),
    }
}

/// An hourly funding rate, signed, with four decimals: `0.000013` is `+0.0013%`.
pub fn funding_hourly(rate: Decimal) -> String {
    pct_signed(rate, 4)
}

/// An hourly funding rate as a yearly one: `0.000013` is `+11.4%`.
pub fn funding_yearly(rate: Decimal) -> String {
    pct_signed(rate * Decimal::from(24 * 365), 1)
}

/// Mark over oracle, signed, with three decimals. `None` is a dash.
pub fn premium(fraction: Option<Decimal>) -> String {
    fraction
        .map(|f| pct_signed(f, 3))
        .unwrap_or_else(|| DASH.into())
}

/// A large USD amount with one decimal: `$190.0M`, `$2.8B`, `$38.0k`.
pub fn usd_abbrev(value: Decimal) -> String {
    let abs = value.abs();
    let sign = if value < Decimal::ZERO { "-" } else { "" };
    let (scaled, unit) = if abs >= Decimal::from(1_000_000_000u64) {
        (abs / Decimal::from(1_000_000_000u64), "B")
    } else if abs >= Decimal::from(1_000_000u64) {
        (abs / Decimal::from(1_000_000u64), "M")
    } else if abs >= Decimal::from(1_000u64) {
        (abs / Decimal::from(1_000u64), "k")
    } else {
        return format!("{sign}${}", round(abs, 0));
    };
    format!("{sign}${:.1}{unit}", round(scaled, 1))
}

/// `0x12ab…9f3c`. Anything too short to shorten is returned whole.
pub fn short_address(address: &str) -> String {
    if address.len() <= 12 || !address.is_ascii() {
        return address.to_string();
    }
    format!("{}\u{2026}{}", &address[..6], &address[address.len() - 4..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    #[test]
    fn usd_amounts() {
        assert_eq!(usd(d("65004.73")), "$65,004.73");
        assert_eq!(usd(d("18330.7")), "$18,330.70");
        assert_eq!(usd(d("-246.25")), "-$246.25");
        assert_eq!(usd(d("0")), "$0.00");
        assert_eq!(usd(d("-0.001")), "$0.00");
        assert_eq!(usd(d("1234567.895")), "$1,234,567.90");
    }

    #[test]
    fn signed_usd_always_shows_which_way() {
        assert_eq!(usd_signed(d("106.06")), "+$106.06");
        assert_eq!(usd_signed(d("-466.2")), "-$466.20");
        assert_eq!(usd_signed(d("0")), "$0.00");
    }

    #[test]
    fn compact_volume() {
        assert_eq!(usd_compact(d("38000")), "$38k");
        assert_eq!(usd_compact(d("38499.99")), "$38k");
        assert_eq!(usd_compact(d("1234")), "$1.2k");
        assert_eq!(usd_compact(d("950.4")), "$950");
        assert_eq!(usd_compact(d("3400000")), "$3.4M");
        assert_eq!(usd_compact(d("194108024191.97")), "$194B");
        assert_eq!(usd_compact(d("0")), "$0");
    }

    #[test]
    fn pnl_with_return_on_equity() {
        assert_eq!(pnl_with_roe(d("-466.20"), d("-0.233")), "-$466.20 (-23.3%)");
        assert_eq!(pnl_with_roe(d("106.06"), d("0.212")), "+$106.06 (+21.2%)");
        assert_eq!(pnl_with_roe(d("-13.10"), d("-0.0104")), "-$13.10 (-1.0%)");
    }

    #[test]
    fn fee_rates() {
        assert_eq!(fee_pair(d("0.00045"), d("0.00015")), "0.0450% / 0.0150%");
        assert_eq!(fee_pct(d("0")), "0.0000%");
    }

    #[test]
    fn drawdown_percent_or_dash() {
        assert_eq!(drawdown(Some(d("0.0623"))), "6.23%");
        assert_eq!(drawdown(Some(d("0"))), "0.00%");
        assert_eq!(drawdown(None), "\u{2014}");
    }

    #[test]
    fn prices_keep_the_venues_precision() {
        assert_eq!(price(d("17.308")), "17.308");
        assert_eq!(price(d("64891")), "64,891");
        assert_eq!(price(d("1832.2")), "1,832.2");
        assert_eq!(price(d("47.0480")), "47.0480");
        assert_eq!(price_or(None, "N/A"), "N/A");
    }

    #[test]
    fn a_derived_price_borrows_precision() {
        assert_eq!(price_like(d("1831.20384"), d("1832.2")), "1,831.2");
        assert_eq!(price_like(d("64990.4"), d("64891")), "64,990");
    }

    #[test]
    fn sizes_carry_their_symbol() {
        assert_eq!(size(d("13.6429"), "ETH"), "13.6429 ETH");
        assert_eq!(size(d("-0.15424"), "BTC"), "-0.15424 BTC");
        assert_eq!(size(d("12.50"), "HYPE"), "12.5 HYPE");
        assert_eq!(size(d("0"), "HYPE"), "0 HYPE");
        assert_eq!(size(d("1234567"), "PURR"), "1,234,567 PURR");
        assert_eq!(size(d("1467709.6000000001"), "STRK"), "1,467,709.6 STRK");
    }

    #[test]
    fn market_figures() {
        assert_eq!(pct_signed_f(0.0142, 2), "+1.42%");
        assert_eq!(pct_signed_f(-0.0264, 2), "-2.64%");
        assert_eq!(pct_signed_f(0.0, 2), "0.00%");
        assert_eq!(pct_signed_f(-0.00001, 2), "0.00%");
        assert_eq!(volatility(Some(0.019)), "1.9%");
        assert_eq!(volatility(Some(0.0)), "0.0%");
        assert_eq!(volatility(None), "\u{2014}");
    }

    #[test]
    fn funding_and_premium() {
        assert_eq!(funding_hourly(d("0.000013")), "+0.0013%");
        assert_eq!(funding_hourly(d("-0.000013")), "-0.0013%");
        assert_eq!(funding_yearly(d("0.000013")), "+11.4%");
        assert_eq!(funding_yearly(d("-0.000013")), "-11.4%");
        assert_eq!(premium(Some(d("-0.00117"))), "-0.117%");
        assert_eq!(premium(None), "\u{2014}");
    }

    #[test]
    fn abbreviated_usd() {
        assert_eq!(usd_abbrev(d("190000000")), "$190.0M");
        assert_eq!(usd_abbrev(d("2840000000")), "$2.8B");
        assert_eq!(usd_abbrev(d("38000")), "$38.0k");
        assert_eq!(usd_abbrev(d("950")), "$950");
    }

    #[test]
    fn addresses_are_shortened() {
        assert_eq!(
            short_address("0x12ab34cd56ef7890aabbccddeeff00119f3c9f3c"),
            "0x12ab\u{2026}9f3c"
        );
        assert_eq!(short_address("0x12"), "0x12");
    }
}
