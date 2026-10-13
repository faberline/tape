use super::*;

#[test]
fn fixed_profile_is_mechanically_valid() {
    assert!(DurableV1Profile::default().validate().is_ok());
}

#[test]
fn profile_rejects_changed_workload() {
    let p = DurableV1Profile {
        samples: 4,
        ..DurableV1Profile::default()
    };
    assert_eq!(
        p.validate().unwrap_err(),
        "durable-v1 requires warmup and five 60-second samples"
    );
}

#[test]
fn image_validation_rejects_mutable_and_non_amd64() {
    assert!(validate_image_digest("nats:latest", "amd64").is_err());
    assert!(validate_image_digest(
        "nats@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "arm64"
    )
    .is_err());
    assert!(validate_image_digest(
        "nats@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "amd64"
    )
    .is_ok());
}
