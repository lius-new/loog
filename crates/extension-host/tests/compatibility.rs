//! Link-compatibility fixtures (design §7.4, M0-12). Small hand-written
//! components give each mismatch a deterministic, reproducible result.
use loom_extension_api::ErrorKind;
use loom_extension_host::{Limits, Runtime};

fn outcome(wat: &str) -> Result<(), loom_extension_api::ExtensionError> {
    let runtime = Runtime::new().unwrap();
    let component = runtime.compile(wat.as_bytes())?;
    runtime
        .instantiate(&component, Limits::default())
        .map(|_| ())
}

#[test]
fn an_import_the_host_does_not_provide_is_refused_before_instantiation() {
    let error = outcome(r#"(component (import "acme:other/thing@1.0.0" (instance)))"#).unwrap_err();
    assert_eq!(error.kind, ErrorKind::UnsupportedApi, "{error}");
    assert!(error.message.contains("acme:other/thing@1.0.0"));
    assert!(
        error.message.contains("loom:extension/host@0.1.0"),
        "names what is available: {error}"
    );
}

#[test]
fn a_newer_loom_host_interface_is_not_silently_stubbed() {
    let error =
        outcome(r#"(component (import "loom:extension/host@0.2.0" (instance)))"#).unwrap_err();
    assert_eq!(error.kind, ErrorKind::UnsupportedApi, "{error}");
}

#[test]
fn a_component_without_the_guest_export_fails_to_link() {
    let error = outcome("(component)").unwrap_err();
    assert_eq!(error.kind, ErrorKind::ExtensionFailed, "{error}");
}

#[test]
fn a_guest_export_of_another_contract_version_fails_to_link() {
    let error =
        outcome(r#"(component (instance $i) (export "loom:extension/guest@0.2.0" (instance $i)))"#)
            .unwrap_err();
    assert_eq!(error.kind, ErrorKind::ExtensionFailed, "{error}");
}

#[test]
fn an_unused_wasi_import_is_allowed_but_stubbed_to_trap() {
    // Toolchains may declare WASI imports they never call; they must not block
    // activation, and must not be granted.
    let wat = r#"(component (import "wasi:io/poll@0.2.0" (instance)))"#;
    let error = outcome(wat).unwrap_err();
    // It reaches linking (so it was not rejected as unsupported) and then lacks the guest export.
    assert_eq!(error.kind, ErrorKind::ExtensionFailed, "{error}");
}
