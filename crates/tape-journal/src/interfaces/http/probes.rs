//! The journal as a source for the shared probe router: `/readyz` reports
//! 503 once SIGTERM flips the drain flag, and `/metrics` renders the
//! journal's Prometheus exposition.

use crate::application::JournalService;

impl service_http::ReadinessHook for JournalService {
    fn is_draining(&self) -> bool {
        JournalService::is_draining(self)
    }
}

impl service_http::MetricsProvider for JournalService {
    fn render_metrics(&self) -> String {
        JournalService::render_metrics(self)
    }
}
