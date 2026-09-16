//! Mechanical dependency contract for Tape's Lumen 0.6 shared-service profile.
//!
//! This is deliberately a manifest contract, not a test of Cargo's resolved
//! dependency graph.  A transitive dependency is not a reason for Tape to add
//! the same crate as a direct dependency.  The small parser below is enough for
//! the Cargo manifest forms used by this contract and keeps the check free of a
//! new production dependency.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

const REQUIRED_RUNTIME: &[&str] = &[
    "service-http",
    "service-auth",
    "cli-std",
    "openapi-codegen",
    "metrics-prometheus",
    "raft-runtime",
    "service-backup",
    "peer-tls",
    "service-k8s",
    "storage-durable",
];

const FORBIDDEN_RUNTIME: &[&str] = &[
    "server-http",
    "server-lifecycle",
    "raft-core",
    "service-observability",
    "index-text",
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Manifest {
    sections: BTreeMap<String, BTreeMap<String, String>>,
}

impl Manifest {
    fn value(&self, section: &str, key: &str) -> Option<&str> {
        self.sections
            .get(section)
            .and_then(|values| values.get(key))
            .map(String::as_str)
    }

    fn dependency_section(&self, section: &str) -> BTreeSet<String> {
        self.sections
            .get(section)
            .into_iter()
            .flat_map(|values| values.keys())
            .map(|key| normalize_name(key))
            .collect()
    }
}

fn normalize_name(name: &str) -> String {
    name.trim().trim_matches('"').replace('_', "-")
}

/// Parse the flat key/value portions of a Cargo manifest.
///
/// Dependency specifications are retained as text because this check only
/// needs to inspect `optional = true` and feature references.  Array tables
/// such as `[[test]]` are accepted as independent sections and do not affect
/// the dependency checks.
fn parse_manifest(input: &str) -> Result<Manifest, String> {
    let mut manifest = Manifest::default();
    let mut section = String::new();

    for (line_number, raw_line) in input.lines().enumerate() {
        let line = strip_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }

        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_string();
            manifest.sections.entry(section.clone()).or_default();
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("line {} is not a Cargo key/value", line_number + 1));
        };
        let key = key.trim();
        if key.is_empty() || section.is_empty() {
            return Err(format!("line {} has no section or key", line_number + 1));
        }
        manifest
            .sections
            .entry(section.clone())
            .or_default()
            .insert(key.to_string(), value.trim().to_string());
    }

    Ok(manifest)
}

fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    let mut escaped = false;
    for (index, byte) in line.bytes().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match byte {
            b'\\' if quoted => escaped = true,
            b'"' => quoted = !quoted,
            b'#' if !quoted => return &line[..index],
            _ => {}
        }
    }
    line
}

fn inline_field<'a>(spec: &'a str, field: &str) -> Option<&'a str> {
    let table = spec.trim().strip_prefix('{')?.strip_suffix('}')?;
    split_top_level(table, ',').into_iter().find_map(|item| {
        let (key, value) = item.split_once('=')?;
        (key.trim() == field).then_some(value.trim())
    })
}

fn is_true(spec: &str, field: &str) -> bool {
    inline_field(spec, field) == Some("true")
}

fn split_top_level(input: &str, separator: char) -> Vec<&str> {
    let mut result = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    let mut depth = 0usize;

    for (index, character) in input.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '[' | '{' if !quoted => depth += 1,
            ']' | '}' if !quoted => depth = depth.saturating_sub(1),
            _ if character == separator && !quoted && depth == 0 => {
                result.push(input[start..index].trim());
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    result.push(input[start..].trim());
    result
}

fn feature_references(manifest: &Manifest, dependency: &str) -> BTreeSet<String> {
    let needle = format!("dep:{dependency}");
    manifest
        .sections
        .get("features")
        .into_iter()
        .flat_map(|features| features.iter())
        .filter(|(_, value)| value.contains(&needle))
        .map(|(feature, _)| feature.clone())
        .collect()
}

fn validate_manifest(manifest_text: &str, e2e_dir: &Path) -> Result<(), String> {
    let manifest = parse_manifest(manifest_text)?;
    let runtime = manifest.dependency_section("dependencies");
    let build = manifest.dependency_section("build-dependencies");
    let dev = manifest.dependency_section("dev-dependencies");

    for dependency in REQUIRED_RUNTIME {
        if !runtime.contains(*dependency) {
            return Err(format!("missing direct runtime dependency `{dependency}`"));
        }
    }

    if !build.contains("build-stamp") {
        return Err("`build-stamp` must be a direct build dependency".into());
    }

    for dependency in FORBIDDEN_RUNTIME {
        if runtime.contains(*dependency) {
            return Err(format!(
                "forbidden direct runtime dependency `{dependency}`"
            ));
        }
    }

    let service_k8s = manifest
        .value("dependencies", "service-k8s")
        .ok_or("missing raw `service-k8s` dependency")?;
    if is_true(service_k8s, "optional") && feature_references(&manifest, "service-k8s").is_empty() {
        return Err("optional `service-k8s` is not enabled by any feature".into());
    }

    let async_nats = manifest
        .value("dependencies", "async-nats")
        .ok_or("missing benchmark dependency `async-nats`")?;
    if !is_true(async_nats, "optional") {
        return Err("`async-nats` must be optional".into());
    }
    if !dev.is_disjoint(&BTreeSet::from(["async-nats".to_string()])) {
        return Err("`async-nats` must not be a dev dependency".into());
    }
    let async_features = feature_references(&manifest, "async-nats");
    if async_features != BTreeSet::from(["jetstream-benchmark".to_string()]) {
        return Err(format!(
            "`async-nats` must be referenced only by `jetstream-benchmark`, found {async_features:?}"
        ));
    }

    if runtime.contains("transport-h2c") {
        return Err("`transport-h2c` may only be a dev dependency when tests use it".into());
    }
    if dev.contains("transport-h2c") && !e2e_uses_transport_h2c(e2e_dir)? {
        return Err("dev `transport-h2c` has no direct e2e use".into());
    }

    Ok(())
}

fn e2e_uses_transport_h2c(e2e_dir: &Path) -> Result<bool, String> {
    let contract_name = Path::new(file!())
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("tape_lumen_profile_contract.rs");
    let entries = fs::read_dir(e2e_dir).map_err(|error| format!("read e2e directory: {error}"))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("read e2e entry: {error}"))?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("rs")
            || path.file_name().and_then(|name| name.to_str()) == Some(contract_name)
        {
            continue;
        }
        let source = fs::read_to_string(&path)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        if source.contains("transport_h2c::") || source.contains("use transport_h2c") {
            return Ok(true);
        }
    }
    Ok(false)
}

#[test]
fn tape_declares_lumen_06_profile_without_lumen_only_direct_dependencies() {
    let manifest = include_str!("../Cargo.toml");
    let e2e_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("e2e");
    validate_manifest(manifest, &e2e_dir).unwrap_or_else(|error| panic!("profile drift: {error}"));
}

#[test]
fn missing_required_dependency_is_rejected() {
    let manifest = minimal_manifest("service-http = \"1\"");
    let error =
        validate_manifest(&manifest, Path::new(".")).expect_err("a partial profile must not pass");
    assert!(error.contains("service-auth"), "unexpected error: {error}");
}

#[test]
fn forbidden_lumen_only_dependency_is_rejected() {
    let manifest = valid_fixture("server-http = \"1\"\n");
    let error = validate_manifest(&manifest, Path::new("."))
        .expect_err("Lumen-only direct dependencies must fail");
    assert!(error.contains("server-http"), "unexpected error: {error}");
}

#[test]
fn benchmark_dependency_must_be_optional_and_feature_scoped() {
    let manifest = valid_fixture("async-nats = \"1\"\n");
    let error = validate_manifest(&manifest, Path::new("."))
        .expect_err("serving dependency must not carry async-nats");
    assert!(error.contains("optional"), "unexpected error: {error}");

    let manifest = valid_fixture(
        "async-nats = { version = \"1\", optional = true }\n\n[features]\njetstream-benchmark = []\nother = [\"dep:async-nats\"]\n",
    );
    let error = validate_manifest(&manifest, Path::new("."))
        .expect_err("benchmark adapter must be isolated to its feature");
    assert!(
        error.contains("jetstream-benchmark"),
        "unexpected error: {error}"
    );
}

#[test]
fn h2c_is_rejected_as_a_serving_dependency() {
    let manifest = valid_fixture("transport-h2c = \"1\"\n");
    let error = validate_manifest(&manifest, Path::new("."))
        .expect_err("h2c must not become a direct serving dependency");
    assert!(error.contains("transport-h2c"), "unexpected error: {error}");
}

fn minimal_manifest(extra_runtime: &str) -> String {
    format!(
        "[dependencies]\n{extra_runtime}\n\n[build-dependencies]\nbuild-stamp = \"1\"\n\n[features]\njetstream-benchmark = [\"dep:async-nats\"]\n"
    )
}

fn valid_fixture(extra_runtime: &str) -> String {
    format!(
        "[dependencies]\nservice-http = \"1\"\nservice-auth = \"1\"\ncli-std = \"1\"\nopenapi-codegen = \"1\"\nmetrics-prometheus = \"1\"\nraft-runtime = \"1\"\nservice-backup = \"1\"\npeer-tls = \"1\"\nservice-k8s = {{ version = \"1\", optional = true }}\nstorage-durable = \"1\"\nasync-nats = {{ version = \"1\", optional = true }}\n{extra_runtime}\n\n[dev-dependencies]\ntransport-h2c = \"1\"\n\n[build-dependencies]\nbuild-stamp = \"1\"\n\n[features]\noperator = [\"dep:service-k8s\"]\njetstream-benchmark = [\"dep:async-nats\"]\n"
    )
}
