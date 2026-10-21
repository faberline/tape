//! `spec.placement` and `spec.imagePullSecrets` applied to rendered pod specs.

use serde_json::{json, Value};

use super::super::crd::Tape;

/// `spec.placement.nodeSelector` as a pod `nodeSelector`, or `None` when the
/// CR names no node labels.
pub(super) fn placement_node_selector(tape: &Tape) -> Option<Value> {
    tape.spec
        .placement
        .as_ref()
        .filter(|p| !p.node_selector.is_empty())
        .map(|p| json!(p.node_selector))
}

/// `spec.placement.tolerations` as pod `tolerations`.
pub(super) fn placement_tolerations(tape: &Tape) -> Vec<Value> {
    tape.spec
        .placement
        .iter()
        .flat_map(|p| &p.tolerations)
        .map(|t| json!(t))
        .collect()
}

/// Copy `spec.imagePullSecrets` into a rendered pod spec. Leaves the pod spec
/// untouched when the CR names none.
pub(super) fn set_image_pull_secrets(pod_spec: &mut Value, tape: &Tape) {
    if !tape.spec.image_pull_secrets.is_empty() {
        pod_spec["imagePullSecrets"] = json!(tape.spec.image_pull_secrets);
    }
}

/// Apply all of `spec.placement` and `spec.imagePullSecrets` to a pod spec
/// rendered without them (the backup CronJob's job template).
pub(super) fn apply_pod_placement(pod_spec: &mut Value, tape: &Tape) {
    if let Some(selector) = placement_node_selector(tape) {
        pod_spec["nodeSelector"] = selector;
    }
    let tolerations = placement_tolerations(tape);
    if !tolerations.is_empty() {
        pod_spec["tolerations"] = json!(tolerations);
    }
    set_image_pull_secrets(pod_spec, tape);
}
