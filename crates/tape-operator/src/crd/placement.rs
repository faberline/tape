//! Node placement and image-pull wiring for the operator-rendered pods.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Node placement for the operator-rendered pods: the serving StatefulSet
/// and the optional backup CronJob. Layered on top of the operator-owned
/// one-pod-per-node anti-affinity, which it never replaces.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TapePlacement {
    /// Pod `nodeSelector`, e.g. `{ "faberline.dev/pool": "tape-server" }`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub node_selector: BTreeMap<String, String>,

    /// Pod taint tolerations, so a dedicated node pool can carry a taint that
    /// keeps every other workload off it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tolerations: Vec<Toleration>,
}

/// One entry of `placement.tolerations`, mirroring the Kubernetes
/// `v1.Toleration` fields.
///
/// Declared here rather than reused from `k8s-openapi` because the CRD schema
/// is derived with `schemars`, which `k8s-openapi`'s types do not implement.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Toleration {
    /// The taint key this tolerates. Empty with `operator: Exists` tolerates
    /// every taint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,

    /// `Exists` or `Equal`. Unset means `Equal` (the Kubernetes default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<String>,

    /// The taint value to match. Only meaningful with `operator: Equal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,

    /// `NoSchedule`, `PreferNoSchedule`, or `NoExecute`. Unset tolerates every
    /// effect of the matching taint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<String>,

    /// How long the pod stays bound after the node gains a matching taint.
    /// Only meaningful with `effect: NoExecute`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toleration_seconds: Option<i64>,
}

/// A Secret named in the instance's namespace, mirroring the Kubernetes
/// `v1.LocalObjectReference`. Declared here because the CRD schema is
/// derived with `schemars`, which `k8s-openapi`'s types do not implement.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LocalObjectReference {
    /// Name of a Secret of type `kubernetes.io/dockerconfigjson` in the
    /// instance's namespace.
    pub name: String,
}
