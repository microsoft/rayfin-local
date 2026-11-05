//! The state folder: `.rayfin-local/` at the project root, or the folder given with
//! `--state-dir` (`RAYFIN_LOCAL_STATE_DIR`).
//!
//! ```text
//! .rayfin-local/
//!   .gitignore     `*`, so nothing in here is ever committed (written inside the project or a
//!                  git working tree)
//!   state.json     app id, publishable key, local workspace/item/tenant ids, last ports
//!   db.sqlite      the app's data, users and sessions
//!   keys/          token signing keys
//!   storage/       uploaded files
//!   cache/         compiled analysis output, extracted helper scripts
//!   secrets.json   local values of the secrets declared in rayfin.yml
//!   serve.lock     locked while `rayfin-local serve` runs the project
//!   serve.json     who holds that lock: process id and URLs
//! ```

use std::collections::BTreeMap;
use std::fs::TryLockError;
use std::io::Write;
use std::path::{Path, PathBuf};

use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

use crate::{EnvFile, Project, ProjectError};

/// The state folder's name, at the project root.
pub const STATE_DIR: &str = ".rayfin-local";
/// Prefix of the publishable keys rayfin-local generates.
pub const PUBLISHABLE_KEY_PREFIX: &str = "pk-local-";

const STATE_FILE: &str = "state.json";
const SERVE_LOCK_FILE: &str = "serve.lock";
const SERVE_INFO_FILE: &str = "serve.json";
const SUBDIRECTORIES: [&str; 3] = ["keys", "storage", "cache"];
/// Everything rayfin-local keeps in a state folder; `reset` deletes these and nothing else from
/// a folder given with `--state-dir`.
const STATE_ENTRIES: [&str; 12] = [
    "db.sqlite",
    "db.sqlite-wal",
    "db.sqlite-shm",
    "db.sqlite-journal",
    STATE_FILE,
    "secrets.json",
    "keys",
    "storage",
    "cache",
    SERVE_LOCK_FILE,
    SERVE_INFO_FILE,
    ".gitignore",
];
/// The name prefix of rayfin-local's temporary entries in a state folder (a restore's staging),
/// which `reset` deletes too.
pub const TEMP_PREFIX: &str = ".rl-";

/// The contents of `state.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateData {
    /// The local app's id (a UUID).
    pub app_id: String,
    /// The key the frontend sends as `X-Publishable-Key`: `pk-local-` and 24 letters or digits.
    pub publishable_key: String,
    /// The Fabric workspace the app pretends to live in (from `rayfin/.env` when the app was
    /// deployed, else a generated UUID).
    pub workspace_id: String,
    /// The app's Fabric item id (from `rayfin/.env` when the app was deployed, else generated).
    pub item_id: String,
    /// The Entra tenant (from `rayfin/.env` when known).
    pub tenant_id: Option<String>,
    /// The backend port of the last run.
    pub api_port: Option<u16>,
    /// The frontend port of the last run.
    pub frontend_port: Option<u16>,
}

/// `state.json` as stored; missing values are filled in on load.
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredState {
    app_id: Option<String>,
    publishable_key: Option<String>,
    workspace_id: Option<String>,
    item_id: Option<String>,
    tenant_id: Option<String>,
    api_port: Option<u16>,
    frontend_port: Option<u16>,
}

/// The `.rayfin-local/` folder of a project and its `state.json`.
#[derive(Debug, Clone)]
pub struct LocalState {
    dir: PathBuf,
    data: StateData,
}

impl LocalState {
    /// Opens the project's state folder, creating it on first use (with its subfolders,
    /// `state.json` and, where git could see it, a `.gitignore`). Values missing from
    /// `state.json` are filled in and saved.
    pub fn load_or_create(project: &Project) -> Result<Self, ProjectError> {
        let dir = project.state_dir();
        prepare_dir(&dir, &DirChecks::of(project))?;
        Self::load_in(&dir, project.env())
    }

    /// [`LocalState::load_or_create`] for `serve`: the `serve` lock is taken (and the process
    /// recorded in `serve.json`) before `state.json` is read or created, so two first runs
    /// can't create different app ids and publishable keys.
    pub fn open_for_serve(project: &Project) -> Result<(Self, ServeLock), ProjectError> {
        let (state, lock) = Self::open_locked(project)?;
        lock.publish(&ServeInfo {
            pid: std::process::id(),
            ..ServeInfo::default()
        })?;
        Ok((state, lock))
    }

    /// [`LocalState::load_or_create`] with the `serve` lock taken first and not published: for
    /// commands that replace the data (`restore`), which no `serve` may run during. Fails with
    /// [`ProjectError::AlreadyServing`] while `serve` runs the project.
    pub fn open_locked(project: &Project) -> Result<(Self, ServeLock), ProjectError> {
        let dir = project.state_dir();
        prepare_dir(&dir, &DirChecks::of(project))?;
        let lock = ServeLock::acquire(&dir)?;
        let state = Self::load_in(&dir, project.env())?;
        Ok((state, lock))
    }

    #[cfg(test)]
    fn load_or_create_in(dir: &Path, env: &EnvFile) -> Result<Self, ProjectError> {
        prepare_dir(
            dir,
            &DirChecks {
                gitignore: true,
                foreign: false,
            },
        )?;
        Self::load_in(dir, env)
    }

    /// Reads `state.json` in the prepared folder `dir`, creating it or filling in missing values.
    fn load_in(dir: &Path, env: &EnvFile) -> Result<Self, ProjectError> {
        // Two processes may create `state.json` at once; the second reads the first's.
        let mut attempts = 0;
        loop {
            attempts += 1;
            let state = Self::read_or_fill(dir, env)?;
            let (state, existed, changed) = state;
            if existed {
                if changed {
                    state.save()?;
                }
                return Ok(state);
            }
            match state.save_new() {
                Ok(()) => return Ok(state),
                Err(ProjectError::Write { source, .. })
                    if source.kind() == std::io::ErrorKind::AlreadyExists && attempts < 3 => {}
                Err(err) => return Err(err),
            }
        }
    }

    /// The state in `dir`'s `state.json` with missing values filled in, whether the file
    /// existed, and whether values were filled in.
    fn read_or_fill(dir: &Path, env: &EnvFile) -> Result<(Self, bool, bool), ProjectError> {
        let path = dir.join(STATE_FILE);
        let (stored, existed) = match std::fs::read_to_string(&path) {
            Ok(content) => {
                let stored = serde_json::from_str::<StoredState>(&content).map_err(|err| {
                    ProjectError::InvalidState {
                        path: path.clone(),
                        message: err.to_string(),
                    }
                })?;
                (stored, true)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                (StoredState::default(), false)
            }
            Err(source) => return Err(ProjectError::Read { path, source }),
        };
        let mut changed = false;
        let mut fill = |value: Option<String>, default: &dyn Fn() -> String| {
            value.filter(|v| !v.trim().is_empty()).unwrap_or_else(|| {
                changed = true;
                default()
            })
        };
        let new_uuid = || uuid::Uuid::new_v4().to_string();
        let app_id = fill(stored.app_id, &new_uuid);
        let publishable_key = fill(stored.publishable_key, &|| {
            format!("{PUBLISHABLE_KEY_PREFIX}{}", random_alphanumeric(24))
        });
        let workspace_id = fill(stored.workspace_id, &|| {
            env.workspace_id().map_or_else(new_uuid, str::to_string)
        });
        let item_id = fill(stored.item_id, &|| {
            env.item_id().map_or_else(new_uuid, str::to_string)
        });
        let tenant_id = stored
            .tenant_id
            .filter(|t| !t.trim().is_empty())
            .or_else(|| {
                let tenant = env.tenant_id().map(str::to_string);
                changed |= tenant.is_some();
                tenant
            });

        let state = Self {
            dir: dir.to_path_buf(),
            data: StateData {
                app_id,
                publishable_key,
                workspace_id,
                item_id,
                tenant_id,
                api_port: stored.api_port,
                frontend_port: stored.frontend_port,
            },
        };
        Ok((state, existed, changed))
    }

    /// Writes `state.json` (atomically).
    pub fn save(&self) -> Result<(), ProjectError> {
        let path = self.state_path();
        let json = self.state_json()?;
        write_atomic(&path, json.as_bytes()).map_err(|source| ProjectError::Write { path, source })
    }

    /// Creates `state.json`, failing with [`std::io::ErrorKind::AlreadyExists`] when another
    /// process just did.
    fn save_new(&self) -> Result<(), ProjectError> {
        let path = self.state_path();
        let json = self.state_json()?;
        create_atomic(&path, json.as_bytes()).map_err(|source| ProjectError::Write { path, source })
    }

    fn state_json(&self) -> Result<String, ProjectError> {
        let mut json =
            serde_json::to_string_pretty(&self.data).map_err(|err| ProjectError::InvalidState {
                path: self.state_path(),
                message: err.to_string(),
            })?;
        json.push('\n');
        Ok(json)
    }

    /// Deletes the project's state (database, users, keys, files, secrets). The default
    /// `.rayfin-local/` folder goes entirely; from a folder given with `--state-dir`, only what
    /// rayfin-local keeps there is deleted, and the folder itself when that leaves it empty.
    /// Returns whether there was a state folder. Refused while `rayfin-local serve` runs the
    /// project.
    pub fn reset(project: &Project) -> Result<bool, ProjectError> {
        let dir = project.state_dir();
        ensure_not_serving(&dir)?;
        if !dir.exists() {
            return Ok(false);
        }
        let failed = |source| ProjectError::Reset {
            path: dir.clone(),
            source,
        };
        // The database goes first, since another program may hold it open, and `.gitignore`
        // last: a failure never leaves the keys or secrets in a folder git would pick up.
        for suffix in ["", "-wal", "-shm", "-journal"] {
            match std::fs::remove_file(dir.join(format!("db.sqlite{suffix}"))) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(failed(err)),
                _ => {}
            }
        }
        let everything = project.has_default_state_dir();
        for entry in std::fs::read_dir(&dir).map_err(failed)? {
            let entry = entry.map_err(failed)?;
            let name = entry.file_name();
            let ours = name
                .to_str()
                .is_some_and(|name| STATE_ENTRIES.contains(&name) || name.starts_with(TEMP_PREFIX));
            if name == ".gitignore" || !(everything || ours) {
                continue;
            }
            let removed = if entry.file_type().map_err(failed)?.is_dir() {
                std::fs::remove_dir_all(entry.path())
            } else {
                std::fs::remove_file(entry.path())
            };
            removed.map_err(failed)?;
        }
        if everything {
            std::fs::remove_dir_all(&dir).map_err(failed)?;
        } else {
            match std::fs::remove_file(dir.join(".gitignore")) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(failed(err)),
                _ => {}
            }
            // Folders the developer keeps their own files in stay.
            let _ = std::fs::remove_dir(&dir);
        }
        Ok(true)
    }

    /// Takes the project's `serve` lock, so that a second `rayfin-local serve` (or a `reset`)
    /// can't run on the project at the same time, and records this process in `serve.json`.
    /// Fails with [`ProjectError::AlreadyServing`] while another process holds it. The operating
    /// system releases the lock when this process ends, so a crash leaves no stale lock.
    pub fn lock_serve(&self) -> Result<ServeLock, ProjectError> {
        let lock = ServeLock::acquire(&self.dir)?;
        lock.publish(&ServeInfo {
            pid: std::process::id(),
            ..ServeInfo::default()
        })?;
        Ok(lock)
    }

    /// The contents of `state.json`.
    pub fn data(&self) -> &StateData {
        &self.data
    }

    /// For updates (such as the ports of this run); call [`LocalState::save`] after.
    pub fn data_mut(&mut self) -> &mut StateData {
        &mut self.data
    }

    /// `<root>/.rayfin-local`.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `.rayfin-local/state.json`.
    pub fn state_path(&self) -> PathBuf {
        self.dir.join(STATE_FILE)
    }

    /// `.rayfin-local/db.sqlite`.
    pub fn db_path(&self) -> PathBuf {
        self.dir.join("db.sqlite")
    }

    /// `.rayfin-local/keys/` (created by [`LocalState::load_or_create`]).
    pub fn keys_dir(&self) -> PathBuf {
        self.dir.join("keys")
    }

    /// `.rayfin-local/storage/` (created by [`LocalState::load_or_create`]).
    pub fn storage_dir(&self) -> PathBuf {
        self.dir.join("storage")
    }

    /// `.rayfin-local/cache/` (created by [`LocalState::load_or_create`]).
    pub fn cache_dir(&self) -> PathBuf {
        self.dir.join("cache")
    }

    /// `.rayfin-local/secrets.json`: a JSON object of secret name → value.
    pub fn secrets_path(&self) -> PathBuf {
        self.dir.join("secrets.json")
    }

    /// The local secret values (empty when `secrets.json` does not exist).
    pub fn load_secrets(&self) -> Result<BTreeMap<String, String>, ProjectError> {
        let path = self.secrets_path();
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                serde_json::from_str(&content).map_err(|err| ProjectError::InvalidState {
                    path,
                    message: err.to_string(),
                })
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(source) => Err(ProjectError::Read { path, source }),
        }
    }

    /// Replaces the local secret values (atomically). On Unix, only the owner may read the file.
    pub fn save_secrets(&self, secrets: &BTreeMap<String, String>) -> Result<(), ProjectError> {
        let path = self.secrets_path();
        let mut json =
            serde_json::to_string_pretty(secrets).map_err(|err| ProjectError::InvalidState {
                path: path.clone(),
                message: err.to_string(),
            })?;
        json.push('\n');
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        replace_atomically(&path, json.as_bytes(), &options)
            .map_err(|source| ProjectError::Write { path, source })
    }
}

fn create_dir(dir: &Path) -> Result<(), ProjectError> {
    std::fs::create_dir_all(dir).map_err(|source| ProjectError::Write {
        path: dir.to_path_buf(),
        source,
    })
}

/// What [`prepare_dir`] does besides creating the folder.
struct DirChecks {
    /// Write a `.gitignore` of `*` when there is none.
    gitignore: bool,
    /// Refuse a folder that has files but no rayfin-local state (a `--state-dir` typo, such as
    /// a home folder), which `reset` would otherwise share with the developer's files.
    foreign: bool,
}

impl DirChecks {
    /// The default `.rayfin-local/`, and any state folder inside the project or a git working
    /// tree, gets a `.gitignore`, so the keys and secrets in it are never committed; elsewhere
    /// (a folder outside any repository) nothing but the state is written. Folders given with
    /// `--state-dir` must be empty or hold rayfin-local state.
    fn of(project: &Project) -> Self {
        let dir = project.state_dir();
        let default = project.has_default_state_dir();
        Self {
            gitignore: default || dir.starts_with(project.root()) || in_git_work_tree(&dir),
            foreign: !default,
        }
    }
}

/// Whether `dir` or one of its parents has a `.git`.
fn in_git_work_tree(dir: &Path) -> bool {
    dir.ancestors().any(|dir| dir.join(".git").exists())
}

/// Creates the state folder `dir` with its subfolders (and `.gitignore`).
fn prepare_dir(dir: &Path, checks: &DirChecks) -> Result<(), ProjectError> {
    if checks.foreign
        && let Ok(entries) = std::fs::read_dir(dir)
    {
        let names: Vec<_> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .collect();
        let ours = names.iter().any(|name| {
            name.to_str()
                .is_some_and(|name| name != ".gitignore" && STATE_ENTRIES.contains(&name))
        });
        if !names.is_empty() && !ours {
            return Err(ProjectError::InvalidStateDir {
                path: dir.to_path_buf(),
                reason: "it has files, but none of rayfin-local's (state.json, db.sqlite, keys/, \
                         storage/)"
                    .into(),
            });
        }
    }
    create_state_dir(dir)?;
    let gitignore = dir.join(".gitignore");
    if checks.gitignore && !gitignore.exists() {
        write_atomic(&gitignore, b"*\n").map_err(|source| ProjectError::Write {
            path: gitignore,
            source,
        })?;
    }
    for name in SUBDIRECTORIES {
        create_dir(&dir.join(name))?;
    }
    Ok(())
}

/// Creates the state folder `dir`. On Unix, only its owner may open it, which keeps the keys, the
/// database and the secrets in it private; a folder an earlier version made open to others is
/// closed.
fn create_state_dir(dir: &Path) -> Result<(), ProjectError> {
    let failed = |source| ProjectError::Write {
        path: dir.to_path_buf(),
        source,
    };
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir).map_err(failed)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir).map_err(failed)?.permissions().mode();
        if mode & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .map_err(failed)?;
        }
    }
    Ok(())
}

/// Who is serving a project (`.rayfin-local/serve.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServeInfo {
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_url: Option<String>,
}

/// The project's `serve` lock, from [`LocalState::lock_serve`]; released on drop.
#[derive(Debug)]
pub struct ServeLock {
    info_path: PathBuf,
    /// The locked `serve.lock`; `None` where the file system can't lock files.
    file: Option<std::fs::File>,
}

impl ServeLock {
    /// Locks `dir/serve.lock` (the folder must exist). Fails with
    /// [`ProjectError::AlreadyServing`] while another process holds it. On a file system that
    /// can't lock files, the lock is not exclusive ([`ServeLock::is_exclusive`]).
    fn acquire(dir: &Path) -> Result<Self, ProjectError> {
        let path = dir.join(SERVE_LOCK_FILE);
        let info_path = dir.join(SERVE_INFO_FILE);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|source| ProjectError::Write {
                path: path.clone(),
                source,
            })?;
        match try_lock(&file) {
            Ok(()) => Ok(Self {
                info_path,
                file: Some(file),
            }),
            Err(TryLockError::WouldBlock) => Err(ProjectError::AlreadyServing {
                info: read_serve_info(&info_path),
            }),
            Err(TryLockError::Error(err)) => {
                tracing::debug!("could not lock {}: {err}", path.display());
                Ok(Self {
                    info_path,
                    file: None,
                })
            }
        }
    }

    /// Whether the lock keeps other processes out. `false` on a file system that can't lock
    /// files (some network shares and mounted folders): then nothing stops a second `serve`, a
    /// `reset` or a `restore` from running on the same state folder at the same time.
    pub fn is_exclusive(&self) -> bool {
        self.file.is_some()
    }
    /// Records who is serving, for the error a second `serve` or a `reset` reports.
    pub fn publish(&self, info: &ServeInfo) -> Result<(), ProjectError> {
        let write = |source| ProjectError::Write {
            path: self.info_path.clone(),
            source,
        };
        let json = serde_json::to_vec_pretty(info).map_err(|err| write(err.into()))?;
        write_atomic(&self.info_path, &json).map_err(write)
    }
}

impl Drop for ServeLock {
    fn drop(&mut self) {
        // Runs before `file` closes, so only the lock's holder removes the record.
        if self.file.is_some() {
            let _ = std::fs::remove_file(&self.info_path);
        }
    }
}

/// Fails with [`ProjectError::AlreadyServing`] while a `rayfin-local serve` holds the lock of the
/// state folder `dir`.
fn ensure_not_serving(dir: &Path) -> Result<(), ProjectError> {
    let Ok(file) = std::fs::OpenOptions::new()
        .write(true)
        .open(dir.join(SERVE_LOCK_FILE))
    else {
        return Ok(());
    };
    // A lock taken here is released when `file` closes.
    match try_lock(&file) {
        Err(TryLockError::WouldBlock) => Err(ProjectError::AlreadyServing {
            info: read_serve_info(&dir.join(SERVE_INFO_FILE)),
        }),
        _ => Ok(()),
    }
}

fn read_serve_info(path: &Path) -> Option<ServeInfo> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

#[cfg(test)]
thread_local! {
    /// Tests: behave as on a file system that can't lock files.
    static NO_FILE_LOCKS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn try_lock(file: &std::fs::File) -> Result<(), TryLockError> {
    #[cfg(test)]
    if NO_FILE_LOCKS.with(std::cell::Cell::get) {
        return Err(TryLockError::Error(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "no file locks (simulated)",
        )));
    }
    file.try_lock()
}

/// Creates `path` with `contents` atomically, failing with
/// [`std::io::ErrorKind::AlreadyExists`] when it exists: the bytes go to a temporary file that
/// is then hard-linked to `path` (renamed where the file system has no hard links, which can
/// replace a file created at the same moment).
fn create_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let temp = temp_sibling(path)?;
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        match std::fs::hard_link(&temp, path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Err(err),
            Err(_) if path.exists() => Err(std::io::ErrorKind::AlreadyExists.into()),
            Err(_) => std::fs::rename(&temp, path),
        }
    })();
    let _ = std::fs::remove_file(&temp);
    result
}

fn temp_sibling(path: &Path) -> std::io::Result<PathBuf> {
    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the path has no file name",
        )
    })?;
    let mut temp_name = std::ffi::OsString::from(".");
    temp_name.push(file_name);
    temp_name.push(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
    Ok(path.with_file_name(temp_name))
}

/// Replaces `path` with `contents` atomically: the bytes go to a temporary file in the same
/// folder, which is then renamed over `path`, so readers never see a partial file.
pub fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    replace_atomically(
        path,
        contents,
        std::fs::OpenOptions::new().write(true).create_new(true),
    )
}

/// [`write_atomic`] with the temporary file opened by `options` (which create it, for writing).
fn replace_atomically(
    path: &Path,
    contents: &[u8],
    options: &std::fs::OpenOptions,
) -> std::io::Result<()> {
    let temp = temp_sibling(path)?;
    let result = (|| {
        let mut file = options.open(&temp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

fn random_alphanumeric(len: usize) -> String {
    const ALPHABET: &[u8; 62] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let rng = SystemRandom::new();
    let mut out = String::with_capacity(len);
    let mut bytes = [0u8; 32];
    while out.len() < len {
        rng.fill(&mut bytes)
            .expect("the operating system's random number generator failed");
        // 248 = 4 × 62: rejecting larger bytes keeps every character equally likely.
        for byte in bytes.iter().filter(|b| **b < 248).take(len - out.len()) {
            out.push(char::from(ALPHABET[usize::from(byte % 62)]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(content: &str) -> EnvFile {
        EnvFile::from_content(Path::new("rayfin/.env"), content)
    }

    #[test]
    fn creates_the_folder_and_persists_state() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(STATE_DIR);
        let env = env(
            "RAYFIN_PUBLIC_WORKSPACE_ID=ws-1\nRAYFIN_PUBLIC_ITEM_ID=item-1\n\
                       RAYFIN_PUBLIC_TENANT_ID=tenant-1\n",
        );

        let mut state = LocalState::load_or_create_in(&dir, &env).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join(".gitignore")).unwrap(),
            "*\n"
        );
        assert!(state.keys_dir().is_dir() && state.storage_dir().is_dir());
        assert!(state.cache_dir().is_dir());
        let data = state.data().clone();
        assert_eq!(
            (
                data.workspace_id.as_str(),
                data.item_id.as_str(),
                data.tenant_id.as_deref()
            ),
            ("ws-1", "item-1", Some("tenant-1"))
        );
        assert!(uuid::Uuid::parse_str(&data.app_id).is_ok());
        let key = data
            .publishable_key
            .strip_prefix(PUBLISHABLE_KEY_PREFIX)
            .unwrap();
        assert!(
            key.len() == 24 && key.bytes().all(|b| b.is_ascii_alphanumeric()),
            "{key}"
        );

        state.data_mut().api_port = Some(7171);
        state.save().unwrap();
        let reloaded = LocalState::load_or_create_in(&dir, &EnvFile::default()).unwrap();
        assert_eq!(
            reloaded.data(),
            &StateData {
                api_port: Some(7171),
                ..data
            }
        );
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(state.state_path()).unwrap()).unwrap();
        assert_eq!(json["apiPort"], 7171);
        assert_eq!(
            json["publishableKey"],
            state.data().publishable_key.as_str()
        );
    }

    #[test]
    fn one_serve_at_a_time() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(STATE_DIR);
        let state = LocalState::load_or_create_in(&dir, &EnvFile::default()).unwrap();
        assert!(ensure_not_serving(&dir).is_ok());

        let lock = state.lock_serve().unwrap();
        lock.publish(&ServeInfo {
            pid: 42,
            app_url: Some("http://localhost:5173/".into()),
            api_url: Some("http://localhost:7171".into()),
        })
        .unwrap();
        let message = state.lock_serve().unwrap_err().to_string();
        assert!(
            message.contains("(process 42: app http://localhost:5173/, API http://localhost:7171)"),
            "{message}"
        );
        assert!(matches!(
            ensure_not_serving(&dir),
            Err(ProjectError::AlreadyServing { .. })
        ));

        drop(lock);
        assert!(!dir.join(SERVE_INFO_FILE).exists());
        assert!(ensure_not_serving(&dir).is_ok());
        drop(state.lock_serve().unwrap());
    }

    #[test]
    fn concurrent_first_runs_agree_on_the_state() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(STATE_DIR);
        prepare_dir(
            &dir,
            &DirChecks {
                gitignore: false,
                foreign: false,
            },
        )
        .unwrap();
        let barrier = std::sync::Barrier::new(8);
        let states: Vec<StateData> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        LocalState::load_in(&dir, &EnvFile::default()).unwrap().data
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let on_disk = LocalState::load_in(&dir, &EnvFile::default()).unwrap().data;
        assert!(states.iter().all(|s| s == &on_disk), "{states:#?}");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| n.to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn without_file_locks_the_lock_is_not_exclusive() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(STATE_DIR);
        let state = LocalState::load_or_create_in(&dir, &EnvFile::default()).unwrap();
        NO_FILE_LOCKS.with(|c| c.set(true));
        let first = state.lock_serve().unwrap();
        let second = state.lock_serve().unwrap();
        assert!(ensure_not_serving(&dir).is_ok());
        NO_FILE_LOCKS.with(|c| c.set(false));
        assert!(!first.is_exclusive() && !second.is_exclusive());
        drop((first, second));
        assert!(state.lock_serve().unwrap().is_exclusive());
    }

    #[test]
    fn generates_stable_ids_without_a_deployment() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(STATE_DIR);
        let first = LocalState::load_or_create_in(&dir, &EnvFile::default()).unwrap();
        assert!(uuid::Uuid::parse_str(&first.data().workspace_id).is_ok());
        assert_ne!(first.data().workspace_id, first.data().item_id);
        assert_eq!(first.data().tenant_id, None);
        let second = LocalState::load_or_create_in(&dir, &EnvFile::default()).unwrap();
        assert_eq!(first.data(), second.data());
    }

    #[test]
    fn fills_in_missing_values_and_keeps_the_rest() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(STATE_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".gitignore"), "custom\n").unwrap();
        std::fs::write(
            dir.join(STATE_FILE),
            r#"{"appId":"app-1","frontendPort":5180}"#,
        )
        .unwrap();

        let state =
            LocalState::load_or_create_in(&dir, &env("RAYFIN_PUBLIC_TENANT_ID=t\n")).unwrap();
        assert_eq!(state.data().app_id, "app-1");
        assert_eq!(state.data().frontend_port, Some(5180));
        assert_eq!(state.data().tenant_id.as_deref(), Some("t"));
        assert!(
            state
                .data()
                .publishable_key
                .starts_with(PUBLISHABLE_KEY_PREFIX)
        );
        let saved = std::fs::read_to_string(dir.join(STATE_FILE)).unwrap();
        assert!(saved.contains(&state.data().publishable_key), "{saved}");
        assert_eq!(
            std::fs::read_to_string(dir.join(".gitignore")).unwrap(),
            "custom\n"
        );
    }

    #[test]
    fn rejects_a_corrupt_state_file() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(STATE_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(STATE_FILE), "{").unwrap();
        let err = LocalState::load_or_create_in(&dir, &EnvFile::default()).unwrap_err();
        assert!(err.to_string().contains("rayfin-local reset"), "{err}");
    }

    #[test]
    fn round_trips_secrets() {
        let temp = tempfile::tempdir().unwrap();
        let state =
            LocalState::load_or_create_in(&temp.path().join(STATE_DIR), &EnvFile::default())
                .unwrap();
        assert!(state.load_secrets().unwrap().is_empty());
        let secrets = BTreeMap::from([("API_KEY".to_string(), "s3cret".to_string())]);
        state.save_secrets(&secrets).unwrap();
        assert_eq!(state.load_secrets().unwrap(), secrets);
    }

    #[cfg(unix)]
    #[test]
    fn the_state_folder_and_the_secrets_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(STATE_DIR);
        let state = LocalState::load_or_create_in(&dir, &EnvFile::default()).unwrap();
        state.save_secrets(&BTreeMap::new()).unwrap();
        fn mode(path: &Path) -> u32 {
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777
        }
        assert_eq!((mode(&dir), mode(&state.secrets_path())), (0o700, 0o600));

        // A folder an earlier version left open to others is closed.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        LocalState::load_or_create_in(&dir, &EnvFile::default()).unwrap();
        assert_eq!(mode(&dir), 0o700);
    }

    #[test]
    fn write_atomic_replaces_files_without_leftovers() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("file.json");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }
}
