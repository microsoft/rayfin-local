//! Runs the embedded `js/analyze.mjs` with Node.js on the `basic` fixture project and compares
//! its result with the golden analysis. Skipped (with a note) without Node.js or without the
//! fixture's node_modules (`npm install` in tests/fixtures/apps/basic).

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use rl_model::connector::ConnectorsAnalysis;
use rl_model::{
    ANALYZE_JS, AnalyzeInput, AppModel, ConnectorSource, ConnectorsAnalyzeInput, Dialect,
};
use serde_json::Value;

fn fixture_project() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo = manifest.parent().and_then(Path::parent).unwrap();
    repo.join("tests")
        .join("fixtures")
        .join("apps")
        .join("basic")
}

fn node() -> Option<OsString> {
    let node = std::env::var_os("RAYFIN_LOCAL_NODE").unwrap_or_else(|| "node".into());
    let found = Command::new(&node)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    found.then_some(node)
}

/// Runs the helper in `project` and returns its output document.
fn run(node: &OsString, project: &Path, input: &Value, scratch: &Path) -> Value {
    let _ = fs::remove_dir_all(scratch);
    fs::create_dir_all(scratch).unwrap();
    let script = scratch.join("analyze.mjs");
    fs::write(&script, ANALYZE_JS).unwrap();
    let input_file = scratch.join("input.json");
    let output_file = scratch.join("output.json");
    fs::write(&input_file, input.to_string()).unwrap();
    let status = Command::new(node)
        .arg(&script)
        .arg("--input")
        .arg(&input_file)
        .arg("--output")
        .arg(&output_file)
        .current_dir(project)
        .env_remove("RAYFIN_FEATURE_FLAGS")
        .status()
        .unwrap();
    assert!(status.success(), "analyze.mjs exited with {status}");
    serde_json::from_str(&fs::read_to_string(&output_file).unwrap()).unwrap()
}

fn prerequisites() -> Option<(OsString, PathBuf)> {
    let project = fixture_project();
    if !project
        .join("node_modules")
        .join("@microsoft")
        .join("rayfin-core")
        .is_dir()
    {
        eprintln!("skipped: run `npm install` in {}", project.display());
        return None;
    }
    let Some(node) = node() else {
        eprintln!("skipped: Node.js was not found (set RAYFIN_LOCAL_NODE)");
        return None;
    };
    Some((node, project))
}

#[test]
fn analyze_js_reproduces_the_golden_analysis() {
    let Some((node, project)) = prerequisites() else {
        return;
    };
    // Outside the project, so the helper must resolve the project's packages itself.
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("rl-model-analyze-golden");
    let mut input = AnalyzeInput::for_project(&project, Dialect::Mssql, true);
    input.out_dir = scratch.join("compiled");
    let output = run(&node, &project, &input.to_json(), &scratch);
    assert_eq!(output["ok"], true, "{output}");
    let golden: Value = serde_json::from_str(include_str!("fixtures/basic.analysis.json")).unwrap();
    assert!(
        output["result"] == golden,
        "analyze.mjs output differs from tests/fixtures/basic.analysis.json; if the change is \
         intended, regenerate it. Output:\n{}",
        serde_json::to_string_pretty(&output["result"]).unwrap()
    );
    AppModel::from_analysis(&output["result"]).unwrap();
    assert!(
        !project.join("rayfin").join(".temp").exists(),
        "wrote into the project"
    );
}

#[test]
fn analyze_js_writes_inside_a_project_only_under_rayfin_local() {
    let Some((node, project)) = prerequisites() else {
        return;
    };
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("rl-model-analyze-outdir");
    let mut input = AnalyzeInput::for_project(&project, Dialect::Mssql, false);
    input.out_dir = project.join("rayfin").join("compiled");
    let output = run(&node, &project, &input.to_json(), &scratch);
    assert_eq!(output["ok"], false, "{output}");
    let message = output["error"]["message"].as_str().unwrap();
    assert!(message.contains(".rayfin-local"), "{message}");
    assert!(!input.out_dir.exists());
}

/// A new project in `CARGO_TARGET_TMPDIR/<name>`: the fixture's `rayfin/tsconfig.json`,
/// `tsconfig.json` and `package.json`, its `node_modules` linked, and `files` (relative path,
/// contents). Returns the project and a scratch folder outside it.
fn scratch_project(fixture: &Path, name: &str, files: &[(&str, &str)]) -> (PathBuf, PathBuf) {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let project = base.join("project");
    let modules = project.join("node_modules");
    if modules.exists() {
        // The link first, so that removing the project never reaches the fixture's packages.
        if fs::remove_dir(&modules).is_err() {
            let _ = fs::remove_file(&modules);
        }
    }
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(project.join("rayfin")).unwrap();
    for file in ["package.json", "tsconfig.json", "rayfin/tsconfig.json"] {
        fs::copy(fixture.join(file), project.join(file)).unwrap();
    }
    for (path, contents) in files {
        let path = project.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
    link_dir(&fixture.join("node_modules"), &modules);
    (project, base.join("scratch"))
}

#[cfg(windows)]
fn link_dir(target: &Path, link: &Path) {
    // A junction needs no privilege, unlike a symbolic link.
    let status = Command::new("cmd")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "could not link {}", link.display());
}

#[cfg(unix)]
fn link_dir(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

/// Analyzes `project` (with storage), its compiled output in `scratch`.
fn analyze_scratch(node: &OsString, project: &Path, scratch: &Path) -> Value {
    let mut input = AnalyzeInput::for_project(project, Dialect::Mssql, true);
    input.out_dir = scratch.join("compiled");
    let script_dir = scratch.join("run");
    run(node, project, &input.to_json(), &script_dir)
}

fn warnings(result: &Value) -> Vec<String> {
    result["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["message"].as_str().unwrap().to_owned())
        .collect()
}

const FOLDER: &str = "\
import { role } from '@microsoft/rayfin-core';
import { blob, StorageObject } from '@microsoft/rayfin-core/experimental';

@blob({ name: 'Files' })
@role('authenticated', ['create', 'read'])
export class Files extends StorageObject {}
";

#[test]
fn an_empty_entity_folder_is_a_model_without_entities() {
    let Some((node, fixture)) = prerequisites() else {
        return;
    };
    let (project, scratch) = scratch_project(
        &fixture,
        "rl-model-analyze-empty",
        &[("rayfin/storage/Files.ts", FOLDER)],
    );
    fs::create_dir_all(project.join("rayfin").join("data")).unwrap();
    let output = analyze_scratch(&node, &project, &scratch);
    assert_eq!(output["ok"], true, "{output}");
    let result = &output["result"];
    assert_eq!(result["entities"], serde_json::json!([]), "{result}");
    assert_eq!(
        result["config"]["entities"],
        serde_json::json!({}),
        "{result}"
    );
    assert!(
        warnings(result)
            .iter()
            .any(|w| w.contains("rayfin/data has no @entity() classes yet")),
        "{result}"
    );
    let model = AppModel::from_analysis(result).unwrap();
    assert!(model.entities.is_empty());
    // Storage is analyzed all the same.
    let folders = &model.storage.as_ref().unwrap().folders;
    assert_eq!(folders.len(), 1, "{result}");
}

#[test]
fn a_missing_entity_folder_is_a_model_without_entities() {
    let Some((node, fixture)) = prerequisites() else {
        return;
    };
    let (project, scratch) = scratch_project(&fixture, "rl-model-analyze-missing", &[]);
    let output = analyze_scratch(&node, &project, &scratch);
    assert_eq!(output["ok"], true, "{output}");
    let result = &output["result"];
    assert_eq!(result["entities"], serde_json::json!([]), "{result}");
    let warnings = warnings(result);
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("rayfin/data does not exist")),
        "{result}"
    );
    assert!(
        warnings.iter().any(|w| w.contains("rayfin/storage")),
        "{result}"
    );
    assert!(AppModel::from_analysis(result).unwrap().entities.is_empty());
}

#[test]
fn modules_without_entities_are_a_model_without_entities() {
    let Some((node, fixture)) = prerequisites() else {
        return;
    };
    let (project, scratch) = scratch_project(
        &fixture,
        "rl-model-analyze-no-entity",
        &[("rayfin/data/shared.ts", "export const LIMIT = 10;\n")],
    );
    let output = analyze_scratch(&node, &project, &scratch);
    assert_eq!(output["ok"], true, "{output}");
    let result = &output["result"];
    assert_eq!(result["entities"], serde_json::json!([]), "{result}");
    assert_eq!(
        result["sources"]["data"],
        serde_json::json!(["rayfin/data/shared.ts"])
    );
    assert!(
        warnings(result)
            .iter()
            .any(|w| w.contains("no @entity() classes yet")),
        "{result}"
    );
}

#[test]
fn entities_that_do_not_compile_still_fail_the_analysis() {
    let Some((node, fixture)) = prerequisites() else {
        return;
    };
    let (project, scratch) = scratch_project(
        &fixture,
        "rl-model-analyze-broken",
        &[("rayfin/data/Label.ts", "export class Label {\n")],
    );
    let output = analyze_scratch(&node, &project, &scratch);
    assert_eq!(output["ok"], false, "{output}");
    let message = output["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("TypeScript compilation of rayfin failed") && message.contains("Label.ts"),
        "{message}"
    );
}

#[test]
fn a_data_package_without_an_entity_folder_is_not_warned_about() {
    let Some((node, fixture)) = prerequisites() else {
        return;
    };
    // The data package's exports are imported as built; `rayfin/data` is not looked at.
    let (project, scratch) = scratch_project(
        &fixture,
        "rl-model-analyze-package",
        &[
            (
                "package.json",
                r#"{ "name": "pkg", "private": true, "type": "module", "main": "dist/index.js" }"#,
            ),
            ("dist/index.js", "export const nothing = 1;\n"),
        ],
    );
    let output = analyze_scratch(&node, &project, &scratch);
    assert_eq!(output["ok"], true, "{output}");
    let result = &output["result"];
    let warnings = warnings(result);
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("Loaded entities from the data package exports: dist/index.js")),
        "{result}"
    );
    assert!(
        !warnings
            .iter()
            .any(|w| w.contains("rayfin/data does not exist") || w.contains("no @entity() classes")),
        "{result}"
    );
}

/// The `connectors` fixture app (the `inventory` SQL connector), with its packages installed.
fn connectors_prerequisites() -> Option<(OsString, PathBuf)> {
    let project = fixture_project().parent().unwrap().join("connectors");
    if !project
        .join("node_modules")
        .join("@microsoft")
        .join("rayfin-connectors")
        .is_dir()
    {
        eprintln!("skipped: run `npm install` in {}", project.display());
        return None;
    }
    let Some(node) = node() else {
        eprintln!("skipped: Node.js was not found (set RAYFIN_LOCAL_NODE)");
        return None;
    };
    Some((node, project))
}

#[test]
fn analyze_js_reproduces_the_golden_connector_analysis() {
    let Some((node, project)) = connectors_prerequisites() else {
        return;
    };
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("rl-model-analyze-connectors");
    let source = |name: &str| ConnectorSource {
        name: name.to_owned(),
        dir: project.join("rayfin").join("connectors").join(name),
        dialect: "mssql".to_owned(),
    };
    let mut input =
        ConnectorsAnalyzeInput::for_project(&project, vec![source("inventory"), source("missing")]);
    input.out_dir = scratch.join("compiled");
    let output = run(&node, &project, &input.to_json(), &scratch);
    assert_eq!(output["ok"], true, "{output}");
    let connectors = &output["result"]["connectors"];
    let golden: Value =
        serde_json::from_str(include_str!("fixtures/inventory.connector.json")).unwrap();
    assert!(
        connectors[0] == golden,
        "analyze.mjs output differs from tests/fixtures/inventory.connector.json; if the change \
         is intended, regenerate it. Output:\n{}",
        serde_json::to_string_pretty(&connectors[0]).unwrap()
    );
    let message = connectors[1]["error"]["message"].as_str().unwrap();
    assert!(message.contains("rayfin/connectors/missing"), "{message}");
    let analysis = ConnectorsAnalysis::from_result(&output["result"]).unwrap();
    assert!(analysis.connectors[0].model.is_ok());
    assert!(analysis.connectors[1].model.is_err());
    assert!(
        analysis
            .inputs
            .iter()
            .any(|input| input.ends_with("inventory/Product.ts")),
        "{:?}",
        analysis.inputs
    );
}
