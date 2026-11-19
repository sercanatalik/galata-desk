//! The portfolio's figures, from recorded venue answers.

use std::collections::HashMap;

use galata_desk_core::model::*;
use galata_desk_core::venue::*;
use rust_decimal::Decimal;

fn load<T: serde::de::DeserializeOwned>(name: &'static str) -> T {
    let path = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
    decode(name, &std::fs::read(path).unwrap()).unwrap()
}

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

struct World {
    main: ClearinghouseState,
    xyz: ClearinghouseState,
    spot: SpotState,
    meta: SpotMeta,
    mids: HashMap<String, Decimal>,
    dexes: Vec<String>,
    perp_main: PerpMeta,
    perp_xyz: PerpMeta,
}

fn world() -> World {
    World {
        main: load("clearinghouseState"),
        xyz: load("clearinghouseState_xyz"),
        spot: load("spotClearinghouseState"),
        meta: load("spotMeta"),
        mids: load("allMids"),
        // As `perpDexs.json` lists them: main, `xyz`, `flx`.
        dexes: vec!["".into(), "xyz".into(), "flx".into()],
        perp_main: load("meta"),
        perp_xyz: load("meta_xyz"),
    }
}

impl World {
    fn holdings(&self) -> Holdings {
        self.holdings_with(Collateral::Separate)
    }

    fn holdings_with(&self, collateral: Collateral) -> Holdings {
        self.holdings_knowing(collateral, true)
    }

    /// With the dexes' lists of markets known, or not yet.
    fn holdings_knowing(&self, collateral: Collateral, markets: bool) -> Holdings {
        let dexes = [
            DexState {
                dex: "",
                state: &self.main,
                dexes: &self.dexes,
                perp: markets.then_some(&self.perp_main),
            },
            DexState {
                dex: "xyz",
                state: &self.xyz,
                dexes: &self.dexes,
                perp: markets.then_some(&self.perp_xyz),
            },
        ];
        holdings(&dexes, &self.spot, &self.meta, &self.mids, collateral)
    }
}

#[test]
fn positions_span_dexes_largest_first() {
    let h = world().holdings();
    assert_eq!(h.positions.len(), 8);
    assert!(h.positions.windows(2).all(|w| w[0].value >= w[1].value));
    let nvda = h.positions.iter().find(|p| p.market == "NVDA").unwrap();
    assert_eq!(nvda.dex, "xyz");
    assert_eq!(nvda.leverage, 10);
    assert_eq!(nvda.mode, MarginMode::Cross);
    assert!(nvda.is_long());
    let btc = h.positions.iter().find(|p| p.market == "BTC").unwrap();
    assert_eq!(btc.dex, "");
    assert!(!btc.is_long());
}

#[test]
fn a_short_positions_value_is_negative() {
    let h = world().holdings();
    let btc = h.positions.iter().find(|p| p.market == "BTC").unwrap();
    assert!(
        btc.value > Decimal::ZERO,
        "the venue reports value unsigned"
    );
    assert_eq!(btc.signed_value(), -btc.value);
    let nvda = h.positions.iter().find(|p| p.market == "NVDA").unwrap();
    assert_eq!(nvda.signed_value(), nvda.value);
}

#[test]
fn a_position_is_numbered_for_actions_once_its_dexs_markets_are_known() {
    let w = world();
    let h = w.holdings();
    // `xyz` is the second dex, and NVDA the first of its markets.
    let nvda = h.positions.iter().find(|p| p.market == "NVDA").unwrap();
    let asset = nvda.asset.unwrap();
    assert_eq!(asset.number, 110_000);
    assert!(!asset.spot);
    let btc = h.positions.iter().find(|p| p.market == "BTC").unwrap();
    assert_eq!(btc.asset.map(|a| a.number), Some(0));

    let unknown = w.holdings_knowing(Collateral::Separate, false);
    assert!(unknown.positions.iter().all(|p| p.asset.is_none()));
}

#[test]
fn mark_price_is_value_over_size() {
    let h = world().holdings();
    let nvda = h.positions.iter().find(|p| p.market == "NVDA").unwrap();
    // 19533.71407 / 123.467
    assert_eq!(nvda.mark.unwrap().round_dp(2), d("158.21"));
}

#[test]
fn funding_is_shown_from_the_accounts_side() {
    let h = world().holdings();
    // The venue reports funding paid; NVDA paid 22.3, so the row shows -22.3.
    let nvda = h.positions.iter().find(|p| p.market == "NVDA").unwrap();
    assert_eq!(nvda.funding, d("-22.3"));
}

#[test]
fn an_absent_liquidation_price_stays_absent() {
    let h = world().holdings();
    let silver = h.positions.iter().find(|p| p.market == "SILVER").unwrap();
    assert_eq!(silver.liquidation, None);
    assert_eq!(silver.mode, MarginMode::Isolated);
}

#[test]
fn balances_list_perps_then_spot_and_skip_empty_tokens() {
    let h = world().holdings();
    let names: Vec<_> = h
        .balances
        .iter()
        .map(|b| (b.asset.as_str(), b.wallet))
        .collect();
    assert_eq!(
        names,
        [
            ("USDC", Wallet::Perps),
            ("USDC", Wallet::Spot),
            ("HYPE", Wallet::Spot)
        ]
    );
    let hype = &h.balances[2];
    assert_eq!(hype.total, d("12.5"));
    assert_eq!(hype.on_hold, d("2.0"));
    assert_eq!(hype.available, d("10.5"));
    // 12.5 x 70.0585
    assert_eq!(hype.usd, Some(d("875.73125")));
}

#[test]
fn equity_adds_up() {
    let w = world();
    let h = w.holdings();
    assert_eq!(
        h.perps_equity,
        w.main.margin_summary.account_value + w.xyz.margin_summary.account_value
    );
    // 1250.5 USDC + 875.73125 of HYPE.
    assert_eq!(h.spot_equity, d("2126.23125"));

    let vaults: Vec<VaultEquity> = load("userVaultEquities");
    let staking: DelegatorSummary = load("delegatorSummary");
    let (vault, staking_hype) = slow_equity(&vaults, &staking);
    let equity = Equity {
        perps: h.perps_equity,
        spot: h.spot_equity,
        vault,
        staking_hype,
        collateral: h.collateral,
        venue_total: None,
    };
    assert_eq!(equity.total(), h.perps_equity + h.spot_equity + vault);
    assert_eq!(staking_hype, Decimal::ZERO);
}

#[test]
fn a_token_without_a_price_has_no_usd_value_and_adds_no_equity() {
    let mut w = world();
    w.mids.remove("@107");
    let h = w.holdings();
    assert_eq!(h.balances[2].usd, None);
    assert_eq!(h.spot_equity, d("1250.5"));
}

#[test]
fn orders_are_named_and_sorted_newest_first() {
    let w = world();
    let raw: Vec<OpenOrder> = load("frontendOpenOrders");
    let mut rows = orders("", &raw, &w.meta, &[], None);
    sort_orders(&mut rows);
    assert_eq!(rows.len(), 6);
    assert!(rows.windows(2).all(|x| x[0].placed_ms >= x[1].placed_ms));
    assert_eq!(rows[0].kind, "Limit");
    assert_eq!(rows[0].trigger, None);
}

#[test]
fn an_order_knows_whether_it_can_be_amended_and_what_its_market_is_called() {
    let w = world();
    let dexes = vec![String::new(), "xyz".to_string()];
    let main: PerpMeta = load("meta");
    let mut raw: Vec<OpenOrder> = load("frontendOpenOrders");
    // A stop order: no time in force, and not a plain limit order.
    raw[1].is_trigger = true;
    raw[1].order_type = "Stop Market".into();
    raw[1].tif = None;
    // A spot order comes with the main dex.
    raw[2].coin = "@107".into();
    raw[3].coin = "DELISTED".into();
    let rows = orders("", &raw, &w.meta, &dexes, Some(&main));

    // SOL is the third market in the list, sized to two places.
    assert_eq!(rows[0].tif.as_deref(), Some("Alo"));
    assert!(rows[0].amendable);
    let sol = rows[0].asset.unwrap();
    assert_eq!((sol.number, sol.sz_decimals, sol.spot), (2, 2, false));

    assert!(!rows[1].amendable);
    assert!(rows[1].asset.is_some());

    let spot = rows[2].asset.unwrap();
    assert_eq!((spot.number, spot.spot), (10_107, true));

    // A market that is not in the venue's list cannot be acted on.
    assert_eq!(rows[3].asset, None);
    // Nor can any perpetual before the list is known; spot still can.
    let early = orders("", &raw, &w.meta, &dexes, None);
    assert_eq!(early[0].asset, None);
    assert!(early[2].asset.is_some());

    // A builder-dex market is numbered from its dex's offset.
    let xyz: PerpMeta = load("meta_xyz");
    raw[0].coin = "xyz:GOLD".into();
    let on_xyz = orders("xyz", &raw, &w.meta, &dexes, Some(&xyz));
    assert_eq!(on_xyz[0].asset.unwrap().number, 110_001);
}

#[test]
fn spot_pairs_and_builder_coins_get_readable_names() {
    let w = world();
    let mut raw: Vec<OpenOrder> = load("frontendOpenOrders");
    raw[0].coin = "@107".into();
    raw[1].is_trigger = true;
    raw[1].trigger_condition = "Price above 80".into();
    assert_eq!(orders("", &raw, &w.meta, &[], None)[0].market, "HYPE/USDC");
    assert_eq!(
        orders("", &raw, &w.meta, &[], None)[1].trigger.as_deref(),
        Some("Price above 80")
    );
    raw[0].coin = "xyz:NVDA".into();
    assert_eq!(orders("xyz", &raw, &w.meta, &[], None)[0].market, "NVDA");
}

#[test]
fn fourteen_day_volume_sums_taker_and_maker() {
    let fees: UserFees = load("userFees");
    let expected: Decimal = fees
        .daily_user_vlm
        .iter()
        .rev()
        .take(14)
        .map(|v| v.user_cross + v.user_add)
        .sum();
    assert_eq!(volume_14d(&fees), expected);
    assert!(expected > Decimal::ZERO);
}

#[test]
fn period_stats_follow_scope_and_period() {
    let periods: Vec<(String, PeriodHistory)> = load("portfolio");
    let history = PortfolioHistory { periods };
    let week = period_stats(&history, Scope::All, Period::Week).unwrap();
    let all = period_stats(&history, Scope::All, Period::AllTime).unwrap();
    assert_ne!(week.volume, all.volume);
    assert_eq!(
        week.pnl,
        history
            .period("week")
            .unwrap()
            .pnl_history
            .last()
            .map(|p| p.1)
    );
    assert!(week.max_drawdown.is_some());
    assert_eq!(
        series(&history, Series::AccountValue, Scope::Perps, Period::Day),
        &history.period("perpDay").unwrap().account_value_history[..]
    );
}

#[test]
fn a_period_the_venue_does_not_report_has_no_stats() {
    let history = PortfolioHistory { periods: vec![] };
    assert!(period_stats(&history, Scope::All, Period::Day).is_none());
    assert!(series(&history, Series::Pnl, Scope::All, Period::Day).is_empty());
}

#[test]
fn the_venues_words_for_account_modes_are_understood() {
    assert_eq!(Collateral::from_venue("default"), Collateral::Separate);
    assert_eq!(Collateral::from_venue("disabled"), Collateral::Separate);
    assert_eq!(Collateral::from_venue("unifiedAccount"), Collateral::InSpot);
    assert_eq!(
        Collateral::from_venue("portfolioMargin"),
        Collateral::InSpot
    );
    assert_eq!(Collateral::from_venue("somethingNew"), Collateral::Unknown);
}

/// The shape of a real unified account: 18,244.29 in spot, of which
/// perpetuals account for 7,291.88. The venue's page says 18,250.40, the
/// spot figure a few seconds later, and not their sum.
#[test]
fn with_collateral_in_spot_perps_are_not_counted_twice() {
    let equity = Equity {
        perps: d("7291.88"),
        spot: d("18244.29"),
        vault: d("0"),
        staking_hype: d("0"),
        collateral: Collateral::InSpot,
        venue_total: Some(d("18250.40")),
    };
    assert_eq!(equity.total(), d("18244.29"));

    let separate = Equity {
        collateral: Collateral::Separate,
        ..equity.clone()
    };
    assert_eq!(separate.total(), d("25536.17"));
}

#[test]
fn vault_deposits_count_in_every_mode() {
    let equity = Equity {
        perps: d("100"),
        spot: d("1000"),
        vault: d("50"),
        staking_hype: d("0"),
        collateral: Collateral::InSpot,
        venue_total: None,
    };
    assert_eq!(equity.total(), d("1050"));
}

#[test]
fn an_unknown_mode_trusts_the_venues_own_figure() {
    let mut equity = Equity {
        perps: d("7291.88"),
        spot: d("18244.29"),
        vault: d("0"),
        staking_hype: d("0"),
        collateral: Collateral::Unknown,
        venue_total: Some(d("18250.40")),
    };
    assert_eq!(equity.total(), d("18250.40"));
    // With nothing from the venue to go on, the parts are summed.
    equity.venue_total = None;
    assert_eq!(equity.total(), d("25536.17"));
}

#[test]
fn with_collateral_in_spot_there_is_one_usdc_balance() {
    let h = world().holdings_with(Collateral::InSpot);
    let names: Vec<_> = h
        .balances
        .iter()
        .map(|b| (b.asset.as_str(), b.wallet))
        .collect();
    assert_eq!(names, [("USDC", Wallet::Spot), ("HYPE", Wallet::Spot)]);
    assert_eq!(h.collateral, Collateral::InSpot);
    // Positions are unaffected by the mode.
    assert_eq!(h.positions.len(), 8);
}

fn history() -> PortfolioHistory {
    let periods: Vec<(String, PeriodHistory)> = load("portfolio");
    PortfolioHistory { periods }
}

#[test]
fn the_account_summary_adds_up_what_the_footer_shows() {
    let w = world();
    let h = w.holdings();
    let equity = Equity {
        perps: h.perps_equity,
        spot: h.spot_equity,
        vault: d("0"),
        staking_hype: d("0"),
        collateral: h.collateral,
        venue_total: None,
    };
    let history = history();
    let s = account_summary(&h, &equity, &history);

    assert_eq!(s.equity, equity.total());
    assert_eq!(s.positions, 8);
    let unrealised: Decimal = h.positions.iter().map(|p| p.pnl).sum();
    assert_eq!(s.unrealised, unrealised);
    assert_eq!(s.unrealised_share, Some(unrealised / equity.total()));
    let margin: Decimal = h.positions.iter().map(|p| p.margin).sum();
    assert_eq!(s.margin_share, Some(margin / equity.total()));
    // The period figures are the venue's own.
    assert_eq!(
        s.pnl_day,
        history
            .period("day")
            .unwrap()
            .pnl_history
            .last()
            .map(|p| p.1)
    );
    assert_eq!(
        s.pnl_all_time,
        history
            .period("allTime")
            .unwrap()
            .pnl_history
            .last()
            .map(|p| p.1)
    );
}

#[test]
fn an_account_with_nothing_in_it_has_no_shares_to_show() {
    let empty = Holdings::default();
    let s = account_summary(
        &empty,
        &Equity::default(),
        &PortfolioHistory { periods: vec![] },
    );
    assert_eq!(s.equity, d("0"));
    assert_eq!(s.unrealised, d("0"));
    assert_eq!(s.positions, 0);
    // Dividing by a worthless account would be nonsense, not zero.
    assert_eq!(s.unrealised_share, None);
    assert_eq!(s.margin_share, None);
    assert_eq!(s.pnl_day, None);
    assert_eq!(s.pnl_all_time, None);
}

#[test]
fn with_collateral_in_spot_the_summary_uses_the_single_pot() {
    let h = world().holdings_with(Collateral::InSpot);
    let equity = Equity {
        perps: d("7291.88"),
        spot: d("18244.29"),
        vault: d("0"),
        staking_hype: d("0"),
        collateral: Collateral::InSpot,
        venue_total: None,
    };
    let s = account_summary(&h, &equity, &history());
    assert_eq!(s.equity, d("18244.29"));
}

#[test]
fn a_positions_take_profit_and_stop_loss_are_told_from_fixed_size_triggers() {
    use galata_desk_core::trade::sign::Tpsl;
    let w = world();
    // Recorded on testnet: a fixed-size TP and SL, then a position-wide pair.
    let raw: Vec<OpenOrder> = load("frontendOpenOrders_tpsl");
    let rows = orders("xyz", &raw, &w.meta, &[], None);
    assert_eq!(rows.len(), 4);
    let by_oid = |oid: u64| rows.iter().find(|r| r.oid == oid).unwrap();

    // Fixed size: trigger orders, but not the position's TP/SL.
    let fixed_sl = by_oid(65559626178);
    assert_eq!(fixed_sl.tpsl, None);
    assert_eq!(fixed_sl.trigger_px, Some(d("240.16")));
    assert_eq!(fixed_sl.remaining, d("0.046"));
    assert_eq!(by_oid(65559626177).tpsl, None);

    // Position-wide: listed with no size of their own.
    let sl = by_oid(65559727016);
    assert_eq!(sl.tpsl, Some(Tpsl::Sl));
    assert_eq!(sl.trigger_px, Some(d("224.15")));
    assert_eq!(sl.remaining, Decimal::ZERO);
    assert!(sl.reduce_only);
    assert!(!sl.amendable);
    assert_eq!(sl.market, "AMZN");
    let tp = by_oid(65559726352);
    assert_eq!(tp.tpsl, Some(Tpsl::Tp));
    assert_eq!(tp.trigger_px, Some(d("400.28")));
}

#[test]
fn a_plain_order_has_no_trigger_price() {
    let w = world();
    let raw: Vec<OpenOrder> = load("frontendOpenOrders");
    for row in orders("", &raw, &w.meta, &[], None) {
        assert_eq!(row.trigger_px, None);
        assert_eq!(row.tpsl, None);
    }
}
