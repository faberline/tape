//! Threat model doc gate (#113).
//!
//! `docs/security/threat-model.md` names tape's trust boundaries and, for each
//! one, the six STRIDE threats with the control in place and the known gap.
//! These tests keep the page in step with its required shape: every boundary
//! section names all six STRIDE categories, the page is linked from the docs
//! index, and the known gaps stay listed.

use std::path::PathBuf;

const PAGE: &str = "docs/security/threat-model.md";

const BOUNDARIES: [&str; 4] = [
    "## Boundary: public API",
    "## Boundary: peer raft port",
    "## Boundary: operator and Kubernetes API",
    "## Boundary: backup bucket",
];

const STRIDE: [&str; 6] = [
    "Spoofing",
    "Tampering",
    "Repudiation",
    "Information disclosure",
    "Denial of service",
    "Elevation of privilege",
];

/// Read `rel` from the repository root (two levels above `crates/tape`).
fn manifest_doc(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The text of the section that starts at `heading`, up to the next `## `
/// heading (or the end of the page).
fn section<'a>(page: &'a str, heading: &str) -> &'a str {
    let Some(start) = page.find(heading) else {
        return "";
    };
    let body = &page[start + heading.len()..];
    match body.find("\n## ") {
        Some(end) => &body[..end],
        None => body,
    }
}

#[test]
fn threat_model_names_every_boundary_and_stride_category() {
    let page = manifest_doc(PAGE);
    for boundary in BOUNDARIES {
        assert!(
            page.contains(boundary),
            "{PAGE} has no heading `{boundary}`"
        );
        let text = section(&page, boundary);
        for word in STRIDE {
            assert!(
                text.contains(word),
                "section `{boundary}` of {PAGE} does not name `{word}`"
            );
        }
    }
}

#[test]
fn threat_model_is_linked_from_the_docs_index() {
    let index = manifest_doc("docs/README.md");
    assert!(
        index.contains("security/threat-model.md"),
        "docs/README.md does not link security/threat-model.md"
    );
}

#[test]
fn threat_model_lists_the_known_gaps() {
    let page = manifest_doc(PAGE).to_lowercase();
    let gaps = section(&page, "## known gaps");
    for needle in ["no tls", "rotation", "audited"] {
        assert!(
            gaps.contains(needle),
            "the `## Known gaps` section of {PAGE} does not mention `{needle}`"
        );
    }
}
