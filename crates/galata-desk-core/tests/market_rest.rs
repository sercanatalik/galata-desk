//! The market-data REST queries decode their recorded answers.

use galata_desk_core::venue::*;
use serde_json::Value;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

#[test]
fn the_list_of_markets_decodes_for_every_dex() {
    let metas: Vec<PerpMeta> = decode("allPerpMetas", &fixture("allPerpMetas")).unwrap();
    assert_eq!(metas.len(), 2);
    let main: Vec<&str> = metas[0].universe.iter().map(|m| m.name.as_str()).collect();
    assert!(main.starts_with(&["BTC", "ETH", "SOL", "HYPE"]));
    assert_eq!(metas[0].universe[0].max_leverage, 40);
    assert!(!metas[0].universe[0].is_delisted);
    // A delisted market says so; one that is listed does not carry the flag.
    assert!(metas[0].universe.last().unwrap().is_delisted);
    assert!(metas[1].universe.iter().any(|m| m.name == "xyz:NVDA"));
}

#[test]
fn candle_history_decodes_for_a_main_and_a_builder_dex_market() {
    for name in ["candleSnapshot_BTC", "candleSnapshot_xyz_NVDA"] {
        let candles: Vec<WireCandle> = decode("candleSnapshot", &fixture(name)).unwrap();
        assert!(candles.len() >= 8, "{name}");
        assert!(
            candles.windows(2).all(|w| w[0].t < w[1].t),
            "{name}: oldest first"
        );
        assert!(
            candles
                .iter()
                .all(|c| c.l <= c.o && c.o <= c.h && c.l <= c.c && c.c <= c.h)
        );
        assert_eq!(candles[0].i, "15m");
    }
    let nvda: Vec<WireCandle> =
        decode("candleSnapshot", &fixture("candleSnapshot_xyz_NVDA")).unwrap();
    assert_eq!(nvda[0].s, "xyz:NVDA");
}

#[test]
fn a_candle_missing_a_price_is_an_error_that_names_its_path() {
    let mut value: Value = serde_json::from_slice(&fixture("candleSnapshot_BTC")).unwrap();
    value[3].as_object_mut().unwrap().remove("h");
    let error = decode::<Vec<WireCandle>>("candleSnapshot", &serde_json::to_vec(&value).unwrap())
        .unwrap_err();
    match error {
        VenueError::Shape {
            query,
            path,
            detail,
        } => {
            assert_eq!(query, "candleSnapshot");
            assert_eq!(path, "[3]");
            assert!(detail.contains('h'), "{detail}");
        }
        other => panic!("expected a shape error, got {other:?}"),
    }
}

#[test]
fn a_market_missing_its_leverage_is_an_error() {
    let mut value: Value = serde_json::from_slice(&fixture("allPerpMetas")).unwrap();
    value[0]["universe"][0]
        .as_object_mut()
        .unwrap()
        .remove("maxLeverage");
    let error =
        decode::<Vec<PerpMeta>>("allPerpMetas", &serde_json::to_vec(&value).unwrap()).unwrap_err();
    assert!(matches!(error, VenueError::Shape { path, .. } if path == "[0].universe[0]"));
}
