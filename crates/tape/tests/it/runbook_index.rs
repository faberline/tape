//! Offline checks for the runbook index: every alert in the rule files has a
//! row, the raft member-loss page has its cases, and local links resolve.

use serde_yaml::Value;
use std::path::Path;

const INDEX: &str = include_str!("../../../../docs/runbooks/README.md");
const RAFT_PAGE: &str = include_str!("../../../../docs/runbooks/raft-member-loss.md");
const OBSERVABILITY_RULES: &str =
    include_str!("../../../../k8s/components/observability/prometheusrule.yaml");
const OPERATOR_RULES: &str =
    include_str!("../../../../k8s/components/operator-monitoring/prometheusrule.yaml");

fn alert_names(rule_yaml: &str) -> Vec<String> {
    let doc: Value = serde_yaml::from_str(rule_yaml).expect("prometheus rule file parses");
    let mut names = Vec::new();
    if let Some(groups) = doc["spec"]["groups"].as_sequence() {
        for group in groups {
            if let Some(rules) = group["rules"].as_sequence() {
                for rule in rules {
                    if let Some(alert) = rule["alert"].as_str() {
                        names.push(alert.to_string());
                    }
                }
            }
        }
    }
    names
}

fn missing_rows(index: &str, alerts: &[String]) -> Vec<String> {
    alerts
        .iter()
        .filter(|alert| {
            let row_start = format!("| `{alert}` ");
            !index.lines().any(|line| line.starts_with(&row_start))
        })
        .cloned()
        .collect()
}

fn local_link_targets(page: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut rest = page;
    while let Some(start) = rest.find("](") {
        let after = &rest[start + 2..];
        let Some(end) = after.find(')') else {
            break;
        };
        let target = &after[..end];
        if !target.is_empty() && !target.contains("://") && !target.starts_with('#') {
            targets.push(target.split('#').next().unwrap_or(target).to_string());
        }
        rest = &after[end..];
    }
    targets
}

#[test]
fn every_alert_has_a_row_in_the_index() {
    let mut alerts = alert_names(OBSERVABILITY_RULES);
    alerts.extend(alert_names(OPERATOR_RULES));
    assert_eq!(alerts.len(), 13, "guards the parser: found {alerts:?}");
    let missing = missing_rows(INDEX, &alerts);
    assert!(
        missing.is_empty(),
        "alerts without a row in docs/runbooks/README.md: {missing:?}"
    );
}

#[test]
fn missing_rows_reports_an_alert_without_a_row() {
    let missing = missing_rows("| `A` | m | c | l |\n", &["A".to_string(), "B".to_string()]);
    assert_eq!(missing, vec!["B".to_string()]);
}

#[test]
fn index_rows_are_not_only_in_prose() {
    let missing = missing_rows(
        "See TapeRaftLeaderAbsent in prose.\n",
        &["TapeRaftLeaderAbsent".to_string()],
    );
    assert_eq!(missing, vec!["TapeRaftLeaderAbsent".to_string()]);
}

#[test]
fn raft_member_loss_page_has_its_three_cases() {
    for heading in [
        "## Check the group",
        "## One of three replicas lost",
        "## Two of three replicas lost (quorum loss)",
        "## A replaced PVC",
    ] {
        assert!(
            RAFT_PAGE.lines().any(|line| line == heading),
            "raft-member-loss.md lacks heading {heading:?}"
        );
    }
    for needle in ["/raftz", "quorum", "cold-restore.md"] {
        assert!(
            RAFT_PAGE.contains(needle),
            "raft-member-loss.md lacks {needle:?}"
        );
    }
}

#[test]
fn runbook_links_resolve() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/runbooks");
    for (name, page) in [("README.md", INDEX), ("raft-member-loss.md", RAFT_PAGE)] {
        for target in local_link_targets(page) {
            assert!(
                dir.join(&target).exists(),
                "{name} links to a missing target {target}"
            );
        }
    }
}
