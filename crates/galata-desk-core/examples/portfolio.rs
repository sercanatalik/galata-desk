//! Print one account's portfolio from the live venue.
//!
//! ```text
//! cargo run -p galata-desk-core --example portfolio -- 0x<address>
//! ```
//!
//! Read-only: it sends the same unsigned queries the app does.

use galata_desk_core::feed::Refresher;
use galata_desk_core::format as f;
use galata_desk_core::model::{self, Equity, Period, Scope, Wallet};
use galata_desk_core::venue::{Http, MarginMode, Side};

#[tokio::main]
async fn main() {
    let Some(user) = std::env::args().nth(1) else {
        eprintln!("usage: portfolio 0x<address>");
        std::process::exit(2);
    };
    let mut r = Refresher::new(Http::new(), user.clone());
    r.refresh_holdings(true).await;
    r.refresh_orders(true).await;
    r.refresh_overview().await;
    let snap = r.snapshot();
    if let Some(e) = snap.failure() {
        eprintln!("{e}");
        std::process::exit(1);
    }
    let holdings = snap.holdings.data.as_ref().expect("no failure");
    let orders = snap.orders.data.as_ref().expect("no failure");
    let overview = snap.overview.data.as_ref().expect("no failure");
    let equity = Equity {
        perps: holdings.perps_equity,
        spot: holdings.spot_equity,
        vault: overview.vault_equity,
        staking_hype: overview.staking_hype,
        collateral: holdings.collateral,
        venue_total: overview
            .history
            .period("day")
            .and_then(|d| d.account_value_history.last())
            .map(|p| p.1),
    };

    println!("Account        {}", f::short_address(&user));
    println!("Account value  {}", f::usd(equity.total()));
    println!(
        "14D volume     {}",
        f::usd_compact(model::volume_14d(&overview.fees))
    );
    println!(
        "Fees           perps {}   spot {}",
        f::fee_pair(overview.fees.user_cross_rate, overview.fees.user_add_rate),
        f::fee_pair(
            overview.fees.user_spot_cross_rate,
            overview.fees.user_spot_add_rate
        ),
    );
    println!(
        "Equity         perps {}   spot {}   vault {}   staking {}",
        f::usd(equity.perps),
        f::usd(equity.spot),
        f::usd(equity.vault),
        f::size(equity.staking_hype, "HYPE"),
    );
    println!("Collateral     {:?}", equity.collateral);
    // What the venue itself last recorded as the account's value, to compare.
    if let Some(venue) = overview
        .history
        .period("day")
        .and_then(|h| h.account_value_history.last())
    {
        println!(
            "Venue's value  {}  (latest point of its own history)",
            f::usd(venue.1)
        );
    }

    println!();
    for scope in Scope::ALL {
        for period in Period::ALL {
            if let Some(s) = model::period_stats(&overview.history, scope, period) {
                println!(
                    "{:<22} {:<9} PNL {:>18}  volume {:>22}  max drawdown {:>7}",
                    scope.label(),
                    period.label(),
                    s.pnl.map(f::usd_signed).unwrap_or_default(),
                    f::usd(s.volume),
                    f::drawdown(s.max_drawdown),
                );
            }
        }
    }

    println!("\nPositions ({})", holdings.positions.len());
    for p in holdings.positions.iter().take(12) {
        let tag = if p.dex.is_empty() {
            String::new()
        } else {
            format!(" {}", p.dex)
        };
        println!(
            "  {:<14} {:>20}  value {:>16}  entry {:>12}  mark {:>12}  {:>24}  liq {:>14}  margin {:>14} ({})  funding {:>12}",
            format!("{} {}x{tag}", p.market, p.leverage),
            f::size(p.size, &p.market),
            f::usd(p.value),
            f::price(p.entry),
            p.mark
                .map(|m| f::price_like(m, p.entry))
                .unwrap_or_default(),
            f::pnl_with_roe(p.pnl, p.roe),
            p.liquidation
                .map(|l| f::price_like(l, p.entry))
                .unwrap_or_else(|| "N/A".into()),
            f::usd(p.margin),
            match p.mode {
                MarginMode::Cross => "Cross",
                MarginMode::Isolated => "Isolated",
            },
            f::usd_signed(p.funding),
        );
    }

    println!("\nBalances ({})", holdings.balances.len());
    for b in &holdings.balances {
        println!(
            "  {:<8} {:<6} total {:>20}  on hold {:>16}  value {:>18}",
            b.asset,
            match b.wallet {
                Wallet::Perps => "perps",
                Wallet::Spot => "spot",
            },
            f::amount(b.total.round_dp(6)),
            f::amount(b.on_hold.round_dp(6)),
            f::usd_or_dash(b.usd),
        );
    }

    println!("\nOpen orders ({})", orders.len());
    for o in orders.iter().take(8) {
        println!(
            "  {:<12} {:<8} {:<4} {:>14} / {:<14} @ {:>12}{}{}",
            o.market,
            o.kind,
            match o.side {
                Side::Buy => "Buy",
                Side::Sell => "Sell",
            },
            f::amount(o.remaining),
            f::amount(o.original),
            f::price(o.price),
            if o.reduce_only { "  reduce-only" } else { "" },
            o.trigger
                .as_deref()
                .map(|t| format!("  {t}"))
                .unwrap_or_default(),
        );
    }
}
