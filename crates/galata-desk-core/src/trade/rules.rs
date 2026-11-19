//! The venue's rules for a price and a size, checked before anything is sent.
//!
//! What the user typed is parsed as a decimal, never a float, and what is
//! sent is that decimal written out with no trailing zeros, as the venue's
//! own SDK writes it. So the number on screen is the number sent.

use std::str::FromStr;

use rust_decimal::{Decimal, RoundingStrategy};

use super::assets::Asset;
use super::sign::Tpsl;

/// The venue refuses an order worth less than this many dollars.
pub const MIN_VALUE: u32 = 10;
/// A price may have this many significant figures, unless it is whole.
pub const PRICE_FIGURES: u32 = 5;

/// A rule a typed price or size breaks, in the order they are checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Rule {
    #[error("Price must be a number above zero.")]
    PriceNotNumber,
    #[error("Size must be a number above zero.")]
    SizeNotNumber,
    #[error(
        "Price is too precise: at most {PRICE_FIGURES} significant figures and {places} decimal places here."
    )]
    PriceTooPrecise { places: u32 },
    #[error("Size is too fine: at most {places} decimal places here.")]
    SizeTooFine { places: u32 },
    #[error("The order would be worth less than the ${MIN_VALUE} minimum.")]
    BelowMinimum,
    #[error("Change the price or the size to amend.")]
    Unchanged,
    #[error("Size is more than the position holds.")]
    SizeAbovePosition,
    #[error("A partial close must be worth at least ${MIN_VALUE}. Close all of it, or more of it.")]
    PartialBelowMinimum,
    #[error("{}", wrong_side(*tpsl, *long))]
    WrongSide { tpsl: Tpsl, long: bool },
    #[error("There is no position to reduce.")]
    NoPositionToReduce,
    #[error("{}", if *buy { "A buy cannot reduce a long position." } else { "A sell cannot reduce a short position." })]
    ReduceWrongSide { buy: bool },
    #[error("Not enough to trade this size: at most {} here.", wire(*max))]
    AboveMaximum { max: Decimal },
    #[error("{}", order_trigger_side(*tpsl, *buy))]
    OrderTriggerSide { tpsl: Tpsl, buy: bool },
    #[error("{} price must be a number above zero.", trigger_name(*tpsl))]
    TriggerNotNumber { tpsl: Tpsl },
    #[error("{} price is too precise: at most {PRICE_FIGURES} significant figures and {places} decimal places here.", trigger_name(*tpsl))]
    TriggerTooPrecise { tpsl: Tpsl, places: u32 },
}

fn trigger_name(tpsl: Tpsl) -> &'static str {
    match tpsl {
        Tpsl::Tp => "Take-profit",
        Tpsl::Sl => "Stop-loss",
    }
}

fn order_trigger_side(tpsl: Tpsl, buy: bool) -> &'static str {
    match (tpsl, buy) {
        (Tpsl::Tp, true) => "A take-profit for a buy must be above its price.",
        (Tpsl::Tp, false) => "A take-profit for a sell must be below its price.",
        (Tpsl::Sl, true) => "A stop-loss for a buy must be below its price.",
        (Tpsl::Sl, false) => "A stop-loss for a sell must be above its price.",
    }
}

fn wrong_side(tpsl: Tpsl, long: bool) -> &'static str {
    match (tpsl, long) {
        (Tpsl::Tp, true) => "A take-profit for a long must be above the mark.",
        (Tpsl::Tp, false) => "A take-profit for a short must be below the mark.",
        (Tpsl::Sl, true) => "A stop-loss for a long must be below the mark.",
        (Tpsl::Sl, false) => "A stop-loss for a short must be above the mark.",
    }
}

/// A price and size that passed, as numbers and as the text that is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub price: Decimal,
    pub size: Decimal,
    pub price_text: String,
    pub size_text: String,
}

impl Checked {
    pub fn value(&self) -> Decimal {
        self.price * self.size
    }
}

/// What the order is now, to tell a change from none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Current {
    pub price: Decimal,
    pub size: Decimal,
}

/// A positive plain decimal: digits and at most one point. No sign, no
/// exponent, no thousands separators.
fn number(text: &str) -> Option<Decimal> {
    let text = text.trim();
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    Decimal::from_str(text)
        .ok()
        .filter(|d| *d > Decimal::ZERO)
        .map(|d| d.normalize())
}

/// The number as the venue's SDK writes it: no trailing zeros, no exponent.
pub fn wire(value: Decimal) -> String {
    value.normalize().to_string()
}

/// Significant figures of a normalised positive decimal: `0.00123` has three.
fn figures(value: Decimal) -> u32 {
    value.mantissa().unsigned_abs().to_string().len() as u32
}

/// How far from the mid a market close may fill, in percent.
pub const MARKET_SLIPPAGE_PCT: u32 = 5;
/// How far from its trigger price a market take-profit or stop-loss may
/// fill, in percent.
pub const TRIGGER_SLIPPAGE_PCT: u32 = 10;

/// `value` rounded to `count` significant figures, halves to even, as
/// Python's `f"{x:.5g}"` rounds in the venue's SDK.
fn round_figures(value: Decimal, count: u32) -> Decimal {
    let value = value.normalize();
    if value.is_zero() {
        return value;
    }
    // The power of ten of the leading digit: 154.2 is 2, 0.0123 is -2.
    let exponent = figures(value) as i64 - 1 - i64::from(value.scale());
    let keep = i64::from(count) - 1 - exponent;
    if keep >= 0 {
        value.round_dp_with_strategy(keep as u32, RoundingStrategy::MidpointNearestEven)
    } else {
        let step = Decimal::from(10u64.pow((-keep) as u32));
        (value / step).round_dp_with_strategy(0, RoundingStrategy::MidpointNearestEven) * step
    }
}

/// The worst price an order closing at market may fill at: `pct` percent
/// above `from` for a buy, below it for a sell, rounded as the venue's SDK
/// rounds it (five significant figures, then the market's price decimals).
/// `from` is the mid for a market close, and the trigger price for a market
/// take-profit or stop-loss.
pub fn slippage(from: Decimal, buy: bool, asset: &Asset, pct: u32) -> Decimal {
    let pct = Decimal::new(i64::from(pct), 2);
    let moved = if buy {
        from * (Decimal::ONE + pct)
    } else {
        from * (Decimal::ONE - pct)
    };
    round_figures(moved, PRICE_FIGURES)
        .round_dp_with_strategy(
            asset.price_decimals(),
            RoundingStrategy::MidpointNearestEven,
        )
        .normalize()
}

/// A price's precision against the market's rules.
fn precise_enough(price: Decimal, asset: &Asset) -> Result<(), Rule> {
    let places = asset.price_decimals();
    // A whole-number price is always allowed, however many figures it has.
    let whole = price.scale() == 0;
    if price.scale() > places || (!whole && figures(price) > PRICE_FIGURES) {
        return Err(Rule::PriceTooPrecise { places });
    }
    Ok(())
}

/// A typed size to close of a position holding `held` (signed), valued at
/// `price` (the worst price at market, the entered price at a limit).
/// Positive, no finer than the size step, and no more than the position.
/// A close that leaves part of the position must be worth at least the
/// venue's minimum, which binds reduce-only orders too; the whole position
/// may be closed whatever it is worth.
pub fn close_size(
    text: &str,
    held: Decimal,
    asset: &Asset,
    price: Decimal,
) -> Result<Decimal, Rule> {
    let size = number(text).ok_or(Rule::SizeNotNumber)?;
    if size.scale() > asset.sz_decimals {
        return Err(Rule::SizeTooFine {
            places: asset.sz_decimals,
        });
    }
    if size > held.abs() {
        return Err(Rule::SizeAbovePosition);
    }
    if size < held.abs() && size * price < Decimal::from(MIN_VALUE) {
        return Err(Rule::PartialBelowMinimum);
    }
    Ok(size)
}

/// `pct` percent of the position, rounded down to the size step, so that it
/// can never exceed the position.
pub fn share_of(held: Decimal, pct: u32, asset: &Asset) -> Decimal {
    (held.abs() * Decimal::from(pct) / Decimal::ONE_HUNDRED)
        .round_dp_with_strategy(asset.sz_decimals, RoundingStrategy::ToZero)
        .normalize()
}

/// A typed price for a limit close. Reduce-only, so the minimum value does
/// not apply; only the price's precision is checked.
pub fn limit_close_price(text: &str, asset: &Asset) -> Result<Decimal, Rule> {
    let price = number(text).ok_or(Rule::PriceNotNumber)?;
    precise_enough(price, asset)?;
    Ok(price)
}

/// Whether a limit close at `price` is already through the market, and so
/// fills at once: a sell (closing a long) at or below the mark, a buy
/// (closing a short) at or above it.
pub fn fills_at_once(price: Decimal, mark: Decimal, long: bool) -> bool {
    if long { price <= mark } else { price >= mark }
}

/// A typed take-profit or stop-loss price for a position, `long` or short,
/// marked at `mark`. Precise enough for the market, and on the side of the
/// mark where it does not trigger at once.
pub fn trigger_price(
    text: &str,
    asset: &Asset,
    long: bool,
    mark: Decimal,
    tpsl: Tpsl,
) -> Result<Decimal, Rule> {
    let price = number(text).ok_or(Rule::PriceNotNumber)?;
    precise_enough(price, asset)?;
    let above = price > mark;
    let below = price < mark;
    let ok = match (tpsl, long) {
        (Tpsl::Tp, true) | (Tpsl::Sl, false) => above,
        (Tpsl::Tp, false) | (Tpsl::Sl, true) => below,
    };
    if !ok {
        return Err(Rule::WrongSide { tpsl, long });
    }
    Ok(price)
}

/// How a typed order size is meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SizeUnit {
    /// US dollars: turned into the coin at the order's price.
    #[default]
    Usd,
    /// The market's coin.
    Coin,
}

/// A typed size in `unit`, as the coin size sent at `price`. Dollars are
/// divided by the price and rounded down to the size step, so the order is
/// never larger than typed. A coin size must already be on the step.
pub fn order_size(
    text: &str,
    unit: SizeUnit,
    price: Decimal,
    asset: &Asset,
) -> Result<Decimal, Rule> {
    let typed = number(text).ok_or(Rule::SizeNotNumber)?;
    match unit {
        SizeUnit::Coin => {
            if typed.scale() > asset.sz_decimals {
                return Err(Rule::SizeTooFine {
                    places: asset.sz_decimals,
                });
            }
            Ok(typed)
        }
        SizeUnit::Usd => Ok((typed / price)
            .round_dp_with_strategy(asset.sz_decimals, RoundingStrategy::ToZero)
            .normalize()),
    }
}

/// What the ticket holds, as typed.
#[derive(Debug, Clone, Copy)]
pub struct OrderInput<'a> {
    pub buy: bool,
    pub size: &'a str,
    pub unit: SizeUnit,
    /// The worst price of a market order, or the typed price of a limit one.
    pub price: OrderPrice<'a>,
    pub reduce_only: bool,
    /// Typed take-profit and stop-loss; empty or `None` for none.
    pub take_profit: Option<&'a str>,
    pub stop_loss: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
pub enum OrderPrice<'a> {
    Market { worst: Decimal },
    Limit { text: &'a str },
}

/// What is known about the market and the account.
#[derive(Debug, Clone, Copy)]
pub struct OrderContext {
    pub asset: Asset,
    /// The current position, signed; zero for none.
    pub position: Decimal,
    /// The venue's largest size on the order's side, once known.
    pub max_size: Option<Decimal>,
}

/// An order that passed every check, in the numbers that are sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckedOrder {
    pub size: Decimal,
    /// The price the order is held to: the worst price, or the limit.
    pub price: Decimal,
    pub take_profit: Option<Decimal>,
    pub stop_loss: Option<Decimal>,
}

impl CheckedOrder {
    pub fn value(&self) -> Decimal {
        self.size * self.price
    }
}

/// Check an order from the ticket against the venue's rules, in order:
/// size, size step, limit price, reduce-only, the minimum, the maximum,
/// then the take-profit and stop-loss.
pub fn check_order(input: &OrderInput<'_>, ctx: &OrderContext) -> Result<CheckedOrder, Rule> {
    let asset = &ctx.asset;
    if number(input.size).is_none() {
        return Err(Rule::SizeNotNumber);
    }
    let price = match input.price {
        OrderPrice::Market { worst } => worst,
        OrderPrice::Limit { text } => {
            let price = number(text).ok_or(Rule::PriceNotNumber)?;
            precise_enough(price, asset)?;
            price
        }
    };
    let size = order_size(input.size, input.unit, price, asset)?;
    let held = ctx.position;
    let closes_whole = input.reduce_only && size == held.abs();
    if input.reduce_only {
        if held.is_zero() {
            return Err(Rule::NoPositionToReduce);
        }
        let long = held > Decimal::ZERO;
        if input.buy == long {
            return Err(Rule::ReduceWrongSide { buy: input.buy });
        }
        if size > held.abs() {
            return Err(Rule::SizeAbovePosition);
        }
    }
    if size.is_zero() || (!closes_whole && size * price < Decimal::from(MIN_VALUE)) {
        return Err(Rule::BelowMinimum);
    }
    // A reduce-only order frees margin rather than using it.
    if !input.reduce_only
        && let Some(max) = ctx.max_size
        && size > max
    {
        return Err(Rule::AboveMaximum { max });
    }
    let trigger = |text: Option<&str>, tpsl: Tpsl| -> Result<Option<Decimal>, Rule> {
        let Some(text) = text.filter(|t| !t.trim().is_empty()) else {
            return Ok(None);
        };
        let at = number(text).ok_or(Rule::TriggerNotNumber { tpsl })?;
        precise_enough(at, asset).map_err(|_| Rule::TriggerTooPrecise {
            tpsl,
            places: asset.price_decimals(),
        })?;
        let above = at > price;
        let ok = match (tpsl, input.buy) {
            (Tpsl::Tp, true) | (Tpsl::Sl, false) => above,
            (Tpsl::Tp, false) | (Tpsl::Sl, true) => at < price,
        };
        if !ok {
            return Err(Rule::OrderTriggerSide {
                tpsl,
                buy: input.buy,
            });
        }
        Ok(Some(at))
    };
    let take_profit = trigger(input.take_profit, Tpsl::Tp)?;
    let stop_loss = trigger(input.stop_loss, Tpsl::Sl)?;
    Ok(CheckedOrder {
        size,
        price,
        take_profit,
        stop_loss,
    })
}

/// The estimated profit (positive) or loss of closing a position of signed
/// size `held`, entered at `entry`, at `price`. Fees and funding left out.
pub fn result_at(held: Decimal, entry: Decimal, price: Decimal) -> Decimal {
    (price - entry) * held
}

/// Check a typed price and size for `asset` against the venue's rules.
pub fn check(price: &str, size: &str, asset: &Asset, current: &Current) -> Result<Checked, Rule> {
    let price = number(price).ok_or(Rule::PriceNotNumber)?;
    let size = number(size).ok_or(Rule::SizeNotNumber)?;

    precise_enough(price, asset)?;
    if size.scale() > asset.sz_decimals {
        return Err(Rule::SizeTooFine {
            places: asset.sz_decimals,
        });
    }
    // The venue holds reduce-only orders to the minimum too (learnt on
    // testnet), so an amend never escapes it.
    if price * size < Decimal::from(MIN_VALUE) {
        return Err(Rule::BelowMinimum);
    }
    if price == current.price && size == current.size {
        return Err(Rule::Unchanged);
    }
    Ok(Checked {
        price,
        size,
        price_text: wire(price),
        size_text: wire(size),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).unwrap()
    }

    /// A stock-like perpetual: sizes to two places, so prices to four.
    const NVDA: Asset = Asset {
        number: 110_000,
        sz_decimals: 2,
        spot: false,
    };
    const BTC: Asset = Asset {
        number: 0,
        sz_decimals: 5,
        spot: false,
    };

    fn resting(price: &str, size: &str) -> Current {
        Current {
            price: dec(price),
            size: dec(size),
        }
    }

    #[test]
    fn valid_values_pass_and_are_written_as_the_venue_writes_them() {
        let ok = check("153.80", "15", &NVDA, &resting("154.20", "12")).unwrap();
        assert_eq!(ok.price_text, "153.8");
        assert_eq!(ok.size_text, "15");
        assert_eq!(ok.value(), dec("2307.0"));
        // Only one of the two needs to change.
        assert!(check("154.20", "13", &NVDA, &resting("154.20", "12")).is_ok());
        // Pasted white space is not part of the number.
        assert_eq!(
            check(" 153.8 ", "12.50\n", &NVDA, &resting("154.20", "12"))
                .unwrap()
                .size_text,
            "12.5"
        );
    }

    #[test]
    fn each_rule_is_reported() {
        let now = resting("154.20", "12");
        let cases: &[(&str, &str, Rule)] = &[
            ("", "12", Rule::PriceNotNumber),
            ("abc", "12", Rule::PriceNotNumber),
            ("0", "12", Rule::PriceNotNumber),
            ("-5", "12", Rule::PriceNotNumber),
            ("1e3", "12", Rule::PriceNotNumber),
            ("1,540", "12", Rule::PriceNotNumber),
            ("1.2.3", "12", Rule::PriceNotNumber),
            ("154.2", "", Rule::SizeNotNumber),
            ("154.2", "0.0", Rule::SizeNotNumber),
            // Six significant figures.
            ("154.201", "12", Rule::PriceTooPrecise { places: 4 }),
            // Five figures, but five decimal places where four are allowed.
            ("0.12345", "120", Rule::PriceTooPrecise { places: 4 }),
            ("154.2", "12.001", Rule::SizeTooFine { places: 2 }),
            ("154.2", "0.05", Rule::BelowMinimum),
            ("154.20", "12.00", Rule::Unchanged),
        ];
        for (price, size, rule) in cases {
            assert_eq!(
                check(price, size, &NVDA, &now).unwrap_err(),
                *rule,
                "{price} x {size}"
            );
        }
    }

    #[test]
    fn a_whole_price_may_have_any_number_of_figures() {
        // BTC sizes go to five places, so prices to one.
        let now = resting("91400", "0.05");
        assert_eq!(
            check("123456", "0.05", &BTC, &now).unwrap().price_text,
            "123456"
        );
        assert_eq!(
            check("91400.5", "0.05", &BTC, &now).unwrap_err(),
            Rule::PriceTooPrecise { places: 1 }
        );
        assert!(check("9140.5", "0.05", &BTC, &now).is_ok());
    }

    #[test]
    fn spot_shares_eight_places_between_price_and_size() {
        let purr = Asset {
            number: 10_000,
            sz_decimals: 0,
            spot: true,
        };
        let now = resting("0.2", "100");
        assert!(check("0.12345", "100", &purr, &now).is_ok());
        assert_eq!(
            check("0.2", "100.5", &purr, &now).unwrap_err(),
            Rule::SizeTooFine { places: 0 }
        );
    }

    #[test]
    fn an_amend_is_held_to_the_minimum_even_for_a_reduce_only_order() {
        // 0.05 at 150 is $7.50. The venue refuses a reduce-only order that
        // small, so the form does too.
        let closing = resting("154.20", "0.05");
        assert_eq!(
            check("150", "0.05", &NVDA, &closing),
            Err(Rule::BelowMinimum)
        );
    }

    #[test]
    fn the_messages_say_what_to_fix() {
        assert_eq!(
            Rule::PriceTooPrecise { places: 4 }.to_string(),
            "Price is too precise: at most 5 significant figures and 4 decimal places here."
        );
        assert_eq!(
            Rule::BelowMinimum.to_string(),
            "The order would be worth less than the $10 minimum."
        );
    }

    #[test]
    fn slippage_prices_round_as_the_venues_sdk_rounds_them() {
        let perp = |sz_decimals| Asset {
            number: 110_001,
            sz_decimals,
            spot: false,
        };
        // (from, buy, size decimals, percent, what the SDK's formula gives)
        let cases: &[(&str, bool, u32, u32, &str)] = &[
            ("154.20", false, 2, 5, "146.49"),
            ("151.165", false, 2, 5, "143.61"),
            // BTC: sizes to five places, so prices to one; five figures
            // leave a whole number.
            ("91437.5", true, 5, 5, "96009"),
            ("123456", false, 5, 5, "117280"),
            ("3728.35", true, 2, 5, "3914.8"),
            ("0.0123456", false, 0, 5, "0.011728"),
            // A market trigger's price, 10% past the trigger.
            ("2.71828", true, 1, 10, "2.9901"),
            ("160", false, 2, 10, "144"),
        ];
        for (from, buy, sz, pct, want) in cases {
            assert_eq!(
                wire(slippage(dec(from), *buy, &perp(*sz), *pct)),
                *want,
                "{from} buy={buy} sz={sz} {pct}%"
            );
        }
    }

    #[test]
    fn a_builder_dex_perp_is_not_rounded_as_spot() {
        // The SDK takes any asset number from 10,000 up for spot, which
        // includes builder-dex perpetuals; this app knows which is which.
        let xyz = Asset {
            number: 110_001,
            sz_decimals: 3,
            spot: false,
        };
        assert_eq!(wire(slippage(dec("12.34567"), false, &xyz, 5)), "11.728");
    }

    const XYZ_NVDA: Asset = Asset {
        number: 110_001,
        sz_decimals: 2,
        spot: false,
    };

    #[test]
    fn a_close_size_is_positive_on_the_step_and_within_the_position() {
        let at = dec("154.2");
        for held in [dec("10"), dec("-10")] {
            assert_eq!(close_size("4", held, &XYZ_NVDA, at), Ok(dec("4")));
            assert_eq!(close_size("10.00", held, &XYZ_NVDA, at), Ok(dec("10")));
            assert_eq!(
                close_size("10.01", held, &XYZ_NVDA, at),
                Err(Rule::SizeAbovePosition)
            );
            assert_eq!(
                close_size("2.505", held, &XYZ_NVDA, at),
                Err(Rule::SizeTooFine { places: 2 })
            );
            assert_eq!(
                close_size("0", held, &XYZ_NVDA, at),
                Err(Rule::SizeNotNumber)
            );
            assert_eq!(
                close_size("-1", held, &XYZ_NVDA, at),
                Err(Rule::SizeNotNumber)
            );
        }
        // BTC sizes go to five places.
        assert_eq!(
            close_size("0.00162", dec("0.00162"), &BTC, dec("61163")),
            Ok(dec("0.00162"))
        );
    }

    #[test]
    fn a_partial_close_must_be_worth_the_minimum_and_a_whole_one_need_not() {
        // A long of 0.046 at about 320: $14.73, as on testnet.
        let held = dec("0.046");
        let amzn = Asset {
            number: 750_013,
            sz_decimals: 3,
            spot: false,
        };
        // Half is $7.36: refused. 0.032 at the worst price 304.21 is $9.73.
        assert_eq!(
            close_size("0.023", held, &amzn, dec("320.22")),
            Err(Rule::PartialBelowMinimum)
        );
        assert_eq!(
            close_size("0.032", held, &amzn, dec("304.21")),
            Err(Rule::PartialBelowMinimum)
        );
        // Just over: 0.033 at 304.21 is $10.04.
        assert_eq!(
            close_size("0.033", held, &amzn, dec("304.21")),
            Ok(dec("0.033"))
        );
        // At a limit, valued at the limit price: 0.023 at 480.33 is $11.05.
        assert_eq!(
            close_size("0.023", held, &amzn, dec("480.33")),
            Ok(dec("0.023"))
        );
        // The whole of a $4.89 position, as closed on testnet.
        assert_eq!(
            close_size("0.00008", dec("0.00008"), &BTC, dec("58097")),
            Ok(dec("0.00008"))
        );
        assert_eq!(
            Rule::PartialBelowMinimum.to_string(),
            "A partial close must be worth at least $10. Close all of it, or more of it."
        );
    }

    #[test]
    fn a_share_of_a_position_rounds_down_to_the_step() {
        assert_eq!(share_of(dec("10"), 25, &XYZ_NVDA), dec("2.5"));
        assert_eq!(share_of(dec("-10"), 100, &XYZ_NVDA), dec("10"));
        // 0.333 of 1.01 would round up to 0.34 at two places; down is 0.33.
        assert_eq!(share_of(dec("1.01"), 33, &XYZ_NVDA), dec("0.33"));
        assert_eq!(share_of(dec("0.00162"), 75, &BTC), dec("0.00121"));
        // Every share stays within the position.
        for pct in [25, 50, 75, 100] {
            for held in ["0.07", "1.01", "13.67", "-2.99"] {
                let held = dec(held);
                assert!(share_of(held, pct, &XYZ_NVDA) <= held.abs());
            }
        }
    }

    #[test]
    fn a_limit_close_price_is_checked_for_precision_only() {
        assert_eq!(limit_close_price("158.00", &XYZ_NVDA), Ok(dec("158")));
        assert_eq!(
            limit_close_price("158.123", &XYZ_NVDA),
            Err(Rule::PriceTooPrecise { places: 4 })
        );
        assert_eq!(limit_close_price("x", &XYZ_NVDA), Err(Rule::PriceNotNumber));
        // A tiny reduce-only close is not held to the minimum value.
        assert_eq!(limit_close_price("0.5", &XYZ_NVDA), Ok(dec("0.5")));
    }

    #[test]
    fn a_limit_close_through_the_mark_fills_at_once() {
        let mark = dec("154.20");
        // Closing a long sells.
        assert!(fills_at_once(dec("150"), mark, true));
        assert!(!fills_at_once(dec("158"), mark, true));
        // Closing a short buys.
        assert!(fills_at_once(dec("158"), mark, false));
        assert!(!fills_at_once(dec("150"), mark, false));
    }

    #[test]
    fn a_trigger_must_be_on_the_side_of_the_mark_where_it_waits() {
        let mark = dec("154.20");
        let at = |text, long, tpsl| trigger_price(text, &XYZ_NVDA, long, mark, tpsl);
        assert_eq!(at("160", true, Tpsl::Tp), Ok(dec("160")));
        assert_eq!(at("148", true, Tpsl::Sl), Ok(dec("148")));
        assert_eq!(at("148", false, Tpsl::Tp), Ok(dec("148")));
        assert_eq!(at("160", false, Tpsl::Sl), Ok(dec("160")));
        for (text, long, tpsl) in [
            ("148", true, Tpsl::Tp),
            ("160", true, Tpsl::Sl),
            ("160", false, Tpsl::Tp),
            ("148", false, Tpsl::Sl),
            // At the mark it would trigger at once, either way.
            ("154.2", true, Tpsl::Sl),
        ] {
            assert_eq!(
                at(text, long, tpsl),
                Err(Rule::WrongSide { tpsl, long }),
                "{text} long={long} {tpsl:?}"
            );
        }
        assert_eq!(
            at("160.12345", true, Tpsl::Tp),
            Err(Rule::PriceTooPrecise { places: 4 })
        );
    }

    #[test]
    fn the_new_messages_say_what_to_fix() {
        assert_eq!(
            Rule::SizeAbovePosition.to_string(),
            "Size is more than the position holds."
        );
        assert_eq!(
            Rule::WrongSide {
                tpsl: Tpsl::Sl,
                long: true
            }
            .to_string(),
            "A stop-loss for a long must be below the mark."
        );
        assert_eq!(
            Rule::WrongSide {
                tpsl: Tpsl::Tp,
                long: false
            }
            .to_string(),
            "A take-profit for a short must be below the mark."
        );
    }

    #[test]
    fn the_result_at_a_price_is_signed_by_the_side() {
        // A long of 10 entered at 152, stopped at 148: about $40 lost.
        assert_eq!(result_at(dec("10"), dec("152"), dec("148")), dec("-40"));
        // A short of 10 entered at 152, taken at 148: $40 made.
        assert_eq!(result_at(dec("-10"), dec("152"), dec("148")), dec("40"));
    }

    const HYPE: Asset = Asset {
        number: 4,
        sz_decimals: 2,
        spot: false,
    };

    #[test]
    fn dollars_become_a_coin_size_rounded_down_to_the_step() {
        let at = dec("66.75");
        assert_eq!(order_size("100", SizeUnit::Usd, at, &HYPE), Ok(dec("1.49")));
        // Never more than typed: 1.4981… rounds down, not up.
        assert!(dec("1.49") * at <= dec("100"));
        assert_eq!(
            order_size("1.50", SizeUnit::Coin, at, &HYPE),
            Ok(dec("1.5"))
        );
        assert_eq!(
            order_size("1.505", SizeUnit::Coin, at, &HYPE),
            Err(Rule::SizeTooFine { places: 2 })
        );
        for bad in ["", "0", "-5", "abc"] {
            for unit in [SizeUnit::Usd, SizeUnit::Coin] {
                assert_eq!(order_size(bad, unit, at, &HYPE), Err(Rule::SizeNotNumber));
            }
        }
        // A few cents is no coin at all.
        assert_eq!(order_size("0.5", SizeUnit::Usd, at, &HYPE), Ok(dec("0")));
    }

    fn order<'a>(
        buy: bool,
        size: &'a str,
        unit: SizeUnit,
        price: OrderPrice<'a>,
    ) -> OrderInput<'a> {
        OrderInput {
            buy,
            size,
            unit,
            price,
            reduce_only: false,
            take_profit: None,
            stop_loss: None,
        }
    }

    fn context(position: &str, max: Option<&str>) -> OrderContext {
        OrderContext {
            asset: HYPE,
            position: dec(position),
            max_size: max.map(dec),
        }
    }

    const MARKET: OrderPrice<'static> = OrderPrice::Market {
        worst: Decimal::from_parts(70064, 0, 0, false, 3),
    };

    #[test]
    fn a_valid_order_comes_back_in_the_numbers_sent() {
        let ok = check_order(
            &order(true, "100", SizeUnit::Usd, MARKET),
            &context("0", Some("30")),
        )
        .unwrap();
        // $100 at the worst price of 70.064 is 1.42 HYPE.
        assert_eq!(ok.size, dec("1.42"));
        assert_eq!(ok.price, dec("70.064"));
        let limit = check_order(
            &order(
                false,
                "2",
                SizeUnit::Coin,
                OrderPrice::Limit { text: "72.50" },
            ),
            &context("0", Some("30")),
        )
        .unwrap();
        assert_eq!((limit.size, limit.price), (dec("2"), dec("72.5")));
    }

    #[test]
    fn each_order_rule_is_reported_in_order() {
        let limit = |text| OrderPrice::Limit { text };
        let none = context("0", Some("30"));
        let cases: Vec<(OrderInput, OrderContext, Rule)> = vec![
            (
                order(true, "", SizeUnit::Usd, MARKET),
                none,
                Rule::SizeNotNumber,
            ),
            (
                order(true, "1.505", SizeUnit::Coin, MARKET),
                none,
                Rule::SizeTooFine { places: 2 },
            ),
            (
                order(true, "2", SizeUnit::Coin, limit("x")),
                none,
                Rule::PriceNotNumber,
            ),
            (
                order(true, "2", SizeUnit::Coin, limit("66.12345")),
                none,
                Rule::PriceTooPrecise { places: 4 },
            ),
            (
                order(true, "5", SizeUnit::Usd, MARKET),
                none,
                Rule::BelowMinimum,
            ),
            (
                order(true, "0.10", SizeUnit::Coin, limit("72.5")),
                none,
                Rule::BelowMinimum,
            ),
            (
                order(true, "31", SizeUnit::Coin, MARKET),
                none,
                Rule::AboveMaximum { max: dec("30") },
            ),
        ];
        for (input, ctx, rule) in cases {
            assert_eq!(check_order(&input, &ctx), Err(rule), "{input:?}");
        }
        // Until the venue's maximum is known, it is not held against the order.
        assert!(
            check_order(
                &order(true, "31", SizeUnit::Coin, MARKET),
                &context("0", None)
            )
            .is_ok()
        );
    }

    #[test]
    fn reduce_only_shrinks_the_position_and_nothing_else() {
        let reduce = |buy, size| OrderInput {
            reduce_only: true,
            ..order(buy, size, SizeUnit::Coin, MARKET)
        };
        assert_eq!(
            check_order(&reduce(false, "1"), &context("0", None)),
            Err(Rule::NoPositionToReduce)
        );
        assert_eq!(
            check_order(&reduce(true, "1"), &context("12.5", None)),
            Err(Rule::ReduceWrongSide { buy: true })
        );
        assert_eq!(
            check_order(&reduce(false, "1"), &context("-12.5", None)),
            Err(Rule::ReduceWrongSide { buy: false })
        );
        assert_eq!(
            check_order(&reduce(false, "13"), &context("12.5", None)),
            Err(Rule::SizeAbovePosition)
        );
        // Part of a small position is held to the minimum; all of it is not.
        assert_eq!(
            check_order(&reduce(false, "0.05"), &context("0.1", None)),
            Err(Rule::BelowMinimum)
        );
        assert!(check_order(&reduce(false, "0.1"), &context("0.1", None)).is_ok());
        // Reduce-only frees margin: the maximum does not apply.
        assert!(check_order(&reduce(false, "12.5"), &context("12.5", Some("1"))).is_ok());
    }

    #[test]
    fn an_orders_take_profit_and_stop_loss_are_on_their_sides() {
        let with = |buy, tp, sl| OrderInput {
            take_profit: Some(tp),
            stop_loss: Some(sl),
            ..order(buy, "2", SizeUnit::Coin, OrderPrice::Limit { text: "66.5" })
        };
        let ctx = context("0", None);
        let ok = check_order(&with(true, "72", "63"), &ctx).unwrap();
        assert_eq!(
            (ok.take_profit, ok.stop_loss),
            (Some(dec("72")), Some(dec("63")))
        );
        let sell = check_order(&with(false, "63", "72"), &ctx).unwrap();
        assert_eq!(sell.take_profit, Some(dec("63")));
        // Either may be left empty.
        assert_eq!(
            check_order(&with(true, "72", " "), &ctx).unwrap().stop_loss,
            None
        );
        let cases = [
            (
                with(true, "60", "63"),
                Rule::OrderTriggerSide {
                    tpsl: Tpsl::Tp,
                    buy: true,
                },
            ),
            (
                with(true, "72", "70"),
                Rule::OrderTriggerSide {
                    tpsl: Tpsl::Sl,
                    buy: true,
                },
            ),
            (
                with(false, "70", "72"),
                Rule::OrderTriggerSide {
                    tpsl: Tpsl::Tp,
                    buy: false,
                },
            ),
            (
                with(false, "63", "60"),
                Rule::OrderTriggerSide {
                    tpsl: Tpsl::Sl,
                    buy: false,
                },
            ),
            (
                with(true, "x", "63"),
                Rule::TriggerNotNumber { tpsl: Tpsl::Tp },
            ),
            (
                with(true, "72", "63.12345"),
                Rule::TriggerTooPrecise {
                    tpsl: Tpsl::Sl,
                    places: 4,
                },
            ),
        ];
        for (input, rule) in cases {
            assert_eq!(check_order(&input, &ctx), Err(rule));
        }
    }

    #[test]
    fn the_order_messages_say_what_to_fix() {
        let cases = [
            (Rule::NoPositionToReduce, "There is no position to reduce."),
            (
                Rule::ReduceWrongSide { buy: true },
                "A buy cannot reduce a long position.",
            ),
            (
                Rule::AboveMaximum { max: dec("30.00") },
                "Not enough to trade this size: at most 30 here.",
            ),
            (
                Rule::OrderTriggerSide {
                    tpsl: Tpsl::Tp,
                    buy: true,
                },
                "A take-profit for a buy must be above its price.",
            ),
            (
                Rule::TriggerNotNumber { tpsl: Tpsl::Sl },
                "Stop-loss price must be a number above zero.",
            ),
        ];
        for (rule, text) in cases {
            assert_eq!(rule.to_string(), text);
        }
    }

    #[test]
    fn leading_zeros_are_not_significant() {
        assert_eq!(figures(dec("0.00123")), 3);
        assert_eq!(figures(dec("153.8")), 4);
        assert_eq!(wire(dec("153.80")), "153.8");
        assert_eq!(wire(dec("91400")), "91400");
    }
}
