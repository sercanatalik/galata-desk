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

/// Answers a funding history query from the fixture named after its coin,
/// or with fixed bytes, and records what was asked.
#[derive(Clone, Default)]
struct Funding {
    answer: Option<&'static str>,
    asked: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
}

impl Transport for Funding {
    async fn post(&self, body: Value) -> Result<Vec<u8>, VenueError> {
        let coin = body["coin"].as_str().unwrap().replace(':', "_");
        self.asked.lock().unwrap().push(body);
        Ok(match self.answer {
            Some(text) => text.as_bytes().to_vec(),
            None => fixture(&format!("fundingHistory_{coin}")),
        })
    }
}

#[tokio::test]
async fn funding_history_decodes_for_a_main_and_a_builder_dex_market() {
    let transport = Funding::default();
    let info = Info::new(transport.clone());
    for coin in ["BTC", "xyz:NVDA"] {
        let rows = info.funding_history(coin, 1, 2).await.unwrap();
        assert_eq!(rows.len(), 5, "{coin}");
        assert!(
            rows.windows(2).all(|w| w[1].time - w[0].time > 3_500_000),
            "{coin}: hourly, oldest first"
        );
        // The venue stamps each row just past the hour.
        assert!(rows.iter().all(|r| r.time % 3_600_000 < 1_000), "{coin}");
        assert!(
            rows.iter()
                .all(|r| r.funding_rate.abs() < "0.01".parse().unwrap()),
            "{coin}: an hourly rate is small"
        );
    }
    let asked = transport.asked.lock().unwrap();
    assert_eq!(asked[1]["type"], "fundingHistory");
    assert_eq!(asked[1]["coin"], "xyz:NVDA");
    assert_eq!(asked[1]["startTime"], 1);
    assert_eq!(asked[1]["endTime"], 2);
}

#[tokio::test]
async fn an_empty_funding_history_and_an_unknown_market_are_both_nothing() {
    for answer in ["[]", "null"] {
        let info = Info::new(Funding {
            answer: Some(answer),
            ..Funding::default()
        });
        assert!(info.funding_history("BTC", 1, 2).await.unwrap().is_empty());
    }
}

#[test]
fn a_funding_row_missing_its_rate_is_an_error_that_names_its_path() {
    let mut value: Value = serde_json::from_slice(&fixture("fundingHistory_BTC")).unwrap();
    value[2].as_object_mut().unwrap().remove("fundingRate");
    let error = decode::<Vec<WireFunding>>("fundingHistory", &serde_json::to_vec(&value).unwrap())
        .unwrap_err();
    assert!(
        matches!(&error, VenueError::Shape { query, path, .. } if *query == "fundingHistory" && path == "[2]"),
        "{error:?}"
    );
}
