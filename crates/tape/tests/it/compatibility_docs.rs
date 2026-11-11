//! Version compatibility gate (#74).
//!
//! `docs/operations/compatibility.md` states the rules that hold between two
//! adjacent tape releases. `clients/openapi.baseline.json` is the floor of the
//! HTTP contract: the live `clients/openapi.json` may add routes, methods and
//! fields, but must not drop what the baseline promised. These tests pin the
//! page's required sections, the baseline check, and the links to the page.

use std::path::PathBuf;

/// Read `rel` from the repository root (two levels above `crates/tape`).
fn manifest_doc(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

const METHODS: [&str; 5] = ["get", "post", "put", "delete", "patch"];

/// One string per route, method or schema field that `baseline` has and
/// `live` lacks. Additions in `live` are not reported.
fn missing_from_live(baseline: &serde_json::Value, live: &serde_json::Value) -> Vec<String> {
    let mut missing = Vec::new();

    if let Some(paths) = baseline["paths"].as_object() {
        for (path, item) in paths {
            match live["paths"].get(path) {
                None => missing.push(format!("route {path}")),
                Some(live_item) => {
                    for method in METHODS {
                        if item.get(method).is_some() && live_item.get(method).is_none() {
                            missing.push(format!("method {} {path}", method.to_uppercase()));
                        }
                    }
                }
            }
        }
    }

    if let Some(schemas) = baseline["components"]["schemas"].as_object() {
        for (name, schema) in schemas {
            match live["components"]["schemas"].get(name) {
                None => missing.push(format!("schema {name}")),
                Some(live_schema) => {
                    if let Some(props) = schema["properties"].as_object() {
                        for prop in props.keys() {
                            if live_schema["properties"].get(prop).is_none() {
                                missing.push(format!("field {name}.{prop}"));
                            }
                        }
                    }
                }
            }
        }
    }

    missing
}

fn parse(text: &str, what: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("parse {what}: {e}"))
}

#[test]
fn openapi_keeps_every_baseline_route_method_and_field() {
    let baseline = parse(
        &manifest_doc("clients/openapi.baseline.json"),
        "clients/openapi.baseline.json",
    );
    let live = parse(
        &manifest_doc("clients/openapi.json"),
        "clients/openapi.json",
    );
    let missing = missing_from_live(&baseline, &live);
    assert!(
        missing.is_empty(),
        "clients/openapi.json drops {} thing(s) the baseline promises: {missing:?}",
        missing.len()
    );
}

#[test]
fn missing_from_live_detects_a_removed_route_method_and_field() {
    let baseline = parse(
        &manifest_doc("clients/openapi.baseline.json"),
        "clients/openapi.baseline.json",
    );
    let mut live = baseline.clone();
    live["paths"]
        .as_object_mut()
        .expect("paths object")
        .remove("/healthz");
    live["paths"]["/topics/{topic}/append"]
        .as_object_mut()
        .expect("append item object")
        .remove("post");
    live["components"]["schemas"]["AppendEventRequest"]["properties"]
        .as_object_mut()
        .expect("AppendEventRequest properties object")
        .remove("payload");

    let mut missing = missing_from_live(&baseline, &live);
    missing.sort();
    let mut expected = vec![
        "route /healthz".to_string(),
        "method POST /topics/{topic}/append".to_string(),
        "field AppendEventRequest.payload".to_string(),
    ];
    expected.sort();
    assert_eq!(missing, expected);
}

#[test]
fn missing_from_live_accepts_additions() {
    let baseline = parse(
        &manifest_doc("clients/openapi.baseline.json"),
        "clients/openapi.baseline.json",
    );
    let mut live = baseline.clone();
    live["paths"]["/new"] = serde_json::json!({
        "get": { "responses": { "200": { "description": "new route" } } }
    });
    live["components"]["schemas"]["AppendEventRequest"]["properties"]["new_field"] =
        serde_json::json!({ "type": "string" });

    let missing = missing_from_live(&baseline, &live);
    assert!(
        missing.is_empty(),
        "additions reported as losses: {missing:?}"
    );
}

#[test]
fn compatibility_page_has_the_four_guarantee_sections() {
    let page = manifest_doc("docs/operations/compatibility.md");
    for needle in [
        "## API",
        "## Data directory and WAL",
        "## Raft peer protocol",
        "## CRD",
        "openapi.baseline.json",
        "deny_unknown_fields",
        "RollingUpdate",
        "v1alpha1",
    ] {
        assert!(
            page.contains(needle),
            "docs/operations/compatibility.md is missing `{needle}`"
        );
    }
}

#[test]
fn status_and_indexes_point_at_the_compatibility_page() {
    let status = manifest_doc("STATUS.md");
    assert!(
        status.contains("version-compatibility"),
        "STATUS.md has no version-compatibility row"
    );
    assert!(
        status.contains("docs/operations/compatibility.md"),
        "STATUS.md does not link docs/operations/compatibility.md"
    );

    let index = manifest_doc("docs/operations/README.md");
    assert!(
        index.contains("compatibility.md"),
        "docs/operations/README.md does not list compatibility.md"
    );
}

#[test]
fn compatibility_page_links_resolve() {
    let page = manifest_doc("docs/operations/compatibility.md");
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
            "link `{target}` in docs/operations/compatibility.md does not resolve to {}",
            resolved.display()
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "docs/operations/compatibility.md has no relative links to check"
    );
}
