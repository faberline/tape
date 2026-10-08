//! Per-op request metrics as a `route_layer` middleware over the data plane,
//! so the journal hot path never carries a metrics probe.

use std::sync::Arc;
use std::time::Instant;

use axum::extract::{MatchedPath, Request, State};
use axum::middleware::Next;
use axum::response::Response;

use crate::application::TapeMetrics;

/// `route_layer` middleware: time each matched data-plane request and record
/// it against its op family. Matched patterns collapse `{topic}`/`{consumer}`
/// cardinality, so the metric set stays bounded.
pub async fn track(State(metrics): State<Arc<TapeMetrics>>, req: Request, next: Next) -> Response {
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| req.uri().path().to_string());
    let method = req.method().clone();
    let start = Instant::now();
    let resp = next.run(req).await;
    metrics.observe_method(method.as_str(), &route, start.elapsed().as_millis() as u64);
    resp
}
