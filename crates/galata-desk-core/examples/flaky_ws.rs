//! A stand-in market feed for checking the not-live state: it relays to the
//! venue's WebSocket for a while, then drops the connection and stops
//! listening.
//!
//! ```text
//! cargo run -p galata-desk-core --example flaky_ws -- 18801 8
//! GALATA_DESK_WS_URL=ws://127.0.0.1:18801 cargo run -p galata-desk
//! ```

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use galata_desk_core::market::ws::WS_URL;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let port: u16 = args.next().and_then(|a| a.parse().ok()).unwrap_or(18801);
    let seconds: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(8);
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("the port is free");
    let (stream, _) = listener.accept().await.expect("a client connects");
    // Stop listening: after the drop, reconnects are refused.
    drop(listener);
    let client = tokio_tungstenite::accept_async(stream)
        .await
        .expect("a WebSocket client");
    let (upstream, _) = tokio_tungstenite::connect_async(WS_URL)
        .await
        .expect("the venue answers");
    let (mut to_client, mut from_client) = client.split();
    let (mut to_venue, mut from_venue) = upstream.split();

    let relay = async {
        loop {
            tokio::select! {
                Some(Ok(message)) = from_client.next() => {
                    if to_venue.send(message).await.is_err() { return; }
                }
                Some(Ok(message)) = from_venue.next() => {
                    if to_client.send(message).await.is_err() { return; }
                }
                else => return,
            }
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(seconds), relay).await;
    eprintln!("flaky_ws: dropped the connection after {seconds}s");
}
