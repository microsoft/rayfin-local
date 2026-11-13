//! Runs alone so other tests cannot mask process-global provider changes.

use std::path::Path;

use rl_runtime::Runtime;
use serde_json::json;

#[tokio::test]
async fn serving_does_not_install_a_global_provider_or_change_host_environment() {
    let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/apps/basic");
    if !project.join("node_modules/@microsoft/rayfin-core").is_dir() {
        eprintln!("skipped: install tests/fixtures/apps/basic's npm packages");
        return;
    }
    let state = tempfile::tempdir().unwrap();
    let cwd = std::env::current_dir().unwrap();
    let environment = std::env::vars_os().collect::<std::collections::BTreeMap<_, _>>();
    assert!(rustls::crypto::CryptoProvider::get_default().is_none());
    let runtime = Runtime::new(
        serde_json::from_value(json!({
            "projectDir": project,
            "stateDir": state.path(),
            "frontend": {"mode": "off"},
            "functions": {"enabled": false},
        }))
        .unwrap(),
    )
    .unwrap();
    runtime.start().await.unwrap();
    assert!(rustls::crypto::CryptoProvider::get_default().is_none());
    runtime.reload(false).await.unwrap();
    assert!(runtime.close().await.cleanup_errors.is_empty());
    assert!(rustls::crypto::CryptoProvider::get_default().is_none());
    assert_eq!(std::env::current_dir().unwrap(), cwd);
    assert_eq!(
        std::env::vars_os().collect::<std::collections::BTreeMap<_, _>>(),
        environment
    );
}
