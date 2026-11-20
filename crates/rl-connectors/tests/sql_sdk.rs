//! The real Rayfin SDK Category A client, 1.36.2 and 2.0.0, against rayfin-local's
//! `/connectors/<name>/graphql` over a real SQL Server (`tests/sdk/category-a.mjs`), with the
//! `connectors` fixture app's entity classes compiled by `js/analyze.mjs` against each line's
//! packages: the `connectors` fixture's (1.36) and the `sdk-v2` fixture's (2.0).
//!
//! Needs `RAYFIN_LOCAL_TEST_MSSQL` (see `sql_server.rs`), Node.js, and the fixtures' packages
//! (`npm ci` in `tests/fixtures/apps/connectors` and `tests/fixtures/apps/sdk-v2`); skipped
//! without them.

mod common;

use std::path::{Path, PathBuf};

use common::serve;
use common::sql::{Database, HAMMER, RAKE, router, server};
use rl_model::{ConnectorSource, ConnectorsAnalyzeInput};
use serde_json::{Value, json};
use tokio::process::Command;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/apps")
        .join(name)
        .canonicalize()
        .unwrap()
}

/// Each SDK line's major version and the fixture whose packages it takes.
fn lines() -> [(u64, PathBuf); 2] {
    [(1, fixture("connectors")), (2, fixture("sdk-v2"))]
}

/// A project with the `connectors` fixture's entities, under `packages`' `.rayfin-local/` so that
/// they compile against and import that fixture's `node_modules`.
fn project_on(packages: &Path) -> PathBuf {
    let source = fixture("connectors").join("rayfin");
    let project = packages.join(".rayfin-local/sdk-test-project");
    let _ = std::fs::remove_dir_all(&project);
    let entities = project.join("rayfin/connectors/inventory");
    std::fs::create_dir_all(&entities).unwrap();
    std::fs::write(
        project.join("package.json"),
        r#"{ "private": true, "type": "module" }"#,
    )
    .unwrap();
    std::fs::copy(
        source.join("tsconfig.json"),
        project.join("rayfin/tsconfig.json"),
    )
    .unwrap();
    for file in std::fs::read_dir(source.join("connectors/inventory")).unwrap() {
        let file = file.unwrap();
        std::fs::copy(file.path(), entities.join(file.file_name())).unwrap();
    }
    project
}

/// Compiles the project's connector entities with the analysis helper, into its `.rayfin-local/`
/// so they import its packages; returns their folder.
async fn compile_entities(project: &Path, major: u64) -> PathBuf {
    let scratch =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("rl-connectors-sql-sdk-{major}"));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    let mut input = ConnectorsAnalyzeInput::for_project(
        project,
        vec![ConnectorSource {
            name: "inventory".into(),
            dir: project.join("rayfin/connectors/inventory"),
            dialect: "mssql".into(),
        }],
    );
    input.out_dir = project.join(".rayfin-local/cache/sdk-test-connectors");
    let script = scratch.join("analyze.mjs");
    std::fs::write(&script, rl_model::ANALYZE_JS).unwrap();
    std::fs::write(scratch.join("input.json"), input.to_json().to_string()).unwrap();
    let status = Command::new("node")
        .arg(&script)
        .arg("--input")
        .arg(scratch.join("input.json"))
        .arg("--output")
        .arg(scratch.join("output.json"))
        .current_dir(project)
        .status()
        .await
        .unwrap();
    assert!(status.success());
    let output: Value =
        serde_json::from_slice(&std::fs::read(scratch.join("output.json")).unwrap()).unwrap();
    assert_eq!(output["ok"], true, "{output}");
    input.out_dir.join("connectors").join("inventory")
}

#[tokio::test(flavor = "multi_thread")]
async fn the_sdk_reads_and_writes_through_rayfin_local() {
    let Some(server) = server() else {
        return;
    };
    if Command::new("node")
        .arg("--version")
        .output()
        .await
        .is_err()
    {
        rl_test_support::skip("node isn't on PATH");
        return;
    }
    for (major, packages) in lines() {
        let node_modules = packages.join("node_modules");
        if !node_modules
            .join("@microsoft/rayfin-connector-fabric-graphql")
            .is_dir()
        {
            rl_test_support::skip(format_args!("run `npm ci` in {}", packages.display()));
            continue;
        }
        println!("SDK {major}");
        reads_and_writes(&server, &project_on(&packages), &node_modules, major).await;
    }
}

async fn reads_and_writes(
    server: &common::sql::Server,
    project: &Path,
    node_modules: &Path,
    major: u64,
) {
    let compiled = compile_entities(project, major).await;
    let database = Database::create(server, &format!("sdk{major}")).await;
    let api = serve(router(
        server,
        &database.name,
        &[
            ("inventory", "fabric-sqldatabase"),
            ("stock", "fabric-warehouse"),
            ("lake", "fabric-sqlanalytics"),
        ],
        None,
    ))
    .await;

    let settings = json!({
        "nodeModules": node_modules,
        "sdkMajor": major,
        "compiled": compiled,
        "api": api,
        "hammer": HAMMER,
        "rake": RAKE,
    });
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sdk/category-a.mjs");
    let output = Command::new("node")
        .arg(script)
        .env("RL_SDK_TEST", settings.to_string())
        .output()
        .await
        .unwrap();
    database.drop().await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("{stdout}");
    assert!(output.status.success(), "{stdout}\n{stderr}");
}
