//! The Prometheus exposition `/metrics` serves.

use metrics_prometheus::{escape_label_value, Label, LabeledSample, SampleGroup};

use super::JournalService;

impl JournalService {
    /// Prometheus exposition: tape's per-op request counts + latency, plus
    /// topic latest offset and subscription lag gauges computed at scrape
    /// time under the journal mutex (#2485).
    pub fn render_metrics(&self) -> String {
        let mut output = self.metrics.render();

        // The journal is an in-memory BTreeMap; walking it is O(topics +
        // cursors) and consistent by construction. A very large topic count
        // would lengthen the scrape's lock hold — documented as acceptable at
        // tape's scale.
        let journal = match self.journal.lock() {
            Ok(j) => j,
            Err(_) => return output, // Poisoned mutex; return request metrics only
        };

        let mut topic_offset_rows: Vec<(String, u64)> = Vec::new();
        let mut lag_rows: Vec<(String, String, u64)> = Vec::new();

        for subscription in journal.all_subscriptions() {
            let topic = &subscription.topic;
            let end_offset = journal.end_offset(topic);
            let escaped_topic = escape_label_value(topic);

            // tape_topic_latest_offset{topic} — collect once per topic,
            // dedup after loop.
            topic_offset_rows.push((escaped_topic.clone(), end_offset));

            // tape_subscription_lag{topic,subscription}
            let checkpoint = journal.checkpoint(topic, &subscription.name);
            let cursor_offset = checkpoint.map(|c| c.offset).unwrap_or(0);
            let lag = end_offset.saturating_sub(cursor_offset);

            let escaped_subscription = escape_label_value(&subscription.name);
            lag_rows.push((escaped_topic, escaped_subscription, lag));
        }

        topic_offset_rows.sort_by(|a, b| a.0.cmp(&b.0));
        topic_offset_rows.dedup_by(|a, b| a.0 == b.0);

        if !topic_offset_rows.is_empty() {
            let topic_samples: Vec<LabeledSample> = topic_offset_rows
                .iter()
                .map(|(topic, offset)| {
                    LabeledSample::new(vec![Label::new("topic", topic)], *offset)
                })
                .collect();
            let topic_group = SampleGroup::new(
                "tape_topic_latest_offset",
                "gauge",
                "Latest offset for each topic (topic index + 1).",
                &topic_samples,
            );
            output.push_str(&metrics_prometheus::render_labeled(&[topic_group]));
        }

        if !lag_rows.is_empty() {
            let lag_samples: Vec<LabeledSample> = lag_rows
                .iter()
                .map(|(topic, subscription, lag)| {
                    LabeledSample::new(
                        vec![
                            Label::new("subscription", subscription),
                            Label::new("topic", topic),
                        ],
                        *lag,
                    )
                })
                .collect();
            let lag_group = SampleGroup::new(
                "tape_subscription_lag",
                "gauge",
                "Lag of each subscription's checkpoint behind the topic's latest offset.",
                &lag_samples,
            );
            output.push_str(&metrics_prometheus::render_labeled(&[lag_group]));
        }

        output
    }
}
