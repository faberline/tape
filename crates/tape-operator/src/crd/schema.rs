//! The generated `Tape` CRD YAML and the CEL rules attached to its spec schema.

use super::Tape;

/// The CEL rule enforcing that a `Tape` names at most one token source
/// (#2765).
///
/// Presence tests only. Both fields render as `nullable: true`, which reads
/// like it needs an explicit `!= null` guard — it does not, and adding one
/// breaks the CRD outright. Kubernetes types a nullable string as plain
/// `string`, so `self.tokensSecret != null` fails CEL compilation at the API
/// server ("found no matching overload for '_!=_' applied to '(string,
/// null)'"), and every local test still passes because they assert on YAML
/// text, never on the compiled expression. The guard is also unnecessary:
/// Kubernetes prunes an explicitly-null field before CEL runs, so `has()`
/// already reports it absent. Lumen shipped the `!= null` shape once and it
/// installed on no cluster; it was caught only by
/// `kubectl apply --dry-run=server`. The contract is documented at
/// `core/crates/service-k8s/src/crd.rs`.
const ONE_TOKEN_SOURCE_RULE: &str =
    "!(has(self.tokensSecret) && has(self.tokensSecretProviderClass))";

const ONE_TOKEN_SOURCE_MESSAGE: &str =
    "set at most one of spec.tokensSecret (a Kubernetes Secret) or \
     spec.tokensSecretProviderClass (a Secret Manager CSI projection); with both set there is no \
     way to tell which registry is actually being served";

/// The CEL rule rejecting a backup schedule that can only ever 401: a
/// `spec.backup` with no `adminTokenSecret` on an instance running
/// `auth: required` (#3058). Presence tests only, for the same reason as
/// [`ONE_TOKEN_SOURCE_RULE`].
const BACKUP_NEEDS_ADMIN_TOKEN_RULE: &str =
    "!(has(self.backup) && self.auth == 'required' && !has(self.backup.adminTokenSecret))";

const BACKUP_NEEDS_ADMIN_TOKEN_MESSAGE: &str =
    "set spec.backup.adminTokenSecret when spec.auth is required, or the scheduled \
     backup runs authenticate with no token and every run fails 401 while the instance \
     keeps reporting Ready";

/// The `Tape` CustomResourceDefinition as YAML, for `kubectl apply`.
///
/// The schema is normalized to be Kubernetes-OpenAPI compatible: schemars
/// emits `format: uint32`/`uint64` for tape's unsigned counts, which the API
/// server's structural-schema validation rejects, so those are rewritten to a
/// plain integer with a `minimum: 0` floor (relay/keep's pattern).
pub fn crd_yaml() -> String {
    use kube::CustomResourceExt;
    let mut crd = serde_json::to_value(Tape::crd()).expect("CRD serializes to JSON");
    service_k8s::crd::normalize_unsigned_integer_formats(&mut crd);
    let attached = service_k8s::crd::add_spec_validation_rule(
        &mut crd,
        ONE_TOKEN_SOURCE_RULE,
        ONE_TOKEN_SOURCE_MESSAGE,
    );
    assert!(
        attached > 0,
        "the one-token-source rule must reach the spec schema; the generated CRD changed shape"
    );
    let attached = service_k8s::crd::add_spec_validation_rule(
        &mut crd,
        BACKUP_NEEDS_ADMIN_TOKEN_RULE,
        BACKUP_NEEDS_ADMIN_TOKEN_MESSAGE,
    );
    assert!(
        attached > 0,
        "the backup-admin-token rule must reach the spec schema; the generated CRD changed shape"
    );
    let yaml = serde_yaml::to_string(&crd).expect("CRD serializes");
    service_k8s::crd::quote_yaml_1_1_boolean_like_strings(&yaml)
}
