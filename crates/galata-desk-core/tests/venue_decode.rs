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
