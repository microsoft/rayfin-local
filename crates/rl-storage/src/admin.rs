//! Stored files as a host's control API reads and deletes them, whatever the folders' rules:
//! each folder's files and their bytes, pages of a folder's files, one file and where its bytes
//! are, and deleting one file or all of a folder's. Before the storage service first starts
//! there are no storage tables, and so no files. [`Replacement`] stores seed files in place of
//! every stored file.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::http::HeaderValue;
use ring::digest;
use rl_db::Db;
use rl_model::{AppModel, StorageFolder};
use rusqlite::Connection;
use serde::Serialize;
use serde_json::{Map, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::store::{self, Object};
use crate::{StorageConfig, handlers, names};

/// The most files [`files`] reads at once.
pub const MAX_FILES: u32 = 500;

/// A storage folder: one the app's model declares, or one that only stored files still name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Folder {
    /// The folder's name, as the client names it.
    pub name: String,
    /// Its `@blob` class's name; `None` when the model no longer declares the folder.
    pub display_name: Option<String>,
    pub files: u64,
    /// The size of its files together.
    pub bytes: u64,
}

/// A page of a folder's files, in path order.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Files {
    /// Each as `GET /api/storage/list` lists it, plus its `ownerId`.
    pub files: Vec<Value>,
    /// How many files the folder holds.
    pub total: u64,
}

/// A stored file: its listing, and the file on disk that holds its bytes.
#[derive(Clone, Debug)]
pub struct StoredFile {
    /// As [`Files::files`] lists it.
    pub listing: Value,
    pub content_type: String,
    pub size: u64,
    pub bytes: PathBuf,
}

/// Why stored files could not be read, deleted or replaced.
#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    #[error("the storage database failed: {0}")]
    Database(String),
    #[error("could not copy {} into storage: {source}", path.display())]
    Copy {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl From<rusqlite::Error> for AdminError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error.to_string())
    }
}

impl From<rl_db::DbError> for AdminError {
    fn from(error: rl_db::DbError) -> Self {
        Self::Database(error.to_string())
    }
}

/// The folders `model` declares, in its order, then those only stored files name, by name.
pub async fn folders(db: &Db, model: &AppModel) -> Result<Vec<Folder>, AdminError> {
    let totals = db
        .call(|conn| -> rusqlite::Result<_> {
            if !store::has_tables(conn)? {
                return Ok(Vec::new());
            }
            store::folder_totals(conn)
        })
        .await??;
    let declared = model.storage.iter().flat_map(|storage| &storage.folders);
    let total = |name: &str| {
        totals
            .iter()
            .find(|(folder, ..)| folder == name)
            .map_or((0, 0), |&(_, files, bytes)| (files, bytes))
    };
    let mut folders: Vec<Folder> = declared
        .map(|folder| {
            let (files, bytes) = total(&folder.name);
            Folder {
                name: folder.name.clone(),
                display_name: Some(folder.display_name.clone()),
                files,
                bytes,
            }
        })
        .collect();
    for (name, files, bytes) in &totals {
        if !folders.iter().any(|folder| folder.name == *name) {
            folders.push(Folder {
                name: name.clone(),
                display_name: None,
                files: *files,
                bytes: *bytes,
            });
        }
    }
    Ok(folders)
}

/// `limit` (1 to [`MAX_FILES`]) of `folder`'s files in path order, after skipping `offset`.
pub async fn files(db: &Db, folder: &str, offset: u64, limit: u32) -> Result<Files, AdminError> {
    let folder = folder.to_owned();
    let limit = limit.clamp(1, MAX_FILES);
    let (objects, total) = db
        .call(move |conn| -> rusqlite::Result<_> {
            if !store::has_tables(conn)? {
                return Ok((Vec::new(), 0));
            }
            let objects = store::objects_page(conn, &folder, offset, limit)?;
            Ok((objects, store::count_objects(conn, &folder)?))
        })
        .await??;
    Ok(Files {
        files: objects.iter().map(listing).collect(),
        total,
    })
}

/// The file at `path` in `folder`, with its bytes under `root` (the storage folder of
/// [`crate::StorageConfig::root`]); `None` when there is none.
pub async fn file(
    db: &Db,
    root: &Path,
    folder: &str,
    path: &str,
) -> Result<Option<StoredFile>, AdminError> {
    let (folder, path) = (folder.to_owned(), path.to_owned());
    let object = db
        .call(move |conn| -> rusqlite::Result<_> {
            if !store::has_tables(conn)? {
                return Ok(None);
            }
            store::object_at(conn, &folder, &path)
        })
        .await??;
    Ok(object.map(|object| StoredFile {
        listing: listing(&object),
        content_type: object.content_type.clone(),
        size: object.size,
        bytes: root.join("objects").join(&object.blob),
    }))
}

/// Deletes the file at `path` in `folder`, under `root`; `false` when there was none.
pub async fn delete_file(
    db: &Db,
    root: &Path,
    folder: &str,
    path: &str,
) -> Result<bool, AdminError> {
    let (folder, path) = (folder.to_owned(), path.to_owned());
    let blob = db
        .call(move |conn| -> rusqlite::Result<_> {
            if !store::has_tables(conn)? {
                return Ok(None);
            }
            match store::object_at(conn, &folder, &path)? {
                Some(object) => store::delete_object(conn, &object.id),
                None => Ok(None),
            }
        })
        .await??;
    if let Some(blob) = &blob {
        store::remove_file(&root.join("objects").join(blob));
    }
    Ok(blob.is_some())
}

/// Deletes every file in `folder`, under `root`; returns how many there were. Uploads in
/// progress finish as they would.
pub async fn clear_folder(db: &Db, root: &Path, folder: &str) -> Result<u64, AdminError> {
    let folder = folder.to_owned();
    let blobs = db
        .call(move |conn| -> rusqlite::Result<_> {
            if !store::has_tables(conn)? {
                return Ok(Vec::new());
            }
            store::delete_folder(conn, &folder)
        })
        .await??;
    for blob in &blobs {
        store::remove_file(&root.join("objects").join(blob));
    }
    Ok(u64::try_from(blobs.len()).unwrap_or_default())
}

fn listing(object: &Object) -> Value {
    let mut listing = object.to_wire();
    listing["ownerId"] = object.owner_id.clone().into();
    listing
}

/// A file to store from a file on disk, for [`Replacement`].
#[derive(Clone, Debug, PartialEq)]
pub struct NewFile {
    /// The storage folder's name.
    pub folder: String,
    /// The file's path in the folder: `/`-separated names.
    pub path: String,
    /// The file on disk that holds its bytes, which are copied.
    pub source: PathBuf,
    pub content_type: String,
    /// The `sub` of its owner, which the folder's rules see as `item.owner_id`; `None` for none,
    /// as after an anonymous upload.
    pub owner_id: Option<String>,
    /// Its app fields.
    pub fields: Map<String, Value>,
}

/// Why a file of `size` bytes with `content_type` and app `fields` can't be stored at `path` in
/// `folder`, as an upload could not store it there; empty when it can.
pub fn check_file(
    folder: &StorageFolder,
    path: &str,
    content_type: &str,
    size: u64,
    fields: &Map<String, Value>,
) -> Vec<String> {
    let mut problems = Vec::new();
    if let Err(problem) = names::check_path(path) {
        problems.push(problem);
    }
    if HeaderValue::from_str(content_type).is_err() {
        problems.push(format!(
            "The content type {content_type:?} isn't a valid HTTP header value."
        ));
    } else if !names::content_type_allowed(&folder.allowed_content_types, content_type) {
        problems.push(format!(
            "Storage folder '{}' accepts {}, not '{content_type}'.",
            folder.name,
            folder.allowed_content_types.join(", ")
        ));
    }
    let limit = folder
        .max_size
        .map_or(StorageConfig::DEFAULT_MAX_FILE_BYTES, |max| {
            max.min(StorageConfig::DEFAULT_MAX_FILE_BYTES)
        });
    if size > limit {
        problems.push(format!(
            "The file is {size} bytes; storage folder '{}' accepts at most {limit} bytes per file.",
            folder.name
        ));
    }
    if let Err(problem) = handlers::check_fields(folder, Some(Value::Object(fields.clone()))) {
        problems.push(problem.to_string());
    }
    problems
}

/// Copies of files on disk, to store in place of every stored file within a transaction of the
/// caller's. They wait in a folder of their own under the storage folder, which the storage
/// service's sweeps of unreferenced files don't touch: [`Replacement::apply`], in the
/// transaction and with the database held, moves them among the stored files and records them;
/// then [`Replaced::finish`] once it commits, or [`Replacement::discard`] if it doesn't.
#[derive(Debug)]
pub struct Replacement {
    root: PathBuf,
    /// `<root>/seeding-<id>`, where the copies wait.
    dir: PathBuf,
    objects: Vec<Object>,
}

/// The bytes of the files and uploads [`Replacement::apply`] replaced.
#[derive(Debug)]
#[must_use = "`finish` deletes the bytes the replaced files leave behind"]
pub struct Replaced {
    root: PathBuf,
    dir: PathBuf,
    blobs: Vec<String>,
    staged: Vec<String>,
}

/// The name of the folders where copies wait.
const SEEDING_PREFIX: &str = "seeding-";
/// How old such a folder is when [`Replacement::prepare`] deletes it as one an interrupted
/// run left behind.
const STALE_SEEDING: Duration = Duration::from_secs(24 * 60 * 60);

impl Replacement {
    /// Copies the bytes of `files` under `root` (the storage folder of
    /// [`crate::StorageConfig::root`]), as an upload stores them.
    pub async fn prepare(root: &Path, files: Vec<NewFile>) -> Result<Self, AdminError> {
        let root = root.to_owned();
        let task_root = root.clone();
        tokio::task::spawn_blocking(move || Self::copy(task_root, files))
            .await
            .map_err(|error| AdminError::Copy {
                path: root,
                source: std::io::Error::other(error.to_string()),
            })?
    }

    fn copy(root: PathBuf, files: Vec<NewFile>) -> Result<Self, AdminError> {
        remove_stale_seeding(&root);
        let dir = root.join(format!("{SEEDING_PREFIX}{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).map_err(|source| AdminError::Copy {
            path: dir.clone(),
            source,
        })?;
        let now = store::iso(OffsetDateTime::now_utc());
        let mut replacement = Self {
            root,
            dir,
            objects: Vec::with_capacity(files.len()),
        };
        for file in files {
            let blob = Uuid::new_v4().simple().to_string();
            let (size, etag) = match copy_hashed(&file.source, &replacement.dir.join(&blob)) {
                Ok(copied) => copied,
                Err(source) => {
                    replacement.discard();
                    return Err(AdminError::Copy {
                        path: file.source,
                        source,
                    });
                }
            };
            let name = file.path.rsplit('/').next().unwrap_or_default().to_owned();
            replacement.objects.push(Object {
                id: Uuid::new_v4().to_string(),
                folder: file.folder,
                path: file.path,
                name,
                blob,
                size,
                content_type: file.content_type,
                content_disposition: None,
                cache_control: None,
                etag,
                owner_id: file.owner_id,
                fields: file.fields,
                created_at: now.clone(),
                updated_at: now.clone(),
            });
        }
        Ok(replacement)
    }

    /// In `conn`'s transaction, while the caller holds the database (the storage service sweeps
    /// unreferenced files only while it holds it): moves the copies among the stored files,
    /// deletes the records of every stored file and every upload in progress, and records the
    /// copies. Fails when there are copies to record and no storage tables: the storage service
    /// creates them when it first starts.
    pub fn apply(&self, conn: &Connection) -> Result<Replaced, AdminError> {
        let mut replaced = Replaced {
            root: self.root.clone(),
            dir: self.dir.clone(),
            blobs: Vec::new(),
            staged: Vec::new(),
        };
        if !store::has_tables(conn)? {
            if self.objects.is_empty() {
                return Ok(replaced);
            }
            return Err(AdminError::Database(
                "there are no storage tables: the storage service has never started".to_owned(),
            ));
        }
        let objects = self.root.join("objects");
        let moved = std::fs::create_dir_all(&objects).and_then(|()| {
            self.objects.iter().try_for_each(|object| {
                std::fs::rename(self.dir.join(&object.blob), objects.join(&object.blob))
            })
        });
        moved.map_err(|source| AdminError::Copy {
            path: objects.clone(),
            source,
        })?;
        replaced.blobs = conn
            .prepare("SELECT blob FROM _rl_storage_objects")?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        replaced.staged = conn
            .prepare(
                "SELECT staged_blob FROM _rl_storage_operations WHERE staged_blob IS NOT NULL",
            )?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        conn.execute("DELETE FROM _rl_storage_objects", [])?;
        conn.execute("DELETE FROM _rl_storage_operations", [])?;
        for object in &self.objects {
            store::save_object(conn, object)?;
        }
        Ok(replaced)
    }

    /// Deletes the copies, wherever they are, when the transaction rolled back or never ran.
    pub fn discard(self) {
        for object in &self.objects {
            store::remove_file(&self.root.join("objects").join(&object.blob));
        }
        remove_dir(&self.dir);
    }
}

impl Replaced {
    /// Deletes the bytes of the files and uploads replaced, once the transaction committed.
    pub fn finish(self) {
        for blob in &self.blobs {
            store::remove_file(&self.root.join("objects").join(blob));
        }
        for blob in &self.staged {
            store::remove_file(&self.root.join("staging").join(blob));
        }
        remove_dir(&self.dir);
    }
}

/// Best effort.
fn remove_dir(dir: &Path) {
    if let Err(error) = std::fs::remove_dir_all(dir)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::debug!("could not remove {}: {error}", dir.display());
    }
}

/// Deletes the folders of copies that runs interrupted while loading left behind.
fn remove_stale_seeding(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| modified.elapsed().is_ok_and(|age| age > STALE_SEEDING));
        if stale
            && entry
                .file_name()
                .to_string_lossy()
                .starts_with(SEEDING_PREFIX)
        {
            remove_dir(&entry.path());
        }
    }
}

/// Copies `source` to `target`; its size and its quoted SHA-256 ETag, as an upload's.
fn copy_hashed(source: &Path, target: &Path) -> std::io::Result<(u64, String)> {
    let mut input = std::fs::File::open(source)?;
    let mut output = std::fs::File::create(target)?;
    let mut hash = digest::Context::new(&digest::SHA256);
    let mut buffer = vec![0; 64 * 1024];
    let mut size: u64 = 0;
    loop {
        let read = match input.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        hash.update(&buffer[..read]);
        output.write_all(&buffer[..read])?;
        size += read as u64;
    }
    output.flush()?;
    Ok((size, format!("\"{}\"", hex::encode(hash.finish()))))
}
