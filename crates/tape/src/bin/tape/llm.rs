//! `tape llm`: agent-facing topics, offline.

use anyhow::Result;
use tape_journal::interfaces::spec;

#[derive(clap::Args)]
pub(crate) struct LlmArgs {
    /// Topic: outline, workflow, api, or boundaries.
    #[arg(long, default_value = "outline")]
    pub(crate) topic: String,
    /// Output format: md or json.
    #[arg(long, default_value = "md")]
    pub(crate) format: String,
}

pub(crate) const LLM_TOPICS: &[cli_std::llm::Topic] = &[
    cli_std::llm::Topic {
        id: "workflow",
        summary: "topic replay model, first CLI slice, and deferred production gates",
        body: spec::llm_workflow_md(),
    },
    cli_std::llm::Topic {
        id: "api",
        summary: "append, replay, checkpoint, retention, and standard endpoint contract",
        body: spec::llm_api_md(),
    },
    cli_std::llm::Topic {
        id: "boundaries",
        summary: "Tape boundary against Relay, Loom, and Keep",
        body: spec::llm_boundaries_md(),
    },
    cli_std::llm::Topic {
        id: "operations",
        summary: "deploy artifacts — k8s crd/operator/instance render, dockerfile render",
        body: "# tape — deploying to Kubernetes\n\n\
            Deploy artifacts are offline renders; the checked-in files under `` \
            are the fixtures, and these commands are their in-binary form (#1328):\n\n\
            - `tape k8s crd render` — the Tape CustomResourceDefinition (tape.dev/v1alpha1).\n\
            - `tape k8s operator render [--namespace tape-system] [--monitoring]` — the \
              operator control plane: RBAC, Deployment, the metrics Service and the PDB. \
              `--monitoring` appends the ServiceMonitor + PrometheusRule, off by default \
              because both are `monitoring.coreos.com/v1` CRDs a vanilla cluster rejects. \
              `tape k8s operator run` runs the reconcile controller (needs a build with \
              `--features operator`).\n\
            - `tape k8s instance render --profile dev|staging|prod|template` — a `kind: \
              Tape` CR; prod is the 3-replica raft-HA shape (the operator renders the \
              StatefulSet topology — `k8s/` base stays a single-node direct install for \
              kind/smoke).\n\
            - `tape dockerfile render --variant source|release [--version]` — the \
              from-source and published-release images.\n\n\
            `spec.auth` is a closed enum, `disabled` or `required`, and **omitting it means \
            `required`** (#2765): a typo or an omission is rejected by the API server instead \
            of quietly serving open. Under `required` the CR must name exactly one token \
            source — `tokensSecret` or `tokensSecretProviderClass`, never both — or the pod \
            fails startup for want of a registry. Spell the off-state `disabled`, not `off`: \
            YAML 1.1 reads a bare `off` as the boolean `false` and the API server rejects it. \
            (`off` is what the serving process's own `TAPE_AUTH` env var takes — the two \
            spellings are not interchangeable.) Every profile states its mode: dev and \
            staging render `auth: disabled`, prod and template render `auth: required`.\n\n\
            HA is auto-mode raft: scale the StatefulSet and set `REPLICAS_PER_SHARD` > 1 \
            (plus `POD_NAME`, `SHARD_COUNT=1`, `VOTER_COUNT` from the downward API) and the \
            same `tape` bin runs a raft group; `--peer-service` (`TAPE_PEER_SERVICE`) names \
            the headless Service for peer DNS. No cluster env = plain single-node.\n\n\
            Backup destinations: `tape llm --topic backup` — rendered at call time \
            from the shared `service-backup` scheme table, never hand-copied (#2483).\n",
    },
];

pub(crate) fn llm(args: LlmArgs) -> Result<()> {
    // #2483/#2494: the backup-destination contract is a SectionedTopic whose
    // scheme facts render at call time from service-backup's
    // SUPPORTED_SCHEMES — route it through render_sectioned so the list can
    // never rot; every other topic stays on the static path.
    if spec::LLM_BACKUP_TOPICS.iter().any(|t| t.id == args.topic) {
        let out = cli_std::llm::render_sectioned(
            crate::cli::TOOL.project,
            crate::cli::TOOL.version,
            spec::LLM_BACKUP_TOPICS,
            &args.topic,
            cli_std::llm::Format::parse(&args.format),
        )?;
        println!("{out}");
        return Ok(());
    }
    let out = cli_std::llm::render(
        crate::cli::TOOL.project,
        crate::cli::TOOL.version,
        LLM_TOPICS,
        &args.topic,
        cli_std::llm::Format::parse(&args.format),
    )?;
    println!("{out}");
    Ok(())
}
