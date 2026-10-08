//! tape benchmarks: the local append/replay/checkpoint benchmark and its
//! competitive budget ([`run_benchmark`], [`verify_report`]), and the durable
//! group-commit scaling benchmark over a real HTTP socket
//! ([`run_durable_benchmark`]).

mod durable;
mod report;

pub use durable::{run_durable_benchmark, DurableBenchReport, DurableConnectionSample};
pub use report::{
    default_baseline, external_replay_win, run_benchmark, verify_external_replay_win,
    verify_report, BenchReport, CompetitiveBaseline, ExternalReplayWin, PeerCalibration,
    PerfBudget,
};
