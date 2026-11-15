//! Limits page drift gate (#115).
//!
//! `docs/operations/limits.md` lists every size and rate limit of Tape with its
//! value and source. These tests tie the page to the source constants, so a
//! changed default makes the test fail and tells the editor to update the page.

use std::path::PathBuf;

/// Read `rel` from the repository root (two levels above `crates/tape`).
fn manifest_doc(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn limits_page_matches_the_body_limit_constant() {
    let serve = manifest_doc("crates/tape/src/bin/tape/serve.rs");
    assert!(
        serve.contains("default_value_t = 8 * 1024 * 1024"),
        "the body limit default changed: update docs/operations/limits.md"
    );

    let page = manifest_doc("docs/operations/limits.md");
    assert!(page.contains("8 MiB"), "limits.md must state 8 MiB");
    assert!(
        page.contains("8388608"),
        "limits.md must state 8388608 bytes"
    );
    assert!(
        page.contains("TAPE_BODY_LIMIT_BYTES"),
        "limits.md must name TAPE_BODY_LIMIT_BYTES"
    );
}

#[test]
fn limits_page_matches_the_pull_batch_constants() {
    let subscription = manifest_doc("crates/tape-shared-kernel/src/subscription.rs");
    assert!(
        subscription.contains("pub const DEFAULT_PULL_BATCH: usize = 100;"),
        "DEFAULT_PULL_BATCH changed: update docs/operations/limits.md"
    );
    assert!(
        subscription.contains("pub const MAX_PULL_BATCH: usize = 1_000;"),
        "MAX_PULL_BATCH changed: update docs/operations/limits.md"
    );

    let page = manifest_doc("docs/operations/limits.md");
    for needle in ["DEFAULT_PULL_BATCH", "MAX_PULL_BATCH", "1000", "100"] {
        assert!(page.contains(needle), "limits.md must contain {needle}");
    }
}

#[test]
fn limits_page_is_linked_and_names_what_is_not_supported() {
    let readme = manifest_doc("docs/operations/README.md");
    assert!(
        readme.contains("(limits.md)"),
        "docs/operations/README.md must link limits.md"
    );

    let page = manifest_doc("docs/operations/limits.md");
    assert!(
        page.contains("## Not supported"),
        "limits.md must have a `## Not supported` section"
    );
    assert!(
        page.to_lowercase().contains("per-topic quota"),
        "limits.md must name the per-topic quota as not supported"
    );
    for link in ["crd-capacity-and-quotas.md", "environment-and-http.md"] {
        assert!(page.contains(link), "limits.md must link {link}");
    }
}
