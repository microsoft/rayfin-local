//! Backups of a project's data: exactly what `serve --reset` deletes (the database and the
//! stored files), never the signing keys, `state.json`, the local secrets or the caches.
//!
//! ```text
//! <backup>/
//!   manifest.json          format, versions, and every file's size and SHA-256
//!   db.sqlite              the database without sessions (absent when there was none)
//!   storage/objects/<blob> the stored files the database refers to
//! ```
//!
//! A backup is taken from a running `serve` without its lock: SQLite's online backup copies
//! the database in one read transaction (in WAL mode it doesn't block writers), and the stored
//! files it refers to are copied after it. Sessions, refresh tokens, handoff codes, the session
//! epoch and pending storage operations are left out: they are only meaningful to the backend
//! that issued them. Users stay, since the app's rows and stored files refer to their ids.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use ring::digest;
use rl_project::LocalState;
use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// `manifest.json`'s `format`.
pub const FORMAT: &str = "rayfin-local-backup";
/// The newest backup format this version writes and restores.
pub const FORMAT_VERSION: u32 = 1;
pub const MANIFEST: &str = "manifest.json";
const DB_FILE: &str = "db.sqlite";
const OBJECTS: &str = "storage/objects";

/// Tables whose rows a backup leaves out, children first.
const RUNTIME_TABLES: [&str; 5] = [
    "_rl_auth_refresh_tokens",
    "_rl_auth_handoff_codes",
    "_rl_auth_sessions",
    "_rl_auth_session_epoch",
    "_rl_storage_operations",
];

/// How often a backup starts over when stored files changed while it copied them.
const ATTEMPTS: u32 = 3;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub format: String,
    pub format_version: u32,
    /// When the database was copied (RFC 3339, UTC).
    pub created_at: String,
    pub rayfin_local_version: String,
    /// Whether there was a database; without one the backup is of an empty state.
    pub database: bool,
    pub files: Vec<FileEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    /// Relative to the backup folder, with `/`.
    pub path: String,
    pub size: u64,
    /// Lowercase hex.
    pub sha256: String,
}

impl Manifest {
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|file| file.size).sum()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("{} is not empty; back up into a new or empty folder.", path.display())]
    OutputNotEmpty { path: PathBuf },
    #[error("Can't back up into {}: {reason}", path.display())]
    OutputInvalid { path: PathBuf, reason: String },
    #[error(
        "The stored files kept changing while they were copied ({blob} went missing {ATTEMPTS} \
         times); try again."
    )]
    StorageChanged { blob: String },
    #[error("{} is not a usable backup: {reason}", path.display())]
    InvalidBackup { path: PathBuf, reason: String },
    #[error(
        "{} was written by a newer rayfin-local (backup format {version}; this version reads up \
         to {FORMAT_VERSION}). Update rayfin-local to restore it.",
        path.display()
    )]
    UnsupportedFormat { path: PathBuf, version: u32 },
    #[error("{context}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
    #[error("{context}")]
    Sqlite {
        context: String,
        #[source]
        source: rusqlite::Error,
    },
}

impl BackupError {
    /// The stable code of `--json` results.
    pub fn code(&self) -> &'static str {
        match self {
            Self::OutputNotEmpty { .. } => "output-not-empty",
            Self::OutputInvalid { .. } => "output-invalid",
            Self::StorageChanged { .. } => "storage-changed",
            Self::InvalidBackup { .. } => "backup-invalid",
            Self::UnsupportedFormat { .. } => "backup-unsupported",
            Self::Io { .. } | Self::Sqlite { .. } => "internal",
        }
    }
}

fn io_error(context: impl FnOnce() -> String) -> impl FnOnce(io::Error) -> BackupError {
    move |source| BackupError::Io {
        context: context(),
        source,
    }
}

fn sql_error(context: impl FnOnce() -> String) -> impl FnOnce(rusqlite::Error) -> BackupError {
    move |source| BackupError::Sqlite {
        context: context(),
        source,
    }
}

/// Backs up the state folder `state_dir` into the new or empty folder `out` (absolute): written
/// next to it first and renamed into place, so a failed backup leaves nothing at `out`. Never
/// creates the database, and works whether or not `serve` runs.
pub fn create(state_dir: &Path, out: &Path) -> Result<Manifest, BackupError> {
    create_with(state_dir, out, &mut |_| {})
}

/// [`create`], calling `after_snapshot` with the attempt between copying the database and
/// copying the stored files (tests change the files there).
fn create_with(
    state_dir: &Path,
    out: &Path,
    after_snapshot: &mut dyn FnMut(u32),
) -> Result<Manifest, BackupError> {
    check_output(state_dir, out)?;
    let parent = out.parent().ok_or_else(|| BackupError::OutputInvalid {
        path: out.to_path_buf(),
        reason: "it has no parent folder".into(),
    })?;
    fs::create_dir_all(parent).map_err(io_error(|| {
        format!("Could not create {}", parent.display())
    }))?;
    let name = out
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = parent.join(format!(".{name}.rl-tmp-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir(&temp).map_err(io_error(|| format!("Could not create {}", temp.display())))?;
    let result = fill(state_dir, &temp, after_snapshot).and_then(|manifest| {
        // `out` may be an empty folder (checked above), which a rename can't replace everywhere.
        match fs::remove_dir(out) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => {
                return Err(io_error(|| format!("Could not replace {}", out.display()))(
                    err,
                ));
            }
            _ => {}
        }
        fs::rename(&temp, out).map_err(io_error(|| {
            format!("Could not move the backup into {}", out.display())
        }))?;
        Ok(manifest)
    });
    if result.is_err() {
        let _ = fs::remove_dir_all(&temp);
    }
    result
}

/// `out` must be new or an empty folder, and not inside the state folder.
fn check_output(state_dir: &Path, out: &Path) -> Result<(), BackupError> {
    let invalid = |reason: &str| BackupError::OutputInvalid {
        path: out.to_path_buf(),
        reason: reason.into(),
    };
    if resolved(out).starts_with(resolved(state_dir)) {
        return Err(invalid(
            "it is inside the state folder, which a reset or restore would delete",
        ));
    }
    match fs::metadata(out) {
        Ok(meta) if !meta.is_dir() => return Err(invalid("it is a file")),
        Ok(_) => {
            let mut entries = fs::read_dir(out)
                .map_err(io_error(|| format!("Could not read {}", out.display())))?;
            if entries.next().is_some() {
                return Err(BackupError::OutputNotEmpty {
                    path: out.to_path_buf(),
                });
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(io_error(|| format!("Could not read {}", out.display()))(
                err,
            ));
        }
    }
    Ok(())
}

/// `path` with its longest existing ancestor resolved (symbolic links, `..`, case on Windows).
fn resolved(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut base = path;
    loop {
        if let Ok(real) = dunce_canonicalize(base) {
            let mut out = real;
            out.extend(rest.iter().rev());
            return out;
        }
        match (base.parent(), base.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                base = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

fn dunce_canonicalize(path: &Path) -> io::Result<PathBuf> {
    let real = fs::canonicalize(path)?;
    // Without the `\\?\` prefix, so that it compares with paths that don't have it.
    #[cfg(windows)]
    if let Some(plain) = real.to_str().and_then(|s| s.strip_prefix(r"\\?\"))
        && !plain.starts_with("UNC\\")
    {
        return Ok(PathBuf::from(plain));
    }
    Ok(real)
}

/// Writes the backup into the empty folder `dir`: the database, then the stored files it refers
/// to, then `manifest.json`.
fn fill(
    state_dir: &Path,
    dir: &Path,
    after_snapshot: &mut dyn FnMut(u32),
) -> Result<Manifest, BackupError> {
    let db = state_dir.join(DB_FILE);
    let mut files = Vec::new();
    let database = db.is_file();
    let mut created_at = now_rfc3339();
    if database {
        let mut attempt = 0;
        files = loop {
            attempt += 1;
            created_at = now_rfc3339();
            let copy = dir.join(DB_FILE);
            let blobs = snapshot(&db, &copy)?;
            after_snapshot(attempt);
            match copy_objects(state_dir, dir, &blobs)? {
                Ok(mut objects) => {
                    let (size, sha256) = hash_file(&copy)?;
                    objects.insert(
                        0,
                        FileEntry {
                            path: DB_FILE.into(),
                            size,
                            sha256,
                        },
                    );
                    break objects;
                }
                // A file was replaced or deleted after the database was copied: start over, so
                // the database and the files are of the same moment.
                Err(blob) if attempt < ATTEMPTS => {
                    tracing::debug!("{blob} changed during the backup; starting over");
                    let _ = fs::remove_file(&copy);
                    let _ = fs::remove_dir_all(dir.join("storage"));
                }
                Err(blob) => return Err(BackupError::StorageChanged { blob }),
            }
        };
    }
    let manifest = Manifest {
        format: FORMAT.into(),
        format_version: FORMAT_VERSION,
        created_at,
        rayfin_local_version: env!("CARGO_PKG_VERSION").into(),
        database,
        files,
    };
    let mut json = serde_json::to_vec_pretty(&manifest).expect("a manifest serializes");
    json.push(b'\n');
    let path = dir.join(MANIFEST);
    write_synced(&path, &json)
        .map_err(io_error(|| format!("Could not write {}", path.display())))?;
    Ok(manifest)
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}

/// Copies the database `db` into the new file `copy` in one read transaction, then removes the
/// rows of [`RUNTIME_TABLES`] from the copy; the blobs of the stored files the copy refers to.
fn snapshot(db: &Path, copy: &Path) -> Result<Vec<String>, BackupError> {
    let context = || format!("Could not copy the database {}", db.display());
    // Opened without `CREATE`, so a database deleted meanwhile is not created again.
    let source = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(sql_error(context))?;
    source
        .busy_timeout(Duration::from_secs(5))
        .map_err(sql_error(context))?;
    let mut target = Connection::open(copy).map_err(sql_error(context))?;
    {
        let backup = Backup::new(&source, &mut target).map_err(sql_error(context))?;
        // All pages in one step: one read transaction, which writers to a WAL database don't
        // wait for, and no restarts when they write meanwhile.
        let started = Instant::now();
        loop {
            match backup.step(-1).map_err(sql_error(context))? {
                StepResult::Done => break,
                _ if started.elapsed() < Duration::from_secs(30) => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                _ => {
                    return Err(sql_error(context)(rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                        Some("the database stayed busy for 30 seconds".into()),
                    )));
                }
            }
        }
    }
    drop(source);
    let trim = || format!("Could not prepare the copy of {}", db.display());
    target
        .execute_batch("PRAGMA journal_mode = DELETE;")
        .map_err(sql_error(trim))?;
    for table in RUNTIME_TABLES {
        if has_table(&target, table).map_err(sql_error(trim))? {
            target
                .execute(&format!("DELETE FROM \"{table}\""), [])
                .map_err(sql_error(trim))?;
        }
    }
    target.execute_batch("VACUUM;").map_err(sql_error(trim))?;
    let blobs = if has_table(&target, "_rl_storage_objects").map_err(sql_error(trim))? {
        target
            .prepare("SELECT blob FROM _rl_storage_objects ORDER BY blob")
            .and_then(|mut statement| {
                statement
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()
            })
            .map_err(sql_error(trim))?
    } else {
        Vec::new()
    };
    target.close().map_err(|(_, err)| sql_error(trim)(err))?;
    Ok(blobs)
}

fn has_table(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |_| Ok(()),
    )
    .optional()
    .map(|found| found.is_some())
}

/// Copies the stored files `blobs` from the state folder into the backup folder `dir`; `Err`
/// with the blob that is missing.
fn copy_objects(
    state_dir: &Path,
    dir: &Path,
    blobs: &[String],
) -> Result<Result<Vec<FileEntry>, String>, BackupError> {
    let target = dir.join(OBJECTS);
    fs::create_dir_all(&target).map_err(io_error(|| {
        format!("Could not create {}", target.display())
    }))?;
    let source = state_dir.join(OBJECTS);
    let mut files = Vec::with_capacity(blobs.len());
    for blob in blobs {
        if !is_plain_name(blob) {
            return Err(BackupError::InvalidBackup {
                path: state_dir.join(DB_FILE),
                reason: format!("a stored file has the unexpected name {blob:?}"),
            });
        }
        let from = source.join(blob);
        match copy_hashed(&from, &target.join(blob)) {
            Ok((size, sha256)) => files.push(FileEntry {
                path: format!("{OBJECTS}/{blob}"),
                size,
                sha256,
            }),
            Err(err) if err.kind() == io::ErrorKind::NotFound && !from.exists() => {
                return Ok(Err(blob.clone()));
            }
            Err(err) => {
                return Err(io_error(|| format!("Could not copy {}", from.display()))(
                    err,
                ));
            }
        }
    }
    Ok(Ok(files))
}

/// A single path component that is not `.` or `..`.
fn is_plain_name(name: &str) -> bool {
    let mut components = Path::new(name).components();
    matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
        && !name.contains(['/', '\\', ':'])
}

/// Copies `from` into the new file `to` (synced to disk); its size and SHA-256.
fn copy_hashed(from: &Path, to: &Path) -> io::Result<(u64, String)> {
    let mut input = fs::File::open(from)?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)?;
    let mut hash = digest::Context::new(&digest::SHA256);
    let mut buffer = vec![0; 64 * 1024];
    let mut size = 0;
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
        output.write_all(&buffer[..read])?;
        size += read as u64;
    }
    output.sync_all()?;
    Ok((size, hex::encode(hash.finish())))
}

fn hash_file(path: &Path) -> Result<(u64, String), BackupError> {
    let context = || format!("Could not read {}", path.display());
    let mut input = fs::File::open(path).map_err(io_error(context))?;
    let mut hash = digest::Context::new(&digest::SHA256);
    let mut buffer = vec![0; 64 * 1024];
    let mut size = 0;
    loop {
        let read = input.read(&mut buffer).map_err(io_error(context))?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
        size += read as u64;
    }
    Ok((size, hex::encode(hash.finish())))
}

fn write_synced(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(contents)?;
    file.sync_all()
}

/// Reads and checks `from`'s `manifest.json` (not the files).
pub fn read_manifest(from: &Path) -> Result<Manifest, BackupError> {
    let path = from.join(MANIFEST);
    let invalid = |reason: String| BackupError::InvalidBackup {
        path: from.to_path_buf(),
        reason,
    };
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Err(invalid(format!("it has no {MANIFEST}")));
        }
        Err(err) => {
            return Err(io_error(|| format!("Could not read {}", path.display()))(
                err,
            ));
        }
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|err| invalid(format!("{MANIFEST} is not valid JSON ({err})")))?;
    if value.get("format").and_then(serde_json::Value::as_str) != Some(FORMAT) {
        return Err(invalid(format!(
            "{MANIFEST} is not a rayfin-local backup manifest"
        )));
    }
    // Checked before the rest, which a newer format may have changed.
    if let Some(version) = value
        .get("formatVersion")
        .and_then(serde_json::Value::as_u64)
        && version > u64::from(FORMAT_VERSION)
    {
        return Err(BackupError::UnsupportedFormat {
            path: from.to_path_buf(),
            version: u32::try_from(version).unwrap_or(u32::MAX),
        });
    }
    let manifest: Manifest = serde_json::from_value(value)
        .map_err(|err| invalid(format!("{MANIFEST} is incomplete ({err})")))?;
    if manifest.format_version == 0 {
        return Err(invalid(format!("{MANIFEST} has format version 0")));
    }
    let mut seen = std::collections::HashSet::new();
    for file in &manifest.files {
        let known = file.path == DB_FILE
            || file
                .path
                .strip_prefix(OBJECTS)
                .and_then(|rest| rest.strip_prefix('/'))
                .is_some_and(is_plain_name);
        if !known || !seen.insert(file.path.as_str()) {
            return Err(invalid(format!(
                "{MANIFEST} lists the unexpected file {:?}",
                file.path
            )));
        }
    }
    if manifest.database != seen.contains(DB_FILE) {
        return Err(invalid(format!(
            "{MANIFEST} and the files disagree on whether there is a database"
        )));
    }
    Ok(manifest)
}

/// What a restore did.
#[derive(Clone, Debug)]
pub struct Restored {
    pub manifest: Manifest,
    /// Sessions of the replaced database that are now signed out.
    pub signed_out: usize,
}

/// Replaces the data in `state`'s folder (the database and the stored files) with the backup in
/// `from`, keeping the keys, `state.json` and the secrets. The caller holds the `serve` lock.
/// Every file is checked against the manifest while it is copied next to the data, and the data
/// is only replaced once all are; then everyone is signed out (a new session epoch, and the
/// replaced database's sessions revoked). A backup without a database leaves the state as
/// `serve --reset` does.
pub fn restore(state: &LocalState, from: &Path) -> Result<Restored, BackupError> {
    let manifest = read_manifest(from)?;
    let dir = state.dir();
    remove_stale_staging(dir);
    let stage = dir.join(format!(
        "{}restore-{}",
        rl_project::TEMP_PREFIX,
        uuid::Uuid::new_v4().simple()
    ));
    let result = stage_backup(state, from, &manifest, &stage)
        .and_then(|signed_out| swap_in(state, &stage).map(|()| signed_out));
    let _ = fs::remove_dir_all(&stage);
    Ok(Restored {
        manifest,
        signed_out: result?,
    })
}

/// Staging folders of restores that stopped halfway (not the `-old` data of an interrupted
/// swap, which is kept for the developer to recover).
fn remove_stale_staging(dir: &Path) {
    let prefix = format!("{}restore-", rl_project::TEMP_PREFIX);
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(&prefix) && !name.ends_with("-old") {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Copies and checks the backup into `stage`, and prepares its database: the number of sessions
/// signed out.
fn stage_backup(
    state: &LocalState,
    from: &Path,
    manifest: &Manifest,
    stage: &Path,
) -> Result<usize, BackupError> {
    fs::create_dir_all(stage.join(OBJECTS))
        .map_err(io_error(|| format!("Could not create {}", stage.display())))?;
    for file in &manifest.files {
        let source = from.join(&file.path);
        let copied = copy_hashed(&source, &stage.join(&file.path)).map_err(|err| {
            if err.kind() == io::ErrorKind::NotFound && !source.exists() {
                BackupError::InvalidBackup {
                    path: from.to_path_buf(),
                    reason: format!("{} is missing", file.path),
                }
            } else {
                io_error(|| format!("Could not copy {}", source.display()))(err)
            }
        })?;
        if copied != (file.size, file.sha256.clone()) {
            return Err(BackupError::InvalidBackup {
                path: from.to_path_buf(),
                reason: format!(
                    "{} does not match its checksum in {MANIFEST} (changed or damaged since the \
                     backup)",
                    file.path
                ),
            });
        }
    }
    if !manifest.database {
        return Ok(0);
    }
    prepare_database(&stage.join(DB_FILE), &state.db_path(), from)
}

/// Checks the staged database and signs everyone out: a new session epoch (pages drop the
/// sessions they stored) and the sessions of the database it replaces revoked, so that their
/// access tokens, which the kept signing keys still verify, are refused until they expire.
fn prepare_database(staged: &Path, current: &Path, from: &Path) -> Result<usize, BackupError> {
    let invalid = |reason: String| BackupError::InvalidBackup {
        path: from.to_path_buf(),
        reason,
    };
    let context = || {
        format!(
            "Could not prepare the restored database {}",
            staged.display()
        )
    };
    let conn = Connection::open_with_flags(
        staged,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(sql_error(context))?;
    let check: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|err| invalid(format!("its database is damaged ({err})")))?;
    if check != "ok" {
        return Err(invalid(format!("its database is damaged ({check})")));
    }
    if !has_table(&conn, "_rl_migrations").map_err(sql_error(context))? {
        return Err(invalid(
            "its database is not a rayfin-local database".into(),
        ));
    }
    let mut signed_out = 0;
    if has_table(&conn, "_rl_auth_sessions").map_err(sql_error(context))? && current.is_file() {
        let now = OffsetDateTime::now_utc().unix_timestamp() * 1000;
        conn.execute(
            "ATTACH DATABASE ?1 AS replaced",
            [current.to_string_lossy()],
        )
        .map_err(sql_error(context))?;
        let replaced_has_sessions: bool = conn
            .query_row(
                "SELECT 1 FROM replaced.sqlite_master WHERE type = 'table' \
                 AND name = '_rl_auth_sessions'",
                [],
                |_| Ok(()),
            )
            .optional()
            .map_err(sql_error(context))?
            .is_some();
        if replaced_has_sessions {
            signed_out = conn
                .execute(
                    "INSERT OR IGNORE INTO main._rl_auth_sessions
                       (id, user_id, created_at, expires_at, last_refresh_at, revoked_at)
                     SELECT id, user_id, created_at, expires_at, last_refresh_at,
                            COALESCE(revoked_at, ?1)
                     FROM replaced._rl_auth_sessions",
                    [now],
                )
                .map_err(sql_error(context))?;
        }
        conn.execute("DETACH DATABASE replaced", [])
            .map_err(sql_error(context))?;
    }
    if has_table(&conn, "_rl_auth_session_epoch").map_err(sql_error(context))? {
        conn.execute(
            "INSERT OR REPLACE INTO _rl_auth_session_epoch (id, epoch)
             VALUES (1, lower(hex(randomblob(8))))",
            [],
        )
        .map_err(sql_error(context))?;
    }
    conn.close().map_err(|(_, err)| sql_error(context)(err))?;
    Ok(signed_out)
}

/// Moves the current data aside and the staged data into place, moving the current data back if
/// that fails.
fn swap_in(state: &LocalState, stage: &Path) -> Result<(), BackupError> {
    let dir = state.dir();
    let old = PathBuf::from(format!("{}-old", stage.display()));
    fs::create_dir(&old).map_err(io_error(|| format!("Could not create {}", old.display())))?;
    let data: Vec<String> = ["", "-wal", "-shm", "-journal"]
        .iter()
        .map(|suffix| format!("{DB_FILE}{suffix}"))
        .chain(["storage".to_owned()])
        .collect();
    let mut moved_aside = Vec::new();
    let mut moved_in = Vec::new();
    let result = (|| {
        for name in &data {
            match fs::rename(dir.join(name), old.join(name)) {
                Ok(()) => moved_aside.push(name.clone()),
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(io_error(|| {
                        format!(
                            "Could not move {} aside. Close programs that have the project's \
                             database or files open, then try again.",
                            dir.join(name).display()
                        )
                    })(err));
                }
            }
        }
        for name in [DB_FILE, "storage"] {
            match fs::rename(stage.join(name), dir.join(name)) {
                Ok(()) => moved_in.push(name),
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(io_error(|| {
                        format!("Could not move the restored {name} into place")
                    })(err));
                }
            }
        }
        Ok(())
    })();
    if result.is_err() {
        for name in moved_in {
            let path = dir.join(name);
            let _ = fs::remove_dir_all(&path).or_else(|_| fs::remove_file(&path));
        }
        let mut restored = true;
        for name in &moved_aside {
            restored &= fs::rename(old.join(name), dir.join(name)).is_ok();
        }
        if !restored {
            // Never delete the previous data: say where it is.
            return result.map_err(|err| BackupError::Io {
                context: format!(
                    "{err}. The previous data could not be moved back either; it is in {}: move \
                     its contents into {} before running serve.",
                    old.display(),
                    dir.display()
                ),
                source: io::Error::other("restore interrupted"),
            });
        }
    }
    let storage = state.storage_dir();
    if result.is_ok() && !storage.exists() {
        let _ = fs::create_dir_all(&storage);
    }
    let _ = fs::remove_dir_all(&old);
    result
}

#[cfg(test)]
mod tests;
