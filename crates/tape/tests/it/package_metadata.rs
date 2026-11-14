//! The workspace package metadata carries no personal contact details.

const ROOT_MANIFEST: &str = include_str!("../../../../Cargo.toml");

#[test]
fn workspace_authors_are_faberline_contributors_without_an_email_address() {
    let authors: Vec<&str> = ROOT_MANIFEST
        .lines()
        .filter(|line| line.trim_start().starts_with("authors"))
        .collect();
    assert_eq!(
        authors,
        vec![r#"authors = ["Faberline contributors"]"#],
        "the root manifest must have exactly one authors line"
    );
    assert!(
        !authors[0].contains('@'),
        "authors must not hold an email address"
    );
}
