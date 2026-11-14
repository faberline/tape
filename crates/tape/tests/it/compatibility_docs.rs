//! Version compatibility gate (#74).
//!
//! `docs/operations/compatibility.md` states the rules that hold between two
//! adjacent tape releases. `clients/openapi.baseline.json` is the floor of the
//! HTTP contract: the live `clients/openapi.json` may add routes, methods and
//! fields, but must not drop what the baseline promised. These tests pin the
//! page's required sections, the baseline check, and the links to the page.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

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

#[test]
fn stability_promise_page_names_the_five_surfaces() {
    let page = manifest_doc("docs/operations/stability-promise.md");
    for needle in [
        "## API",
        "## Wire format",
        "## On-disk format",
        "## CRD",
        "## CLI flags",
        "2.0.0",
        "v1alpha1",
        "openapi.baseline.json",
        "## What is not promised",
    ] {
        assert!(
            page.contains(needle),
            "docs/operations/stability-promise.md is missing `{needle}`"
        );
    }
}

#[test]
fn stability_promise_is_linked_from_the_index_and_compatibility_page() {
    let index = manifest_doc("docs/operations/README.md");
    assert!(
        index.contains("stability-promise.md"),
        "docs/operations/README.md does not link stability-promise.md"
    );

    let page = manifest_doc("docs/operations/compatibility.md");
    assert!(
        page.contains("](stability-promise.md)"),
        "docs/operations/compatibility.md does not link stability-promise.md"
    );
    assert!(
        !page.contains("Tape is pre-1.0: a minor release may change behaviour, but each rule"),
        "docs/operations/compatibility.md still says Tape is pre-1.0 without pointing to the stability promise"
    );
}

/// Surface name -> sorted items. Used by the WAL, CRD and CLI baselines (#101).
type Surface = BTreeMap<String, Vec<String>>;

fn tape_bin() -> &'static str {
    env!("CARGO_BIN_EXE_tape")
}

/// One string per key or item that `baseline` has and `live` lacks. A key
/// missing from `live` is reported alone; additions in `live` are not reported.
fn missing_from_live_map(baseline: &Surface, live: &Surface) -> Vec<String> {
    let mut missing = Vec::new();
    for (key, items) in baseline {
        match live.get(key) {
            None => missing.push(key.clone()),
            Some(live_items) => {
                for item in items {
                    if !live_items.contains(item) {
                        missing.push(format!("{key}: {item}"));
                    }
                }
            }
        }
    }
    missing.sort();
    missing
}

/// Compare `live` with the committed baseline at `baseline_rel`. With
/// `COMPAT_BLESS` set, write `live` to that file instead (never set in CI).
fn check_surface(name: &str, baseline_rel: &str, live: Surface) {
    if std::env::var_os("COMPAT_BLESS").is_some() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(baseline_rel);
        std::fs::create_dir_all(path.parent().expect("baseline parent directory"))
            .expect("create compat directory");
        let mut text = serde_json::to_string_pretty(&live).expect("serialize baseline");
        text.push('\n');
        std::fs::write(&path, text).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        return;
    }
    let baseline: Surface = serde_json::from_str(&manifest_doc(baseline_rel))
        .unwrap_or_else(|e| panic!("parse {baseline_rel}: {e}"));
    let missing = missing_from_live_map(&baseline, &live);
    assert!(
        missing.is_empty(),
        "{name} drops {} thing(s) the baseline promises: {missing:?}",
        missing.len()
    );
}

fn load_baseline(baseline_rel: &str) -> Surface {
    serde_json::from_str(&manifest_doc(baseline_rel))
        .unwrap_or_else(|e| panic!("parse {baseline_rel}: {e}"))
}

/// `TapeCommand` variants and their field names in `command.rs`.
fn live_wal_variants() -> Surface {
    let src = manifest_doc("crates/tape-shared-kernel/src/command.rs");
    let mut out = Surface::new();
    let mut current: Option<String> = None;
    let mut in_enum = false;
    for line in src.lines() {
        if !in_enum {
            in_enum = line == "pub enum TapeCommand {";
            continue;
        }
        if line == "}" {
            break;
        }
        let rest = line.trim_start_matches(' ');
        let indent = line.len() - rest.len();
        let first = rest.chars().next();
        if indent == 4 && first.is_some_and(|c| c.is_ascii_uppercase()) {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            out.entry(name.clone()).or_default();
            current = Some(name);
        } else if indent == 8 && first.is_some_and(|c| c.is_ascii_lowercase() || c == '_') {
            if let (Some(variant), Some(field)) = (&current, rest.split(':').next()) {
                out.entry(variant.clone())
                    .or_default()
                    .push(field.trim().to_string());
            }
        }
    }
    for fields in out.values_mut() {
        fields.sort();
    }
    out
}

#[test]
fn live_wal_variants_reads_the_enum() {
    let live = live_wal_variants();
    let append = live.get("Append").expect("Append variant");
    assert!(append.iter().any(|f| f == "topic"));
    assert!(append.iter().any(|f| f == "timestamp_ms"));
}

#[test]
fn missing_from_live_map_accepts_additions() {
    let baseline: Surface = BTreeMap::from([("a".to_string(), vec!["x".to_string()])]);
    let live: Surface = BTreeMap::from([
        ("a".to_string(), vec!["x".to_string(), "y".to_string()]),
        ("b".to_string(), vec!["z".to_string()]),
    ]);
    assert!(missing_from_live_map(&baseline, &live).is_empty());
}

/// Dotted paths of every property below `schema`; array items add `[]`.
fn walk(schema: &serde_json::Value, prefix: &str, out: &mut Vec<String>) {
    let Some(props) = schema["properties"].as_object() else {
        return;
    };
    for (name, child) in props {
        out.push(format!("{prefix}{name}"));
        walk(child, &format!("{prefix}{name}."), out);
        if child.get("items").is_some() {
            walk(&child["items"], &format!("{prefix}{name}[]."), out);
        }
    }
}

/// `spec` and `status` field paths of the `v1alpha1` schema in the CRD.
fn live_crd_fields() -> Surface {
    let crd: serde_json::Value =
        serde_yaml::from_str(&manifest_doc("k8s/operator/crd.yaml")).expect("parse crd.yaml");
    let version = crd["spec"]["versions"]
        .as_array()
        .expect("versions array")
        .iter()
        .find(|v| v["name"] == "v1alpha1")
        .expect("v1alpha1 version");
    let mut out = Surface::new();
    for key in ["spec", "status"] {
        let schema = &version["schema"]["openAPIV3Schema"]["properties"][key];
        let mut fields = Vec::new();
        walk(schema, "", &mut fields);
        fields.sort();
        out.insert(key.to_string(), fields);
    }
    out
}

fn cli_help(path: &[String]) -> String {
    let output = Command::new(tape_bin())
        .args(path)
        .arg("--help")
        .output()
        .expect("run tape --help");
    if !output.status.success() {
        panic!(
            "`tape {} --help` failed: {}",
            path.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout).expect("utf-8 help text")
}

/// A flag line in a help text: `--bind <BIND>`, `-h, --help`.
fn is_flag_line(trimmed: &str) -> bool {
    let after_short = match trimmed.as_bytes() {
        [b'-', c, b',', b' ', ..] if c.is_ascii_alphabetic() => &trimmed[4..],
        _ => trimmed,
    };
    after_short.starts_with("--")
        && after_short[2..]
            .chars()
            .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
            .count()
            >= 1
}

/// Visit `path` and its subcommands depth first, one `--help` run each.
fn visit_cli(path: &[String], out: &mut Surface) {
    let help = cli_help(path);
    let key = if path.is_empty() {
        "tape".to_string()
    } else {
        format!("tape {}", path.join(" "))
    };
    let mut items = Vec::new();
    let mut subcommands = Vec::new();
    let mut in_commands = false;
    for line in help.lines() {
        if line == "Commands:" {
            in_commands = true;
            continue;
        }
        let rest = line.trim_start_matches(' ');
        let indent = line.len() - rest.len();
        if in_commands {
            if line.is_empty() {
                in_commands = false;
            } else if indent == 2 && rest.starts_with(|c: char| c.is_ascii_lowercase()) {
                let name = rest.split_whitespace().next().unwrap_or_default();
                if name != "help" {
                    subcommands.push(name.to_string());
                }
            }
            continue;
        }
        if (indent == 2 || indent == 6) && is_flag_line(rest) {
            for token in rest.split_whitespace() {
                if token.starts_with('<') || token.starts_with('[') {
                    break;
                }
                let token = token.trim_end_matches(',');
                if token.starts_with("--") {
                    items.push(token.to_string());
                }
            }
        }
    }
    items.retain(|f| f != "--help" && f != "--version");
    for name in &subcommands {
        items.push(format!("cmd:{name}"));
    }
    items.sort();
    items.dedup();
    out.insert(key, items);
    for name in subcommands {
        let mut child = path.to_vec();
        child.push(name);
        visit_cli(&child, out);
    }
}

/// Long flags and subcommands of every `tape` command, from `--help` output.
fn live_cli_flags() -> Surface {
    let mut out = Surface::new();
    visit_cli(&[], &mut out);
    out
}

#[test]
fn wal_variants_keep_every_baseline_variant_and_field() {
    check_surface(
        "TapeCommand",
        "compat/wal-variants.baseline.json",
        live_wal_variants(),
    );
}

#[test]
fn crd_keeps_every_baseline_field() {
    check_surface(
        "crd.yaml",
        "compat/crd-fields.baseline.json",
        live_crd_fields(),
    );
}

#[test]
fn cli_keeps_every_baseline_flag_and_command() {
    check_surface(
        "tape --help",
        "compat/cli-flags.baseline.json",
        live_cli_flags(),
    );
}

#[test]
fn wal_check_detects_a_removed_variant_and_field() {
    let baseline = load_baseline("compat/wal-variants.baseline.json");
    let mut live = baseline.clone();
    live.remove("SubscriptionAck");
    live.get_mut("Append")
        .expect("Append variant")
        .retain(|f| f != "payload");
    assert_eq!(
        missing_from_live_map(&baseline, &live),
        vec!["Append: payload".to_string(), "SubscriptionAck".to_string()]
    );
}

#[test]
fn crd_check_detects_a_removed_field() {
    let baseline = load_baseline("compat/crd-fields.baseline.json");
    let first = baseline
        .get("spec")
        .and_then(|fields| fields.first())
        .cloned()
        .expect("spec fields are not empty");
    let mut live = baseline.clone();
    live.get_mut("spec")
        .expect("spec key")
        .retain(|f| *f != first);
    assert_eq!(
        missing_from_live_map(&baseline, &live),
        vec![format!("spec: {first}")]
    );
}

#[test]
fn cli_check_detects_a_removed_flag_and_command() {
    let baseline = load_baseline("compat/cli-flags.baseline.json");
    let mut live = baseline.clone();
    live.get_mut("tape serve")
        .expect("tape serve key")
        .retain(|f| f != "--bind");
    live.remove("tape backup");
    assert_eq!(
        missing_from_live_map(&baseline, &live),
        vec!["tape backup".to_string(), "tape serve: --bind".to_string()]
    );
}
