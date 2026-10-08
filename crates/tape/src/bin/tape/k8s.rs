//! `tape k8s`: render the CRD, the operator control plane, and Tape
//! instances offline; run the operator (feature `operator`).

use std::path::PathBuf;

use anyhow::Result;
use clap::{Subcommand, ValueEnum};

use crate::cli::write_or_print;

/// `tape k8s <crd|operator|instance>` — cluster artifacts split by lifecycle
/// layer (#1328).
#[derive(clap::Args, Debug)]
pub(crate) struct K8sArgs {
    #[command(subcommand)]
    pub(crate) cmd: K8sCmd,
}

#[derive(Subcommand, Debug)]
pub(crate) enum K8sCmd {
    /// Cluster-scoped API layer: render the Tape CRD.
    Crd(K8sCrdArgs),
    /// Operator control-plane layer: render assets or run the controller.
    Operator(K8sOperatorArgs),
    /// App-namespace declaration: render a Tape custom resource.
    Instance(K8sInstanceArgs),
}

#[derive(clap::Args, Debug)]
pub(crate) struct K8sCrdArgs {
    #[command(subcommand)]
    pub(crate) cmd: K8sCrdCmd,
}

#[derive(Subcommand, Debug)]
pub(crate) enum K8sCrdCmd {
    /// Render the Tape CustomResourceDefinition YAML.
    Render(K8sFileOutputArgs),
}

#[derive(clap::Args, Debug)]
pub(crate) struct K8sOperatorArgs {
    #[command(subcommand)]
    pub(crate) cmd: Option<K8sOperatorCmd>,
}

#[derive(Subcommand, Debug)]
pub(crate) enum K8sOperatorCmd {
    /// Container entrypoint: run the reconcile controller (needs `--features
    /// operator`). The default when no subcommand is given.
    Run,
    /// Render operator namespace/RBAC/deployment YAML.
    Render(K8sOperatorRenderArgs),
}

#[derive(clap::Args, Debug)]
pub(crate) struct K8sOperatorRenderArgs {
    /// Namespace that owns the operator control plane.
    #[arg(long, default_value = "tape-system")]
    pub(crate) namespace: String,
    /// Also emit the operator's ServiceMonitor and PrometheusRule.
    /// Off by default because both are `monitoring.coreos.com/v1` CRDs and a
    /// cluster without prometheus-operator rejects the whole apply; the
    /// scrape *target* Service carries no CRD dependency and is always
    /// rendered. Mirrors the opt-in `k8s/components/operator-monitoring`
    /// kustomize component.
    #[arg(long)]
    pub(crate) monitoring: bool,
    /// Write to this path instead of stdout. A directory receives
    /// `operator.yaml`.
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub(crate) struct K8sInstanceArgs {
    #[command(subcommand)]
    pub(crate) cmd: K8sInstanceCmd,
}

#[derive(Subcommand, Debug)]
pub(crate) enum K8sInstanceCmd {
    /// Render a namespaced `kind: Tape` custom resource.
    Render(K8sInstanceRenderArgs),
}

#[derive(clap::Args, Debug)]
pub(crate) struct K8sInstanceRenderArgs {
    /// Built-in instance profile.
    #[arg(long, value_enum, default_value_t = K8sInstanceProfile::Dev)]
    pub(crate) profile: K8sInstanceProfile,
    /// Tape CR name. HA (replicasPerShard > 1) instances must keep the
    /// default `tape` — serve derives raft peer DNS as
    /// `tape-<ordinal>.<peer-service>`.
    #[arg(long)]
    pub(crate) name: Option<String>,
    /// Namespace where the app-facing Tape instance lives.
    #[arg(long)]
    pub(crate) namespace: Option<String>,
    /// Journal image. Defaults are profile-specific.
    #[arg(long)]
    pub(crate) image: Option<String>,
    /// Write to this path instead of stdout. A directory receives `tape.yaml`.
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum K8sInstanceProfile {
    /// Small local/kind CR: one journal pod, small disk, verbose logs.
    Dev,
    /// Pre-prod CR: prod-shaped single node, info logs, mid disk.
    Staging,
    /// Production-shape CR: 3-replica raft-HA group, large disk, auth on.
    Prod,
    /// Fill-in-the-blanks CR skeleton for app teams.
    Template,
}

#[derive(clap::Args, Debug)]
pub(crate) struct K8sFileOutputArgs {
    /// Write to this path instead of stdout.
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
}

/// `tape k8s` — cluster artifacts split by lifecycle layer. Only `operator
/// run` needs kube-rs at runtime; the render paths are offline and work from
/// the binary (the generated CRD is embedded, the operator manifests are
/// string-templated, the instance CRs are profile-templated) (#1328).
pub(crate) async fn k8s(args: K8sArgs) -> Result<()> {
    match args.cmd {
        K8sCmd::Crd(a) => match a.cmd {
            K8sCrdCmd::Render(a) => write_or_print(a.out.as_deref(), "crd.yaml", &crd_yaml()),
        },
        K8sCmd::Operator(a) => match a.cmd.unwrap_or(K8sOperatorCmd::Run) {
            K8sOperatorCmd::Run => run_operator().await,
            K8sOperatorCmd::Render(a) => {
                let yaml = render_operator_yaml(&a.namespace, a.monitoring);
                write_or_print(a.out.as_deref(), "operator.yaml", &yaml)
            }
        },
        K8sCmd::Instance(a) => match a.cmd {
            K8sInstanceCmd::Render(a) => {
                let yaml = render_instance_yaml(&a);
                write_or_print(a.out.as_deref(), "tape.yaml", &yaml)
            }
        },
    }
}

#[cfg(feature = "operator")]
pub(crate) async fn run_operator() -> Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
    tape_operator::run().await
}

#[cfg(not(feature = "operator"))]
pub(crate) async fn run_operator() -> Result<()> {
    anyhow::bail!(
        "this tape build was compiled without operator support; rebuild with \
         `--features operator` (the published image includes it)"
    )
}

#[cfg(feature = "operator")]
pub(crate) fn crd_yaml() -> String {
    tape_operator::crd_yaml()
}

#[cfg(not(feature = "operator"))]
pub(crate) fn crd_yaml() -> String {
    cli_std::artifact::ensure_trailing_newline(include_str!("../../../../../k8s/operator/crd.yaml"))
}

/// Render the operator control-plane manifests -- RBAC, Deployment, the
/// metrics Service, and the PDB, plus the ServiceMonitor/PrometheusRule pair
/// when `monitoring` is set -- with the namespace substituted, from the
/// checked-in fixtures. Same set, same order as `k8s/operator/kustomization.yaml`.
pub(crate) fn render_operator_yaml(namespace: &str, monitoring: bool) -> String {
    let mut out = String::new();
    out.push_str(&cli_std::artifact::replace_kubernetes_namespace(
        include_str!("../../../../../k8s/operator/rbac.yaml"),
        "tape-system",
        namespace,
    ));
    out.push_str("\n---\n");
    out.push_str(&cli_std::artifact::replace_kubernetes_namespace(
        include_str!("../../../../../k8s/operator/deployment.yaml"),
        "tape-system",
        namespace,
    ));
    // The operator's own scrape target. Unconditional: it is plain
    // core/v1, so it applies on a cluster with no monitoring stack at all, and
    // shipping it always means turning monitoring on later never has to come
    // back and add the target. Kept in the same order as
    // k8s/operator/kustomization.yaml so the two paths stay comparable.
    out.push_str("\n---\n");
    out.push_str(&cli_std::artifact::replace_kubernetes_namespace(
        &cli_std::artifact::strip_source_ownership_markers(include_str!(
            "../../../../../k8s/operator/service.yaml"
        )),
        "tape-system",
        namespace,
    ));
    // The PDB ships with the Deployment: `replicas: 2` only survives a node
    // drain if evictions are serialized. Render consumers get the same
    // operator layer the kustomize consumers get.
    out.push_str("\n---\n");
    out.push_str(&cli_std::artifact::replace_kubernetes_namespace(
        &cli_std::artifact::strip_source_ownership_markers(include_str!(
            "../../../../../k8s/operator/pdb.yaml"
        )),
        "tape-system",
        namespace,
    ));
    // Opt-in tail, byte-identical to the `operator-monitoring` component so a
    // kustomize consumer and a render consumer get the same alerts. Gated
    // because these two are monitoring.coreos.com CRDs: emitting them
    // unconditionally would make `kubectl apply` of the whole render fail on
    // any cluster without prometheus-operator, taking the operator down with
    // the alerts.
    if monitoring {
        for manifest in [
            include_str!("../../../../../k8s/components/operator-monitoring/servicemonitor.yaml"),
            include_str!("../../../../../k8s/components/operator-monitoring/prometheusrule.yaml"),
        ] {
            out.push_str("\n---\n");
            out.push_str(&rewrite_monitoring_namespace(manifest, namespace));
        }
    }
    cli_std::artifact::ensure_trailing_newline(&out)
}

/// Rewrite the control-plane namespace in the two monitoring manifests.
///
/// `cli_std::artifact::replace_kubernetes_namespace` rewrites the `name:` and
/// `namespace:` keys. The monitoring pair needs additional rewrites because the
/// ServiceMonitor's selector and the PrometheusRule's PromQL expressions embed
/// namespace values in contexts that are not metadata labels: the
/// `namespaceSelector.matchNames` list item `- tape-system`, the PromQL label
/// matcher `namespace="tape-system"`, and the runbook annotation `-n tape-system`.
/// Without these, a ServiceMonitor living in the target namespace selects
/// Services in the wrong namespace, and alerts reference the wrong namespace in
/// their PromQL and remediation steps.
pub(crate) fn rewrite_monitoring_namespace(manifest: &str, namespace: &str) -> String {
    let mut out = cli_std::artifact::replace_kubernetes_namespace(
        &cli_std::artifact::strip_source_ownership_markers(manifest),
        "tape-system",
        namespace,
    );
    // ServiceMonitor namespaceSelector: the list item `- tape-system`.
    out = out.replace("    - tape-system\n", &format!("    - {namespace}\n"));
    // PromQL expressions: label matcher `namespace="tape-system"`.
    out = out.replace(
        r#"namespace="tape-system""#,
        &format!(r#"namespace="{}""#, namespace),
    );
    // Runbook annotations: kubectl command `-n tape-system`.
    out = out.replace("-n tape-system ", &format!("-n {namespace} "));
    out
}

/// Render a `kind: Tape` custom resource for the selected profile.
pub(crate) fn render_instance_yaml(args: &K8sInstanceRenderArgs) -> String {
    let default_version = env!("CARGO_PKG_VERSION");
    let (default_name, default_namespace, default_image, body) = match args.profile {
        K8sInstanceProfile::Dev => (
            "tape",
            "default",
            "tape:latest".to_string(),
            InstanceBody::Dev,
        ),
        K8sInstanceProfile::Staging => (
            "tape",
            "staging",
            format!("ghcr.io/faberline/tape:{default_version}"),
            InstanceBody::Staging,
        ),
        K8sInstanceProfile::Prod => (
            "tape",
            "production",
            format!("ghcr.io/faberline/tape:{default_version}"),
            InstanceBody::Prod,
        ),
        K8sInstanceProfile::Template => (
            "tape",
            "REPLACE_ME__APP_NAMESPACE",
            "REPLACE_ME__REGISTRY/tape:REPLACE_ME__IMAGE_TAG".to_string(),
            InstanceBody::Template,
        ),
    };
    let name = args.name.as_deref().unwrap_or(default_name);
    let namespace = args.namespace.as_deref().unwrap_or(default_namespace);
    let image = args.image.as_deref().unwrap_or(&default_image);

    let mut yaml = format!(
        "apiVersion: tape.dev/v1alpha1\nkind: Tape\nmetadata:\n  name: {name}\n  namespace: {namespace}\nspec:\n  image: {image}\n"
    );
    match body {
        // `auth: disabled` is spelled out, not omitted: since #2765 an absent
        // `auth` defaults to `required`, so a tokenless profile that stayed
        // silent would render a CR whose pod refuses to start for want of a
        // token registry. Tokenless is a choice these two profiles make, and
        // the CR should say so.
        InstanceBody::Dev => {
            yaml.push_str(
                "  replicasPerShard: 1\n  voterCount: 1\n  logLevel: debug\n  storage: 1Gi\n  auth: disabled\n  resources:\n    cpu: \"1\"\n    memory: 4Gi\n",
            );
        }
        InstanceBody::Staging => {
            yaml.push_str(
                "  replicasPerShard: 1\n  voterCount: 1\n  logLevel: info\n  storage: 20Gi\n  auth: disabled\n  resources:\n    cpu: \"1\"\n    memory: 4Gi\n",
            );
        }
        InstanceBody::Prod => {
            yaml.push_str(
                "  imagePullPolicy: Always\n  replicasPerShard: 3\n  voterCount: 3\n  logLevel: info\n  storage: 100Gi\n  graceSecs: 30\n  auth: required\n  tokensSecret: tape-token-registry\n  resources:\n    cpu: \"1\"\n    memory: 4Gi\n",
            );
        }
        // The template carries `auth`/`tokensSecret` explicitly even though
        // `required` is now the default, so the token registry the CR needs is
        // a visible REPLACE_ME rather than a startup failure discovered later.
        InstanceBody::Template => {
            yaml.push_str(
                "  imagePullPolicy: IfNotPresent\n  replicasPerShard: REPLACE_ME__REPLICAS_PER_SHARD\n  voterCount: REPLACE_ME__VOTER_COUNT\n  storage: 10Gi\n  auth: required\n  tokensSecret: REPLACE_ME__TOKEN_REGISTRY_SECRET\n  resources:\n    cpu: \"1\"\n    memory: 4Gi\n",
            );
        }
    }
    cli_std::artifact::ensure_trailing_newline(&yaml)
}

pub(crate) enum InstanceBody {
    Dev,
    Staging,
    Prod,
    Template,
}
