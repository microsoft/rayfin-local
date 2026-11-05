//! `--state-dir` (and `RAYFIN_LOCAL_STATE_DIR`) on every command, and `backup` / `restore` of the
//! built binary: their `--json` results, exit codes and refusals.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rl_project::{LoadOptions, LocalState, Project};
use serde_json::Value;

const RAYFIN_YML: &str = "id: app\nname: App\nversion: 1.0.0\nservices:\n  auth:\n    \
                          enabled: true\nsecrets:\n  - name: API_KEY\n";

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("rayfin")).unwrap();
    std::fs::write(dir.path().join("rayfin").join("rayfin.yml"), RAYFIN_YML).unwrap();
    dir
}

fn rayfin_local(project: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rayfin-local"));
    command
        .arg("-C")
        .arg(project)
        .args(args)
        .env_remove("RAYFIN_LOCAL_STATE_DIR")
        .env("NO_COLOR", "1");
    command
}

fn run(command: &mut Command) -> Output {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{command:?} failed ({}):\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is not one JSON object ({err}):\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn same_path(value: &Value, path: &Path) {
    let value = PathBuf::from(value.as_str().unwrap());
    assert_eq!(
        value.canonicalize().unwrap(),
        path.canonicalize().unwrap(),
        "{value:?}"
    );
}

#[test]
fn every_command_uses_the_state_dir() {
    let dir = project();
    let root = dir.path();
    let outside = tempfile::tempdir().unwrap();
    let state = outside.path().join("state");
    let state_arg = state.to_str().unwrap();
    let with_state = |args: &[&str]| {
        let mut command = rayfin_local(root, &["--state-dir", state_arg]);
        command.args(args);
        command
    };

    run(&mut with_state(&[
        "users",
        "add",
        "ana",
        "--password",
        "ana-password",
    ]));
    assert!(stdout(&run(&mut with_state(&["users", "list"]))).contains("ana"));
    run(&mut with_state(&["secret", "set", "API_KEY", "value"]));
    assert!(state.join("secrets.json").is_file());
    assert!(state.join("db.sqlite").is_file());
    assert!(state.join("state.json").is_file());
    // Not in a git work tree or the project: no .gitignore littered in the developer's folder.
    assert!(!state.join(".gitignore").exists());

    let doctor = json(&with_state(&["doctor", "--json"]).output().unwrap());
    let check = doctor["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == "state")
        .unwrap();
    assert_eq!(check["status"], "ok", "{check}");
    assert!(
        check["message"]
            .as_str()
            .unwrap()
            .contains(state.to_str().unwrap()),
        "{check}"
    );

    // `schema` reads the state too; it needs Node.js and the SDK, so only check it doesn't
    // touch the default folder.
    let _ = with_state(&["schema"]).output().unwrap();

    let backup = outside.path().join("backup");
    let result = json(&run(&mut with_state(&[
        "backup",
        backup.to_str().unwrap(),
        "--json",
    ])));
    assert_eq!(result["ok"], true);
    same_path(&result["stateDir"], &state);
    run(&mut with_state(&["users", "delete", "ana", "--yes"]));
    let result = json(&run(&mut with_state(&[
        "restore",
        backup.to_str().unwrap(),
        "--yes",
        "--json",
    ])));
    assert_eq!(result["ok"], true, "{result}");
    same_path(&result["stateDir"], &state);
    assert!(stdout(&run(&mut with_state(&["users", "list"]))).contains("ana"));

    run(&mut with_state(&["reset", "--yes"]));
    assert!(
        !state.exists(),
        "reset leaves an empty custom state folder deleted"
    );

    assert!(
        !root.join(".rayfin-local").exists(),
        "the default state folder was used"
    );
}

#[test]
fn the_env_var_sets_the_state_dir_relative_to_the_working_directory() {
    let dir = project();
    let root = dir.path();
    let outside = tempfile::tempdir().unwrap();
    run(
        rayfin_local(root, &["users", "add", "ana", "--password", "ana-password"])
            .current_dir(outside.path())
            .env("RAYFIN_LOCAL_STATE_DIR", "relative-state"),
    );
    assert!(
        outside
            .path()
            .join("relative-state")
            .join("db.sqlite")
            .is_file()
    );
    assert!(!root.join(".rayfin-local").exists());

    // The flag wins over the variable.
    let flag = outside.path().join("flag-state");
    run(rayfin_local(
        root,
        &["--state-dir", flag.to_str().unwrap(), "users", "list"],
    )
    .env("RAYFIN_LOCAL_STATE_DIR", "relative-state"));
    assert!(flag.join("state.json").is_file());
}

#[test]
fn refuses_unsafe_state_dirs() {
    let dir = project();
    let root = dir.path();
    for state in [root.to_path_buf(), root.join("rayfin").join("state")] {
        let output = rayfin_local(
            root,
            &["--state-dir", state.to_str().unwrap(), "users", "list"],
        )
        .output()
        .unwrap();
        assert_eq!(output.status.code(), Some(1), "{}", state.display());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("state"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    // A folder with other files is not taken over.
    let foreign = tempfile::tempdir().unwrap();
    std::fs::write(foreign.path().join("notes.txt"), "mine").unwrap();
    let output = rayfin_local(
        root,
        &[
            "--state-dir",
            foreign.path().to_str().unwrap(),
            "users",
            "list",
        ],
    )
    .output()
    .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        std::fs::read_dir(foreign.path()).unwrap().count(),
        1,
        "the folder was changed"
    );
}

#[test]
fn backup_runs_while_serving_and_restore_is_refused() {
    let dir = project();
    let root = dir.path();
    run(&mut rayfin_local(
        root,
        &["users", "add", "ana", "--password", "ana-password"],
    ));
    let project = Project::load_with(root, &LoadOptions::default()).unwrap();
    let (_state, lock) = LocalState::open_for_serve(&project).unwrap();
    assert!(lock.is_exclusive());

    let out = tempfile::tempdir().unwrap();
    let backup = out.path().join("backup");
    let result = json(&run(&mut rayfin_local(
        root,
        &["backup", backup.to_str().unwrap(), "--json"],
    )));
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["command"], "backup");
    assert_eq!(result["database"], true);
    assert_eq!(result["formatVersion"], 1);
    assert!(result["files"].as_u64().unwrap() >= 1);
    assert!(result["bytes"].as_u64().unwrap() > 0);
    assert!(result["createdAt"].is_string());
    same_path(&result["path"], &backup);

    let output = rayfin_local(
        root,
        &["restore", backup.to_str().unwrap(), "--yes", "--json"],
    )
    .output()
    .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let result = json(&output);
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["code"], "already-serving", "{result}");

    drop(lock);
    let result = json(&run(&mut rayfin_local(
        root,
        &["restore", backup.to_str().unwrap(), "--yes", "--json"],
    )));
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["command"], "restore");
    assert_eq!(result["signedOutSessions"], 0);
}

#[test]
fn json_failures_have_stable_codes_and_exit_codes() {
    let dir = project();
    let root = dir.path();
    let out = tempfile::tempdir().unwrap();

    // No database yet: an empty backup, and no database created.
    let empty = out.path().join("empty");
    let result = json(&run(&mut rayfin_local(
        root,
        &["backup", empty.to_str().unwrap(), "--json"],
    )));
    assert_eq!(result["ok"], true);
    assert_eq!(result["database"], false);
    assert_eq!(result["files"], 0);
    assert!(!root.join(".rayfin-local").join("db.sqlite").exists());

    let failure = |args: &[&str], code: &str| {
        let output = rayfin_local(root, args).output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        let result = json(&output);
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["error"]["code"], code, "{result}");
        assert!(result["error"]["message"].as_str().unwrap().len() > 10);
    };
    failure(
        &["backup", empty.to_str().unwrap(), "--json"],
        "output-not-empty",
    );
    let inside = root.join(".rayfin-local").join("backup");
    failure(
        &["backup", inside.to_str().unwrap(), "--json"],
        "output-invalid",
    );
    let nothing = out.path().join("nothing");
    std::fs::create_dir(&nothing).unwrap();
    failure(
        &["restore", nothing.to_str().unwrap(), "--yes", "--json"],
        "backup-invalid",
    );
    let elsewhere = tempfile::tempdir().unwrap();
    failure_in(elsewhere.path(), empty.to_str().unwrap());

    // --json needs --yes: a usage error.
    let output = rayfin_local(root, &["restore", empty.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));

    // An empty backup restores to a reset state.
    let result = json(&run(&mut rayfin_local(
        root,
        &["restore", empty.to_str().unwrap(), "--yes", "--json"],
    )));
    assert_eq!(result["ok"], true);
    assert_eq!(result["database"], false);
}

/// Outside a project: `project-not-found`.
fn failure_in(dir: &Path, backup: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_rayfin-local"))
        .args(["restore", backup, "--yes", "--json"])
        .current_dir(dir)
        .env_remove("RAYFIN_LOCAL_STATE_DIR")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(json(&output)["error"]["code"], "project-not-found");
}
