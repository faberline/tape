//! tape benchmarks: the local append/replay/checkpoint benchmark and its
//! competitive budget ([`run_benchmark`], [`verify_report`]), and the durable
//! group-commit scaling benchmark over a real HTTP socket
//! ([`run_durable_benchmark`]), and the fixed durable-v1 Tape-versus-JetStream
//! profile ([`DurableV1Profile`]). The JetStream client that drives that
//! profile is compiled only with the benchmark-only `jetstream-benchmark`
//! feature.

mod durable;
mod durable_v1;
mod report;

pub use durable::{run_durable_benchmark, DurableBenchReport, DurableConnectionSample};
#[cfg(feature = "jetstream-benchmark")]
pub use durable_v1::jetstream_client;
pub use durable_v1::{validate_image_digest, DurableV1Profile};
pub use report::{
    default_baseline, external_replay_win, run_benchmark, verify_external_replay_win,
    verify_report, BenchReport, CompetitiveBaseline, ExternalReplayWin, PeerCalibration,
    PerfBudget,
};
