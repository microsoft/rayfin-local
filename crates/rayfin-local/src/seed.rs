//! Seed data: the rows and files a project declares in `rayfin/seed` for local development
//! (docs/seed-data.md). The control API (`/seed`) and `serve --seed` load them in place of the
//! local data, and the control API exports the local data in the same form.
//!
//! ```text
//! rayfin/seed/data/<Entity>.json        [{"title": "Buy milk", "user_id": "$developer"}, …]
//! rayfin/seed/data/<Entity>.csv         a header row of field names, then a row per line
//! rayfin/seed/storage/<folder>/<path>   a storage folder's files
//! rayfin/seed/storage/<folder>.json     {"<path>": {"owner", "contentType", "fields"}}, optional
//! ```
//!
//! `$developer` and `$user:<username>` stand for a user's id (the token's `sub`), so that rows
//! and files belong to whoever previews the app, or to test users created as needed.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rl_auth::{AuthService, User};
use rl_db::{Db, codec};
use rl_graphql::DataApi;
use rl_graphql::admin::{self as data_admin, MAX_ROWS, NewRows, RowProblem, RowsQuery};
use rl_model::{AppModel, Cardinality, Column, Entity, FieldKind, StorageFolder};
use rl_storage::StorageConfig;
use rl_storage::admin::{self as storage_admin, NewFile, Replaced, Replacement};
use rusqlite::types::Value as SqlValue;
use serde::Serialize;
use serde_json::{Map, Value, json};

/// `rayfin/seed`, from the project root, for messages.
const SEED_PATH: &str = "rayfin/seed";
/// The most problems [`read`] reports.
const MAX_PROBLEMS: usize = 100;
const DEVELOPER: &str = "$developer";
const USER_PREFIX: &str = "$user:";
/// The longest username rl-auth accepts.
const MAX_USERNAME_CHARS: usize = 256;

/// A user seed data names: `$developer`, whoever previews the app (their Entra account, else
/// `developer@example.com`), or `$user:<username>`, a local user created when missing.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UserRef {
    Developer,
    User(String),
}

impl UserRef {
    /// The user `text` names; `None` when it is no placeholder, or why it is a bad one.
    fn parse(text: &str) -> Option<Result<Self, String>> {
        if text == DEVELOPER {
            return Some(Ok(Self::Developer));
        }
        let name = text.strip_prefix(USER_PREFIX)?.trim();
        Some(if name.is_empty() {
            Err(format!(
                "`{USER_PREFIX}` needs a username, as in `{USER_PREFIX}ana@example.com`."
            ))
        } else if name.chars().count() > MAX_USERNAME_CHARS {
            Err(format!(
                "The username of `{USER_PREFIX}` is longer than {MAX_USERNAME_CHARS} characters."
            ))
        } else {
            Ok(Self::User(name.to_owned()))
        })
    }

    pub fn placeholder(&self) -> String {
        match self {
            Self::Developer => DEVELOPER.to_owned(),
            Self::User(name) => format!("{USER_PREFIX}{name}"),
        }
    }
}

/// Whether `text` is shaped like a placeholder after any number of `$`: `$developer`,
/// `$$developer`, `$user:ana`, `$$user:ana`...
fn placeholder_shaped(text: &str) -> bool {
    let rest = text.trim_start_matches('$');
    rest.len() < text.len() && (rest == "developer" || rest.starts_with("user:"))
}

/// Where `text`'s first character that isn't a space is.
fn first_visible(text: &str) -> usize {
    text.len() - text.trim_start().len()
}

/// The text that a placeholder with one `$` more (`$$developer`), with any spaces around it,
/// stands for: one `$` less, the spaces kept. `None` for other values.
fn unescape(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if !(trimmed.starts_with("$$") && placeholder_shaped(trimmed)) {
        return None;
    }
    let mut literal = text.to_owned();
    literal.remove(first_visible(text));
    Some(literal)
}

/// A stored text that would read as a placeholder (spaces around it aside), with one `$` more
/// so that it reads as itself.
fn escape(text: String) -> String {
    if !placeholder_shaped(text.trim()) {
        return text;
    }
    let mut escaped = text;
    escaped.insert(first_visible(&escaped), '$');
    escaped
}

/// Something wrong with the seed data, and where: what loading would refuse.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Problem {
    /// The file, from the project root (`rayfin/seed/data/Todo.csv`).
    pub file: String,
    /// The row: an item's position in a JSON file's array (from 1), or a CSV file's line.
    pub row: Option<u64>,
    /// The field at fault, by its name in the app.
    pub field: Option<String>,
    pub message: String,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.file)?;
        if let Some(row) = self.row {
            write!(f, ", row {row}")?;
        }
        write!(f, ": {}", self.message)
    }
}

/// Which of the app's services run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Services {
    pub data: bool,
    pub storage: bool,
}

/// The seed data of a project, read and checked against the served model.
#[derive(Debug, Default)]
pub struct Seed {
    /// Whether `rayfin/seed` holds seed data, or files where seed data goes.
    pub exists: bool,
    pub tables: Vec<SeedTable>,
    pub folders: Vec<SeedFolder>,
    /// What isn't valid (at most [`MAX_PROBLEMS`]), which [`read`] leaves out; [`load`] loads
    /// only seed data without problems.
    pub problems: Vec<Problem>,
}

/// The rows of an entity.
#[derive(Debug)]
pub struct SeedTable {
    /// The entity's name in the model.
    pub entity: String,
    /// The file, from the project root.
    pub file: String,
    pub rows: Vec<SeedRow>,
}

#[derive(Debug)]
pub struct SeedRow {
    /// As [`Problem::row`].
    pub row: u64,
    /// The values by column (its SQL name); the columns left out get their defaults.
    pub values: Vec<(String, Cell)>,
}

#[derive(Debug, PartialEq)]
pub enum Cell {
    Value(SqlValue),
    /// The user's id.
    User(UserRef),
}

/// The files of a storage folder.
#[derive(Debug)]
pub struct SeedFolder {
    /// The folder's name in the model.
    pub folder: String,
    pub files: Vec<SeedFile>,
}

#[derive(Debug)]
pub struct SeedFile {
    /// The file's path in the folder: `/`-separated names.
    pub path: String,
    pub source: PathBuf,
    pub size: u64,
    pub content_type: String,
    /// `None`: no owner.
    pub owner: Option<UserRef>,
    pub fields: Map<String, Value>,
}

impl Seed {
    /// The users the placeholders name.
    pub fn users(&self) -> BTreeSet<UserRef> {
        let cells = self
            .tables
            .iter()
            .flat_map(|table| &table.rows)
            .flat_map(|row| &row.values)
            .filter_map(|(_, cell)| match cell {
                Cell::User(user) => Some(user.clone()),
                Cell::Value(_) => None,
            });
        let owners = self
            .folders
            .iter()
            .flat_map(|folder| &folder.files)
            .filter_map(|file| file.owner.clone());
        cells.chain(owners).collect()
    }

    /// What `GET /seed` answers.
    pub fn summary(&self) -> Value {
        let entities: Vec<Value> = self
            .tables
            .iter()
            .map(|table| {
                json!({ "name": table.entity, "file": table.file, "rows": table.rows.len() })
            })
            .collect();
        let storage: Vec<Value> = self
            .folders
            .iter()
            .map(|folder| {
                let bytes: u64 = folder.files.iter().map(|file| file.size).sum();
                json!({ "folder": folder.folder, "files": folder.files.len(), "bytes": bytes })
            })
            .collect();
        let users: Vec<String> = self.users().iter().map(UserRef::placeholder).collect();
        json!({
            "exists": self.exists,
            "entities": entities,
            "storage": storage,
            "users": users,
            "problems": self.problems,
        })
    }
}

/// Reads the seed data in `dir` (the project's `rayfin/seed`) and checks it against `model`, as
/// far as it can without the database: [`load`] reports what the database refuses.
pub fn read(dir: &Path, model: &AppModel, services: Services) -> Seed {
    let mut reader = Reader {
        seed: Seed::default(),
        model,
        services,
    };
    reader.data(&dir.join("data"));
    reader.storage(&dir.join("storage"));
    reader.seed
}

struct Reader<'m> {
    seed: Seed,
    model: &'m AppModel,
    services: Services,
}

/// A row as read, before its values are checked.
struct RawRow {
    row: u64,
    values: Vec<(String, Raw)>,
}

/// A row as read, or its row and why it can't be read.
type ReadRow = Result<RawRow, (u64, String)>;

enum Raw {
    Json(Value),
    /// A CSV cell.
    Text(String),
}

/// What a storage folder's `.json` file says about one of its files.
struct FileEntry {
    owner: Option<UserRef>,
    content_type: Option<String>,
    fields: Map<String, Value>,
}

impl Default for FileEntry {
    fn default() -> Self {
        Self {
            owner: Some(UserRef::Developer),
            content_type: None,
            fields: Map::new(),
        }
    }
}

impl Reader<'_> {
    fn problem(
        &mut self,
        file: &str,
        row: Option<u64>,
        field: Option<&str>,
        message: impl Into<String>,
    ) {
        if self.seed.problems.len() < MAX_PROBLEMS {
            self.seed.problems.push(Problem {
                file: file.to_owned(),
                row,
                field: field.map(str::to_owned),
                message: message.into(),
            });
        }
    }

    /// `rayfin/seed/data`: a `.json` or `.csv` file per entity.
    fn data(&mut self, dir: &Path) {
        for (name, path, is_dir) in entries(dir) {
            let file = format!("{SEED_PATH}/data/{name}");
            let (stem, extension) = name
                .rsplit_once('.')
                .map_or((name.as_str(), String::new()), |(stem, extension)| {
                    (stem, extension.to_ascii_lowercase())
                });
            if !is_dir && extension == "md" {
                continue;
            }
            self.seed.exists = true;
            if is_dir {
                self.problem(
                    &file,
                    None,
                    None,
                    "Seed rows go in files right in rayfin/seed/data; folders in it aren't read.",
                );
                continue;
            }
            let csv = match extension.as_str() {
                "json" => false,
                "csv" => true,
                _ => {
                    self.problem(
                        &file,
                        None,
                        None,
                        "Only .json and .csv files hold seed rows, as in Todo.json or Todo.csv.",
                    );
                    continue;
                }
            };
            if !self.services.data {
                self.problem(
                    &file,
                    None,
                    None,
                    "The app's data service is off (services.data in rayfin/rayfin.yml), so there \
                     are no entities to load rows into.",
                );
                continue;
            }
            let Some(entity) = find_entity(self.model, stem) else {
                let message = format!(
                    "There is no entity {stem}: a seed file is named after its entity. The app's \
                     entities are {}.",
                    list(
                        self.model
                            .entities
                            .iter()
                            .map(|entity| entity.name.as_str())
                    )
                );
                self.problem(&file, None, None, message);
                continue;
            };
            if let Some(other) = self.seed.tables.iter().find(|t| t.entity == entity.name) {
                let message = format!(
                    "{} already holds the rows of {}; keep one file per entity.",
                    other.file, entity.name
                );
                self.problem(&file, None, None, message);
                continue;
            }
            let rows = std::fs::read(&path)
                .map_err(|error| format!("The file can't be read: {error}."))
                .and_then(|bytes| {
                    String::from_utf8(bytes).map_err(|_| "The file isn't UTF-8 text.".to_owned())
                })
                .and_then(|text| {
                    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
                    if csv { csv_rows(text) } else { json_rows(text) }
                });
            match rows {
                Ok(rows) => self.table(entity, file, rows),
                Err(message) => self.problem(&file, None, None, message),
            }
        }
    }

    fn table(&mut self, entity: &Entity, file: String, rows: Vec<ReadRow>) {
        let mut table = SeedTable {
            entity: entity.name.clone(),
            file,
            rows: Vec::new(),
        };
        // Each unknown field once, at its first row.
        let mut unknown = HashSet::new();
        for row in rows {
            let raw = match row {
                Ok(raw) => raw,
                Err((row, message)) => {
                    self.problem(&table.file, Some(row), None, message);
                    continue;
                }
            };
            let mut values: Vec<(String, Cell)> = Vec::new();
            let mut valid = true;
            for (key, value) in raw.values {
                let Some(column) = find_column(entity, &key) else {
                    if unknown.insert(key.to_lowercase()) {
                        let message = format!(
                            "{} has no field `{key}`. Its fields are {}.",
                            entity.name,
                            list(entity.columns.iter().map(|column| column.field.as_str()))
                        );
                        self.problem(&table.file, Some(raw.row), Some(&key), message);
                    }
                    valid = false;
                    continue;
                };
                if values.iter().any(|(name, _)| *name == column.name) {
                    let message = format!("`{key}` sets `{}` a second time.", column.field);
                    self.problem(&table.file, Some(raw.row), Some(&column.field), message);
                    valid = false;
                    continue;
                }
                match cell(column, value) {
                    Ok(Some(cell)) => values.push((column.name.clone(), cell)),
                    Ok(None) => {}
                    Err(message) => {
                        self.problem(&table.file, Some(raw.row), Some(&column.field), message);
                        valid = false;
                    }
                }
            }
            if valid {
                table.rows.push(SeedRow {
                    row: raw.row,
                    values,
                });
            }
        }
        self.seed.tables.push(table);
    }

    /// `rayfin/seed/storage`: a folder of files per storage folder, each with an optional
    /// `<folder>.json` beside it.
    fn storage(&mut self, dir: &Path) {
        let mut folders = Vec::new();
        let mut manifests = Vec::new();
        for (name, path, is_dir) in entries(dir) {
            if is_dir {
                self.seed.exists = true;
                folders.push((name, path));
                continue;
            }
            let file = format!("{SEED_PATH}/storage/{name}");
            match name.rsplit_once('.') {
                Some((_, extension)) if extension.eq_ignore_ascii_case("md") => continue,
                Some((stem, extension)) if extension.eq_ignore_ascii_case("json") => {
                    manifests.push((stem.to_owned(), path, file));
                }
                _ => self.problem(
                    &file,
                    None,
                    None,
                    "A storage folder's files go in a folder named after it, as in \
                     rayfin/seed/storage/<folder>/<file>.",
                ),
            }
            self.seed.exists = true;
        }
        let mut total: u64 = 0;
        for (name, path) in folders {
            let display = format!("{SEED_PATH}/storage/{name}");
            let manifest = manifests
                .iter()
                .position(|(stem, ..)| *stem == name)
                .or_else(|| {
                    manifests
                        .iter()
                        .position(|(stem, ..)| stem.eq_ignore_ascii_case(&name))
                })
                .map(|index| manifests.remove(index));
            if !self.services.storage {
                self.problem(
                    &display,
                    None,
                    None,
                    "The app's storage service isn't running, so the seed's files can't be stored: \
                     it needs services.storage on in rayfin/rayfin.yml, and @blob classes in \
                     rayfin/storage.",
                );
                continue;
            }
            let Some(folder) = find_folder(self.model, &name) else {
                let declared: Vec<&str> = self
                    .model
                    .storage
                    .iter()
                    .flat_map(|storage| &storage.folders)
                    .map(|folder| folder.name.as_str())
                    .collect();
                let message = if declared.is_empty() {
                    format!(
                        "There is no storage folder {name}: the app declares none (a @blob class \
                         in rayfin/storage)."
                    )
                } else {
                    format!(
                        "There is no storage folder {name}. The app's storage folders are {}.",
                        list(declared.into_iter())
                    )
                };
                self.problem(&display, None, None, message);
                continue;
            };
            if self
                .seed
                .folders
                .iter()
                .any(|seeded| seeded.folder == folder.name)
            {
                let message = format!(
                    "Another folder in rayfin/seed/storage already holds the files of {}.",
                    folder.name
                );
                self.problem(&display, None, None, message);
                continue;
            }
            let mut entries = match manifest {
                Some((_, path, file)) => self.manifest(&path, &file),
                None => (String::new(), HashMap::new()),
            };
            let mut seeded = SeedFolder {
                folder: folder.name.clone(),
                files: Vec::new(),
            };
            for (relative, source, size) in self.files(&path, &display) {
                let entry = entries.1.remove(&relative).unwrap_or_default();
                let file = format!("{display}/{relative}");
                let content_type = entry
                    .content_type
                    .unwrap_or_else(|| guess_content_type(&relative));
                let problems = storage_admin::check_file(
                    folder,
                    &relative,
                    &content_type,
                    size,
                    &entry.fields,
                );
                if !problems.is_empty() {
                    for problem in problems {
                        self.problem(&file, None, None, problem);
                    }
                    continue;
                }
                total = total.saturating_add(size);
                seeded.files.push(SeedFile {
                    path: relative,
                    source,
                    size,
                    content_type,
                    owner: entry.owner,
                    fields: entry.fields,
                });
            }
            let (manifest_file, left) = entries;
            let mut left: Vec<String> = left.into_keys().collect();
            left.sort();
            for path in left {
                let message = format!("{path:?} isn't a file in {display}.");
                self.problem(&manifest_file, None, None, message);
            }
            self.seed.folders.push(seeded);
        }
        for (stem, _, file) in manifests {
            let message = format!(
                "There is no folder {SEED_PATH}/storage/{stem} with the files this describes."
            );
            self.problem(&file, None, None, message);
        }
        if total > StorageConfig::DEFAULT_MAX_TOTAL_BYTES {
            let message = format!(
                "The seed's files are {total} bytes together; local storage holds at most {}.",
                StorageConfig::DEFAULT_MAX_TOTAL_BYTES
            );
            self.problem(&format!("{SEED_PATH}/storage"), None, None, message);
        }
    }

    /// A storage folder's `.json` file: its display path, and what it says by file path.
    fn manifest(&mut self, path: &Path, file: &str) -> (String, HashMap<String, FileEntry>) {
        let mut entries = HashMap::new();
        let parsed = std::fs::read(path)
            .map_err(|error| format!("The file can't be read: {error}."))
            .and_then(|bytes| {
                let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
                serde_json::from_slice::<Value>(bytes)
                    .map_err(|error| format!("The file isn't valid JSON: {error}."))
            });
        match parsed {
            Ok(Value::Object(object)) => {
                for (path, value) in object {
                    match file_entry(&value) {
                        Ok(entry) => {
                            entries.insert(path, entry);
                        }
                        Err(message) => {
                            self.problem(file, None, None, format!("{path:?}: {message}"))
                        }
                    }
                }
            }
            Ok(_) => self.problem(
                file,
                None,
                None,
                "The file must hold a JSON object of file paths, as in \
                 {\"receipt.pdf\": {\"owner\": \"$user:ana@example.com\"}}.",
            ),
            Err(message) => self.problem(file, None, None, message),
        }
        (file.to_owned(), entries)
    }

    /// The files under `dir`, by `/`-separated path: path, file and size.
    fn files(&mut self, dir: &Path, display: &str) -> Vec<(String, PathBuf, u64)> {
        let mut files = Vec::new();
        let walk = walkdir::WalkDir::new(dir)
            .min_depth(1)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|entry| !ignored(&entry.file_name().to_string_lossy()));
        for entry in walk {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    self.problem(
                        display,
                        None,
                        None,
                        format!("A file can't be read: {error}."),
                    );
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let relative = entry.path().strip_prefix(dir).unwrap_or(entry.path());
            let names: Option<Vec<&str>> = relative
                .components()
                .map(|component| component.as_os_str().to_str())
                .collect();
            let Some(names) = names else {
                let message = format!(
                    "The name of {} isn't valid Unicode.",
                    relative.to_string_lossy()
                );
                self.problem(display, None, None, message);
                continue;
            };
            match entry.metadata() {
                Ok(metadata) => {
                    files.push((names.join("/"), entry.path().to_owned(), metadata.len()));
                }
                Err(error) => {
                    let file = format!("{display}/{}", names.join("/"));
                    self.problem(
                        &file,
                        None,
                        None,
                        format!("The file can't be read: {error}."),
                    );
                }
            }
        }
        files
    }
}

/// The entries of `dir` without the ignored ones, by name (whatever the case): name, path, and
/// whether it's a folder. None when `dir` isn't there.
fn entries(dir: &Path) -> Vec<(String, PathBuf, bool)> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<(String, PathBuf, bool)> = read
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = entry.file_type().ok()?.is_dir();
            (!ignored(&name)).then(|| (name, entry.path(), is_dir))
        })
        .collect();
    entries.sort_by(|a, b| (a.0.to_lowercase(), &a.0).cmp(&(b.0.to_lowercase(), &b.0)));
    entries
}

/// Hidden files and the ones file managers leave behind.
fn ignored(name: &str) -> bool {
    name.starts_with('.')
        || name.eq_ignore_ascii_case("Thumbs.db")
        || name.eq_ignore_ascii_case("desktop.ini")
}

fn list<'a>(names: impl Iterator<Item = &'a str>) -> String {
    let names: Vec<&str> = names.collect();
    if names.is_empty() {
        "none".to_owned()
    } else {
        names.join(", ")
    }
}

fn guess_content_type(path: &str) -> String {
    mime_guess::from_path(path)
        .first_or_octet_stream()
        .essence_str()
        .to_owned()
}

/// The entity a seed file is named after: its name, else its table's (`Todos.json`).
fn find_entity<'m>(model: &'m AppModel, name: &str) -> Option<&'m Entity> {
    let entities = &model.entities;
    entities
        .iter()
        .find(|entity| entity.name == name)
        .or_else(|| {
            entities
                .iter()
                .find(|entity| entity.name.eq_ignore_ascii_case(name))
        })
        .or_else(|| {
            entities
                .iter()
                .find(|entity| entity.table.eq_ignore_ascii_case(name))
        })
}

/// The column a row's key sets: a field, a column, or a `@one` relationship (its key column,
/// which takes the related row's id).
fn find_column<'e>(entity: &'e Entity, key: &str) -> Option<&'e Column> {
    let columns = &entity.columns;
    columns
        .iter()
        .find(|column| column.field == key)
        .or_else(|| {
            columns
                .iter()
                .find(|column| column.field.eq_ignore_ascii_case(key))
        })
        .or_else(|| {
            columns
                .iter()
                .find(|column| column.name.eq_ignore_ascii_case(key))
        })
        .or_else(|| {
            let relationship = entity.relationships.iter().find(|relationship| {
                relationship.cardinality == Cardinality::One
                    && relationship.name.eq_ignore_ascii_case(key)
            })?;
            let [column] = relationship.source_columns.as_slice() else {
                return None;
            };
            entity.column(column)
        })
}

/// The storage folder a seed folder is named after: its name, or its `@blob` class's name.
fn find_folder<'m>(model: &'m AppModel, name: &str) -> Option<&'m StorageFolder> {
    let folders: Vec<&StorageFolder> = model
        .storage
        .iter()
        .flat_map(|storage| &storage.folders)
        .collect();
    folders
        .iter()
        .find(|folder| folder.name == name)
        .or_else(|| {
            folders.iter().find(|folder| {
                folder.name.eq_ignore_ascii_case(name)
                    || folder.display_name.eq_ignore_ascii_case(name)
            })
        })
        .copied()
}

fn json_rows(text: &str) -> Result<Vec<ReadRow>, String> {
    let value: Value = serde_json::from_str(text)
        .map_err(|error| format!("The file isn't valid JSON: {error}."))?;
    let Value::Array(items) = value else {
        return Err(
            "The file must hold a JSON array of rows, as in [{\"title\": \"Buy milk\"}].".into(),
        );
    };
    Ok(items
        .into_iter()
        .zip(1..)
        .map(|(item, row)| match item {
            Value::Object(object) => Ok(RawRow {
                row,
                values: object
                    .into_iter()
                    .map(|(key, value)| (key, Raw::Json(value)))
                    .collect(),
            }),
            _ => Err((
                row,
                "A row must be a JSON object of field values.".to_owned(),
            )),
        })
        .collect())
}

/// A header row of field names, then the rows; an empty cell leaves its field out.
fn csv_rows(text: &str) -> Result<Vec<ReadRow>, String> {
    let mut reader = csv::ReaderBuilder::new().from_reader(text.as_bytes());
    let headers: Vec<String> = reader
        .headers()
        .map_err(|error| format!("The header row can't be read: {error}."))?
        .iter()
        .map(|header| header.trim().to_owned())
        .collect();
    if headers.iter().all(String::is_empty) {
        return Err("The file needs a header row of field names, as in `title,user_id`.".into());
    }
    if let Some(position) = headers.iter().position(String::is_empty) {
        return Err(format!(
            "Column {} of the header row has no field name.",
            position + 1
        ));
    }
    let mut seen = HashSet::new();
    if let Some(repeated) = headers
        .iter()
        .find(|header| !seen.insert(header.to_lowercase()))
    {
        return Err(format!("The header row names `{repeated}` twice."));
    }
    Ok(reader
        .records()
        .map(|record| {
            let line = |position: Option<&csv::Position>| position.map_or(0, csv::Position::line);
            match record {
                Ok(record) => Ok(RawRow {
                    row: line(record.position()),
                    values: headers
                        .iter()
                        .zip(record.iter())
                        .filter(|(_, cell)| !cell.is_empty())
                        .map(|(header, cell)| (header.clone(), Raw::Text(cell.to_owned())))
                        .collect(),
                }),
                Err(error) => {
                    let message = match error.kind() {
                        csv::ErrorKind::UnequalLengths {
                            expected_len, len, ..
                        } => format!(
                            "The row has {len} values, and the header row {expected_len} names."
                        ),
                        _ => format!("The row can't be read: {error}."),
                    };
                    Err((line(error.position()), message))
                }
            }
        })
        .collect())
}

/// What a storage folder's `.json` file says about a file: `owner`, `contentType`, `fields`.
fn file_entry(value: &Value) -> Result<FileEntry, String> {
    let Value::Object(object) = value else {
        return Err(
            "Describe the file with an object, as in {\"owner\": \"$user:ana@example.com\"}."
                .into(),
        );
    };
    let mut entry = FileEntry::default();
    for (key, value) in object {
        match (key.as_str(), value) {
            ("owner", Value::Null) => entry.owner = None,
            ("owner", Value::String(text)) => match UserRef::parse(text) {
                Some(Ok(user)) => entry.owner = Some(user),
                Some(Err(message)) => return Err(message),
                None => {
                    return Err(format!(
                        "`owner` is `{DEVELOPER}`, `{USER_PREFIX}<username>` or null, not {text:?}."
                    ));
                }
            },
            ("contentType", Value::String(text)) if !text.trim().is_empty() => {
                entry.content_type = Some(text.trim().to_owned());
            }
            ("fields", Value::Object(fields)) => entry.fields = fields.clone(),
            ("owner", _) => {
                return Err(format!(
                    "`owner` is `{DEVELOPER}`, `{USER_PREFIX}<username>` or null."
                ));
            }
            ("contentType", _) => {
                return Err("`contentType` is a MIME type, as in image/png.".into());
            }
            ("fields", _) => {
                return Err("`fields` is an object of the storage folder's fields.".into());
            }
            (other, _) => {
                return Err(format!(
                    "A file has an `owner`, a `contentType` and `fields`, not `{other}`."
                ));
            }
        }
    }
    Ok(entry)
}

/// The value of `raw` for `column`; `None` for an empty CSV cell, which leaves the column out.
/// Text (a CSV cell, a JSON string) converts as SQL Server converts strings; other JSON values
/// as the data API takes them.
fn cell(column: &Column, raw: Raw) -> Result<Option<Cell>, String> {
    let text = match raw {
        Raw::Text(text) if text.is_empty() => return Ok(None),
        Raw::Text(text) | Raw::Json(Value::String(text)) => text,
        Raw::Json(value @ (Value::Null | Value::Bool(_) | Value::Number(_))) => {
            return codec::encode(&column.kind, &value)
                .map(|value| Some(Cell::Value(value)))
                .map_err(|_| wrong_type(column, &value.to_string()));
        }
        Raw::Json(Value::Array(_)) => return Err(wrong_type(column, "a list")),
        Raw::Json(Value::Object(_)) => return Err(wrong_type(column, "an object")),
    };
    // A placeholder is one with the spaces around it, as after a CSV file's commas.
    if let Some(literal) = unescape(&text) {
        return parse_text(column, &literal);
    }
    match UserRef::parse(text.trim()) {
        Some(Ok(user)) if matches!(column.kind, FieldKind::Uuid | FieldKind::Text { .. }) => {
            Ok(Some(Cell::User(user)))
        }
        Some(Ok(user)) => Err(format!(
            "`{}` must be {}, and {} stands for a user's id: it fills text and uuid fields, such \
             as `user_id`.",
            column.field,
            expected(&column.kind),
            user.placeholder()
        )),
        Some(Err(message)) => Err(message),
        None => parse_text(column, &text),
    }
}

fn parse_text(column: &Column, text: &str) -> Result<Option<Cell>, String> {
    codec::parse_text(&column.kind, text)
        .map(|value| Some(Cell::Value(value)))
        .map_err(|_| wrong_type(column, &format!("{text:?}")))
}

fn wrong_type(column: &Column, given: &str) -> String {
    format!(
        "`{}` must be {}, not {given}.",
        column.field,
        expected(&column.kind)
    )
}

fn expected(kind: &FieldKind) -> String {
    match kind {
        FieldKind::Uuid => "a uuid".into(),
        FieldKind::Text { .. } => "text".into(),
        FieldKind::Email { .. } => "an email address".into(),
        FieldKind::Set { values, .. } => format!("one of {}", values.join(", ")),
        FieldKind::Int { .. } => "a whole number".into(),
        FieldKind::Decimal { .. } => "a number".into(),
        FieldKind::Boolean => "true or false".into(),
        FieldKind::Date => "a date (2025-01-31) or a date and time (2025-01-31T09:30:00Z)".into(),
    }
}

/// The services [`load`] stores seed data with.
pub struct Target<'a> {
    pub auth: &'a AuthService,
    pub db: &'a Db,
    pub model: &'a Arc<AppModel>,
    /// Whether `services.data` is on: rows are replaced only then.
    pub data: bool,
    /// Whether the storage service runs: stored files are replaced only then.
    pub storage: bool,
    /// `.rayfin-local/storage`.
    pub storage_dir: &'a Path,
    /// Find what loading would refuse, and change nothing: the rows go in a transaction that
    /// rolls back, the placeholders stand for made-up ids (no user is created), and the files,
    /// which [`read`] checked, aren't copied.
    pub dry_run: bool,
}

/// What [`load`] stored, or would store in a dry run.
#[derive(Debug)]
pub struct Loaded {
    /// How many rows each entity with seed rows got, in the seed's order.
    pub entities: Vec<(String, u64)>,
    /// How many files each storage folder with seed files got.
    pub storage: Vec<(String, u64)>,
    /// The users the placeholders stand for, created when missing; none in a dry run.
    pub users: Vec<(UserRef, User)>,
}

impl Loaded {
    /// `12 rows in 3 entities and 2 files`, for logs.
    pub fn describe(&self) -> String {
        let rows: u64 = self.entities.iter().map(|(_, rows)| rows).sum();
        let files: u64 = self.storage.iter().map(|(_, files)| files).sum();
        let plural =
            |n: u64, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let entities = u64::try_from(self.entities.len()).unwrap_or_default();
        format!(
            "{} in {} and {}",
            plural(rows, "row", "rows"),
            plural(entities, "entity", "entities"),
            plural(files, "file", "files")
        )
    }
}

#[derive(Debug)]
pub enum LoadError {
    /// The seed data isn't valid, or the database refuses it: nothing changed.
    Invalid(Vec<Problem>),
    Failed(anyhow::Error),
}

impl From<anyhow::Error> for LoadError {
    fn from(error: anyhow::Error) -> Self {
        Self::Failed(error)
    }
}

/// Replaces the local data with `seed`, all at once: every entity's rows and every stored file.
/// The users the placeholders name are created when missing; the other users stay. A dry run
/// ([`Target::dry_run`]) changes nothing.
pub async fn load(seed: Seed, target: Target<'_>) -> Result<Loaded, LoadError> {
    if !seed.problems.is_empty() {
        return Err(LoadError::Invalid(seed.problems));
    }
    let mut ids: HashMap<UserRef, String> = HashMap::new();
    let mut users = Vec::new();
    for user in seed.users() {
        if target.dry_run {
            // A distinct id each, as the users have: unique fields compare the same.
            ids.insert(user, codec::new_uuid());
            continue;
        }
        let found = match &user {
            UserRef::Developer => target.auth.developer_account().await,
            UserRef::User(name) => target.auth.find_or_create_user(name).await,
        };
        let found = match found {
            Ok(found) => found,
            Err(rl_auth::AuthError::InvalidInput(message)) => {
                return Err(LoadError::Invalid(vec![Problem {
                    file: SEED_PATH.to_owned(),
                    row: None,
                    field: None,
                    message: format!("{}: {message}.", user.placeholder()),
                }]));
            }
            Err(error) => {
                return Err(anyhow::Error::new(error)
                    .context(format!("Could not find the user {}", user.placeholder()))
                    .into());
            }
        };
        ids.insert(user.clone(), found.id.clone());
        users.push((user, found));
    }
    let id = |user: &UserRef| ids[user].clone();
    let tables: Vec<NewRows> = seed
        .tables
        .iter()
        .map(|table| NewRows {
            entity: table.entity.clone(),
            rows: table
                .rows
                .iter()
                .map(|row| {
                    row.values
                        .iter()
                        .map(|(column, cell)| {
                            let value = match cell {
                                Cell::Value(value) => value.clone(),
                                Cell::User(user) => SqlValue::Text(id(user)),
                            };
                            (column.clone(), value)
                        })
                        .collect()
                })
                .collect(),
        })
        .collect();
    let replacement = if target.dry_run || !target.storage {
        None
    } else {
        let files: Vec<NewFile> = seed
            .folders
            .iter()
            .flat_map(|folder| {
                folder.files.iter().map(|file| NewFile {
                    folder: folder.folder.clone(),
                    path: file.path.clone(),
                    source: file.source.clone(),
                    content_type: file.content_type.clone(),
                    owner_id: file.owner.as_ref().map(id),
                    fields: file.fields.clone(),
                })
            })
            .collect();
        let prepared = Replacement::prepare(target.storage_dir, files).await;
        Some(prepared.map_err(anyhow::Error::new)?)
    };
    let model = Arc::clone(target.model);
    let data = target.data;
    let commit = !target.dry_run;
    let (replacement, written) = target
        .db
        .call(move |conn| {
            let written = write(conn, &model, data, &tables, replacement.as_ref(), commit);
            (replacement, written)
        })
        .await
        .map_err(anyhow::Error::new)?;
    let counts = match written {
        Ok(Ok((counts, replaced))) => {
            if let Some(replaced) = replaced {
                replaced.finish();
            }
            counts
        }
        Ok(Err(problems)) => {
            if let Some(replacement) = replacement {
                replacement.discard();
            }
            return Err(LoadError::Invalid(
                problems
                    .into_iter()
                    .map(|problem| located(&seed, problem))
                    .collect(),
            ));
        }
        Err(error) => {
            if let Some(replacement) = replacement {
                replacement.discard();
            }
            return Err(error.into());
        }
    };
    users.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(Loaded {
        entities: seed
            .tables
            .iter()
            .zip(counts)
            .map(|(table, rows)| (table.entity.clone(), rows))
            .collect(),
        storage: seed
            .folders
            .iter()
            .map(|folder| {
                let files = u64::try_from(folder.files.len()).unwrap_or_default();
                (folder.folder.clone(), files)
            })
            .collect(),
        users,
    })
}

type Written = Result<(Vec<u64>, Option<Replaced>), Vec<RowProblem>>;

/// In one transaction: the rows (when the data service is on), then the stored files (with a
/// `replacement`: when the storage service runs). Without `commit` (a dry run) it rolls back
/// after the rows instead.
fn write(
    conn: &mut rusqlite::Connection,
    model: &AppModel,
    data: bool,
    tables: &[NewRows],
    replacement: Option<&Replacement>,
    commit: bool,
) -> anyhow::Result<Written> {
    let tx = conn.transaction()?;
    let counts = if data {
        match data_admin::replace_rows(&tx, model, tables)? {
            Ok(counts) => counts,
            Err(problems) => return Ok(Err(problems)),
        }
    } else {
        Vec::new()
    };
    if !commit {
        return Ok(Ok((counts, None)));
    }
    let replaced = replacement
        .map(|replacement| replacement.apply(&tx))
        .transpose()?;
    tx.commit()?;
    Ok(Ok((counts, replaced)))
}

/// A database problem, at its seed file and row.
fn located(seed: &Seed, problem: RowProblem) -> Problem {
    let table = &seed.tables[problem.table];
    Problem {
        file: table.file.clone(),
        row: table.rows.get(problem.row).map(|row| row.row),
        field: problem.field,
        message: problem.message,
    }
}

/// The services [`export`] reads the local data from.
pub struct Source<'a> {
    pub auth: &'a AuthService,
    /// `None` when `services.data` is off.
    pub data: Option<&'a DataApi>,
    pub db: &'a Db,
    pub model: &'a AppModel,
}

/// The local data as seed data, for a host to write to `rayfin/seed` (paths are from there):
///
/// - `data`: for each entity with rows, the rows to write to `file`, without the `null` values
///   that leaving a field out gives too;
/// - `storage`: for each storage folder with files, the folder to write them to (`dir`), each
///   file's `path` and `size`, and what to write to `manifestFile` (`manifest`; `null` when every
///   file is the developer's, has the content type its name suggests, and no fields);
/// - `skipped`: the stored files left out, with why: their paths can't be file paths on every
///   system, or loading the seed data would skip them.
///
/// User ids become placeholders.
pub async fn export(source: Source<'_>) -> anyhow::Result<Value> {
    let users: HashMap<String, String> = source
        .auth
        .list_users()
        .await?
        .into_iter()
        .map(|user| {
            let placeholder = if user.is_developer() {
                UserRef::Developer
            } else {
                UserRef::User(user.username.clone())
            };
            (user.id, placeholder.placeholder())
        })
        .collect();
    let mut data = Vec::new();
    if let Some(api) = source.data {
        for entity in &source.model.entities {
            // Leaving out these fields gives them a value, not `null`.
            let defaulted: HashSet<&str> = entity
                .columns
                .iter()
                .filter(|column| codec::generated_default(column).is_some())
                .map(|column| column.field.as_str())
                .collect();
            let mut rows = Vec::new();
            loop {
                let query = RowsQuery {
                    offset: u64::try_from(rows.len()).unwrap_or_default(),
                    limit: MAX_ROWS,
                    order_by: None,
                    desc: false,
                };
                let page = api.rows(&entity.name, &query).await?;
                let done = page.rows.len() < MAX_ROWS as usize;
                rows.extend(
                    page.rows
                        .into_iter()
                        .map(|row| exported_row(row, &defaulted, &users)),
                );
                if done {
                    break;
                }
            }
            if !rows.is_empty() {
                data.push(json!({
                    "entity": entity.name,
                    "file": format!("data/{}.json", entity.name),
                    "rows": rows,
                }));
            }
        }
    }
    let mut storage = Vec::new();
    let mut skipped = Vec::new();
    for folder in source
        .model
        .storage
        .iter()
        .flat_map(|storage| &storage.folders)
    {
        let mut files = Vec::new();
        let mut manifest = Map::new();
        let mut taken = Taken::default();
        let mut offset = 0;
        loop {
            let page =
                storage_admin::files(source.db, &folder.name, offset, storage_admin::MAX_FILES)
                    .await?;
            offset += u64::try_from(page.files.len()).unwrap_or_default();
            let done = page.files.len() < storage_admin::MAX_FILES as usize;
            for file in page.files {
                let path = file["path"].as_str().unwrap_or_default().to_owned();
                if let Some(message) = unportable(&path).or_else(|| taken.take(&path)) {
                    skipped
                        .push(json!({ "folder": folder.name, "path": path, "message": message }));
                    continue;
                }
                if let Some(entry) = exported_entry(&path, &file, &users) {
                    manifest.insert(path.clone(), entry);
                }
                files.push(json!({ "path": path, "size": file["size"] }));
            }
            if done {
                break;
            }
        }
        if !files.is_empty() {
            storage.push(json!({
                "folder": folder.name,
                "dir": format!("storage/{}", folder.name),
                "manifestFile": format!("storage/{}.json", folder.name),
                "manifest": if manifest.is_empty() { Value::Null } else { Value::Object(manifest) },
                "files": files,
            }));
        }
    }
    Ok(json!({ "data": data, "storage": storage, "skipped": skipped }))
}

/// The paths a storage folder's exported files take, whatever their case, as on Windows and
/// macOS, where paths that differ only in case are one.
#[derive(Default)]
struct Taken {
    files: HashSet<String>,
    /// Each folder the paths are in, lowercase, as first spelled.
    dirs: HashMap<String, String>,
}

impl Taken {
    /// Why `path` can't be written beside the paths taken so far (in path order); `None`, and
    /// it's taken, when it can.
    fn take(&mut self, path: &str) -> Option<String> {
        let key = path.to_lowercase();
        if self.files.contains(&key) || self.dirs.contains_key(&key) {
            return Some(format!(
                "`{path}` differs only in case from another stored file's path, and on Windows \
                 and macOS they'd be one."
            ));
        }
        let mut dirs = Vec::new();
        for (end, _) in path.match_indices('/') {
            let dir = &path[..end];
            let dir_key = dir.to_lowercase();
            if self.files.contains(&dir_key) {
                return Some(format!(
                    "`{dir}` is both a stored file and the folder of `{path}`."
                ));
            }
            if let Some(spelled) = self.dirs.get(&dir_key)
                && spelled != dir
            {
                return Some(format!(
                    "Its folder `{dir}` differs only in case from `{spelled}`, and on Windows and \
                     macOS they'd be one."
                ));
            }
            dirs.push((dir_key, dir.to_owned()));
        }
        self.files.insert(key);
        for (dir_key, dir) in dirs {
            self.dirs.entry(dir_key).or_insert(dir);
        }
        None
    }
}

/// The names Windows keeps for devices, with or without an extension.
const RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Why a stored file's `path` can't be a seed file's path on every system, or would be skipped
/// when the seed data loads; `None` when it can be one.
fn unportable(path: &str) -> Option<String> {
    path.split('/').find_map(|name| {
        let stem = name.split('.').next().unwrap_or_default().trim_end();
        if ignored(name) {
            Some(format!(
                "`{name}` is the name of a hidden or system file, which loading seed data skips."
            ))
        } else if name
            .chars()
            .any(|c| c.is_control() || r#"<>:"\|?*"#.contains(c))
        {
            Some(format!(
                "`{name}` has a character that Windows doesn't allow in file names."
            ))
        } else if name.ends_with(['.', ' ']) {
            Some(format!(
                "`{name}` ends with a dot or a space, which Windows file names can't."
            ))
        } else if RESERVED_NAMES
            .iter()
            .any(|reserved| reserved.eq_ignore_ascii_case(stem))
        {
            Some(format!("`{name}` is a name Windows keeps for a device."))
        } else {
            None
        }
    })
}

/// A row as seed data: user ids as placeholders, and no `null` values but those of the
/// `defaulted` fields, which leaving out would give a value.
fn exported_row(row: Value, defaulted: &HashSet<&str>, users: &HashMap<String, String>) -> Value {
    let Value::Object(object) = row else {
        return row;
    };
    Value::Object(
        object
            .into_iter()
            .filter(|(field, value)| !value.is_null() || defaulted.contains(field.as_str()))
            .map(|(field, value)| {
                let value = match value {
                    Value::String(text) => match users.get(&text) {
                        Some(placeholder) => Value::String(placeholder.clone()),
                        None => Value::String(escape(text)),
                    },
                    value => value,
                };
                (field, value)
            })
            .collect(),
    )
}

/// What a storage folder's `.json` file says about a stored file (as the control API lists it),
/// when it isn't what the seed assumes: the developer's, of the content type its name suggests,
/// without fields.
fn exported_entry(path: &str, file: &Value, users: &HashMap<String, String>) -> Option<Value> {
    let mut entry = Map::new();
    let owner = file["ownerId"]
        .as_str()
        .and_then(|id| users.get(id))
        .map_or(Value::Null, |placeholder| {
            Value::String(placeholder.clone())
        });
    if owner != DEVELOPER {
        entry.insert("owner".into(), owner);
    }
    let content_type = file["contentType"].as_str().unwrap_or_default();
    if content_type != guess_content_type(path) {
        entry.insert("contentType".into(), content_type.into());
    }
    if let Some(fields) = file["fields"]
        .as_object()
        .filter(|fields| !fields.is_empty())
    {
        entry.insert("fields".into(), Value::Object(fields.clone()));
    }
    (!entry.is_empty()).then_some(Value::Object(entry))
}

#[cfg(test)]
mod tests;
