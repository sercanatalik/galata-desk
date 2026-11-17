//! The loopback bridge, driven as a browser page would drive it.

use std::time::Duration;

use galata_desk_core::connect::proof_testing::Wallet;
use galata_desk_core::connect::{Attempt, AttemptEvent, ProofError};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};

/// The pieces of an attempt's URL: `http://127.0.0.1:<port>` and the token.
fn parts(attempt: &Attempt) -> (String, String) {
    let (origin, token) = attempt.url().split_once("/?t=").unwrap();
    (origin.to_string(), token.to_string())
}

/// A page on the bridge's own origin, holding the token.
struct PageClient {
    http: Client,
    origin: String,
    token: String,
}

impl PageClient {
    fn of(attempt: &Attempt) -> Self {
        let (origin, token) = parts(attempt);
        Self {
            http: Client::new(),
            origin,
            token,
        }
    }

    fn post(&self, path: &str, body: Value) -> reqwest::RequestBuilder {
        self.http
            .post(format!("{}{path}", self.origin))
            .header("origin", &self.origin)
            .header("x-desk-token", &self.token)
            .json(&body)
    }

    async fn challenge(&self, address: &str) -> String {
        let reply: Value = self
            .post("/api/challenge", json!({ "address": address }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        reply["message"].as_str().unwrap().to_string()
    }
}

#[tokio::test]
async fn a_valid_proof_connects_and_the_server_stops() {
    let mut attempt = Attempt::start(None).await.unwrap();
    let page = PageClient::of(&attempt);
    let wallet = Wallet::new(7);
    let address = wallet.address();

    let message = page.challenge(address.as_str()).await;
    let signature = wallet.sign(&message);
    let reply = page
        .post(
            "/api/proof",
            json!({ "address": address.as_str(), "signature": signature }),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::OK);
    let body: Value = reply.json().await.unwrap();
    assert_eq!(body["address"], address.as_str());

    assert_eq!(
        attempt.next_event().await,
        Some(AttemptEvent::Connected(address))
    );

    // The attempt is over: its address no longer answers.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let again = Client::new().get(format!("{}/", page.origin)).send().await;
    assert!(again.is_err(), "the server should have stopped");
}

#[tokio::test]
async fn a_signature_from_another_address_is_rejected_and_the_attempt_goes_on() {
    let mut attempt = Attempt::start(None).await.unwrap();
    let page = PageClient::of(&attempt);
    let owner = Wallet::new(7);
    let other = Wallet::new(9);

    let message = page.challenge(owner.address().as_str()).await;
    let reply = page
        .post(
            "/api/proof",
            json!({ "address": owner.address().as_str(), "signature": other.sign(&message) }),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::BAD_REQUEST);
    let body: Value = reply.json().await.unwrap();
    assert_eq!(body["error"], "mismatch");
    assert_eq!(
        attempt.next_event().await,
        Some(AttemptEvent::Rejected(ProofError::Mismatch))
    );

    // The same page may try again, and a valid proof still connects.
    let reply = page
        .post(
            "/api/proof",
            json!({ "address": owner.address().as_str(), "signature": owner.sign(&message) }),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::OK);
    assert_eq!(
        attempt.next_event().await,
        Some(AttemptEvent::Connected(owner.address()))
    );
}

#[tokio::test]
async fn a_proof_from_an_earlier_attempt_is_rejected() {
    let wallet = Wallet::new(7);
    let address = wallet.address();

    let earlier = Attempt::start(None).await.unwrap();
    let old_signature = wallet.sign(&PageClient::of(&earlier).challenge(address.as_str()).await);
    drop(earlier);

    let mut current = Attempt::start(None).await.unwrap();
    let reply = PageClient::of(&current)
        .post(
            "/api/proof",
            json!({ "address": address.as_str(), "signature": old_signature }),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        current.next_event().await,
        Some(AttemptEvent::Rejected(ProofError::Mismatch))
    );
}

#[tokio::test]
async fn an_api_call_without_the_token_is_refused() {
    let attempt = Attempt::start(None).await.unwrap();
    let (origin, _) = parts(&attempt);
    let http = Client::new();

    let missing = http
        .post(format!("{origin}/api/challenge"))
        .header("origin", &origin)
        .json(&json!({ "address": "0xdfc24b077bc1425ad1dea75bcb6f8158e10df303" }))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::FORBIDDEN);

    let wrong = http
        .post(format!("{origin}/api/challenge"))
        .header("origin", &origin)
        .header("x-desk-token", "0".repeat(64))
        .json(&json!({ "address": "0xdfc24b077bc1425ad1dea75bcb6f8158e10df303" }))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_request_naming_another_host_is_refused() {
    let attempt = Attempt::start(None).await.unwrap();
    let (origin, token) = parts(&attempt);
    // What a rebound hostname looks like: the right socket, the wrong name.
    let reply = Client::new()
        .get(format!("{origin}/"))
        .header("host", "evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::FORBIDDEN);

    let reply = Client::new()
        .get(format!("{origin}/api/attempt"))
        .header("host", "localhost")
        .header("x-desk-token", &token)
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn an_api_call_from_another_origin_is_refused() {
    let attempt = Attempt::start(None).await.unwrap();
    let (origin, token) = parts(&attempt);
    let reply = Client::new()
        .post(format!("{origin}/api/challenge"))
        .header("origin", "https://evil.example")
        .header("x-desk-token", &token)
        .json(&json!({ "address": "0xdfc24b077bc1425ad1dea75bcb6f8158e10df303" }))
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::FORBIDDEN);

    // A POST with no Origin at all is not a browser page on this origin.
    let reply = Client::new()
        .post(format!("{origin}/api/challenge"))
        .header("x-desk-token", &token)
        .json(&json!({ "address": "0xdfc24b077bc1425ad1dea75bcb6f8158e10df303" }))
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn the_server_listens_on_loopback_only() {
    let attempt = Attempt::start(None).await.unwrap();
    assert!(attempt.url().starts_with("http://127.0.0.1:"));
}

#[tokio::test]
async fn an_attempt_expires_and_stops_answering() {
    let mut attempt = Attempt::start_expiring(None, Duration::from_millis(150))
        .await
        .unwrap();
    let (origin, _) = parts(&attempt);
    assert_eq!(attempt.next_event().await, Some(AttemptEvent::Expired));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        Client::new()
            .get(format!("{origin}/"))
            .send()
            .await
            .is_err()
    );
    // Nothing more happens after an attempt is over.
    assert_eq!(attempt.next_event().await, None);
}

#[tokio::test]
async fn cancelling_stops_the_server() {
    let attempt = Attempt::start(None).await.unwrap();
    let (origin, _) = parts(&attempt);
    assert!(Client::new().get(format!("{origin}/")).send().await.is_ok());
    drop(attempt);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        Client::new()
            .get(format!("{origin}/"))
            .send()
            .await
            .is_err()
    );
}

#[tokio::test]
async fn the_page_is_served_with_its_security_headers() {
    let attempt = Attempt::start(None).await.unwrap();
    let (origin, _) = parts(&attempt);
    let reply = Client::new().get(attempt.url()).send().await.unwrap();
    assert_eq!(reply.status(), StatusCode::OK);
    let headers = reply.headers().clone();
    assert!(
        headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(headers["referrer-policy"], "no-referrer");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    let csp = headers["content-security-policy"].to_str().unwrap();
    assert!(csp.contains("default-src 'self'"));
    assert!(csp.contains("script-src 'self'"));
    assert!(csp.contains("frame-ancestors 'none'"));
    assert!(reply.text().await.unwrap().contains("galata-desk"));

    let missing = Client::new()
        .get(format!("{origin}/no-such-file.js"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_page_learns_whether_a_relay_project_is_configured() {
    let without = Attempt::start(None).await.unwrap();
    let (origin, token) = parts(&without);
    let info: Value = Client::new()
        .get(format!("{origin}/api/attempt"))
        .header("x-desk-token", &token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(info["projectId"], Value::Null);

    let with = Attempt::start(Some("abc123".into())).await.unwrap();
    let (origin, token) = parts(&with);
    let info: Value = Client::new()
        .get(format!("{origin}/api/attempt"))
        .header("x-desk-token", &token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(info["projectId"], "abc123");
}

#[tokio::test]
async fn a_malformed_address_gets_no_challenge() {
    let attempt = Attempt::start(None).await.unwrap();
    let reply = PageClient::of(&attempt)
        .post("/api/challenge", json!({ "address": "0x1234" }))
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::BAD_REQUEST);
    let body: Value = reply.json().await.unwrap();
    assert_eq!(body["error"], "bad_address");
}
