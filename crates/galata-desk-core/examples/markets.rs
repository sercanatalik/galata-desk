//! Print live market tiles from the venue.
//!
//! ```text
//! cargo run -p galata-desk-core --example markets -- BTC xyz:NVDA
//! cargo run -p galata-desk-core --example markets -- --period 7d ETH SOL
//! ```
//!
//! Read-only: one WebSocket subscription pair per market, and one history
//! query each. `GALATA_DESK_WS_URL` and `GALATA_DESK_INFO_URL` point it at
//! other endpoints.

use std::time::Duration;

use galata_desk_core::format as f;
use galata_desk_core::market::feed::{Connection, MarketFeed};
use galata_desk_core::market::ws::WS_URL;
use galata_desk_core::market::{MarketId, MarketPeriod};
use galata_desk_core::venue::{Http, INFO_URL};

#[tokio::main]
async fn main() {
    let mut period = MarketPeriod::Day;
    let mut coins = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--period" {
            period = match args.next().as_deref() {
                Some("1h") => MarketPeriod::Hour,
                Some("7d") => MarketPeriod::Week,
                _ => MarketPeriod::Day,
            };
        } else {
            coins.push(MarketId::new(arg));
        }
    }
    if coins.is_empty() {
        eprintln!("usage: markets [--period 1h|24h|7d] <coin>...");
        std::process::exit(2);
    }

    let info = std::env::var("GALATA_DESK_INFO_URL").unwrap_or_else(|_| INFO_URL.into());
    let ws = std::env::var("GALATA_DESK_WS_URL").unwrap_or_else(|_| WS_URL.into());
    let feed = MarketFeed::live(Http::at(info), ws, period, coins);
    let mut snapshots = feed.snapshots();

    // Wait until every tile has both its live figures and its history, or
    // ten seconds, whichever comes first.
    let _ = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if snapshots.changed().await.is_err() {
                return;
            }
            let s = snapshots.borrow_and_update();
            if !s.tiles.is_empty()
                && s.tiles
                    .iter()
                    .all(|t| t.unavailable || (t.live.is_some() && t.stats.is_some()))
            {
                return;
            }
        }
    })
    .await;

    let snapshot = snapshots.borrow().clone();
    println!(
        "{} · {}",
        snapshot.period.label(),
        match snapshot.connection {
            Connection::Live => "live",
            Connection::Connecting => "not connected",
            Connection::Down { .. } => "connection lost",
        }
    );
    for tile in &snapshot.tiles {
        let market = tile.market.to_string();
        if tile.unavailable {
            println!("{market:<12} unavailable");
            continue;
        }
        let (Some(live), Some(stats)) = (&tile.live, &tile.stats) else {
            println!("{market:<12} still loading");
            continue;
        };
        println!(
            "{market:<12} last {:>12}  {:>8}  range {:>12} – {:<12} ({:>3.0}%)  vol {:>5}  funding {:>9} ({:>7}/yr)  premium {:>8}  24h {:>9}  OI {:>9}  {}",
            f::price(live.last),
            f::pct_signed_f(stats.change, 2),
            f::price(stats.low),
            f::price(stats.high),
            stats.marker * 100.0,
            f::volatility(stats.volatility),
            f::funding_hourly(live.funding),
            f::funding_yearly(live.funding),
            f::premium(live.premium),
            f::usd_abbrev(live.volume_usd),
            f::usd_abbrev(live.open_interest_usd),
            tile.max_leverage
                .map(|l| format!("up to {l}x"))
                .unwrap_or_default(),
        );
    }
}
