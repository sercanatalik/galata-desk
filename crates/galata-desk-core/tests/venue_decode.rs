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
        let name = match body.get("dex").and_then(Value::as_str) {
            Some(dex) => format!("{kind}_{dex}"),
            None => kind,
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
