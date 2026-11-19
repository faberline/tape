//! Offline contracts for the public API TLS decision: TLS ends at the ingress
//! or the mesh, tape serves plain HTTP on 7137, and the opt-in component
//! `k8s/components/public-ingress` renders a TLS Ingress and a NetworkPolicy
//! that admits only the labeled ingress-controller namespace.

use std::path::PathBuf;

use serde_yaml::Value;

fn yaml(input: &str) -> Value {
    serde_yaml::from_str(input).expect("public ingress manifest parses")
}

/// Read `rel` from the repository root (two levels above `crates/tape`).
fn manifest_doc(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn component_lists_the_ingress_and_the_policy() {
    let component = yaml(include_str!(
        "../../../../k8s/components/public-ingress/kustomization.yaml"
    ));
    assert_eq!(component["kind"], "Component");
    let resources: Vec<&str> = component["resources"]
        .as_sequence()
        .expect("resources is a list")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(resources, ["ingress.yaml", "networkpolicy.yaml"]);
}

#[test]
fn ingress_terminates_tls_and_forwards_plain_http_to_the_tape_service() {
    let ingress = yaml(include_str!(
        "../../../../k8s/components/public-ingress/ingress.yaml"
    ));
    assert_eq!(ingress["kind"], "Ingress");
    assert_eq!(ingress["metadata"]["name"], "tape-public");

    let tls = ingress["spec"]["tls"]
        .as_sequence()
        .expect("spec.tls is a list");
    assert_eq!(tls.len(), 1, "one TLS entry");
    let secret = tls[0]["secretName"].as_str().unwrap_or_default();
    assert!(!secret.is_empty(), "TLS entry names a secret");
    let tls_hosts = tls[0]["hosts"].as_sequence().expect("TLS hosts list");
    assert!(!tls_hosts.is_empty(), "TLS entry names a host");

    let rules = ingress["spec"]["rules"]
        .as_sequence()
        .expect("spec.rules is a list");
    assert_eq!(rules.len(), 1, "one rule");
    let host = rules[0]["host"].as_str().unwrap_or_default();
    assert_eq!(
        Some(host),
        tls_hosts[0].as_str(),
        "rule host is the TLS host"
    );

    let paths = rules[0]["http"]["paths"]
        .as_sequence()
        .expect("http.paths is a list");
    assert_eq!(paths.len(), 1, "one path");
    assert_eq!(paths[0]["path"], "/");
    assert_eq!(paths[0]["pathType"], "Prefix");
    assert_eq!(paths[0]["backend"]["service"]["name"], "tape");
    assert_eq!(paths[0]["backend"]["service"]["port"]["name"], "http");

    assert!(
        ingress["spec"]["ingressClassName"].is_null(),
        "ingressClassName is left to the cluster default"
    );
}

/// The negative case: a broad peer or a second port (7138, raft) fails here.
#[test]
fn policy_admits_only_the_labeled_ingress_controller_on_7137() {
    let policy = yaml(include_str!(
        "../../../../k8s/components/public-ingress/networkpolicy.yaml"
    ));
    assert_eq!(policy["kind"], "NetworkPolicy");
    assert_eq!(policy["metadata"]["name"], "tape-server-public-ingress");
    assert_eq!(policy["spec"]["podSelector"]["matchLabels"]["app"], "tape");
    assert_eq!(
        policy["spec"]["podSelector"]["matchLabels"]["role"],
        "server"
    );
    assert_eq!(policy["spec"]["policyTypes"], yaml("[Ingress]"));

    let rules = policy["spec"]["ingress"]
        .as_sequence()
        .expect("spec.ingress is a list");
    assert_eq!(rules.len(), 1, "exactly one ingress rule");

    let peers = rules[0]["from"].as_sequence().expect("from is a list");
    assert_eq!(peers.len(), 1, "exactly one peer");
    let peer = &peers[0];
    assert_eq!(
        peer["namespaceSelector"]["matchLabels"]["tape.cclab.dev/ingress-controller"],
        "true"
    );
    assert!(peer["podSelector"].is_null(), "no pod selector on the peer");
    assert!(peer["ipBlock"].is_null(), "no ipBlock on the peer");

    let ports = rules[0]["ports"].as_sequence().expect("ports is a list");
    assert_eq!(ports.len(), 1, "exactly one port");
    assert_eq!(ports[0]["protocol"], "TCP");
    assert_eq!(ports[0]["port"], 7137);
}

#[test]
fn page_states_the_decision_and_is_linked() {
    let page = manifest_doc("docs/operations/public-api-tls.md");
    for needle in [
        "TLS ends at the ingress",
        "k8s/components/public-ingress",
        "tape.cclab.dev/ingress-controller",
        "7138",
    ] {
        assert!(page.contains(needle), "public-api-tls.md lacks `{needle}`");
    }

    let index = manifest_doc("docs/operations/README.md");
    assert!(
        index.contains("public-api-tls.md"),
        "docs/operations/README.md does not list public-api-tls.md"
    );

    let status = manifest_doc("STATUS.md");
    assert!(
        status.contains("public-api-tls"),
        "STATUS.md has no public-api-tls row"
    );
    assert!(
        status.contains("docs/operations/public-api-tls.md"),
        "STATUS.md does not link docs/operations/public-api-tls.md"
    );
}

#[test]
fn page_links_resolve() {
    let page = manifest_doc("docs/operations/public-api-tls.md");
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("docs/operations");

    let mut checked = 0usize;
    let mut rest = page.as_str();
    while let Some(start) = rest.find("](") {
        let after = &rest[start + 2..];
        let Some(end) = after.find(')') else { break };
        let target = &after[..end];
        rest = &after[end + 1..];

        if target.contains("://") || target.starts_with('#') || target.is_empty() {
            continue;
        }
        let path_part = target.split('#').next().unwrap_or(target);
        let resolved = base.join(path_part);
        assert!(
            resolved.exists(),
            "link `{target}` in docs/operations/public-api-tls.md does not resolve to {}",
            resolved.display()
        );
        checked += 1;
    }
    assert!(
        checked >= 2,
        "the page links the ingress and the policy file, found {checked}"
    );
}
