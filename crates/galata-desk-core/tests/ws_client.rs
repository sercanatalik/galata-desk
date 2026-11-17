//! The WebSocket client against a server in this process.

use std::net::SocketAddr;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use galata_desk_core::market::ws::{Event, Sub, Subscriptions, Timing, WsClient};
use galata_desk_core::market::{Interval, MarketId};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// What the test tells a connection to do.
enum Tell {
    Send(String),
    Drop,
}

/// One accepted connection: what the client said, and a way to answer.
struct Peer {
    said: mpsc::UnboundedReceiver<Value>,
    tell: mpsc::UnboundedSender<Tell>,
}

impl Peer {
    async fn next(&mut self) -> Value {
        tokio::time::timeout(Duration::from_secs(5), self.said.recv())
            .await
            .expect("the client said something in time")
            .expect("the connection is open")
    }
}

/// A server that hands each accepted connection to the test.
async fn server() -> (SocketAddr, mpsc::UnboundedReceiver<Peer>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (peers_tx, peers) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let Ok(socket) = tokio_tungstenite::accept_async(stream).await else {
                continue;
            };
            let (mut sink, mut stream) = socket.split();
            let (said_tx, said) = mpsc::unbounded_channel();
            let (tell, mut told) = mpsc::unbounded_channel();
            if peers_tx.send(Peer { said, tell }).is_err() {
                return;
            }
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        message = stream.next() => match message {
                            Some(Ok(Message::Text(text))) => {
                                let _ = said_tx.send(serde_json::from_str(text.as_str()).unwrap());
                            }
                            Some(Ok(_)) => {}
                            _ => return,
                        },
                        tell = told.recv() => match tell {
                            Some(Tell::Send(text)) => { let _ = sink.send(Message::text(text)).await; }
                            // Dropping both halves closes the socket without a goodbye.
                            Some(Tell::Drop) | None => return,
                        },
                    }
                }
            });
        }
    });
    (addr, peers)
}

fn quick() -> Timing {
    Timing {
        ping_every: Duration::from_millis(80),
        dead_after: Duration::from_millis(400),
        retry_first: Duration::from_millis(40),
        retry_max: Duration::from_millis(200),
    }
}

async fn next_event(events: &mut mpsc::UnboundedReceiver<Event>) -> Event {
    tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("an event in time")
        .expect("the client is running")
}

fn btc() -> MarketId {
    MarketId::new("BTC")
}

#[tokio::test]
async fn subscriptions_are_sent_and_figures_come_back() {
    let (addr, mut peers) = server().await;
    let (client, mut events) = WsClient::start_with(format!("ws://{addr}"), quick());
    let mut peer = peers.recv().await.unwrap();
    assert_eq!(next_event(&mut events).await, Event::Connected);

    client.subscribe(Sub::Ctx(btc()));
    assert_eq!(
        peer.next().await,
        json!({"method":"subscribe","subscription":{"type":"activeAssetCtx","coin":"BTC"}})
    );
    client.subscribe(Sub::Candles(MarketId::new("xyz:NVDA"), Interval::M15));
    assert_eq!(
        peer.next().await,
        json!({"method":"subscribe","subscription":{"type":"candle","coin":"xyz:NVDA","interval":"15m"}})
    );

    peer.tell
        .send(Tell::Send(
            r#"{"channel":"activeAssetCtx","data":{"coin":"BTC","ctx":{"funding":"0.00001","openInterest":"10","dayNtlVlm":"1000","premium":null,"oraclePx":"100","markPx":"101","midPx":null}}}"#.into(),
        ))
        .unwrap();
    match next_event(&mut events).await {
        Event::Ctx(market, ctx) => {
            assert_eq!(market, btc());
            assert_eq!(ctx.mark_px.to_string(), "101");
        }
        other => panic!("expected figures, got {other:?}"),
    }
}

#[tokio::test]
async fn a_subscription_already_wanted_is_not_sent_again_and_unsubscribe_is() {
    let (addr, mut peers) = server().await;
    let (client, mut events) = WsClient::start_with(format!("ws://{addr}"), quick());
    let mut peer = peers.recv().await.unwrap();
    next_event(&mut events).await;

    client.subscribe(Sub::Ctx(btc()));
    client.subscribe(Sub::Ctx(btc()));
    client.unsubscribe(Sub::Ctx(btc()));
    assert_eq!(peer.next().await["method"], "subscribe");
    // The duplicate was swallowed: the next thing said is the unsubscribe
    // (pings aside).
    loop {
        let said = peer.next().await;
        if said["method"] == "ping" {
            continue;
        }
        assert_eq!(
            said,
            json!({"method":"unsubscribe","subscription":{"type":"activeAssetCtx","coin":"BTC"}})
        );
        break;
    }
}

#[tokio::test]
async fn the_client_pings_to_keep_the_connection_open() {
    let (addr, mut peers) = server().await;
    let (_client, mut events) = WsClient::start_with(format!("ws://{addr}"), quick());
    let mut peer = peers.recv().await.unwrap();
    next_event(&mut events).await;
    assert_eq!(peer.next().await, json!({"method":"ping"}));
    assert_eq!(peer.next().await, json!({"method":"ping"}));
}

#[tokio::test]
async fn a_dropped_connection_is_reported_and_every_subscription_is_put_back() {
    let (addr, mut peers) = server().await;
    let (client, mut events) = WsClient::start_with(format!("ws://{addr}"), quick());
    let mut first = peers.recv().await.unwrap();
    assert_eq!(next_event(&mut events).await, Event::Connected);
    client.subscribe(Sub::Ctx(btc()));
    client.subscribe(Sub::Candles(btc(), Interval::M1));
    first.next().await;
    first.next().await;

    first.tell.send(Tell::Drop).unwrap();
    assert_eq!(next_event(&mut events).await, Event::Disconnected);

    // It reconnects by itself and subscribes again before saying it is up.
    let mut second = peers.recv().await.unwrap();
    let mut resubscribed = [second.next().await, second.next().await];
    resubscribed.sort_by_key(|v| v["subscription"]["type"].as_str().unwrap().to_string());
    assert_eq!(resubscribed[0]["subscription"]["type"], "activeAssetCtx");
    assert_eq!(resubscribed[1]["subscription"]["type"], "candle");
    assert!(resubscribed.iter().all(|v| v["method"] == "subscribe"));
    assert_eq!(next_event(&mut events).await, Event::Connected);
}

#[tokio::test]
async fn a_subscription_asked_for_while_disconnected_is_not_lost() {
    // Nothing is listening yet: reserve a port, then free it.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let (client, mut events) = WsClient::start_with(format!("ws://{addr}"), quick());
    client.subscribe(Sub::Ctx(btc()));
    tokio::time::sleep(Duration::from_millis(150)).await;
    // Never connected, so nothing has been reported.
    assert!(events.try_recv().is_err());

    // The server appears on that port; the client finds it and subscribes.
    let listener = TcpListener::bind(addr).await.unwrap();
    let (stream, _) = listener.accept().await.unwrap();
    let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
    let said = loop {
        match socket.next().await.unwrap().unwrap() {
            Message::Text(text) => break serde_json::from_str::<Value>(text.as_str()).unwrap(),
            _ => continue,
        }
    };
    assert_eq!(said["method"], "subscribe");
    assert_eq!(said["subscription"]["coin"], "BTC");
    assert_eq!(next_event(&mut events).await, Event::Connected);
}

#[tokio::test]
async fn a_connection_that_goes_silent_is_treated_as_dead() {
    let (addr, mut peers) = server().await;
    // Pings are answered by nothing here, so the line is silent.
    let (_client, mut events) = WsClient::start_with(format!("ws://{addr}"), quick());
    let _first = peers.recv().await.unwrap();
    assert_eq!(next_event(&mut events).await, Event::Connected);
    assert_eq!(next_event(&mut events).await, Event::Disconnected);
    // And it tries again.
    let _second = peers.recv().await.unwrap();
    assert_eq!(next_event(&mut events).await, Event::Connected);
}

#[tokio::test]
async fn dropping_the_client_closes_the_connection() {
    let (addr, mut peers) = server().await;
    let (client, mut events) = WsClient::start_with(format!("ws://{addr}"), quick());
    let mut peer = peers.recv().await.unwrap();
    next_event(&mut events).await;
    drop(client);
    // The server's side sees the connection end.
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        while peer.said.recv().await.is_some() {}
    })
    .await;
    assert!(ended.is_ok());
}
