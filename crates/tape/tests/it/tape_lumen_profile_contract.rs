//! Mechanical dependency contract for Tape's Lumen 0.6 shared-service profile.
//!
//! This is deliberately a manifest contract, not a test of Cargo's resolved
//! dependency graph.  A transitive dependency is not a reason for Tape to add
//! the same crate as a direct dependency.  The small parser below is enough for
//! the Cargo manifest forms used by this contract and keeps the check free of a
//! new production dependency.
//!
//! The serving profile is the union of every workspace crate except
//! `tape-bench`.  The JetStream benchmark adapter (`async-nats`) belongs only to
//! `tape-bench`, behind its optional `jetstream-benchmark` feature.

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

// `server-lifecycle` is not listed: Tape uses it on purpose for graceful
// shutdown (tape 0.6.0).
const FORBIDDEN_RUNTIME: &[&str] = &[
    "server-http",
    "raft-core",
    "service-observability",
    "index-text",
];

const BENCH_CRATE: &str = "tape-bench";

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
    let name = name.trim().trim_matches('"').replace('_', "-");
    // `foo.workspace = true` declares the dependency `foo`.
    match name.strip_suffix(".workspace") {
        Some(stripped) => stripped.to_string(),
        None => name,
    }
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
            .insert(normalize_name(key), value.trim().to_string());
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

/// Validate the serving crates' union profile and the benchmark crate's
/// isolation of `async-nats`.
///
/// `serving` holds `(crate name, manifest text)` for every serving crate;
/// `bench` is the `tape-bench` manifest text.
fn validate_workspace(
    serving: &[(&str, &str)],
    bench: &str,
    tests_dir: &Path,
) -> Result<(), String> {
    let mut parsed = Vec::new();
    for (name, text) in serving {
        let manifest = parse_manifest(text).map_err(|error| format!("{name}: {error}"))?;
        parsed.push((*name, manifest));
    }

    let mut runtime = BTreeSet::new();
    let mut build = BTreeSet::new();
    for (_, manifest) in &parsed {
        runtime.extend(manifest.dependency_section("dependencies"));
        build.extend(manifest.dependency_section("build-dependencies"));
    }

    for dependency in REQUIRED_RUNTIME {
        if !runtime.contains(*dependency) {
            return Err(format!("missing direct runtime dependency `{dependency}`"));
        }
    }

    if !build.contains("build-stamp") {
        return Err("`build-stamp` must be a direct build dependency".into());
    }

    for (name, manifest) in &parsed {
        let crate_runtime = manifest.dependency_section("dependencies");
        let crate_dev = manifest.dependency_section("dev-dependencies");

        for dependency in FORBIDDEN_RUNTIME {
            if crate_runtime.contains(*dependency) {
                return Err(format!(
                    "{name}: forbidden direct runtime dependency `{dependency}`"
                ));
            }
        }

        if let Some(service_k8s) = manifest.value("dependencies", "service-k8s") {
            if is_true(service_k8s, "optional")
                && feature_references(manifest, "service-k8s").is_empty()
            {
                return Err(format!(
                    "{name}: optional `service-k8s` is not enabled by any feature"
                ));
            }
        }

        if crate_runtime.contains("async-nats") || crate_dev.contains("async-nats") {
            return Err(format!(
                "{name}: `async-nats` must stay out of the serving crates"
            ));
        }

        if crate_runtime.contains("transport-h2c") {
            return Err(format!(
                "{name}: `transport-h2c` may only be a dev dependency when tests use it"
            ));
        }
        if crate_dev.contains("transport-h2c") && !tests_use_transport_h2c(tests_dir)? {
            return Err(format!(
                "{name}: dev `transport-h2c` has no direct test use"
            ));
        }
    }

    validate_bench(bench)
}

fn validate_bench(bench: &str) -> Result<(), String> {
    let manifest = parse_manifest(bench).map_err(|error| format!("{BENCH_CRATE}: {error}"))?;
    let async_nats = manifest
        .value("dependencies", "async-nats")
        .ok_or("missing benchmark dependency `async-nats`")?;
    if !is_true(async_nats, "optional") {
        return Err("`async-nats` must be optional".into());
    }
    if manifest
        .dependency_section("dev-dependencies")
        .contains("async-nats")
    {
        return Err("`async-nats` must not be a dev dependency".into());
    }
    let async_features = feature_references(&manifest, "async-nats");
    if async_features != BTreeSet::from(["jetstream-benchmark".to_string()]) {
        return Err(format!(
            "`async-nats` must be referenced only by `jetstream-benchmark`, found {async_features:?}"
        ));
    }
    Ok(())
}

fn tests_use_transport_h2c(tests_dir: &Path) -> Result<bool, String> {
    let contract_name = Path::new(file!())
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("tape_lumen_profile_contract.rs");
    let entries =
        fs::read_dir(tests_dir).map_err(|error| format!("read tests directory: {error}"))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("read tests entry: {error}"))?;
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

/// The integration-test directory scanned for direct `transport-h2c` use.
fn tests_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/it")
}

/// Read every workspace crate manifest as `(crate directory name, text)`.
fn workspace_manifests() -> Vec<(String, String)> {
    let crates_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tape manifest must be under crates/")
        .to_path_buf();
    let mut manifests = Vec::new();
    for entry in fs::read_dir(&crates_dir).expect("read crates directory") {
        let path = entry.expect("read crates entry").path().join("Cargo.toml");
        if !path.is_file() {
            continue;
        }
        let name = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .expect("crate directory name is UTF-8")
            .to_string();
        let text = fs::read_to_string(&path).expect("read crate manifest");
        manifests.push((name, text));
    }
    manifests.sort();
    manifests
}

#[test]
fn tape_declares_lumen_06_profile_without_lumen_only_direct_dependencies() {
    let manifests = workspace_manifests();
    let bench = manifests
        .iter()
        .find(|(name, _)| name == BENCH_CRATE)
        .map(|(_, text)| text.as_str())
        .expect("tape-bench crate exists");
    let serving: Vec<(&str, &str)> = manifests
        .iter()
        .filter(|(name, _)| name != BENCH_CRATE)
        .map(|(name, text)| (name.as_str(), text.as_str()))
        .collect();
    assert!(
        serving.iter().any(|(name, _)| *name == "tape"),
        "the tape serving crate must be part of the profile"
    );
    let tests_dir = tests_dir();
    validate_workspace(&serving, bench, &tests_dir)
        .unwrap_or_else(|error| panic!("profile drift: {error}"));
}

#[test]
fn missing_required_dependency_is_rejected() {
    let manifest = minimal_manifest("service-http = \"1\"");
    let error = validate_workspace(&[("tape", &manifest)], BENCH_FIXTURE, &tests_dir())
        .expect_err("a partial profile must not pass");
    assert!(error.contains("service-auth"), "unexpected error: {error}");
}

#[test]
fn forbidden_lumen_only_dependency_is_rejected() {
    let manifest = valid_fixture("server-http = \"1\"\n");
    let error = validate_workspace(&[("tape", &manifest)], BENCH_FIXTURE, &tests_dir())
        .expect_err("Lumen-only direct dependencies must fail");
    assert!(error.contains("server-http"), "unexpected error: {error}");
}

#[test]
fn server_lifecycle_is_allowed_for_graceful_shutdown() {
    let manifest = valid_fixture("server-lifecycle = \"1\"\n");
    validate_workspace(&[("tape", &manifest)], BENCH_FIXTURE, &tests_dir())
        .expect("server-lifecycle backs graceful shutdown");
}

#[test]
fn profile_is_the_union_of_serving_crates() {
    let tape = minimal_manifest("service-http = \"1\"");
    let journal = "[dependencies]\nservice-auth = \"1\"\ncli-std = \"1\"\nopenapi-codegen = \"1\"\nmetrics-prometheus = \"1\"\nraft-runtime = \"1\"\nservice-backup = \"1\"\npeer-tls = \"1\"\nservice-k8s = \"1\"\nstorage-durable = \"1\"\n";
    validate_workspace(
        &[("tape", &tape), ("tape-journal", journal)],
        BENCH_FIXTURE,
        &tests_dir(),
    )
    .expect("required dependencies may live in any serving crate");
}

#[test]
fn benchmark_dependency_must_stay_out_of_serving_crates() {
    let manifest = valid_fixture("async-nats = { version = \"1\", optional = true }\n");
    let error = validate_workspace(&[("tape", &manifest)], BENCH_FIXTURE, &tests_dir())
        .expect_err("serving crates must not carry async-nats");
    assert!(error.contains("async-nats"), "unexpected error: {error}");
}

#[test]
fn benchmark_dependency_must_be_optional_and_feature_scoped() {
    let serving = valid_fixture("");
    let bench = "[dependencies]\nasync-nats = \"1\"\n\n[features]\njetstream-benchmark = [\"dep:async-nats\"]\n";
    let error = validate_workspace(&[("tape", &serving)], bench, &tests_dir())
        .expect_err("the benchmark adapter must be optional");
    assert!(error.contains("optional"), "unexpected error: {error}");

    let bench = "[dependencies]\nasync-nats = { version = \"1\", optional = true }\n\n[features]\njetstream-benchmark = []\nother = [\"dep:async-nats\"]\n";
    let error = validate_workspace(&[("tape", &serving)], bench, &tests_dir())
        .expect_err("benchmark adapter must be isolated to its feature");
    assert!(
        error.contains("jetstream-benchmark"),
        "unexpected error: {error}"
    );
}

#[test]
fn h2c_is_rejected_as_a_serving_dependency() {
    let manifest = valid_fixture("transport-h2c = \"1\"\n");
    let error = validate_workspace(&[("tape", &manifest)], BENCH_FIXTURE, &tests_dir())
        .expect_err("h2c must not become a direct serving dependency");
    assert!(error.contains("transport-h2c"), "unexpected error: {error}");
}

const BENCH_FIXTURE: &str = "[dependencies]\nasync-nats = { version = \"1\", optional = true }\n\n[features]\njetstream-benchmark = [\"dep:async-nats\"]\n";

fn minimal_manifest(extra_runtime: &str) -> String {
    format!("[dependencies]\n{extra_runtime}\n\n[build-dependencies]\nbuild-stamp = \"1\"\n")
}

fn valid_fixture(extra_runtime: &str) -> String {
    format!(
        "[dependencies]\nservice-http = \"1\"\nservice-auth = \"1\"\ncli-std = \"1\"\nopenapi-codegen = \"1\"\nmetrics-prometheus = \"1\"\nraft-runtime = \"1\"\nservice-backup = \"1\"\npeer-tls = \"1\"\nservice-k8s = {{ version = \"1\", optional = true }}\nstorage-durable = \"1\"\n{extra_runtime}\n\n[dev-dependencies]\ntransport-h2c = \"1\"\n\n[build-dependencies]\nbuild-stamp = \"1\"\n\n[features]\noperator = [\"dep:service-k8s\"]\n"
    )
}
