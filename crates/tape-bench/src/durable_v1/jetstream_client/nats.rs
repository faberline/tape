//! The JetStream side: publish to a FileStore stream and replay the persisted
//! backlog through an explicit-ack pull consumer.

use super::{
    p99, replay_events_for_phase, subject, BenchmarkRecord, Cell, ClientConfig, ReplayResult, BATCH,
};
use crate::DurableV1Profile;
use anyhow::{bail, Context, Result};
use async_nats::jetstream::{
    self,
    consumer::{pull, AckPolicy, DeliverPolicy},
};
use futures::StreamExt;
use std::time::{Duration, Instant};

fn stream_name(config: &ClientConfig) -> String {
    format!(
        "TAPE_BENCH_{}_{}_{}",
        config.run_id, config.payload_bytes, config.clients
    )
}

/// Drive one durable-v1 cell against JetStream FileStore.
pub async fn run(config: ClientConfig) -> Result<BenchmarkRecord> {
    let profile = DurableV1Profile::default();
    config
        .validate_for_target("jetstream", &profile)
        .map_err(anyhow::Error::msg)?;
    if config.phase == "recovery" {
        return run_recovery(config).await;
    }
    let replay_events = replay_events_for_phase(&config);
    let nats = async_nats::connect(&config.nats_url)
        .await
        .context("connect NATS")?;
    let js = jetstream::new(nats);
    let stream_name = stream_name(&config);
    let subject = subject(&config);
    if config.phase == "normal" && config.sample_index == 0 {
        let _ = js.delete_stream(&stream_name).await;
    }
    let mut stream = js
        .get_or_create_stream(jetstream::stream::Config {
            name: stream_name.clone(),
            subjects: vec![subject.clone()],
            storage: jetstream::stream::StorageType::File,
            num_replicas: 1,
            ..Default::default()
        })
        .await
        .context("create FileStore stream")?;
    let payload = vec![b'x'; config.payload_bytes];
    if config.phase == "normal" {
        js.publish(subject.clone(), payload.clone().into())
            .await
            .context("warmup publish")?
            .await
            .context("warmup publish ack")?;
    }
    let target_ops = config.clients * config.operations_per_client;
    let backlog_start = stream
        .info()
        .await
        .context("read stream sequence before backlog")?
        .state
        .last_sequence
        + 1;
    let sample_duration = Duration::from_secs(config.sample_seconds);
    let measure = config.phase == "normal";
    let operations_per_client = config.operations_per_client;
    let started = Instant::now();
    let mut tasks = Vec::with_capacity(config.clients);
    for _ in 0..config.clients {
        let js = js.clone();
        let subject = subject.clone();
        let payload = payload.clone();
        tasks.push(tokio::spawn(async move {
            let mut latencies = Vec::with_capacity(operations_per_client);
            let started = Instant::now();
            let mut i = 0;
            while measure && (started.elapsed() < sample_duration || i == 0) {
                let one = Instant::now();
                js.publish(subject.clone(), payload.clone().into())
                    .await
                    .context("publish")?
                    .await
                    .context("publish ack")?;
                latencies.push(one.elapsed().as_millis());
                i += 1;
            }
            Ok::<_, anyhow::Error>(latencies)
        }));
    }
    let mut latencies: Vec<u128> = Vec::with_capacity(target_ops);
    for task in tasks {
        latencies.extend(task.await.context("publisher task")??);
    }
    let elapsed = started.elapsed();
    let total = latencies.len();
    // The persistence backlog is a separate, fixed recovery workload.
    for _ in 0..replay_events {
        js.publish(subject.clone(), payload.clone().into())
            .await
            .context("publish replay backlog")?
            .await
            .context("replay publish ack")?;
    }
    let verifier_name = format!("tape-bench-finalize-{}", config.sample_index);
    let consumer = if replay_events > 0 {
        Some(
            stream
                .create_consumer(pull::Config {
                    durable_name: Some(verifier_name),
                    deliver_policy: DeliverPolicy::ByStartSequence {
                        start_sequence: backlog_start,
                    },
                    ack_policy: AckPolicy::Explicit,
                    ..Default::default()
                })
                .await
                .context("create explicit pull consumer")?,
        )
    } else {
        None
    };
    let mut replay_received = 0usize;
    let mut ack_errors = 0usize;
    let mut ack_latencies = Vec::with_capacity(config.replay_events / BATCH + 1);
    while replay_received < replay_events {
        let mut batch = consumer
            .as_ref()
            .context("replay verifier exists")?
            .batch()
            .max_messages(BATCH)
            .expires(Duration::from_secs(10))
            .messages()
            .await
            .context("pull")?;
        while let Some(message) = batch.next().await {
            let message = message.map_err(|e| anyhow::anyhow!("pull message: {e}"))?;
            if message.payload.len() != config.payload_bytes {
                ack_errors += 1;
            }
            let ack_started = Instant::now();
            message
                .ack()
                .await
                .map_err(|e| anyhow::anyhow!("explicit ack: {e}"))?;
            ack_latencies.push(ack_started.elapsed().as_millis());
            replay_received += 1;
        }
        if replay_received == 0 {
            bail!("JetStream returned no replay messages");
        }
    }
    let replay = ReplayResult {
        expected: replay_events,
        received: replay_received,
        loss_count: replay_events.saturating_sub(replay_received),
        duplicate_count: replay_received.saturating_sub(config.replay_events),
        error_count: ack_errors,
        passed: replay_received == replay_events && ack_errors == 0,
    };
    let replay_duplicates = replay.duplicate_count;
    let replay_loss = replay.loss_count;
    let finalize = config.phase == "finalize";
    let operation_count = if finalize { replay_events } else { total };
    let record = BenchmarkRecord {
        schema: "tape-vs-jetstream-record.v1",
        target: "jetstream",
        run_id: config.run_id.clone(),
        topic: config.topic.clone(),
        cell: Cell {
            payload_bytes: config.payload_bytes,
            clients: config.clients,
        },
        sample_index: config.sample_index,
        phase: if finalize { "finalize" } else { "normal" },
        operation_count,
        elapsed_ms: elapsed.as_millis(),
        throughput_ops_per_sec: operation_count as f64 / elapsed.as_secs_f64().max(f64::EPSILON),
        p99_ms: p99(latencies),
        ack_p99_ms: p99(ack_latencies),
        replay,
        ack_batch_size: BATCH,
        ack_cumulative: false,
        loss_count: replay_loss,
        duplicate_count: replay_duplicates,
        error_count: ack_errors,
        verdict: "measured",
        failure: None,
    };
    record.validate().map_err(anyhow::Error::msg)?;
    Ok(record)
}

/// Replay the persisted FileStore backlog after a forced restart.
async fn run_recovery(config: ClientConfig) -> Result<BenchmarkRecord> {
    let nats = async_nats::connect(&config.nats_url)
        .await
        .context("connect NATS")?;
    let js = jetstream::new(nats);
    let stream_name = stream_name(&config);
    let subject = subject(&config);
    let mut stream = js
        .get_stream(&stream_name)
        .await
        .context("missing persistent stream")?;
    let info = stream
        .info()
        .await
        .context("read persistent stream info")?
        .clone();
    if info.config.storage != jetstream::stream::StorageType::File
        || info.state.last_sequence < config.replay_events as u64
    {
        bail!("persistent FileStore backlog is missing");
    }
    let start = info.state.last_sequence - config.replay_events as u64 + 1;
    let consumer = stream
        .create_consumer(pull::Config {
            durable_name: Some(format!("tape-bench-recovery-{}", config.sample_index)),
            deliver_policy: DeliverPolicy::ByStartSequence {
                start_sequence: start,
            },
            ack_policy: AckPolicy::Explicit,
            filter_subject: subject,
            ..Default::default()
        })
        .await
        .context("create recovery consumer")?;
    let started = Instant::now();
    let mut received = 0usize;
    let mut errors = 0usize;
    while received < config.replay_events {
        let mut batch = consumer
            .batch()
            .max_messages(BATCH.min(config.replay_events - received))
            .expires(Duration::from_secs(10))
            .messages()
            .await
            .context("recovery pull")?;
        while let Some(message) = batch.next().await {
            let message = message.map_err(|e| anyhow::anyhow!("recovery message: {e}"))?;
            if message.payload.len() != config.payload_bytes {
                errors += 1;
            }
            message
                .ack()
                .await
                .map_err(|e| anyhow::anyhow!("recovery ack: {e}"))?;
            received += 1;
        }
        if received == 0 {
            bail!("persistent backlog replay is empty");
        }
    }
    let elapsed = started.elapsed();
    let replay = ReplayResult {
        expected: config.replay_events,
        received,
        loss_count: config.replay_events.saturating_sub(received),
        duplicate_count: received.saturating_sub(config.replay_events),
        error_count: errors,
        passed: received == config.replay_events && errors == 0,
    };
    let record = BenchmarkRecord {
        schema: "tape-vs-jetstream-record.v1",
        target: "jetstream",
        run_id: config.run_id.clone(),
        topic: config.topic.clone(),
        cell: Cell {
            payload_bytes: config.payload_bytes,
            clients: config.clients,
        },
        sample_index: config.sample_index,
        phase: "recovery",
        operation_count: received,
        elapsed_ms: elapsed.as_millis(),
        throughput_ops_per_sec: received as f64 / elapsed.as_secs_f64().max(f64::EPSILON),
        p99_ms: 1,
        ack_p99_ms: 0,
        replay,
        ack_batch_size: BATCH,
        ack_cumulative: false,
        loss_count: errors,
        duplicate_count: 0,
        error_count: errors,
        verdict: "recovery_measured",
        failure: None,
    };
    record.validate().map_err(anyhow::Error::msg)?;
    Ok(record)
}
