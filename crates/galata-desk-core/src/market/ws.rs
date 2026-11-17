//! The venue's WebSocket: live figures and candles for the watched markets.
//!
//! One task owns the socket. It keeps the set of wanted subscriptions, so a
//! subscription asked for while disconnected is not lost, and it puts them
//! all back after every reconnect. It only ever sends three things:
//! subscribe, unsubscribe and ping.

use std::collections::HashSet;
use std::sync::Once;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

use super::candles::Candle;
use super::{Interval, MarketId};
use crate::venue::{AssetCtx, WireCandle};

/// The venue's production WebSocket endpoint.
pub const WS_URL: &str = "wss://api.hyperliquid.xyz/ws";

/// Something to be kept informed about.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Sub {
    /// A market's live figures.
    Ctx(MarketId),
    /// A market's current candle at an interval, as it forms.
    Candles(MarketId, Interval),
}

impl Sub {
    fn subscription(&self) -> Value {
        match self {
            Sub::Ctx(market) => json!({ "type": "activeAssetCtx", "coin": market.coin() }),
            Sub::Candles(market, interval) => {
                json!({ "type": "candle", "coin": market.coin(), "interval": interval.as_str() })
            }
        }
    }

    fn request(&self, method: &str) -> String {
        json!({ "method": method, "subscription": self.subscription() }).to_string()
    }
}

/// What the stream reports.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The connection is up and every wanted subscription has been sent.
    Connected,
    /// The connection that was up is gone. A reconnect is under way.
    Disconnected,
    Ctx(MarketId, AssetCtx),
    Candle(MarketId, Interval, Candle),
}

/// Where subscriptions are changed. The real client is one; tests record.
pub trait Subscriptions: Send + 'static {
    fn subscribe(&self, sub: Sub);
    fn unsubscribe(&self, sub: Sub);
}

/// The stream's timing. Tests shorten it.
#[derive(Debug, Clone)]
pub struct Timing {
    /// How often to ping. The venue closes a connection idle for a minute.
    pub ping_every: Duration,
    /// How long without any message means the connection is dead.
    pub dead_after: Duration,
    /// The first wait before reconnecting; it doubles up to `retry_max`.
    pub retry_first: Duration,
    pub retry_max: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            ping_every: Duration::from_secs(30),
            dead_after: Duration::from_secs(45),
            retry_first: Duration::from_secs(1),
            retry_max: Duration::from_secs(30),
        }
    }
}

enum Command {
    Subscribe(Sub),
    Unsubscribe(Sub),
}

/// A running client. Dropping it closes the connection.
pub struct WsClient {
    commands: mpsc::UnboundedSender<Command>,
    task: JoinHandle<()>,
}

impl WsClient {
    /// Connect to `url` on the current Tokio runtime and keep connected.
    pub fn start(url: impl Into<String>) -> (Self, mpsc::UnboundedReceiver<Event>) {
        Self::start_with(url, Timing::default())
    }

    pub fn start_with(
        url: impl Into<String>,
        timing: Timing,
    ) -> (Self, mpsc::UnboundedReceiver<Event>) {
        let (commands, rx) = mpsc::unbounded_channel();
        let (tx, events) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(url.into(), timing, rx, tx));
        (Self { commands, task }, events)
    }
}

impl Subscriptions for WsClient {
    fn subscribe(&self, sub: Sub) {
        let _ = self.commands.send(Command::Subscribe(sub));
    }

    fn unsubscribe(&self, sub: Sub) {
        let _ = self.commands.send(Command::Unsubscribe(sub));
    }
}

impl Drop for WsClient {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// TLS needs one process-wide choice of cryptography. The HTTP client already
/// uses this one; naming it here keeps the two from disagreeing.
fn tls() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

/// Why a connection ended.
enum Ended {
    /// The client was dropped: stop for good.
    Stopped,
    /// The connection failed or went quiet: reconnect.
    Lost,
}

async fn run(
    url: String,
    timing: Timing,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: mpsc::UnboundedSender<Event>,
) {
    tls();
    let mut wanted: HashSet<Sub> = HashSet::new();
    let mut wait = timing.retry_first;
    loop {
        if let Ok((socket, _)) = tokio_tungstenite::connect_async(url.as_str()).await {
            wait = timing.retry_first;
            match connected(socket, &timing, &mut wanted, &mut commands, &events).await {
                Ended::Stopped => return,
                Ended::Lost => {
                    let _ = events.send(Event::Disconnected);
                }
            }
        }
        // Wait before trying again, still taking subscription changes so
        // none is lost while disconnected.
        let retry_at = Instant::now() + wait;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(retry_at) => break,
                command = commands.recv() => match command {
                    Some(Command::Subscribe(sub)) => { wanted.insert(sub); }
                    Some(Command::Unsubscribe(sub)) => { wanted.remove(&sub); }
                    None => return,
                },
            }
        }
        wait = (wait * 2).min(timing.retry_max);
    }
}

async fn connected<S>(
    socket: S,
    timing: &Timing,
    wanted: &mut HashSet<Sub>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    events: &mpsc::UnboundedSender<Event>,
) -> Ended
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + futures_util::Sink<Message>
        + Unpin,
{
    let (mut sink, mut stream) = socket.split();
    for sub in wanted.iter() {
        if sink
            .send(Message::text(sub.request("subscribe")))
            .await
            .is_err()
        {
            return Ended::Lost;
        }
    }
    let _ = events.send(Event::Connected);

    let mut ping = tokio::time::interval_at(Instant::now() + timing.ping_every, timing.ping_every);
    let mut heard = Instant::now();
    loop {
        tokio::select! {
            command = commands.recv() => {
                let text = match command {
                    None => return Ended::Stopped,
                    // Already wanted: nothing to send.
                    Some(Command::Subscribe(sub)) => wanted.insert(sub.clone()).then(|| sub.request("subscribe")),
                    Some(Command::Unsubscribe(sub)) => wanted.remove(&sub).then(|| sub.request("unsubscribe")),
                };
                if let Some(text) = text
                    && sink.send(Message::text(text)).await.is_err()
                {
                    return Ended::Lost;
                }
            }
            message = stream.next() => match message {
                Some(Ok(Message::Text(text))) => {
                    heard = Instant::now();
                    if let Some(event) = decode(text.as_str()) {
                        let _ = events.send(event);
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return Ended::Lost,
                // Pings, pongs and binary frames only show the line is alive.
                Some(Ok(_)) => heard = Instant::now(),
            },
            _ = ping.tick() => {
                if heard.elapsed() > timing.dead_after {
                    return Ended::Lost;
                }
                if sink.send(Message::text(r#"{"method":"ping"}"#)).await.is_err() {
                    return Ended::Lost;
                }
            }
        }
    }
}

#[derive(Deserialize)]
struct Envelope {
    channel: String,
    #[serde(default)]
    data: Value,
}

#[derive(Deserialize)]
struct CtxMessage {
    coin: String,
    ctx: AssetCtx,
}

/// One message from the stream, if it is one this app reads. Channels it does
/// not know are ignored, and a message it knows but cannot read is skipped: a
/// stream must survive one odd message.
pub fn decode(text: &str) -> Option<Event> {
    let envelope: Envelope = serde_json::from_str(text).ok()?;
    match envelope.channel.as_str() {
        "activeAssetCtx" => {
            let message: CtxMessage = serde_json::from_value(envelope.data).ok()?;
            Some(Event::Ctx(MarketId::new(message.coin), message.ctx))
        }
        "candle" => {
            let wire: WireCandle = serde_json::from_value(envelope.data).ok()?;
            let interval = Interval::from_venue(&wire.i)?;
            Some(Event::Candle(
                MarketId::new(wire.s.clone()),
                interval,
                Candle::from(wire),
            ))
        }
        _ => None,
    }
}

impl From<WireCandle> for Candle {
    fn from(wire: WireCandle) -> Self {
        Candle {
            open_ms: wire.t,
            open: wire.o,
            high: wire.h,
            low: wire.l,
            close: wire.c,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Recorded from the live stream, 2026-11-17.
    const CTX_BUILDER: &str = r#"{"channel":"activeAssetCtx","data":{"coin":"xyz:NVDA","ctx":{"funding":"0.0000215729","openInterest":"2673690.0020000003","prevDayPx":"161.37","dayNtlVlm":"504176558.8780502081","premium":"0.000618969","oraclePx":"157.52","markPx":"157.63","midPx":"157.615","impactPxs":["157.61","157.625"],"dayBaseVlm":"3189042.0580000016"}}}"#;
    const CTX_MAIN: &str = r#"{"channel":"activeAssetCtx","data":{"coin":"BTC","ctx":{"funding":"0.0000053875","openInterest":"37690.7182","prevDayPx":"64609.0","dayNtlVlm":"3862264071.0493512154","premium":"-0.0004319188","oraclePx":"64827.0","markPx":"64796.1","midPx":"64798.5","impactPxs":["64790.0","64799.0"],"dayBaseVlm":"59702.87556"}}}"#;
    const CTX_NULLS: &str = r#"{"channel":"activeAssetCtx","data":{"coin":"abcd:USA500","ctx":{"funding":"0.0","openInterest":"0.0","prevDayPx":"6980.0","dayNtlVlm":"0.0","premium":null,"oraclePx":"6980.0","markPx":"6980.0","midPx":null}}}"#;
    const CANDLE_BUILDER: &str = r#"{"channel":"candle","data":{"t":1794892260000,"T":1794892319999,"s":"xyz:NVDA","i":"1m","o":"157.65","c":"157.64","h":"157.67","l":"157.62","v":"9.748","n":12}}"#;

    fn d(s: &str) -> rust_decimal::Decimal {
        s.parse().unwrap()
    }

    #[test]
    fn live_figures_decode_for_main_and_builder_dex_markets() {
        let Some(Event::Ctx(market, ctx)) = decode(CTX_BUILDER) else {
            panic!("not a ctx event");
        };
        assert_eq!(market, MarketId::new("xyz:NVDA"));
        assert_eq!(ctx.mark_px, d("157.63"));
        assert_eq!(ctx.mid_px, Some(d("157.615")));
        assert_eq!(ctx.oracle_px, d("157.52"));
        assert_eq!(ctx.premium, Some(d("0.000618969")));
        assert_eq!(ctx.funding, d("0.0000215729"));

        let Some(Event::Ctx(market, ctx)) = decode(CTX_MAIN) else {
            panic!("not a ctx event");
        };
        assert_eq!(market, MarketId::new("BTC"));
        assert!(ctx.premium.unwrap() < d("0"));
    }

    #[test]
    fn a_market_with_no_mid_and_no_premium_decodes() {
        let Some(Event::Ctx(_, ctx)) = decode(CTX_NULLS) else {
            panic!("not a ctx event");
        };
        assert_eq!(ctx.mid_px, None);
        assert_eq!(ctx.premium, None);
    }

    #[test]
    fn a_candle_decodes_with_its_market_and_interval() {
        let Some(Event::Candle(market, interval, candle)) = decode(CANDLE_BUILDER) else {
            panic!("not a candle event");
        };
        assert_eq!(market, MarketId::new("xyz:NVDA"));
        assert_eq!(interval, Interval::M1);
        assert_eq!(candle.open_ms, 1_794_892_260_000);
        assert_eq!(candle.high, d("157.67"));
        assert_eq!(candle.close, d("157.64"));
    }

    #[test]
    fn other_channels_and_malformed_messages_are_passed_over() {
        assert_eq!(decode(r#"{"channel":"pong"}"#), None);
        assert_eq!(
            decode(r#"{"channel":"subscriptionResponse","data":{"method":"subscribe"}}"#),
            None
        );
        assert_eq!(decode(r#"{"channel":"somethingNew","data":{"x":1}}"#), None);
        // Known channel, wrong shape.
        assert_eq!(
            decode(r#"{"channel":"activeAssetCtx","data":{"coin":"BTC"}}"#),
            None
        );
        assert_eq!(
            decode(
                r#"{"channel":"candle","data":{"t":1,"s":"BTC","i":"7m","o":"1","c":"1","h":"1","l":"1"}}"#
            ),
            None
        );
        assert_eq!(decode("not json"), None);
    }

    #[test]
    fn the_only_requests_are_subscribe_and_unsubscribe() {
        let sub = Sub::Candles(MarketId::new("xyz:NVDA"), Interval::M15);
        let v: Value = serde_json::from_str(&sub.request("subscribe")).unwrap();
        assert_eq!(v["method"], "subscribe");
        assert_eq!(
            v["subscription"],
            json!({"type":"candle","coin":"xyz:NVDA","interval":"15m"})
        );
        let v: Value =
            serde_json::from_str(&Sub::Ctx(MarketId::new("BTC")).request("unsubscribe")).unwrap();
        assert_eq!(
            v,
            json!({"method":"unsubscribe","subscription":{"type":"activeAssetCtx","coin":"BTC"}})
        );
    }
}
