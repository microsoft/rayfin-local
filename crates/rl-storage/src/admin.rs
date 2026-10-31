//! Stored files as a host's control API reads and deletes them, whatever the folders' rules:
//! each folder's files and their bytes, pages of a folder's files, one file and where its bytes
//! are, and deleting one file or all of a folder's. Before the storage service first starts
//! there are no storage tables, and so no files.

use std::path::{Path, PathBuf};

use rl_db::Db;
use rl_model::AppModel;
use serde::Serialize;
use serde_json::Value;

use crate::store::{self, Object};

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

/// Why stored files could not be read or deleted.
#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    #[error("the storage database failed: {0}")]
    Database(String),
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
