//! How a market tile is coloured.
//!
//! A tile's background is the panel colour mixed toward a target: blue for a
//! gain or positive funding, orange for a loss or negative funding, amber for
//! volatility. The mix runs from a trace at zero to 42% at a fixed bound, and
//! stops there: that is as strong as the amber can get while the tile's
//! lighter grey text keeps a contrast ratio of 4.5 to 1.

use galata_desk_core::market::MarketPeriod;
use galata_desk_core::market::feed::Tile;
use galata_desk_core::rust_decimal::prelude::ToPrimitive;
use gpui_kit::{Hsla, Rgba};

/// What the tiles are coloured by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColourBy {
    #[default]
    Change,
    Volatility,
    Funding,
}

impl ColourBy {
    pub const ALL: [ColourBy; 3] = [ColourBy::Change, ColourBy::Volatility, ColourBy::Funding];

    pub fn label(self) -> &'static str {
        match self {
            ColourBy::Change => "Change",
            ColourBy::Volatility => "Volatility",
            ColourBy::Funding => "Funding",
        }
    }

    /// The value at which the colour is strongest, as a fraction.
    pub fn bound(self, period: MarketPeriod) -> f64 {
        match (self, period) {
            (ColourBy::Change, MarketPeriod::Hour) => 0.01,
            (ColourBy::Change, MarketPeriod::Day) => 0.06,
            (ColourBy::Change, MarketPeriod::Week) => 0.15,
            (ColourBy::Volatility, MarketPeriod::Hour) => 0.015,
            (ColourBy::Volatility, MarketPeriod::Day) => 0.08,
            (ColourBy::Volatility, MarketPeriod::Week) => 0.20,
            // Funding is hourly whatever the period: 0.008% an hour.
            (ColourBy::Funding, _) => 0.00008,
        }
    }
}

type Rgb = (f32, f32, f32);

const PANEL: Rgb = (21., 24., 27.);
const GAIN: Rgb = (106., 169., 224.);
const LOSS: Rgb = (232., 132., 63.);
const AMBER: Rgb = (240., 167., 58.);

/// The weakest and strongest mix toward the target.
const TRACE: f32 = 0.06;
const FULL: f32 = 0.42;

fn mix(target: Rgb, strength: f32) -> Rgb {
    let t = TRACE + (FULL - TRACE) * strength.clamp(0.0, 1.0);
    (
        PANEL.0 + (target.0 - PANEL.0) * t,
        PANEL.1 + (target.1 - PANEL.1) * t,
        PANEL.2 + (target.2 - PANEL.2) * t,
    )
}

fn hsla(rgb: Rgb) -> Hsla {
    Rgba {
        r: rgb.0 / 255.,
        g: rgb.1 / 255.,
        b: rgb.2 / 255.,
        a: 1.,
    }
    .into()
}

/// The colour for a value on a scale. `signed` scales run from loss through
/// nothing to gain; the other runs from nothing to amber.
fn shade(colour: ColourBy, value: f64, bound: f64) -> Rgb {
    let strength = (value.abs() / bound) as f32;
    match colour {
        ColourBy::Volatility => mix(AMBER, strength),
        _ if value >= 0.0 => mix(GAIN, strength),
        _ => mix(LOSS, strength),
    }
}

/// The value a tile is coloured by, if it has arrived.
fn measure(colour: ColourBy, tile: &Tile) -> Option<f64> {
    match colour {
        ColourBy::Change => tile.stats.as_ref().map(|s| s.change),
        ColourBy::Volatility => tile.stats.as_ref().and_then(|s| s.volatility),
        ColourBy::Funding => tile.live.as_ref().and_then(|l| l.funding.to_f64()),
    }
}

/// A tile's background. A tile whose measure has not arrived is the plain panel.
pub fn background(colour: ColourBy, period: MarketPeriod, tile: &Tile) -> Hsla {
    match measure(colour, tile) {
        Some(value) => hsla(shade(colour, value, colour.bound(period))),
        None => hsla(PANEL),
    }
}

/// The legend: what the low end says, seven steps of colour, what the high
/// end says.
pub fn legend(colour: ColourBy, period: MarketPeriod) -> (String, Vec<Hsla>, String) {
    let bound = colour.bound(period);
    let steps: Vec<Hsla> = match colour {
        ColourBy::Volatility => (0..7)
            .map(|i| hsla(shade(colour, bound * i as f64 / 6.0, bound)))
            .collect(),
        _ => (-3..=3)
            .map(|i| hsla(shade(colour, bound * i as f64 / 3.0, bound)))
            .collect(),
    };
    let (low, high) = match colour {
        ColourBy::Change => {
            let pct = bound * 100.0;
            (format!("-{pct:.0}%"), format!("+{pct:.0}%"))
        }
        ColourBy::Volatility => ("calm".into(), "volatile".into()),
        ColourBy::Funding => ("shorts pay".into(), "longs pay".into()),
    };
    (low, steps, high)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG relative luminance of an sRGB colour.
    fn luminance(rgb: Rgb) -> f32 {
        let lin = |c: f32| {
            let c = c / 255.;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(rgb.0) + 0.7152 * lin(rgb.1) + 0.0722 * lin(rgb.2)
    }

    fn contrast(a: Rgb, b: Rgb) -> f32 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    /// The tile's text: the palette's `text`, and the lighter grey used for
    /// the range and volatility lines.
    const TEXT: Rgb = (236., 232., 223.);
    const SOFT: Rgb = (216., 211., 200.);

    #[test]
    fn text_stays_readable_on_the_strongest_tile() {
        for target in [GAIN, LOSS, AMBER] {
            let strongest = mix(target, 1.0);
            assert!(
                contrast(TEXT, strongest) >= 4.5,
                "{target:?}: {}",
                contrast(TEXT, strongest)
            );
            assert!(
                contrast(SOFT, strongest) >= 4.5,
                "{target:?}: {}",
                contrast(SOFT, strongest)
            );
        }
    }

    #[test]
    fn half_the_bound_is_half_strength() {
        // Up 3% over 24 hours, where the bound is 6%.
        let half = shade(
            ColourBy::Change,
            0.03,
            ColourBy::Change.bound(MarketPeriod::Day),
        );
        assert_eq!(half, mix(GAIN, 0.5));
    }

    #[test]
    fn beyond_the_bound_is_full_strength() {
        let bound = ColourBy::Change.bound(MarketPeriod::Day);
        assert_eq!(shade(ColourBy::Change, -0.20, bound), mix(LOSS, 1.0));
        assert_eq!(shade(ColourBy::Change, -0.06, bound), mix(LOSS, 1.0));
    }

    #[test]
    fn negative_funding_uses_the_loss_colour() {
        let bound = ColourBy::Funding.bound(MarketPeriod::Day);
        assert_eq!(shade(ColourBy::Funding, -0.00008, bound), mix(LOSS, 1.0));
        assert_eq!(shade(ColourBy::Funding, 0.00004, bound), mix(GAIN, 0.5));
    }

    #[test]
    fn the_legend_names_its_ends() {
        let (low, steps, high) = legend(ColourBy::Change, MarketPeriod::Day);
        assert_eq!((low.as_str(), high.as_str()), ("-6%", "+6%"));
        assert_eq!(steps.len(), 7);
        let (low, _, high) = legend(ColourBy::Change, MarketPeriod::Week);
        assert_eq!((low.as_str(), high.as_str()), ("-15%", "+15%"));
        let (low, _, high) = legend(ColourBy::Funding, MarketPeriod::Day);
        assert_eq!((low.as_str(), high.as_str()), ("shorts pay", "longs pay"));
        let (low, _, high) = legend(ColourBy::Volatility, MarketPeriod::Hour);
        assert_eq!((low.as_str(), high.as_str()), ("calm", "volatile"));
    }
}
