//! Every query decodes its recorded answer, and a missing field is an error
//! that names where it was expected.

use std::collections::HashMap;

use galata_desk_core::venue::*;
use rust_decimal::Decimal;
use serde_json::Value;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn dec(s: &str) -> Decimal {
    s.parse().unwrap()
}

/// Answers each query type from its fixture, and records what was asked.
#[derive(Clone, Default)]
struct Recorded {
    asked: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
}

impl Transport for Recorded {
    async fn post(&self, body: Value) -> Result<Vec<u8>, VenueError> {
        let kind = body["type"].as_str().unwrap().to_string();
        // A per-coin query is answered from `<type>_<coin>`, the coin's
        // prefix colon written as an underscore (`activeAssetData_xyz_NVDA`).
        let name = match (
            body.get("dex").and_then(Value::as_str),
            body.get("coin").and_then(Value::as_str),
        ) {
            (Some(dex), _) => format!("{kind}_{dex}"),
            (None, Some(coin)) if kind == "activeAssetData" => {
                format!("{kind}_{}", coin.replace(':', "_"))
            }
            _ => kind,
        };
        self.asked.lock().unwrap().push(body);
        Ok(fixture(&name))
    }
}

const USER: &str = "0x010461c14e146ac35fe42271bdc1134ee31c703a";

#[tokio::test]
async fn every_query_decodes_its_recorded_answer() {
    let info = Info::new(Recorded::default());

    assert_eq!(info.perp_dexs().await.unwrap(), ["", "xyz", "flx"]);

    let main = info.clearinghouse_state(USER, "").await.unwrap();
    assert_eq!(main.asset_positions.len(), 6);
    let btc = &main.asset_positions[0].position;
    assert_eq!(btc.coin, "BTC");
    assert_eq!(btc.szi, dec("-0.08698"));
    assert_eq!(btc.leverage.mode, MarginMode::Cross);
    assert_eq!(btc.leverage.value, 20);

    let xyz = info.clearinghouse_state(USER, "xyz").await.unwrap();
    assert_eq!(xyz.asset_positions[0].position.coin, "xyz:NVDA");
    assert_eq!(xyz.asset_positions[1].position.liquidation_px, None);
    assert_eq!(
        xyz.asset_positions[1].position.leverage.mode,
        MarginMode::Isolated
    );

    let spot = info.spot_state(USER).await.unwrap();
    assert_eq!(spot.balances[1].coin, "HYPE");
    assert_eq!(spot.balances[1].hold, dec("2.0"));

    let orders = info.open_orders(USER, "").await.unwrap();
    assert_eq!(orders.len(), 6);
    assert_eq!(orders[0].side, Side::Sell);
    assert_eq!(orders[0].order_type, "Limit");
    assert_eq!(orders[0].tif.as_deref(), Some("Alo"));
    assert_eq!(orders[0].oid, 607756224194);

    // Size steps, and the numbers a spot pair is acted on by.
    let meta = info.perp_meta("").await.unwrap();
    assert_eq!(meta.universe[0].name, "BTC");
    assert_eq!(meta.universe[0].sz_decimals, 5);
    let spot = info.spot_meta().await.unwrap();
    assert_eq!(spot.universe[1].index, 107);
    assert_eq!(spot.tokens[1].sz_decimals, 0);

    let history = info.portfolio(USER).await.unwrap();
    for key in [
        "day",
        "week",
        "month",
        "allTime",
        "perpDay",
        "perpWeek",
        "perpMonth",
        "perpAllTime",
    ] {
        assert!(history.period(key).is_some(), "{key}");
    }

    let fees = info.user_fees(USER).await.unwrap();
    assert!(fees.user_cross_rate > Decimal::ZERO);
    assert!(!fees.daily_user_vlm.is_empty());

    assert_eq!(info.vault_equities(USER).await.unwrap().len(), 7);
    assert_eq!(
        info.delegator_summary(USER).await.unwrap().delegated,
        Decimal::ZERO
    );

    let meta = info.spot_meta().await.unwrap();
    assert_eq!(meta.universe[1].name, "@107");

    let mids: HashMap<String, Decimal> = info.all_mids().await.unwrap();
    assert_eq!(mids["@107"], dec("70.0585"));
}

#[tokio::test]
async fn the_main_dex_is_asked_without_a_dex_key() {
    let transport = Recorded::default();
    let info = Info::new(transport.clone());
    info.clearinghouse_state(USER, "").await.unwrap();
    info.clearinghouse_state(USER, "xyz").await.unwrap();
    let asked = transport.asked.lock().unwrap();
    assert!(asked[0].get("dex").is_none());
    assert_eq!(asked[1]["dex"], "xyz");
}

#[tokio::test]
async fn a_builder_dex_mid_is_asked_of_that_dex_and_found_by_its_prefixed_name() {
    let transport = Recorded::default();
    let info = Info::new(transport.clone());
    // `allMids_xyz.json` was recorded from the live venue: its keys carry the
    // dex's prefix.
    assert_eq!(info.mid("xyz", "NVDA").await.unwrap(), Some(dec("151.165")));
    assert_eq!(info.mid("xyz", "NOPE").await.unwrap(), None);
    // The main dex is asked without a dex, and named without a prefix.
    assert_eq!(info.mid("", "@107").await.unwrap(), Some(dec("70.0585")));
    let asked = transport.asked.lock().unwrap();
    assert_eq!(asked[0]["dex"], "xyz");
    assert!(asked[2].get("dex").is_none());
}

#[test]
fn a_missing_field_is_an_error_that_names_its_path() {
    let mut value: Value = serde_json::from_slice(&fixture("clearinghouseState")).unwrap();
    value["assetPositions"][2]["position"]
        .as_object_mut()
        .unwrap()
        .remove("unrealizedPnl");
    let bytes = serde_json::to_vec(&value).unwrap();
    let error = decode::<ClearinghouseState>("clearinghouseState", &bytes).unwrap_err();
    match error {
        VenueError::Shape {
            query,
            path,
            detail,
        } => {
            assert_eq!(query, "clearinghouseState");
            assert_eq!(path, "assetPositions[2].position");
            assert!(detail.contains("unrealizedPnl"), "{detail}");
        }
        other => panic!("expected a shape error, got {other:?}"),
    }
}

#[test]
fn venue_precision_survives_decoding() {
    let xyz: ClearinghouseState =
        decode("clearinghouseState", &fixture("clearinghouseState_xyz")).unwrap();
    assert_eq!(
        xyz.asset_positions[0]
            .position
            .liquidation_px
            .unwrap()
            .to_string(),
        "17.308"
    );
}

#[test]
fn funding_rates_pair_each_market_with_its_context() {
    use galata_desk_core::venue::{AssetCtx, PerpMeta};
    for (file, coin) in [
        ("metaAndAssetCtxs", "BTC"),
        ("metaAndAssetCtxs_xyz", "xyz:NVDA"),
    ] {
        let (meta, ctxs): (PerpMeta, Vec<AssetCtx>) =
            decode("metaAndAssetCtxs", &fixture(file)).unwrap();
        assert_eq!(meta.universe.len(), ctxs.len());
        let at = meta.universe.iter().position(|m| m.name == coin).unwrap();
        assert!(
            ctxs[at].funding.abs() < "0.01".parse().unwrap(),
            "an hourly rate is small"
        );
    }
}

#[test]
fn user_fills_decode_with_side_price_size_time_and_realised_pnl() {
    use galata_desk_core::venue::{Side, UserFill};
    let fills: Vec<UserFill> = decode("userFills", &fixture("userFills")).unwrap();
    assert_eq!(fills.len(), 6);
    assert!(
        fills
            .iter()
            .all(|f| f.time > 1_700_000_000_000 && !f.sz.is_zero())
    );
    let nvda = fills.iter().find(|f| f.coin == "xyz:NVDA").unwrap();
    assert_eq!(nvda.side, Side::Buy);
    assert_eq!(nvda.px.to_string(), "155.42");
    assert_eq!(nvda.sz.to_string(), "12.5");
    assert!(nvda.closed_pnl.is_zero());
    assert_eq!(nvda.fee.to_string(), "0.006392");
    assert_eq!(nvda.tid, 1);
    let pnut = fills.iter().find(|f| f.coin == "PNUT").unwrap();
    assert_eq!(pnut.tid, 327_364_237_134_565);
    assert_eq!(pnut.closed_pnl.to_string(), "0.22899");
    // A maker fill with no fee decodes as zero, not as missing.
    let snx = fills.iter().find(|f| f.coin == "SNX").unwrap();
    assert!(snx.fee.is_zero());
    // Every trade id differs.
    let mut ids: Vec<u64> = fills.iter().map(|f| f.tid).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), fills.len());
}

#[test]
fn a_fill_says_what_it_did_who_took_and_which_order() {
    use galata_desk_core::market::fills::Fill;
    use galata_desk_core::venue::UserFill;
    let fills: Vec<Fill> = decode::<Vec<UserFill>>("userFills", &fixture("userFills"))
        .unwrap()
        .into_iter()
        .map(Fill::from)
        .collect();
    let pnut = fills.iter().find(|f| f.coin == "PNUT").unwrap();
    assert_eq!(pnut.dir, "Close Short");
    assert!(pnut.taker);
    assert_eq!(pnut.oid, 608_255_639_822);
    assert!(pnut.hash.starts_with("0x4e5ba955"));
    // A maker fill that opened a position, and one that closed one.
    let useless = fills.iter().find(|f| f.coin == "USELESS").unwrap();
    assert_eq!(useless.dir, "Open Long");
    assert!(!useless.taker);
    let snx = fills.iter().find(|f| f.coin == "SNX").unwrap();
    assert_eq!(snx.dir, "Close Long");
    // A builder-dex fill keeps its dex in the coin.
    let nvda = fills.iter().find(|f| f.coin == "xyz:NVDA").unwrap();
    assert_eq!(nvda.dir, "Open Long");
    assert!(nvda.taker);
}

#[test]
fn order_history_lists_each_change_of_status_newest_first() {
    use galata_desk_core::venue::HistoricalOrder;
    let rows: Vec<HistoricalOrder> =
        decode("historicalOrders", &fixture("historicalOrders")).unwrap();
    assert_eq!(rows.len(), 6);
    assert!(
        rows.windows(2)
            .all(|w| w[0].status_timestamp >= w[1].status_timestamp)
    );
    // One order that rested and then filled is two rows.
    let nvda: Vec<&HistoricalOrder> = rows
        .iter()
        .filter(|r| r.order.oid == 610_305_538_201)
        .collect();
    let statuses: Vec<&str> = nvda.iter().map(|r| r.status.as_str()).collect();
    assert_eq!(statuses, ["filled", "open"]);
    assert_eq!(nvda[0].order.coin, "xyz:NVDA");
    // A trigger order that triggered.
    let stop = rows.iter().find(|r| r.status == "triggered").unwrap();
    assert!(stop.order.is_trigger);
    assert_eq!(stop.order.order_type, "Stop Market");
    assert!(stop.order.trigger_condition.starts_with("Price above"));
    // A reduce-only cancel, as the venue words it.
    let cancelled = rows
        .iter()
        .find(|r| r.status == "reduceOnlyCanceled" && r.order.coin == "xyz:ANT")
        .unwrap();
    assert!(cancelled.order.reduce_only);
    // A main-dex order.
    assert!(rows.iter().any(|r| r.order.coin == "BTC"));
}

#[test]
fn an_order_status_this_build_has_not_met_still_decodes() {
    use galata_desk_core::venue::HistoricalOrder;
    let rows: Vec<HistoricalOrder> = decode(
        "historicalOrders",
        br#"[{"order":{"coin":"BTC","side":"B","limitPx":"60000.0","sz":"0.0","oid":9,"timestamp":1795100000000,"triggerCondition":"N/A","isTrigger":false,"triggerPx":"0.0","children":[],"isPositionTpsl":false,"reduceOnly":false,"orderType":"Limit","origSz":"0.1","tif":"Gtc","cloid":null},"status":"someNewStatus","statusTimestamp":1795100000001}]"#,
    )
    .unwrap();
    assert_eq!(rows[0].status, "someNewStatus");
    let empty: Vec<HistoricalOrder> = decode("historicalOrders", b"[]").unwrap();
    assert!(empty.is_empty());
}

#[test]
fn funding_payments_decode_signed_with_how_many_hours_each_holds() {
    use galata_desk_core::venue::UserFundingRow;
    let rows: Vec<UserFundingRow> = decode("userFunding", &fixture("userFunding")).unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows.windows(2).all(|w| w[0].time <= w[1].time));
    // Received on the main dex, by a short.
    let btc = rows.iter().find(|r| r.delta.coin == "BTC").unwrap();
    assert!(btc.delta.usdc > Decimal::ZERO);
    assert!(btc.delta.szi < Decimal::ZERO);
    assert_eq!(btc.delta.n_samples, 1);
    // Paid on a builder dex, by a long.
    let meta = rows.iter().find(|r| r.delta.coin == "xyz:META").unwrap();
    assert_eq!(meta.delta.usdc.to_string(), "-0.054547");
    assert!(meta.delta.szi > Decimal::ZERO);
    // An older row summed over a day.
    let spcx = rows.iter().find(|r| r.delta.coin == "xyz:SPCX").unwrap();
    assert_eq!(spcx.delta.n_samples, 24);
    assert_eq!(spcx.delta.funding_rate.to_string(), "0.000003962");
}

#[test]
fn ledger_rows_decode_by_type_and_keep_the_unknown() {
    use galata_desk_core::venue::{LedgerDelta, LedgerRow};
    let rows: Vec<LedgerRow> = decode(
        "userNonFundingLedgerUpdates",
        &fixture("userNonFundingLedgerUpdates"),
    )
    .unwrap();
    assert_eq!(rows.len(), 14);
    assert!(rows.windows(2).all(|w| w[0].time <= w[1].time));
    assert!(rows.iter().all(|r| r.hash.starts_with("0x")));
    let find = |f: fn(&LedgerDelta) -> bool| rows.iter().find(|r| f(&r.delta)).unwrap();
    // A deposit has no fee to report.
    let deposit = find(|d| matches!(d, LedgerDelta::Deposit { .. }));
    assert_eq!(deposit.delta, LedgerDelta::Deposit { usdc: dec("100.0") });
    assert!(matches!(
        find(|d| matches!(d, LedgerDelta::Withdraw { .. })).delta,
        LedgerDelta::Withdraw { .. }
    ));
    let LedgerDelta::AccountClassTransfer { to_perp, .. } =
        find(|d| matches!(d, LedgerDelta::AccountClassTransfer { .. })).delta
    else {
        unreachable!()
    };
    assert!(to_perp);
    let LedgerDelta::Send {
        source_dex, token, ..
    } = &find(|d| matches!(d, LedgerDelta::Send { .. })).delta
    else {
        unreachable!()
    };
    assert_eq!(source_dex, "spot");
    assert_eq!(token, "UPUMP");
    let LedgerDelta::VaultWithdraw {
        requested_usd,
        net_withdrawn_usd,
        ..
    } = find(|d| matches!(d, LedgerDelta::VaultWithdraw { .. })).delta
    else {
        unreachable!()
    };
    assert_eq!(requested_usd, net_withdrawn_usd);
    for typed in [
        |d: &LedgerDelta| matches!(d, LedgerDelta::InternalTransfer { .. }),
        |d: &LedgerDelta| matches!(d, LedgerDelta::SubAccountTransfer { .. }),
        |d: &LedgerDelta| matches!(d, LedgerDelta::SpotTransfer { .. }),
        |d: &LedgerDelta| matches!(d, LedgerDelta::VaultCreate { .. }),
        |d: &LedgerDelta| matches!(d, LedgerDelta::VaultDeposit { .. }),
        |d: &LedgerDelta| matches!(d, LedgerDelta::VaultDistribution { .. }),
    ] {
        assert!(rows.iter().any(|r| typed(&r.delta)));
    }
    // Types read as `Other`, under their own names.
    let mut others: Vec<&str> = rows
        .iter()
        .filter_map(|r| match &r.delta {
            LedgerDelta::Other { kind, .. } => Some(kind.as_str()),
            _ => None,
        })
        .collect();
    others.sort();
    assert_eq!(others, ["cStakingTransfer", "liquidation", "spotGenesis"]);

    // A type never seen is kept, with its fields.
    let unseen: Vec<LedgerRow> = decode(
        "userNonFundingLedgerUpdates",
        br#"[{"time":1795100000000,"hash":"0xab","delta":{"type":"borrowLend","token":"USDC","amount":"5.0"}}]"#,
    )
    .unwrap();
    let LedgerDelta::Other { kind, fields } = &unseen[0].delta else {
        panic!("{:?}", unseen[0].delta)
    };
    assert_eq!(kind, "borrowLend");
    assert_eq!(fields["amount"], "5.0");
}

#[test]
fn a_fill_without_a_fee_is_an_error_not_a_free_fill() {
    use galata_desk_core::venue::UserFill;
    let result: Result<Vec<UserFill>, _> = decode(
        "userFills",
        br#"[{"coin":"BTC","px":"64000","sz":"0.1","side":"A","time":1794930208267,"closedPnl":"12.5","tid":7}]"#,
    );
    let message = result.unwrap_err().to_string();
    assert!(message.contains("fee"), "{message}");
}

#[test]
fn fills_by_time_decode_like_the_latest_fills() {
    use galata_desk_core::venue::UserFill;
    let fills: Vec<UserFill> = decode("userFillsByTime", &fixture("userFillsByTime")).unwrap();
    assert_eq!(fills.len(), 3);
    assert!(fills.windows(2).all(|w| w[0].time <= w[1].time));
    assert!(fills.iter().all(|f| f.tid > 0));
    let empty: Vec<UserFill> = decode("userFillsByTime", b"[]").unwrap();
    assert!(empty.is_empty());
}

#[test]
fn a_role_answer_says_whose_api_wallet_an_address_is() {
    let agent: UserRole = decode(
        "userRole",
        br#"{"role":"agent","data":{"user":"0x010461c14e146ac35fe42271bdc1134ee31c703a"}}"#,
    )
    .unwrap();
    assert_eq!(agent.role, "agent");
    assert_eq!(
        agent.data.unwrap().user.as_deref(),
        Some("0x010461c14e146ac35fe42271bdc1134ee31c703a")
    );
    // Recorded 2026-11-17 for an ordinary address: no data at all.
    let user: UserRole = decode("userRole", br#"{"role":"user"}"#).unwrap();
    assert_eq!((user.role.as_str(), user.data), ("user", None));
    let missing: UserRole = decode("userRole", br#"{"role":"missing"}"#).unwrap();
    assert_eq!(missing.role, "missing");
}

#[test]
fn what_an_account_can_trade_in_a_market_is_read() {
    // Recorded on mainnet for a public account that trades HYPE at 20x.
    let hype: ActiveAsset = decode("activeAssetData", &fixture("activeAssetData_HYPE")).unwrap();
    assert_eq!(hype.leverage.value, 20);
    assert_eq!(hype.leverage.mode, MarginMode::Cross);
    assert_eq!(hype.max_size(true), dec("928851.47"));
    assert_eq!(hype.max_size(false), dec("930454.47"));
    assert_eq!(hype.available(true), dec("3073987.4973909999"));
    assert_eq!(hype.mark_px, dec("66.189"));

    // The same account on a builder dex, where it holds no collateral:
    // nothing can be traded there.
    let nvda: ActiveAsset =
        decode("activeAssetData", &fixture("activeAssetData_xyz_NVDA")).unwrap();
    assert_eq!(nvda.leverage.value, 10);
    assert!(nvda.max_size(true).is_zero() && nvda.max_size(false).is_zero());
}

#[tokio::test]
async fn the_active_asset_is_asked_for_one_user_and_coin() {
    let transport = Recorded::default();
    let info = Info::new(transport.clone());
    let nvda = info.active_asset(USER, "xyz:NVDA").await.unwrap();
    assert_eq!(nvda.leverage.value, 10);
    let asked = transport.asked.lock().unwrap();
    assert_eq!(
        asked[0],
        serde_json::json!({ "type": "activeAssetData", "user": USER, "coin": "xyz:NVDA" })
    );
}

#[test]
fn an_hourly_funding_payment_with_null_samples_counts_as_one_hour() {
    use galata_desk_core::venue::UserFundingRow;
    // As the venue answers today: `nSamples` is `null`, not left out.
    let rows: Vec<UserFundingRow> = decode(
        "userFunding",
        br#"[{"time":1794751200060,"hash":"0x0000000000000000000000000000000000000000000000000000000000000000","delta":{"type":"funding","coin":"ETH","usdc":"-0.068805","szi":"5.4579","fundingRate":"0.0000068858","nSamples":null}},
             {"time":1794754800000,"hash":"0x00","delta":{"type":"funding","coin":"BTC","usdc":"0.01","szi":"-0.1","fundingRate":"0.00001"}}]"#,
    )
    .unwrap();
    assert_eq!(rows[0].delta.n_samples, 1);
    assert_eq!(rows[1].delta.n_samples, 1);
}

#[test]
fn a_dex_in_full_names_its_deployer_oracle_setters_and_market_parameters() {
    let dexs: Vec<Option<DexInfo>> = decode("perpDexs", &fixture("perpDexs_full")).unwrap();
    // The main dex comes first, as null.
    assert!(dexs[0].is_none());
    let xyz = dexs.iter().flatten().find(|d| d.name == "xyz").unwrap();
    assert_eq!(xyz.full_name, "XYZ");
    assert!(xyz.deployer.as_deref().is_some_and(|d| d.starts_with("0x")));
    // No named updater: the oracle is set by a sub-deployer.
    assert_eq!(xyz.oracle_updater, None);
    assert!(!xyz.oracle_setters().is_empty());
    assert!(xyz.oi_cap("xyz:NVDA").is_some_and(|c| c > Decimal::ZERO));
    assert!(xyz.funding_multiplier("xyz:NVDA").is_some());
    assert_eq!(xyz.oi_cap("xyz:NOPE"), None);
    let flx = dexs.iter().flatten().find(|d| d.name == "flx").unwrap();
    assert_eq!(flx.full_name, "Felix Exchange");
    // The named updater is listed once though it is also a sub-deployer.
    let setters = flx.oracle_setters();
    assert_eq!(setters[0], flx.oracle_updater.clone().unwrap());
    assert_eq!(
        setters
            .iter()
            .filter(|s| s.eq_ignore_ascii_case(&setters[0]))
            .count(),
        1
    );
    // The trimmed fixture, with names only, still decodes.
    let trimmed: Vec<Option<DexInfo>> = decode("perpDexs", &fixture("perpDexs")).unwrap();
    assert!(trimmed.iter().flatten().all(|d| d.oi_caps.is_empty()));
}

#[test]
fn a_market_annotation_decodes_and_a_main_dex_market_has_none() {
    let ant: Option<PerpAnnotation> =
        decode("perpAnnotation", &fixture("perpAnnotation_xyz_ANT")).unwrap();
    let ant = ant.unwrap();
    assert_eq!(ant.category.as_deref(), Some("preipo"));
    assert!(ant.description.unwrap().contains("Anthropic, PBC"));
    assert_eq!(ant.keywords, vec!["anthropic", "claude"]);
    let btc: Option<PerpAnnotation> = decode("perpAnnotation", b"null").unwrap();
    assert_eq!(btc, None);
}
