//! `rayfin-local backup <OUT>` and `rayfin-local restore <FROM>`: copy the project's data (the
//! database and stored files) out, and replace it with a copy (see [`crate::backup`]).
//!
//! With `--json`, stdout gets one JSON object: `{"ok":true,...}`, or `{"ok":false,"error":{...}}`
//! with a stable `code`. Exit codes: 0 done (or cancelled), 1 failed or refused, 2 bad arguments.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rl_project::{LocalState, ProjectError};
use serde_json::{Value, json};

use super::Location;
use crate::backup::{self, BackupError, Manifest};
use crate::ui;

pub fn run_backup(at: &Location, out: &Path, json: bool) -> anyhow::Result<ExitCode> {
    let result = (|| -> anyhow::Result<(PathBuf, PathBuf, Manifest)> {
        let project = at.discover()?;
        let state_dir = project.state_dir();
        let out = absolute(out)?;
        let manifest = backup::create(&state_dir, &out)?;
        Ok((state_dir, out, manifest))
    })();
    finish(json, result, |(state_dir, out, manifest)| {
        if manifest.database {
            ui::step(format_args!(
                "Backed up {} into {} ({} files, {})",
                state_dir.display(),
                out.display(),
                manifest.files.len(),
                size(manifest.bytes())
            ));
        } else {
            ui::step(format_args!(
                "Backed up an empty state into {} ({} has no database yet)",
                out.display(),
                state_dir.display()
            ));
        }
        json!({
            "ok": true,
            "command": "backup",
            "stateDir": state_dir,
            "path": out,
            "database": manifest.database,
            "files": manifest.files.len(),
            "bytes": manifest.bytes(),
            "createdAt": manifest.created_at,
            "formatVersion": manifest.format_version,
        })
    })
}

pub fn run_restore(at: &Location, from: &Path, yes: bool, json: bool) -> anyhow::Result<ExitCode> {
    let result = (|| -> anyhow::Result<Option<(PathBuf, PathBuf, backup::Restored)>> {
        let project = at.discover()?;
        let from = absolute(from)?;
        // Refuse a bad backup before asking, and before the state folder is created.
        backup::read_manifest(&from)?;
        let (state, lock) = LocalState::open_locked(&project)?;
        if !lock.is_exclusive() {
            super::serve::warn_lock_unavailable(&state);
        }
        if !yes && has_data(&state) && !confirm(&state)? {
            return Ok(None);
        }
        let restored = backup::restore(&state, &from)?;
        drop(lock);
        Ok(Some((state.dir().to_path_buf(), from, restored)))
    })();
    finish(json, result, |restored| {
        let Some((state_dir, from, restored)) = restored else {
            ui::status("Cancelled; nothing was changed.");
            return json!({ "ok": false, "cancelled": true });
        };
        let manifest = &restored.manifest;
        if manifest.database {
            ui::step(format_args!(
                "Restored the backup of {} from {} into {} ({} files, {}); everyone is signed \
                 out",
                manifest.created_at,
                from.display(),
                state_dir.display(),
                manifest.files.len(),
                size(manifest.bytes())
            ));
        } else {
            ui::step(format_args!(
                "Restored an empty backup from {} into {}: the database and stored files are \
                 deleted",
                from.display(),
                state_dir.display()
            ));
        }
        json!({
            "ok": true,
            "command": "restore",
            "stateDir": state_dir,
            "path": from,
            "database": manifest.database,
            "files": manifest.files.len(),
            "bytes": manifest.bytes(),
            "createdAt": manifest.created_at,
            "formatVersion": manifest.format_version,
            "signedOutSessions": restored.signed_out,
        })
    })
}

/// Prints the result: with `--json` one object on stdout (and exit code 1 on failure), else the
/// status lines from `done`, or the error.
fn finish<T>(
    json: bool,
    result: anyhow::Result<T>,
    done: impl FnOnce(T) -> Value,
) -> anyhow::Result<ExitCode> {
    match result {
        Ok(value) => {
            let value = done(value);
            if json {
                print_json(&value)?;
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(err) if json => {
            ui::error(&err);
            print_json(&json!({
                "ok": false,
                "error": {
                    "code": error_code(&err),
                    "message": ui::error_lines(&err).join("\n"),
                },
            }))?;
            Ok(ExitCode::FAILURE)
        }
        Err(err) => Err(err),
    }
}

fn print_json(value: &Value) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)?;
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(())
}

/// The stable code of a failure in `--json` results.
fn error_code(err: &anyhow::Error) -> &'static str {
    for cause in err.chain() {
        if let Some(err) = cause.downcast_ref::<BackupError>() {
            return err.code();
        }
        if let Some(err) = cause.downcast_ref::<ProjectError>() {
            return match err {
                ProjectError::NotFound { .. } => "project-not-found",
                ProjectError::AlreadyServing { .. } => "already-serving",
                ProjectError::Read { .. }
                | ProjectError::Write { .. }
                | ProjectError::Reset { .. } => "internal",
                _ => "config-invalid",
            };
        }
    }
    "internal"
}

fn absolute(path: &Path) -> anyhow::Result<PathBuf> {
    Ok(std::path::absolute(path)?)
}

/// Whether there is data a restore would replace.
fn has_data(state: &LocalState) -> bool {
    state.db_path().exists()
        || std::fs::read_dir(state.storage_dir().join("objects"))
            .is_ok_and(|mut entries| entries.next().is_some())
}

fn confirm(state: &LocalState) -> anyhow::Result<bool> {
    super::reset::confirm(&format!(
        "Replace the database and stored files in {} with the backup (everyone is signed out)? \
         [y/N] ",
        state.dir().display()
    ))
}

fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}
