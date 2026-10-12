//! Ephemeral loopback WebAuthn bridge, protected by a random fragment token, Host and Origin.
//! No credential response is accepted from another origin; no CORS or persistent listener.
use crate::{AuthError, err};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use subtle::ConstantTimeEq;
use tokio::sync::oneshot;
struct PromptState {
    token: String,
    host: String,
    origin: String,
    options: serde_json::Value,
    finish: Mutex<Option<oneshot::Sender<String>>>,
}
/// A single prompt. Dropping it closes the server; wait accepts at most one response.
pub struct BrowserPrompt {
    url: String,
    result: Option<oneshot::Receiver<String>>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for BrowserPrompt {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl BrowserPrompt {
    /// Opens a browser prompt for registration or authentication, with the peer name visible.
    pub async fn start(
        options: serde_json::Value,
        registration: bool,
        device: &str,
        chinese: bool,
    ) -> Result<Self, AuthError> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(err)?;
        let host = format!("localhost:{}", listener.local_addr().map_err(err)?.port());
        let token = crate::random_id()?;
        let origin = format!("http://{host}");
        let url = format!("{origin}/#{token}");
        let (tx, rx) = oneshot::channel();
        let state = Arc::new(PromptState {
            token,
            host,
            origin,
            options: serde_json::json!({"options":options,"registration":registration,"device":device,"chinese":chinese}),
            finish: Mutex::new(Some(tx)),
        });
        let app = Router::new()
            .route("/", get(page))
            .route("/challenge", get(challenge))
            .route("/finish", post(finish))
            .layer(DefaultBodyLimit::max(128 * 1024))
            .with_state(state);
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self {
            url,
            result: Some(rx),
            server,
        })
    }
    /// Browser URL. The secret is a fragment and is never sent in an HTTP URL or Referer.
    pub fn url(&self) -> &str {
        &self.url
    }
    /// Launches the default system browser without a shell.
    pub fn open(&self) -> Result<(), AuthError> {
        #[cfg(target_os = "macos")]
        let mut command = std::process::Command::new("open");
        #[cfg(windows)]
        let mut command = std::process::Command::new("explorer.exe");
        #[cfg(not(any(target_os = "macos", windows)))]
        let mut command = std::process::Command::new("xdg-open");
        command
            .arg(&self.url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(err)?;
        Ok(())
    }
    /// Waits up to two minutes for a single browser response. Verification happens in the agent.
    pub async fn wait(mut self) -> Result<String, AuthError> {
        let receiver = self
            .result
            .take()
            .ok_or_else(|| err("prompt already consumed"))?;
        tokio::time::timeout(Duration::from_secs(120), receiver)
            .await
            .map_err(|_| err("browser prompt expired"))?
            .map_err(err)
    }
}
fn authorized(state: &PromptState, headers: &HeaderMap, require_origin: bool) -> bool {
    if headers.get("host").and_then(|h| h.to_str().ok()) != Some(state.host.as_str()) {
        return false;
    }
    if require_origin
        && headers.get("origin").and_then(|h| h.to_str().ok()) != Some(state.origin.as_str())
    {
        return false;
    }
    headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .is_some_and(|t| bool::from(t.as_bytes().ct_eq(state.token.as_bytes())))
}
async fn page(State(state): State<Arc<PromptState>>, headers: HeaderMap) -> Response {
    if headers.get("host").and_then(|h| h.to_str().ok()) != Some(state.host.as_str()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = Html(include_str!("prompt.html")).into_response();
    response.headers_mut().insert(
        "cache-control",
        axum::http::HeaderValue::from_static("no-store"),
    );
    response.headers_mut().insert(
        "referrer-policy",
        axum::http::HeaderValue::from_static("no-referrer"),
    );
    response.headers_mut().insert("content-security-policy",axum::http::HeaderValue::from_static("default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'"));
    response
}
async fn challenge(State(state): State<Arc<PromptState>>, headers: HeaderMap) -> Response {
    if !authorized(&state, &headers, false) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = Json(state.options.clone()).into_response();
    response.headers_mut().insert(
        "cache-control",
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}
async fn finish(
    State(state): State<Arc<PromptState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if !authorized(&state, &headers, true) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if serde_json::from_str::<serde_json::Value>(&body).is_err() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let tx = state
        .finish
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if tx.is_some_and(|tx| tx.send(body).is_ok()) {
        StatusCode::ACCEPTED.into_response()
    } else {
        StatusCode::GONE.into_response()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn origin_host_and_token_are_all_checked() {
        let (tx, _) = oneshot::channel();
        let s = PromptState {
            token: "secret".into(),
            host: "localhost:1234".into(),
            origin: "http://localhost:1234".into(),
            options: serde_json::Value::Null,
            finish: Mutex::new(Some(tx)),
        };
        let mut h = HeaderMap::new();
        h.insert("host", "localhost:1234".parse().unwrap());
        h.insert("authorization", "Bearer secret".parse().unwrap());
        h.insert("origin", "https://evil.example".parse().unwrap());
        assert!(!authorized(&s, &h, true));
        h.insert("origin", "http://localhost:1234".parse().unwrap());
        assert!(authorized(&s, &h, true));
        h.insert("host", "evil.example:1234".parse().unwrap());
        assert!(!authorized(&s, &h, true));
    }
}
