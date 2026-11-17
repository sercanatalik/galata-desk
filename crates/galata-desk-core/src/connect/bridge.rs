//! The local server that carries a wallet's proof from the browser to the app.
//!
//! It exists for one connection attempt: it serves the connect page, hands
//! out the challenge, takes the proof, and stops. What keeps it to its own
//! page:
//!
//! - it listens on the loopback interface only;
//! - every API call must carry the attempt's one-time token;
//! - every request must name this server as its `Host`, and API calls must
//!   also come from it as `Origin` (so a page elsewhere, or a hostname that
//!   has been rebound to 127.0.0.1, is refused);
//! - it stops on success, on cancel, and after [`EXPIRY`].

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::header::{self, HeaderValue};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;

use super::proof::{Address, Challenge, ProofError};

/// How long an attempt waits for a valid proof.
pub const EXPIRY: Duration = Duration::from_secs(300);

/// The header that carries the one-time token on API calls.
const TOKEN_HEADER: &str = "x-desk-token";

/// The connect page and its assets, built by `connect/` and embedded here.
#[derive(rust_embed::RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../../connect/dist"]
struct Page;

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("the local connection server could not start: {0}")]
    Bind(#[from] std::io::Error),
}

/// What happens during an attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptEvent {
    /// The wallet proved ownership. The attempt is over.
    Connected(Address),
    /// A proof arrived and was refused. The attempt goes on; the page may try again.
    Rejected(ProofError),
    /// No valid proof arrived in time. The attempt is over.
    Expired,
}

/// One connection attempt. Dropping it cancels the attempt and stops the server.
pub struct Attempt {
    url: String,
    events: mpsc::UnboundedReceiver<AttemptEvent>,
    stop: Arc<Notify>,
    task: JoinHandle<()>,
}

struct Shared {
    token: String,
    /// `127.0.0.1:<port>`, the only `Host` answered.
    host: String,
    /// `http://127.0.0.1:<port>`, the only `Origin` answered.
    origin: String,
    challenge: Challenge,
    project_id: Option<String>,
    events: mpsc::UnboundedSender<AttemptEvent>,
    stop: Arc<Notify>,
}

impl Attempt {
    /// Start an attempt on the current Tokio runtime. `project_id` is the
    /// WalletConnect relay project, without which the page offers no QR code.
    pub async fn start(project_id: Option<String>) -> Result<Self, BridgeError> {
        Self::start_expiring(project_id, EXPIRY).await
    }

    #[doc(hidden)]
    pub async fn start_expiring(
        project_id: Option<String>,
        expiry: Duration,
    ) -> Result<Self, BridgeError> {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).await?;
        let port = listener.local_addr()?.port();
        let host = format!("127.0.0.1:{port}");
        let origin = format!("http://{host}");

        let mut token = [0u8; 32];
        getrandom::fill(&mut token).expect("the operating system provides randomness");
        let token = hex::encode(token);

        let (tx, events) = mpsc::unbounded_channel();
        let stop = Arc::new(Notify::new());
        let shared = Arc::new(Shared {
            token: token.clone(),
            host,
            origin: origin.clone(),
            challenge: Challenge::new(),
            project_id: project_id.filter(|id| !id.trim().is_empty()),
            events: tx.clone(),
            stop: stop.clone(),
        });

        let api = Router::new()
            .route("/attempt", get(attempt))
            .route("/challenge", post(challenge))
            .route("/proof", post(proof))
            .layer(middleware::from_fn_with_state(shared.clone(), guard_api));
        let app = Router::new()
            .nest("/api", api)
            .fallback(asset)
            .layer(middleware::from_fn_with_state(shared.clone(), guard_host))
            .with_state(shared);

        let stopped = stop.clone();
        let task = tokio::spawn(async move {
            let shutdown = async move {
                tokio::select! {
                    _ = stopped.notified() => {}
                    _ = tokio::time::sleep(expiry) => {
                        let _ = tx.send(AttemptEvent::Expired);
                    }
                }
            };
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(shutdown)
                .await;
        });

        Ok(Self {
            url: format!("{origin}/?t={token}"),
            events,
            stop,
            task,
        })
    }

    /// The address to open in the browser. It carries the one-time token.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The next thing that happens; `None` once the attempt is over.
    pub async fn next_event(&mut self) -> Option<AttemptEvent> {
        self.events.recv().await
    }
}

impl Drop for Attempt {
    fn drop(&mut self) {
        // `notify_one` leaves a permit, so the server stops even if it has
        // not started waiting yet. Abort covers a connection that lingers.
        self.stop.notify_one();
        self.task.abort();
    }
}

/// Refuse any request that does not name this server as its host.
async fn guard_host(State(shared): State<Arc<Shared>>, request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok());
    if host != Some(shared.host.as_str()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = next.run(request).await;
    harden(response.headers_mut());
    response
}

/// Refuse any API call from another origin or without the attempt's token.
async fn guard_api(State(shared): State<Arc<Shared>>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    let origin = headers.get(header::ORIGIN).and_then(|h| h.to_str().ok());
    // A same-origin GET may omit Origin; a POST never does. Either way, an
    // Origin that is present must be this server's.
    let origin_ok = match origin {
        Some(origin) => origin == shared.origin,
        None => request.method() == axum::http::Method::GET,
    };
    let token = headers
        .get(TOKEN_HEADER)
        .map(|h| h.as_bytes())
        .unwrap_or_default();
    let token_ok: bool = token.ct_eq(shared.token.as_bytes()).into();
    if !origin_ok || !token_ok {
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(request).await
}

/// What the page may load and talk to: itself, and WalletConnect's relay and
/// verification service, which the QR flow needs. Its scripts, styles and
/// fonts all come from this server.
const CSP: &str = "default-src 'self'; \
    script-src 'self'; \
    style-src 'self'; \
    font-src 'self'; \
    img-src 'self' data:; \
    connect-src 'self' https://*.walletconnect.com wss://*.walletconnect.com https://*.walletconnect.org wss://*.walletconnect.org; \
    frame-src https://verify.walletconnect.com https://verify.walletconnect.org; \
    object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

fn harden(headers: &mut HeaderMap) {
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
}

/// The page and its assets. Unknown paths are not found; there is no listing.
async fn asset(uri: Uri) -> Response {
    let path = match uri.path().trim_start_matches('/') {
        "" => "index.html",
        path => path,
    };
    match Page::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            (
                [(header::CONTENT_TYPE, mime.as_ref().to_string())],
                file.data,
            )
                .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AttemptInfo<'a> {
    /// `null` when no relay project is configured: the page hides the QR option.
    project_id: Option<&'a str>,
}

async fn attempt(State(shared): State<Arc<Shared>>) -> Response {
    Json(AttemptInfo {
        project_id: shared.project_id.as_deref(),
    })
    .into_response()
}

#[derive(Deserialize)]
struct ChallengeRequest {
    address: String,
}

#[derive(Serialize)]
struct ChallengeReply {
    message: String,
}

#[derive(Serialize)]
struct Refusal {
    error: &'static str,
    detail: String,
}

fn refuse(error: &ProofError) -> Response {
    let code = match error {
        ProofError::BadAddress => "bad_address",
        ProofError::BadSignature => "bad_signature",
        ProofError::Mismatch => "mismatch",
    };
    (
        StatusCode::BAD_REQUEST,
        Json(Refusal {
            error: code,
            detail: error.to_string(),
        }),
    )
        .into_response()
}

/// The text for the wallet to sign, for the address it reported.
async fn challenge(
    State(shared): State<Arc<Shared>>,
    Json(request): Json<ChallengeRequest>,
) -> Response {
    match Address::parse(&request.address) {
        Some(address) => Json(ChallengeReply {
            message: shared.challenge.message(&address),
        })
        .into_response(),
        None => refuse(&ProofError::BadAddress),
    }
}

#[derive(Deserialize)]
struct ProofRequest {
    address: String,
    signature: String,
}

#[derive(Serialize)]
struct Accepted<'a> {
    address: &'a str,
}

async fn proof(State(shared): State<Arc<Shared>>, Json(request): Json<ProofRequest>) -> Response {
    match shared
        .challenge
        .verify(&request.address, &request.signature)
    {
        Ok(address) => {
            let reply = Json(Accepted {
                address: address.as_str(),
            })
            .into_response();
            let _ = shared.events.send(AttemptEvent::Connected(address));
            // Graceful: this reply is still delivered, then the server stops.
            shared.stop.notify_one();
            reply
        }
        Err(error) => {
            let _ = shared.events.send(AttemptEvent::Rejected(error.clone()));
            refuse(&error)
        }
    }
}
