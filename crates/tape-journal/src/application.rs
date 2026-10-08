//! Journal use cases: the [`JournalService`] every serving path drives, the
//! wall-clock edge of the journal, and the request metrics.

mod clock;
mod error;
mod metrics;
mod service;

pub use clock::{now_ms, JournalClock};
pub use error::JournalError;
pub use metrics::TapeMetrics;
pub use service::{JournalService, REPLAY_CONTENT_TYPE};
