const TAPE_SERVE: &str = include_str!("../../src/bin/tape/serve.rs");
const TAPE_MANIFEST: &str = include_str!("../../Cargo.toml");

#[test]
fn tape_maps_optional_otlp_to_the_shared_initializer() {
    assert!(TAPE_SERVE.contains("TAPE_OTLP_ENDPOINT"));
    assert!(TAPE_SERVE.contains("service_http::init_tracing_with_identity"));
    assert!(TAPE_SERVE.contains("ServiceIdentity::new(\"tape\""));
}

#[test]
fn tape_otel_feature_enables_shared_service_http_export() {
    assert!(TAPE_MANIFEST.contains("service-http/otlp"));
    assert!(!TAPE_SERVE.contains("opentelemetry_otlp::new_pipeline"));
}
