//! A stand-in for `galata-watcher-api`, answering with the answers recorded
//! from the real one (`tests/fixtures/watcher`), and keeping its version
//! rules: `PUT /config` needs the current version in `If-Match`, and a body
//! naming an address, where an account's alias belongs or at all, is refused
//! with the recorded problems.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use tokio::net::TcpListener;

pub const TOKEN: &str = "fake-token-SECRET-123";

/// One request the fake saw.
#[derive(Debug, Clone)]
pub struct Seen {
    pub method: Method,
    pub path: String,
    pub authorization: Option<String>,
    pub if_match: Option<String>,
    pub body: Option<Value>,
}

#[derive(Default)]
pub struct Inner {
    /// `None` until the first write, as a fresh watcher.
    pub config: Option<Value>,
    pub version: u64,
    pub seen: Vec<Seen>,
    /// While set, every request gets this status and an empty body.
    pub fail_with: Option<StatusCode>,
    /// While set, every request is redirected here with a 307.
    pub redirect_to: Option<String>,
}

#[derive(Clone, Default)]
pub struct Fake(pub Arc<Mutex<Inner>>);

impl Fake {
    /// A watcher holding the recorded configuration at version 1.
    pub fn configured() -> Self {
        let fake = Self::default();
        {
            let mut i = fake.0.lock().unwrap();
            i.config = Some(fixture("config.json"));
            i.version = 1;
        }
        fake
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.0.lock().unwrap().seen.clone()
    }

    pub fn writes(&self) -> Vec<Seen> {
        self.seen()
            .into_iter()
            .filter(|s| s.method == Method::PUT)
            .collect()
    }

    pub fn redirect_to(&self, location: Option<String>) {
        self.0.lock().unwrap().redirect_to = location;
    }

    pub fn fail_with(&self, status: Option<StatusCode>) {
        self.0.lock().unwrap().fail_with = status;
    }

    /// Change the configuration as another writer would.
    pub fn edit_elsewhere(&self, edit: impl FnOnce(&mut Value)) {
        let mut i = self.0.lock().unwrap();
        edit(i.config.as_mut().expect("configured"));
        i.version += 1;
        let v = i.version;
        i.config.as_mut().unwrap()["version"] = json!(v);
    }

    pub fn config(&self) -> Option<Value> {
        self.0.lock().unwrap().config.clone()
    }

    /// Serve on a free local port. Returns `http://127.0.0.1:<port>`.
    pub async fn serve(&self) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        let app = Router::new().fallback(handle).with_state(self.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }
}

pub fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/watcher/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn with_etag(status: StatusCode, version: u64, body: Value) -> Response {
    let mut r = (status, axum::Json(body)).into_response();
    r.headers_mut()
        .insert(header::ETAG, format!("\"{version}\"").parse().unwrap());
    r
}

async fn handle(
    State(fake): State<Fake>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let text = |name| {
        headers
            .get(name)
            .and_then(|v: &header::HeaderValue| v.to_str().ok())
            .map(str::to_string)
    };
    let mut i = fake.0.lock().unwrap();
    i.seen.push(Seen {
        method: method.clone(),
        path: uri
            .path_and_query()
            .map(|p| p.to_string())
            .unwrap_or_default(),
        authorization: text(header::AUTHORIZATION),
        if_match: text(header::IF_MATCH),
        body: serde_json::from_slice(&body).ok(),
    });
    if let Some(status) = i.fail_with {
        return status.into_response();
    }
    if let Some(location) = &i.redirect_to {
        let mut r = StatusCode::TEMPORARY_REDIRECT.into_response();
        r.headers_mut().insert(
            header::LOCATION,
            format!("{location}{}", uri.path()).parse().unwrap(),
        );
        return r;
    }
    if text(header::AUTHORIZATION).as_deref() != Some(&format!("Bearer {TOKEN}")) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match (method, uri.path()) {
        (Method::GET, "/health") => {
            let mut h = fixture("health.json");
            h["config_version"] = json!(i.version);
            axum::Json(h).into_response()
        }
        (Method::GET, "/alerts") => axum::Json(fixture("alerts.json")).into_response(),
        (Method::GET, "/accounts") => match &i.config {
            Some(_) => axum::Json(fixture("accounts.json")).into_response(),
            None => (StatusCode::NOT_FOUND, axum::Json(json!({"error": "no configuration yet"}))).into_response(),
        },
        (Method::GET, "/config") => match &i.config {
            Some(c) => with_etag(StatusCode::OK, i.version, c.clone()),
            None => with_etag(StatusCode::NOT_FOUND, 0, fixture("no_config.json")),
        },
        (Method::PUT, "/config") => {
            let Some(named) = text(header::IF_MATCH) else {
                return StatusCode::PRECONDITION_REQUIRED.into_response();
            };
            if named.trim_matches('"') != i.version.to_string() {
                return with_etag(
                    StatusCode::PRECONDITION_FAILED,
                    i.version,
                    fixture("changed.json"),
                );
            }
            let Ok(mut doc) = serde_json::from_slice::<Value>(&body) else {
                return StatusCode::UNPROCESSABLE_ENTITY.into_response();
            };
            let an_address = doc.get("address").is_some()
                || doc["accounts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|a| a["alias"].as_str().is_some_and(|a| a.starts_with("0x")));
            if an_address {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    axum::Json(fixture("problems.json")),
                )
                    .into_response();
            }
            // As the watcher does: version and generation are its own, and a
            // level without an id gets one.
            i.version += 1;
            doc["version"] = json!(i.version);
            for level in doc["levels"].as_array_mut().into_iter().flatten() {
                if level.get("id").is_none() {
                    let id = format!(
                        "{}-{}-{}",
                        level["market"].as_str().unwrap_or_default(),
                        level["direction"].as_str().unwrap_or_default(),
                        level["price"].as_str().unwrap_or_default()
                    );
                    level["id"] = json!(id);
                }
                level["generation"] = json!(0);
                if level.get("state").is_none() {
                    level["state"] = json!("armed");
                }
            }
            i.config = Some(doc.clone());
            with_etag(StatusCode::OK, i.version, doc)
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}
