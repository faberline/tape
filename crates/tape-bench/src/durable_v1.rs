//! The fixed `durable-v1` Tape-versus-JetStream comparison profile.
//!
//! [`DurableV1Profile`] and [`validate_image_digest`] are shared by the shell
//! runner (`scripts/jetstream-benchmark/run`) and the tests so a changed
//! workload cannot silently produce a comparable-looking report. The NATS
//! client that drives the profile lives in `jetstream_client`, behind the
//! benchmark-only `jetstream-benchmark` feature, so it never enters the `tape`
//! serving binary or the default build.

#[cfg(feature = "jetstream-benchmark")]
pub mod jetstream_client;

/// The immutable, deliberately small first Tape/JetStream comparison profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DurableV1Profile {
    pub payload_bytes: [usize; 3],
    pub clients: [usize; 3],
    pub warmup: bool,
    pub samples: usize,
    pub sample_seconds: u64,
    pub replay_events: usize,
    pub subscription_batch: usize,
}

impl Default for DurableV1Profile {
    fn default() -> Self {
        Self {
            payload_bytes: [128, 1024, 4096],
            clients: [1, 16, 64],
            warmup: true,
            samples: 5,
            sample_seconds: 60,
            replay_events: 100_000,
            subscription_batch: 100,
        }
    }
}

impl DurableV1Profile {
    /// Reject any workload that differs from the frozen `durable-v1` shape.
    pub fn validate(&self) -> Result<(), String> {
        if self.payload_bytes != [128, 1024, 4096] {
            return Err("workload payload mismatch".into());
        }
        if self.clients != [1, 16, 64] {
            return Err("workload client mismatch".into());
        }
        if !self.warmup || self.samples != 5 || self.sample_seconds != 60 {
            return Err("durable-v1 requires warmup and five 60-second samples".into());
        }
        if self.replay_events != 100_000 || self.subscription_batch != 100 {
            return Err("durable-v1 recovery workload mismatch".into());
        }
        Ok(())
    }
}

/// Require an immutable `name@sha256:<64 hex>` reference for an amd64 image.
pub fn validate_image_digest(image: &str, architecture: &str) -> Result<(), String> {
    let (name, digest) = image
        .split_once('@')
        .ok_or_else(|| "mutable image: digest is required".to_string())?;
    if name.is_empty()
        || !digest.starts_with("sha256:")
        || digest.len() != 71
        || !digest[7..].chars().all(|c| c.is_ascii_hexdigit())
    {
        return Err("invalid image digest; expected name@sha256:<64 hex>".into());
    }
    if architecture != "amd64" {
        return Err("unsupported architecture; durable-v1 requires amd64".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
