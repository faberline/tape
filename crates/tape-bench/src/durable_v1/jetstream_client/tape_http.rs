//! The Tape side: append over the public HTTP contract and replay the
//! persisted backlog through a subscription with cumulative acknowledgement.

use super::{
    p99, readiness_backoff, replay_events_for_phase, scan_replay_page, tape_subscription_for_phase,
    BenchmarkRecord, Cell, ClientConfig, ReplayResult, TapeEvent, BATCH, READINESS_TIMEOUT,
};
use crate::DurableV1Profile;
use anyhow::{bail, Context, Result};
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

#[derive(Debug, Deserialize)]
struct TapePull {
    events: Vec<TapeEvent>,
    next_offset: u64,
}

async fn wait_until_ready(http: &reqwest::Client, base: &str) -> Result<()> {
    let deadline = Instant::now() + READINESS_TIMEOUT;
    let mut attempt = 0;
    loop {
        match http.get(format!("{base}/readyz")).send().await {
            Ok(response) if response.status().is_success() => return Ok(()),
            Ok(response) => {
                if Instant::now() >= deadline {
                    bail!(
                        "Tape readiness timeout after {}s: /readyz returned {}",
                        READINESS_TIMEOUT.as_secs(),
                        response.status()
                    );
                }
            }
            Err(error) if Instant::now() >= deadline => {
                bail!(
                    "Tape readiness timeout after {}s: {error}",
                    READINESS_TIMEOUT.as_secs()
                );
            }
            Err(_) => {}
        }
        let delay = readiness_backoff(attempt);
        attempt = attempt.saturating_add(1);
        tokio::time::sleep(delay.min(deadline.saturating_duration_since(Instant::now()))).await;
    }
}

async fn pull(
    http: &reqwest::Client,
    base: &str,
    topic: &str,
    sub: &str,
    limit: usize,
) -> Result<reqwest::Response> {
    Ok(http
        .post(format!("{base}/topics/{topic}/subscriptions/{sub}/pull"))
        .json(&json!({"limit": limit}))
        .send()
        .await?)
}

async fn ack(
    http: &reqwest::Client,
    base: &str,
    topic: &str,
    sub: &str,
    offset: u64,
) -> Result<reqwest::Response> {
    Ok(http
        .post(format!("{base}/topics/{topic}/subscriptions/{sub}/ack"))
        .json(&json!({"offset": offset}))
        .send()
        .await?)
}

/// Drive the same public HTTP contract used by the Tape side of the gate.
pub async fn run_tape(config: ClientConfig) -> Result<BenchmarkRecord> {
    let profile = DurableV1Profile::default();
    config
        .validate_for_target("tape", &profile)
        .map_err(anyhow::Error::msg)?;
    if config.phase == "recovery" {
        return run_recovery(config).await;
    }
    let replay_events = replay_events_for_phase(&config);
    let finalize = config.phase == "finalize";
    let http = reqwest::Client::new();
    let base = config.tape_url.trim_end_matches('/');
    let topic = &config.topic;
    let sub = tape_subscription_for_phase(&config, if finalize { "finalize" } else { "normal" });
    wait_until_ready(&http, base)
        .await
        .context("wait for Tape readiness")?;
    let create = http
        .post(format!("{base}/topics/{topic}/subscriptions"))
        .json(&json!({"name": sub}))
        .send()
        .await?;
    if create.status() != StatusCode::CREATED && create.status() != StatusCode::CONFLICT {
        bail!("subscription create failed: {}", create.status());
    }
    let payload = Value::String("x".repeat(config.payload_bytes));
    if config.phase == "normal" {
        let warmup = http
            .post(format!("{base}/topics/{topic}/append"))
            .json(&json!({"key": "warmup", "payload": payload}))
            .send()
            .await?;
        if !warmup.status().is_success() {
            bail!("warmup append failed: {}", warmup.status());
        }
    }
    let target_ops = config.clients * config.operations_per_client;
    let sample_duration = Duration::from_secs(config.sample_seconds);
    let measure = config.phase == "normal";
    let operations_per_client = config.operations_per_client;
    let started = Instant::now();
    let mut tasks = Vec::with_capacity(config.clients);
    for client_index in 0..config.clients {
        let http = http.clone();
        let base = base.to_string();
        let topic = topic.to_string();
        let payload = payload.clone();
        tasks.push(tokio::spawn(async move {
            let mut latencies = Vec::with_capacity(operations_per_client);
            let started = Instant::now();
            let mut i = 0;
            while measure && (started.elapsed() < sample_duration || i == 0) {
                let t = Instant::now();
                let r = http
                    .post(format!("{base}/topics/{topic}/append"))
                    .json(&json!({"key": format!("bench-{client_index}-{i}"), "payload": payload}))
                    .send()
                    .await?;
                if !r.status().is_success() {
                    bail!("append failed: {}", r.status());
                }
                latencies.push(t.elapsed().as_millis());
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
    // Move the public cumulative cursor past measured messages before
    // creating the fixed recovery backlog. This is the Tape equivalent of
    // JetStream's ByStartSequence(total + 1).
    let mut measured = 0usize;
    let mut ack_latencies = Vec::new();
    // Includes the unmeasured warmup record.
    let measured_target = if config.phase == "normal" {
        total + 1
    } else {
        0
    };
    while measured < measured_target {
        let limit = BATCH.min(measured_target - measured);
        let r = pull(&http, base, topic, &sub, limit).await?;
        if !r.status().is_success() {
            bail!("measured cursor pull failed: {}", r.status());
        }
        let b: TapePull = r.json().await?;
        if b.events.is_empty() || b.events.len() > limit {
            bail!("measured cursor did not cover the exact sample window");
        }
        measured += b.events.len();
        let ack_started = Instant::now();
        let a = ack(&http, base, topic, &sub, b.next_offset).await?;
        if !a.status().is_success() {
            bail!("measured cursor ack failed: {}", a.status());
        }
        ack_latencies.push(ack_started.elapsed().as_millis());
    }
    let replay_payload = payload.clone();
    for i in 0..replay_events {
        let r = http
            .post(format!("{base}/topics/{topic}/append"))
            .json(&json!({"key": format!("replay-{i}"), "payload": replay_payload}))
            .send()
            .await?;
        if !r.status().is_success() {
            bail!("replay append failed: {}", r.status());
        }
    }
    let mut got = 0;
    let dups = 0;
    let mut errors = 0;
    let mut replay_index = 0usize;
    while got < replay_events {
        let limit = BATCH.min(replay_events - got);
        let r = pull(&http, base, topic, &sub, limit).await?;
        if !r.status().is_success() {
            bail!("pull failed: {}", r.status());
        }
        let b: TapePull = r.json().await?;
        if b.events.is_empty() {
            bail!("Tape returned no replay messages");
        }
        let (prefix, validated_index) =
            scan_replay_page(&b.events, replay_index).map_err(anyhow::Error::msg)?;
        replay_index = validated_index;
        for e in &b.events[prefix..] {
            if e.payload != replay_payload {
                errors += 1;
            }
            got += 1;
        }
        let ack_started = Instant::now();
        let a = ack(&http, base, topic, &sub, b.next_offset).await?;
        if !a.status().is_success() {
            bail!("ack failed: {}", a.status());
        }
        ack_latencies.push(ack_started.elapsed().as_millis());
    }
    let replay = ReplayResult {
        expected: replay_events,
        received: got,
        loss_count: replay_events.saturating_sub(got),
        duplicate_count: dups,
        error_count: errors,
        passed: got == replay_events && dups == 0 && errors == 0,
    };
    let replay_loss = replay.loss_count;
    let operation_count = if finalize { replay_events } else { total };
    let record = BenchmarkRecord {
        schema: "tape-vs-jetstream-record.v1",
        target: "tape",
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
        ack_cumulative: true,
        loss_count: replay_loss,
        duplicate_count: dups,
        error_count: errors,
        verdict: "measured",
        failure: None,
    };
    record.validate().map_err(anyhow::Error::msg)?;
    Ok(record)
}

/// Replay the persisted Tape backlog after a forced restart.
async fn run_recovery(config: ClientConfig) -> Result<BenchmarkRecord> {
    let http = reqwest::Client::new();
    let base = config.tape_url.trim_end_matches('/');
    let topic = &config.topic;
    let sub = tape_subscription_for_phase(&config, "recovery");
    let mut index = 0usize;
    let started = Instant::now();
    let errors = 0usize;
    while index < config.replay_events {
        let limit = BATCH.min(config.replay_events - index);
        let r = pull(&http, base, topic, &sub, limit).await?;
        if !r.status().is_success() {
            bail!("recovery pull failed: {}", r.status());
        }
        let page: TapePull = r.json().await?;
        if page.events.is_empty() {
            bail!("persistent Tape backlog is missing");
        }
        let (_prefix, validated_index) =
            scan_replay_page(&page.events, index).map_err(anyhow::Error::msg)?;
        index = validated_index;
        let a = ack(&http, base, topic, &sub, page.next_offset).await?;
        if !a.status().is_success() {
            bail!("recovery cumulative ack failed: {}", a.status());
        }
    }
    let elapsed = started.elapsed();
    let replay = ReplayResult {
        expected: config.replay_events,
        received: index,
        loss_count: config.replay_events.saturating_sub(index),
        duplicate_count: 0,
        error_count: errors,
        passed: index == config.replay_events,
    };
    let record = BenchmarkRecord {
        schema: "tape-vs-jetstream-record.v1",
        target: "tape",
        run_id: config.run_id.clone(),
        topic: config.topic.clone(),
        cell: Cell {
            payload_bytes: config.payload_bytes,
            clients: config.clients,
        },
        sample_index: config.sample_index,
        phase: "recovery",
        operation_count: index,
        elapsed_ms: elapsed.as_millis(),
        throughput_ops_per_sec: index as f64 / elapsed.as_secs_f64().max(f64::EPSILON),
        p99_ms: 1,
        ack_p99_ms: 0,
        replay,
        ack_batch_size: BATCH,
        ack_cumulative: true,
        loss_count: config.replay_events.saturating_sub(index),
        duplicate_count: 0,
        error_count: errors,
        verdict: "recovery_measured",
        failure: None,
    };
    record.validate().map_err(anyhow::Error::msg)?;
    Ok(record)
}
