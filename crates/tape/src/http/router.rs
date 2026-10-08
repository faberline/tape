//! The axum app: the journal data plane under auth, request metrics, the body
//! limit, and optional admission, merged onto the shared probe routes and the
//! raft peer routes.

use std::sync::Arc;

use axum::http::{header, Method, StatusCode};
use axum::middleware::{from_fn, from_fn_with_state, Next};
use axum::response::{IntoResponse, Response};
use axum::Router;
use service_auth::ReloadableRoleMapVerifier;
use service_http::MetricsProvider;

use super::AppState;
use tape_journal::interfaces::http::{data_plane_routes, openapi, track};

/// Build the HTTP router for the tape transport: the `/topics` data plane
/// merged onto the shared service shell's standard probe routes.
pub fn router(state: AppState) -> Router {
    router_with_admission(state, None)
}

/// Build the public router for a deployment whose Raft peer routes are owned
/// by the dedicated mTLS listener. The underlying application composition is
/// deliberately unchanged so data, probes, auth, admission, and metrics stay
/// identical; a first middleware rejects the two peer route families before
/// route dispatch can expose them on the public h2c listener.
pub fn router_without_raft_routes(state: AppState) -> Router {
    router_without_raft_routes_with_admission(state, None)
}

/// Build the secure-peer public router with optional shared request admission.
/// Peer route isolation stays outermost, so the public listener rejects Raft
/// routes before admission can account for them.
pub fn router_without_raft_routes_with_admission(
    state: AppState,
    admission: Option<service_http::AdmissionController>,
) -> Router {
    router_with_admission(state, admission).layer(from_fn(reject_public_raft_routes))
}

async fn reject_public_raft_routes(request: axum::extract::Request, next: Next) -> Response {
    let path = request.uri().path();
    if path == "/raftz" || path.starts_with("/raft/") {
        StatusCode::NOT_FOUND.into_response()
    } else {
        next.run(request).await
    }
}

/// Build Tape with optional shared request admission. Tape owns the
/// read/write/admin route classes; `service-http` owns opaque-key retention,
/// token buckets, eviction, observability, and the 429 wire response.
pub fn router_with_admission(
    state: AppState,
    admission: Option<service_http::AdmissionController>,
) -> Router {
    let service = state.service().clone();
    let data_plane = data_plane_routes()
        // Shared bearer auth (#1326) on the data plane ONLY — probes stay
        // tokenless. The blanket middleware authenticates (401 on a
        // missing/unknown token when required) and injects the
        // AuditedRoleMapPrincipal each handler authorizes on its {topic}.
        .route_layer(from_fn_with_state(
            state.verifier(),
            service_auth::auth_middleware::<ReloadableRoleMapVerifier>,
        ))
        // Per-op request metrics (counts + latency). route_layer => only for
        // matched data-plane routes, and MatchedPath is populated. Added
        // after (= outside) the auth layer so rejected requests are still
        // counted.
        .route_layer(from_fn_with_state(state.metrics(), track))
        .with_state(service.clone())
        // Data-plane-only request body cap (#2484); probes below stay
        // unbounded, matching `service_http`'s documented probe behavior.
        // Enforces the configured body_limit_bytes with a structured 413 envelope.
        .layer(service_http::body_limit_layer(state.body_limit_bytes()));
    let data_plane = match admission {
        Some(controller) => data_plane.route_layer(from_fn_with_state(
            service_http::AdmissionMiddleware::new(controller, |request| {
                let path = request.uri().path();
                let class = if path.starts_with("/admin/") {
                    "tape.admin"
                } else if *request.method() == Method::GET {
                    "tape.read"
                } else {
                    "tape.write"
                };
                let key = request
                    .headers()
                    .get(header::AUTHORIZATION)
                    .map(|value| value.as_bytes())
                    .unwrap_or(b"anonymous");
                Some(service_http::AdmissionInput::new(class, key))
            }),
            service_http::admission_middleware,
        )),
        None => data_plane,
    };

    // Standard probes (`/healthz`, `/readyz`, `/metrics`, `/openapi.json`,
    // `/docs`) come from the shared service shell so the operational
    // surface matches every other service in the ecosystem. The journal
    // service supplies readiness + Prometheus metrics; `/readyz` reports 503
    // while draining.
    let probe_state = Arc::new(service);
    let metrics: Arc<dyn MetricsProvider> = probe_state.clone();
    let probes = service_http::standard_probe_routes(probe_state, Some(metrics), openapi);

    let app = probes
        .merge(data_plane)
        // One INFO-level tracing span per request — spans probes + data plane.
        .layer(service_http::trace_layer())
        // Per-request Server-Timing response attribution, composed at the
        // same outermost position as trace_layer() above (#2490).
        .layer(from_fn(service_http::server_timing_middleware));

    // Peer raft RPCs + leader forward + `/raftz` (#1327) — merged OUTSIDE the
    // bearer-auth data plane, like the probes, since this is cluster traffic
    // between tape nodes rather than a client-facing route.
    match state.raft() {
        Some(raft) => app.merge(raft.router()),
        None => app,
    }
}
