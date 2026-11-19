//! Quickstart doc drift gate (#73).
//!
//! `docs/quickstart.md` takes a new user from an install to a published and
//! pulled message, then to a 3-replica group. Every `tape` subcommand and flag
//! it names must exist in the CLI help, every route in its `curl` calls must
//! exist in `clients/openapi.json`, and its links must resolve. These tests keep
//! the page true to the binary and the contract.

use std::collections::BTreeSet;
use std::path::PathBuf;

fn tape_bin() -> &'static str {
    env!("CARGO_BIN_EXE_tape")
}

/// Read `rel` from the repository root (two levels above `crates/tape`).
fn manifest_doc(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Logical lines of every fenced ```bash block. A line that ends with `\` is
/// joined with the next line.
fn bash_blocks(page: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut in_block = false;
    let mut pending = String::new();
    for raw in page.lines() {
        let trimmed = raw.trim();
        if !in_block {
            if trimmed == "```bash" {
                in_block = true;
            }
            continue;
        }
        if trimmed == "```" {
            in_block = false;
            if !pending.is_empty() {
                lines.push(std::mem::take(&mut pending));
            }
            continue;
        }
        if let Some(head) = trimmed.strip_suffix('\\') {
            pending.push_str(head.trim_end());
            pending.push(' ');
            continue;
        }
        pending.push_str(trimmed);
        lines.push(std::mem::take(&mut pending));
    }
    lines
}

/// The `tape` command segments of a line: each `|`-separated part whose first
/// word is `tape`. Yields the command path (words that are all `[a-z]+`) and
/// the remaining words of that segment.
fn tape_segments(line: &str) -> Vec<(Vec<String>, Vec<String>)> {
    line.split('|')
        .filter_map(|segment| {
            let words: Vec<&str> = segment.split_whitespace().collect();
            if words.first() != Some(&"tape") {
                return None;
            }
            let rest = &words[1..];
            let path: Vec<String> = rest
                .iter()
                .take_while(|w| {
                    w.starts_with(|c: char| c.is_ascii_lowercase())
                        && w.chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                })
                .map(|w| w.to_string())
                .collect();
            let all: Vec<String> = rest.iter().map(|w| w.to_string()).collect();
            Some((path, all))
        })
        .collect()
}

fn help_output(path: &[String]) -> (bool, String) {
    let output = std::process::Command::new(tape_bin())
        .args(path)
        .arg("--help")
        .output()
        .unwrap_or_else(|e| panic!("run tape {path:?} --help: {e}"));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), text)
}

#[test]
fn quickstart_tape_subcommands_exist_in_the_cli() {
    let page = manifest_doc("docs/quickstart.md");
    let mut checked = BTreeSet::new();
    for line in bash_blocks(&page) {
        for (path, _) in tape_segments(&line) {
            let (ok, text) = help_output(&path);
            assert!(
                ok,
                "docs/quickstart.md names `tape {}`, which `tape --help` does not list:\n{text}",
                path.join(" ")
            );
            checked.insert(path);
        }
    }
    let expected: Vec<Vec<String>> = [
        vec!["serve"],
        vec!["k8s", "crd", "render"],
        vec!["k8s", "operator", "render"],
        vec!["k8s", "instance", "render"],
        vec!["connect"],
    ]
    .into_iter()
    .map(|p| p.into_iter().map(String::from).collect())
    .collect();
    for path in expected {
        assert!(
            checked.contains(&path),
            "docs/quickstart.md no longer checks `tape {}`",
            path.join(" ")
        );
    }
}

#[test]
fn quickstart_flags_exist_in_the_help_text() {
    let page = manifest_doc("docs/quickstart.md");
    for line in bash_blocks(&page) {
        for (path, words) in tape_segments(&line) {
            let (_, text) = help_output(&path);
            for word in words
                .iter()
                .filter(|w| w.starts_with("--") && w.as_str() != "--")
            {
                let flag = word.split('=').next().unwrap_or(word);
                assert!(
                    text.contains(flag),
                    "docs/quickstart.md uses `{flag}` with `tape {}`, which its --help does not list",
                    path.join(" ")
                );
            }
        }
    }
}

#[test]
fn quickstart_routes_exist_in_the_openapi_contract() {
    let page = manifest_doc("docs/quickstart.md");
    let spec_text = manifest_doc("clients/openapi.json");
    let spec: serde_json::Value =
        serde_json::from_str(&spec_text).expect("clients/openapi.json is valid JSON");
    let paths = spec["paths"].as_object().expect("openapi has `paths`");
    const HOST: &str = "127.0.0.1:7137";
    for line in bash_blocks(&page) {
        if !line.contains("curl") || !line.contains(HOST) {
            continue;
        }
        let after = line.split(HOST).nth(1).expect("host present");
        let raw_path: String = after
            .chars()
            .take_while(|c| !c.is_whitespace() && *c != '"' && *c != '\'')
            .collect();
        let templated: Vec<String> = raw_path
            .split('/')
            .map(|seg| match seg {
                "orders" => "{topic}".to_string(),
                "billing" => "{subscription}".to_string(),
                other => other.to_string(),
            })
            .collect();
        let route = templated.join("/");
        let method = if line.contains("-X POST") {
            "post"
        } else {
            "get"
        };
        let methods = paths.get(&route).unwrap_or_else(|| {
            panic!("route `{route}` from the quickstart is not in clients/openapi.json")
        });
        assert!(
            methods.get(method).is_some(),
            "route `{route}` has no `{method}` method in clients/openapi.json"
        );
    }
}

#[test]
fn quickstart_is_linked_from_readme_and_docs_index() {
    let readme = manifest_doc("README.md");
    assert!(
        readme.contains("docs/quickstart.md"),
        "README.md does not link docs/quickstart.md"
    );
    let docs_index = manifest_doc("docs/README.md");
    assert!(
        docs_index.contains("quickstart.md"),
        "docs/README.md does not list quickstart.md"
    );
}

#[test]
fn quickstart_links_resolve() {
    let page = manifest_doc("docs/quickstart.md");
    let docs_dir = repo_root().join("docs");
    let mut rest = page.as_str();
    while let Some(start) = rest.find("](") {
        let after = &rest[start + 2..];
        let end = after.find(')').expect("markdown link is closed");
        let target = &after[..end];
        rest = &after[end..];
        if target.contains("://") || target.starts_with('#') {
            continue;
        }
        let file = target.split('#').next().unwrap_or(target);
        let resolved = docs_dir.join(file);
        assert!(
            resolved.exists(),
            "docs/quickstart.md links `{target}`, which does not resolve to {}",
            resolved.display()
        );
    }
}

#[test]
fn quickstart_names_the_real_install_inputs() {
    let page = manifest_doc("docs/quickstart.md");
    for needle in [
        "install.sh",
        "ghcr.io/faberline/tape",
        "replicasPerShard: 3",
        "/raftz",
    ] {
        assert!(
            page.contains(needle),
            "docs/quickstart.md is missing `{needle}`"
        );
    }
    assert!(
        repo_root().join("install.sh").exists(),
        "install.sh is missing"
    );
    let release = manifest_doc(".github/workflows/tape-release.yml");
    assert!(
        release.contains("ghcr.io"),
        ".github/workflows/tape-release.yml no longer publishes to ghcr.io"
    );
}
