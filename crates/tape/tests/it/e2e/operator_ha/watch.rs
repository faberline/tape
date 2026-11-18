//! The Kubernetes watch and the JSON log events the suite parses.

use serde_json::Value;

#[derive(Debug, Clone)]
pub(super) struct LogEvent {
    pub(super) event: String,
    pub(super) raw: String,
}

pub(super) fn parse_json_log_events(logs: &str) -> Vec<LogEvent> {
    logs.lines()
        .filter_map(|line| {
            let val: Value = serde_json::from_str(line).ok()?;
            let event = val
                .get("event")
                .and_then(Value::as_str)
                .or_else(|| {
                    val.get("fields")
                        .and_then(|f| f.get("event"))
                        .and_then(Value::as_str)
                })
                .or_else(|| {
                    val.get("attributes")
                        .and_then(|a| a.get("event"))
                        .and_then(Value::as_str)
                })?
                .to_owned();
            Some(LogEvent {
                event,
                raw: line.to_owned(),
            })
        })
        .collect()
}

/// The `shutdown_started` and `raft_leadership` lines of a pod log, one per
/// line, for a failure message. "none" when there are no such lines.
pub(super) fn leadership_lines(events: &[LogEvent]) -> String {
    let lines: Vec<&str> = events
        .iter()
        .filter(|e| e.event == "shutdown_started" || e.event == "raft_leadership")
        .map(|e| e.raw.as_str())
        .collect();
    if lines.is_empty() {
        "none".to_owned()
    } else {
        lines.join("\n")
    }
}

/// Reads the server container's terminated state of the pod with `uid` from
/// `kubectl get pod -w -o json --output-watch-events` output (concatenated
/// watch events). Returns (exitCode, finishedAt as epoch seconds); the epoch
/// is None when finishedAt is missing or unparseable. None when no event
/// shows a terminated server container for that uid.
pub(super) fn terminated_from_watch(stream: &str, uid: &str) -> Option<(i64, Option<u64>)> {
    for event in serde_json::Deserializer::from_str(stream).into_iter::<Value>() {
        let event = event.ok()?;
        let pod = &event["object"];
        if pod["metadata"]["uid"].as_str() != Some(uid) {
            continue;
        }
        let Some(statuses) = pod["status"]["containerStatuses"].as_array() else {
            continue;
        };
        let Some(server) = statuses.iter().find(|s| s["name"] == "server") else {
            continue;
        };
        if let Some(term) = server["state"]["terminated"].as_object() {
            let exit_code = term.get("exitCode").and_then(Value::as_i64)?;
            let finished = term
                .get("finishedAt")
                .and_then(Value::as_str)
                .and_then(rfc3339_to_unix_secs);
            return Some((exit_code, finished));
        }
    }
    None
}

fn rfc3339_to_unix_secs(s: &str) -> Option<u64> {
    let (date, time) = s.split_once('T')?;
    let time = time.trim_end_matches('Z');
    let mut date_parts = date.split('-');
    let year: u64 = date_parts.next()?.parse().ok()?;
    let month: u64 = date_parts.next()?.parse().ok()?;
    let day: u64 = date_parts.next()?.parse().ok()?;
    let mut time_parts = time.split(':');
    let hour: u64 = time_parts.next()?.parse().ok()?;
    let min: u64 = time_parts.next()?.parse().ok()?;
    let sec_str = time_parts.next()?;
    let sec: u64 = sec_str.split('.').next()?.parse().ok()?;

    let mut y = year;
    let mut m = month;
    if m <= 2 {
        y -= 1;
        m += 12;
    }
    let era = y / 400;
    let yoe = y - era * 400;
    let doy = (153 * (m - 3) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hour * 3600 + min * 60 + sec)
}

#[test]
fn rfc3339_to_unix_secs_computes_correct_epoch_timestamp() {
    assert_eq!(rfc3339_to_unix_secs("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(
        rfc3339_to_unix_secs("2026-10-23T17:50:00Z"),
        Some(1792777800)
    );
    assert_eq!(
        rfc3339_to_unix_secs("2026-10-23T17:50:05.123Z"),
        Some(1792777805)
    );
}

#[test]
fn terminated_from_watch_reads_the_leader_terminated_state() {
    let stream = concat!(
        "{\"type\":\"ADDED\",\"object\":{\"metadata\":{\"uid\":\"u1\"},\"status\":{\"containerStatuses\":[{\"name\":\"server\",\"state\":{\"running\":{}}}]}}}\n",
        "{\"type\":\"MODIFIED\",\"object\":{\"metadata\":{\"uid\":\"u1\"},\"status\":{\"containerStatuses\":[{\"name\":\"server\",\"state\":{\"terminated\":{\"exitCode\":0,\"finishedAt\":\"2026-10-23T17:50:05Z\"}}}]}}}\n",
    );
    assert_eq!(
        terminated_from_watch(stream, "u1"),
        Some((0, Some(1792777805)))
    );
}

#[test]
fn terminated_from_watch_without_terminated_state_is_none() {
    let stream = "{\"type\":\"ADDED\",\"object\":{\"metadata\":{\"uid\":\"u1\"},\"status\":{\"containerStatuses\":[{\"name\":\"server\",\"state\":{\"running\":{}}}]}}}\n";
    assert_eq!(terminated_from_watch(stream, "u1"), None);
}

#[test]
fn parse_json_log_events_extracts_event_name_and_fields() {
    let sample = "{\"event\":\"http_drained\",\"message\":\"drained\"}\n\
{\"attributes\":{\"event\":\"raft_shutdown\",\"handoff\":\"Transferred\"}}\n\
{\"fields\":{\"event\":\"other\"},\"level\":\"info\"}\n\
invalid json line\n";
    let events = parse_json_log_events(sample);
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].event, "http_drained");
    assert_eq!(events[1].event, "raft_shutdown");
    assert_eq!(events[2].event, "other");
}

#[test]
fn leadership_lines_keeps_only_the_shutdown_and_leadership_events() {
    let sample = "{\"event\":\"shutdown_started\",\"message\":\"a\"}\n\
{\"event\":\"http_drained\"}\n\
{\"attributes\":{\"event\":\"raft_leadership\",\"phase\":\"sigterm\",\"is_leader\":false}}\n\
{\"fields\":{\"event\":\"raft_leadership\",\"phase\":\"before_handoff\",\"is_leader\":false}}\n";
    let events = parse_json_log_events(sample);
    assert_eq!(
        leadership_lines(&events),
        "{\"event\":\"shutdown_started\",\"message\":\"a\"}\n\
{\"attributes\":{\"event\":\"raft_leadership\",\"phase\":\"sigterm\",\"is_leader\":false}}\n\
{\"fields\":{\"event\":\"raft_leadership\",\"phase\":\"before_handoff\",\"is_leader\":false}}"
    );
}

#[test]
fn leadership_lines_says_none_when_the_log_has_no_such_event() {
    let events = parse_json_log_events("{\"event\":\"http_drained\"}\n");
    assert_eq!(leadership_lines(&events), "none");
}
