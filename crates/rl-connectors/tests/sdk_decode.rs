//! The real Rayfin SDK 1.36.2 and 2.0.0 (`fabricSemanticModel()` on `createConnectorsApi`, as the
//! reference app wires it) against rayfin-local's routes, compared with the SDK's own direct path
//! to the same mock Power BI (`tests/sdk/decode.mjs`).
//!
//! Needs Node and each line's node_modules (the basic and `sdk-v2` fixtures', or
//! `RAYFIN_SDK_NODE_MODULES` and `RAYFIN_SDK_V2_NODE_MODULES`); skipped without them.

mod common;

use std::{path::Path, sync::Arc};

use common::*;
use rl_connectors::router;
use serde_json::json;
use tokio::process::Command;

const TOKEN_ERROR: &str = "az isn't signed in";
const TOKEN_HINT: &str = "Run `az login`, then retry.";

#[tokio::test(flavor = "multi_thread")]
async fn the_sdk_normalizes_rayfin_local_answers_as_its_direct_path() {
    if Command::new("node")
        .arg("--version")
        .output()
        .await
        .is_err()
    {
        rl_test_support::skip("node isn't on PATH");
        return;
    }
    for line in rl_test_support::sdk_lines() {
        let Some(node_modules) = line.with("@microsoft/rayfin-connector-fabric-semanticmodel")
        else {
            continue;
        };
        println!("SDK {}", line.version);
        normalizes_as_the_direct_path(node_modules).await;
    }
}

async fn normalizes_as_the_direct_path(node_modules: &Path) {
    let power_bi = MockPowerBi::start().await;
    let offline_power_bi = closed_origin().await;
    let api = serve(router(
        config(&power_bi.base_url),
        Arc::new(FakeCallers),
        FakeTokens::with_token(DEV_TOKEN),
    ))
    .await;
    let offline_api = serve(router(
        config(&offline_power_bi),
        Arc::new(FakeCallers),
        FakeTokens::with_token(DEV_TOKEN),
    ))
    .await;
    let tokenless_api = serve(router(
        config(&power_bi.base_url),
        Arc::new(FakeCallers),
        FakeTokens::failing(TOKEN_ERROR, TOKEN_HINT),
    ))
    .await;
    let az_api = serve(router(
        config(&power_bi.base_url),
        Arc::new(FakeCallers),
        FakeTokens::with_token(&az_token()),
    ))
    .await;

    let settings = json!({
        "nodeModules": node_modules,
        "api": api,
        "offlineApi": offline_api,
        "tokenlessApi": tokenless_api,
        "azApi": az_api,
        "powerBi": power_bi.base_url,
        "offlinePowerBi": offline_power_bi,
        "workspaceId": WORKSPACE,
        "itemId": ITEM,
        "session": SESSION,
        "tokenError": TOKEN_ERROR,
        "tokenHint": TOKEN_HINT,
    });
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sdk/decode.mjs");
    let output = Command::new("node")
        .arg(script)
        .env("RL_SDK_TEST", settings.to_string())
        .output()
        .await
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("{stdout}");
    assert!(output.status.success(), "{stdout}\n{stderr}");
}
