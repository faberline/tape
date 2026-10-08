//! The opt-in `spec.backup` children: the backup ServiceAccount and the
//! scheduled `tape backup` CronJob.

use serde_json::{json, Value};
use service_k8s::render::{self, RenderCtx};

use super::super::crd::Tape;
use super::{backup_child, BACKUP_COMPONENT, CLIENT_PORT};

/// A stable, per-instance identity for scheduled backup jobs (lumen's #808
/// pattern, adopted for #2574).
///
/// Rendered even when `spec.backup` is unset. The backup runner writes to a
/// cloud object store, so its ServiceAccount is the binding target for cloud
/// IAM — GKE Workload Identity annotates it, and the GCP acceptance harness
/// already pre-creates `<name>-backup` for exactly that
/// (`acceptance/gcp/scripts/render-manifests.sh`). An
/// identity that blinked in and out with the schedule would drop that binding
/// every time the policy was toggled off, so its lifecycle is deliberately
/// decoupled from the policy's. Like every other child it is owned by the
/// `Tape` CR and garbage collected with it; the cloud annotation is set by a
/// different field manager and survives reconcile.
///
/// It is emitted after the PDB so the workload ServiceAccount stays the first
/// `ServiceAccount` in the render order.
pub(super) fn backup_service_account(cx: &RenderCtx) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "ServiceAccount",
        "metadata": cx.meta(&backup_child(cx.name), BACKUP_COMPONENT),
    })
}

/// The optional scheduled-backup CronJob (#2574): `tape backup` run on the
/// CR's schedule against this instance's own client Service.
///
/// Returns `None` when `spec.backup` is unset, which is the default — a CR
/// that declares no backup renders exactly the object set it rendered before
/// this field existed.
///
/// The container reuses the instance's image so the backup runner tracks the
/// CR rather than drifting from it, which is the whole reason to render this
/// instead of hand-authoring a CronJob alongside. It runs under the dedicated
/// [`backup_service_account`], not the serving one: only this pod needs cloud
/// object-store credentials.
///
/// Auth: when `adminTokenSecret` is set the token is projected as
/// `TAPE_BACKUP_TOKEN`, the env var `tape backup --token` already falls back
/// to. `/admin/backup` requires `admin` on `*`, so an instance running
/// `auth: required` without this field will render a CronJob whose runs fail
/// 401 — the CR is accepted either way because `auth: disabled` instances
/// legitimately need no token. Since #2765 made `required` the default, that
/// combination is now the one a CR reaches by saying nothing, so a `backup`
/// block with no `adminTokenSecret` is worth a second look.
pub(super) fn backup_cron_job(tape: &Tape, cx: &RenderCtx) -> Option<Value> {
    let backup = tape.spec.backup.as_ref()?;
    let cron_name = backup_child(cx.name);

    let mut args = vec![
        "backup".to_string(),
        "--url".to_string(),
        format!(
            "http://{}.{}.svc.cluster.local:{CLIENT_PORT}",
            cx.name, cx.ns
        ),
        "--dest".to_string(),
        backup.destination.clone(),
    ];
    if let Some(seconds) = backup.retention_secs {
        args.extend(["--retention-secs".to_string(), seconds.to_string()]);
    }

    let env = match &backup.admin_token_secret {
        Some(secret) => vec![json!({
            "name": "TAPE_BACKUP_TOKEN",
            "valueFrom": { "secretKeyRef": { "name": secret, "key": "token" } },
        })],
        None => vec![],
    };

    Some(render::cron_job(render::CronJob {
        cx,
        name: &cron_name,
        component: BACKUP_COMPONENT,
        schedule: &backup.schedule,
        image: &tape.spec.cluster.image,
        image_pull_policy: tape
            .spec
            .cluster
            .image_pull_policy
            .as_deref()
            .unwrap_or("IfNotPresent"),
        command: vec!["tape".to_string()],
        args,
        env,
        env_from: vec![],
        volumes: vec![],
        volume_mounts: vec![],
        service_account_name: Some(&cron_name),
        cpu: "100m",
        memory: "128Mi",
        successful_jobs_history_limit: 3,
        failed_jobs_history_limit: 3,
    }))
}
