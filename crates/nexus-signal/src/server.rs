//! HTTP surface: the WebSocket endpoint, a health check and Prometheus-style metrics.

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::Router;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use nexus_proto::signal::WS_PATH;
use tokio::net::TcpListener;

use crate::hub::Hub;
use crate::session;

/// Builds the router for `hub`.
pub fn router(hub: Arc<Hub>) -> Router {
    Router::new()
        .route(WS_PATH, get(ws_handler))
        .route("/healthz", get(|| async { "ok" }))
        .route("/metrics", get(metrics))
        .with_state(hub)
}

/// Serves `hub` on `listener` until `shutdown` completes.
pub async fn serve(
    listener: TcpListener,
    hub: Arc<Hub>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let _reaper = hub.spawn_reaper();
    axum::serve(
        listener,
        router(hub).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
}

async fn ws_handler(
    State(hub): State<Arc<Hub>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let ip = client_ip(&hub, addr.ip(), &headers);
    let max = hub.config.max_message_bytes;
    ws.max_message_size(max)
        .max_frame_size(max)
        .on_upgrade(move |socket| session::run(hub, socket, ip))
}

/// The address to rate-limit by. `X-Forwarded-For` is only believed when explicitly enabled,
/// because otherwise any client could pick its own "IP" and dodge the limiter.
fn client_ip(hub: &Hub, peer: IpAddr, headers: &HeaderMap) -> IpAddr {
    if hub.config.trust_forwarded_for
        && let Some(first) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .and_then(|v| v.trim().parse::<IpAddr>().ok())
    {
        return first;
    }
    peer
}

async fn metrics(State(hub): State<Arc<Hub>>) -> impl IntoResponse {
    let m = &hub.metrics;
    let line = |name: &str, kind: &str, help: &str, v: u64| {
        format!("# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {v}\n")
    };
    let body = [
        line(
            "nexus_signal_connections",
            "gauge",
            "Authenticated connections.",
            m.connections.load(Ordering::Relaxed),
        ),
        line(
            "nexus_signal_logins_total",
            "counter",
            "Successful logins.",
            m.logins.load(Ordering::Relaxed),
        ),
        line(
            "nexus_signal_auth_failures_total",
            "counter",
            "Failed logins.",
            m.auth_failures.load(Ordering::Relaxed),
        ),
        line(
            "nexus_signal_envelopes_total",
            "counter",
            "Sealed envelopes routed.",
            m.envelopes.load(Ordering::Relaxed),
        ),
        line(
            "nexus_signal_pairings_registered_total",
            "counter",
            "Pairings registered.",
            m.pairings_registered.load(Ordering::Relaxed),
        ),
        line(
            "nexus_signal_pairings_revoked_total",
            "counter",
            "Pairings revoked.",
            m.pairings_revoked.load(Ordering::Relaxed),
        ),
        line(
            "nexus_signal_rate_limited_total",
            "counter",
            "Requests refused by rate limits.",
            m.rate_limited.load(Ordering::Relaxed),
        ),
    ]
    .concat();
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        body,
    )
}
