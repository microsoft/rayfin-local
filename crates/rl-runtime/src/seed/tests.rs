//! Reading, checking, loading and exporting seed data over the basic model, with the storage
//! tables the storage service creates.

use std::fs;

use rl_auth::AuthSettings;
use rl_graphql::DataApiSettings;
use rl_model::CallerResolver;

use super::*;

const C1: &str = "00000000-0000-4000-8000-0000000000c1";
const C2: &str = "00000000-0000-4000-8000-0000000000c2";
const T1: &str = "00000000-0000-4000-8000-000000000001";
const T2: &str = "00000000-0000-4000-8000-000000000002";
const ALL: Services = Services {
    data: true,
    storage: true,
};

struct Harness {
    dir: tempfile::TempDir,
    db: Db,
    model: Arc<AppModel>,
    auth: Arc<AuthService>,
    data: Arc<DataApi>,
}

async fn harness() -> Harness {
    harness_with(|_| {}).await
}

/// A harness whose model `edit` changes first.
async fn harness_with(edit: impl FnOnce(&mut AppModel)) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("db.sqlite")).unwrap();
    let analysis = include_str!("../../../rl-model/tests/fixtures/basic.analysis.json");
    let mut model = AppModel::from_analysis(&serde_json::from_str(analysis).unwrap()).unwrap();
    edit(&mut model);
    let model = Arc::new(model);
    rl_db::apply(&db, &model, false).await.unwrap();
    let auth = Arc::new(
        AuthService::new(
            db.clone(),
            dir.path().join("keys"),
            AuthSettings::new("6c6f6361-6c00-4000-8000-0000000000c0"),
        )
        .await
        .unwrap(),
    );
    let callers: Arc<dyn CallerResolver> = auth.clone();
    let data = Arc::new(DataApi::new(
        db.clone(),
        Arc::clone(&model),
        Arc::clone(&callers),
        DataApiSettings::default(),
    ));
    // Its tables, as the storage service creates them when it starts.
    let config = StorageConfig::new(dir.path().join("storage"), "http://localhost:7171");
    let _storage = rl_storage::router(config, db.clone(), Arc::clone(&model), callers)
        .await
        .unwrap();
    Harness {
        dir,
        db,
        model,
        auth,
        data,
    }
}

impl Harness {
    fn seed_dir(&self) -> PathBuf {
        self.dir.path().join("seed")
    }

    fn storage_dir(&self) -> PathBuf {
        self.dir.path().join("storage")
    }

    /// Writes `contents` to `path` in the seed folder.
    fn write(&self, path: &str, contents: &str) {
        let path = self.seed_dir().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn clear_seed(&self) {
        fs::remove_dir_all(self.seed_dir()).unwrap();
    }

    fn read(&self) -> Seed {
        read(&self.seed_dir(), &self.model, ALL)
    }

    async fn load(&self) -> Result<Loaded, LoadError> {
        self.run(false, true).await
    }

    /// A dry run.
    async fn check(&self) -> Result<Loaded, LoadError> {
        self.run(true, true).await
    }

    async fn run(&self, dry_run: bool, storage: bool) -> Result<Loaded, LoadError> {
        let storage_dir = self.storage_dir();
        let target = Target {
            auth: &self.auth,
            db: &self.db,
            model: &self.model,
            data: true,
            storage,
            storage_dir: &storage_dir,
            dry_run,
        };
        load(self.read(), target).await
    }

    /// `bytes` stored at `path` in `folder`, as an upload of the developer's leaves them.
    async fn store(&self, folder: &str, path: &str, bytes: &[u8]) {
        let blob = uuid_like(path);
        fs::write(self.storage_dir().join("objects").join(&blob), bytes).unwrap();
        let name = path.rsplit('/').next().unwrap().to_owned();
        let size = i64::try_from(bytes.len()).unwrap();
        let (folder, path) = (folder.to_owned(), path.to_owned());
        self.db
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO _rl_storage_objects (id, folder, path, name, blob, size,
                       content_type, etag, owner_id, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?1, ?5, 'text/plain', '\"e\"', NULL,
                       '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
                    rusqlite::params![blob, folder, path, name, size],
                )
            })
            .await
            .unwrap()
            .unwrap();
    }

    /// Whether a folder of copies waiting to be stored is left behind.
    fn seeding_left(&self) -> bool {
        fs::read_dir(self.storage_dir()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("seeding-")
        })
    }

    async fn export(&self) -> Value {
        let source = Source {
            auth: &self.auth,
            data: Some(&self.data),
            db: &self.db,
            model: &self.model,
        };
        export(source).await.unwrap()
    }

    async fn rows(&self, entity: &str) -> Vec<Value> {
        let query = RowsQuery {
            offset: 0,
            limit: MAX_ROWS,
            order_by: None,
            desc: false,
        };
        self.data.rows(entity, &query).await.unwrap().rows
    }

    async fn files(&self, folder: &str) -> Vec<Value> {
        let files = storage_admin::files(&self.db, folder, 0, storage_admin::MAX_FILES);
        files.await.unwrap().files
    }

    async fn user_id(&self, name: &str) -> String {
        self.auth.find_user(name).await.unwrap().unwrap().id
    }

    /// The files that hold stored bytes.
    fn blobs(&self) -> Vec<String> {
        let mut blobs: Vec<String> = fs::read_dir(self.storage_dir().join("objects"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        blobs.sort();
        blobs
    }
}

fn text(value: &str) -> Cell {
    Cell::Value(SqlValue::Text(value.to_owned()))
}

/// A blob name for a stored file, from its path.
fn uuid_like(path: &str) -> String {
    let hex: String = path.bytes().map(|b| format!("{b:02x}")).collect();
    format!("{hex:0<32}")
}

/// The cell `row` has for `column`.
fn value<'r>(row: &'r SeedRow, column: &str) -> Option<&'r Cell> {
    row.values
        .iter()
        .find(|(name, _)| name == column)
        .map(|(_, cell)| cell)
}

fn messages(problems: &[Problem]) -> Vec<String> {
    problems.iter().map(ToString::to_string).collect()
}

#[tokio::test]
async fn rows_come_from_json_and_csv_files_named_after_their_entities() {
    let h = harness().await;
    // Named after the table, with a byte order mark, as Excel saves it.
    h.write(
        "data/categories.csv",
        &format!("\u{feff}id,name,label,sortOrder,parent\n{C1},Home,House,,\n{C2},Work,,2,{C1}\n"),
    );
    h.write(
        "data/Todo.json",
        &format!(
            r#"[
              {{"id": "{T1}", "user_id": "$developer", "title": "Buy milk", "status": "doing",
                "cost": 1.5, "isPublic": true, "createdAt": "2026-01-01", "category": "{C1}"}},
              {{"USER_ID": "$user: ana@example.com ", "title": "$$developer", "status": "done",
                "cost": "10", "isPublic": "false", "createdAt": "2026-01-02T09:30:00+02:00",
                "estimate": null}}
            ]"#
        ),
    );
    h.write("data/README.md", "Rows for local development.");
    // Spaces after a CSV file's commas don't hide a placeholder.
    h.write("data/Project.csv", "name, owner_id\nLaunch, $developer\n");

    let seed = h.read();
    assert!(seed.exists);
    assert_eq!(messages(&seed.problems), Vec::<String>::new());
    let [category, project, todo] = &seed.tables[..] else {
        panic!("{:?}", seed.tables);
    };
    assert_eq!(
        project.rows[0].values,
        [
            ("name".to_owned(), text("Launch")),
            ("owner_id".to_owned(), Cell::User(UserRef::Developer)),
        ]
    );
    assert_eq!(
        (category.entity.as_str(), category.file.as_str()),
        ("Category", "rayfin/seed/data/categories.csv")
    );
    // A CSV row is its line. An empty cell leaves its field out; a field's column (`label` is
    // `display_label`) and a relationship (`parent` is `parent_id`) name the column.
    let lines: Vec<u64> = category.rows.iter().map(|row| row.row).collect();
    assert_eq!(lines, [2, 3]);
    assert_eq!(
        category.rows[0].values,
        [
            ("id".to_owned(), text(C1)),
            ("name".to_owned(), text("Home")),
            ("display_label".to_owned(), text("House")),
        ]
    );
    assert_eq!(
        value(&category.rows[1], "sortOrder"),
        Some(&Cell::Value(SqlValue::Integer(2)))
    );
    assert_eq!(value(&category.rows[1], "parent_id"), Some(&text(C1)));

    // Placeholders stand for users; `$$` keeps the text.
    let [first, second] = &todo.rows[..] else {
        panic!("{:?}", todo.rows);
    };
    assert_eq!(
        value(first, "user_id"),
        Some(&Cell::User(UserRef::Developer))
    );
    assert_eq!(
        value(second, "user_id"),
        Some(&Cell::User(UserRef::User("ana@example.com".into())))
    );
    assert_eq!(value(second, "title"), Some(&text("$developer")));
    assert_eq!(value(first, "category_id"), Some(&text(C1)));
    // Text converts to the field's type; JSON values as the data API takes them.
    assert_eq!(
        value(first, "createdAt"),
        Some(&text("2026-01-01T00:00:00.0000000Z"))
    );
    assert_eq!(
        value(second, "createdAt"),
        Some(&text("2026-01-02T07:30:00.0000000Z"))
    );
    assert_eq!(
        value(second, "cost"),
        Some(&Cell::Value(SqlValue::Real(10.0)))
    );
    assert_eq!(
        value(second, "isPublic"),
        Some(&Cell::Value(SqlValue::Integer(0)))
    );
    assert_eq!(
        value(second, "estimate"),
        Some(&Cell::Value(SqlValue::Null))
    );

    let summary = seed.summary();
    assert_eq!(
        summary["users"],
        json!(["$developer", "$user:ana@example.com"])
    );
    assert_eq!(
        summary["entities"],
        json!([
            {"name": "Category", "file": "rayfin/seed/data/categories.csv", "rows": 2},
            {"name": "Project", "file": "rayfin/seed/data/Project.csv", "rows": 1},
            {"name": "Todo", "file": "rayfin/seed/data/Todo.json", "rows": 2},
        ])
    );
}

#[tokio::test]
async fn problems_say_which_file_row_and_field_are_wrong() {
    let h = harness().await;
    h.write(
        "data/Todo.json",
        r#"[
          {"title": "A", "nope": 1, "estimate": "many"},
          {"title": "B", "nope": 2, "isPublic": "$developer"},
          "not a row",
          {"title": "C", "user_id": "$user:"}
        ]"#,
    );
    h.write("data/Todos.csv", "title\nD\n");
    h.write("data/Category.csv", "name,sortOrder\nHome,1,extra\n");
    h.write("data/Widget.json", "[]");
    h.write("data/Person.json", r#"{"nickname": "Ana"}"#);
    h.write("data/notes.txt", "");
    h.write("storage/user-avatars/me.png", "png");
    h.write("storage/todo-attachments/notes.txt", "a text file");
    h.write(
        "storage/todo-attachments.json",
        r#"{"notes.txt": {"owner": "ana"}, "gone.pdf": {}}"#,
    );
    h.write("storage/photos/a.png", "png");
    h.write("storage/stray.png", "png");

    let seed = h.read();
    let found = |file: &str, row: Option<u64>| -> Vec<&Problem> {
        let file = format!("rayfin/seed/{file}");
        seed.problems
            .iter()
            .filter(|problem| problem.file == file && problem.row == row)
            .collect()
    };
    let only = |file: &str, row: Option<u64>| -> &Problem {
        let problems = found(file, row);
        assert_eq!(problems.len(), 1, "{file} {row:?}: {problems:?}");
        problems[0]
    };

    // Each unknown field once, at its first row.
    let unknown = found("data/Todo.json", Some(1));
    assert_eq!(unknown.len(), 2, "{unknown:?}");
    assert!(
        unknown[0]
            .message
            .starts_with("Todo has no field `nope`. Its fields are id, ")
    );
    assert_eq!(unknown[0].field.as_deref(), Some("nope"));
    assert_eq!(
        unknown[1].message,
        "`estimate` must be a whole number, not \"many\"."
    );
    assert_eq!(unknown[1].field.as_deref(), Some("estimate"));
    assert_eq!(
        only("data/Todo.json", Some(2)).message,
        "`isPublic` must be true or false, and $developer stands for a user's id: it fills \
         text and uuid fields, such as `user_id`."
    );
    assert_eq!(
        only("data/Todo.json", Some(3)).message,
        "A row must be a JSON object of field values."
    );
    assert_eq!(
        only("data/Todo.json", Some(4)).message,
        "`$user:` needs a username, as in `$user:ana@example.com`."
    );
    assert_eq!(
        only("data/Todos.csv", None).message,
        "rayfin/seed/data/Todo.json already holds the rows of Todo; keep one file per entity."
    );
    assert_eq!(
        only("data/Category.csv", Some(2)).message,
        "The row has 3 values, and the header row 2 names."
    );
    assert!(
        only("data/Widget.json", None)
            .message
            .starts_with("There is no entity Widget: a seed file is named after its entity.")
    );
    assert!(
        only("data/Person.json", None)
            .message
            .starts_with("The file must hold a JSON array of rows")
    );
    assert!(
        only("data/notes.txt", None)
            .message
            .starts_with("Only .json and .csv")
    );
    assert_eq!(
        only("storage/photos", None).message,
        "There is no storage folder photos. The app's storage folders are todo-attachments, \
         user-avatars."
    );
    assert!(
        only("storage/stray.png", None)
            .message
            .starts_with("A storage folder's files go")
    );
    assert_eq!(
        only("storage/todo-attachments/notes.txt", None).message,
        "Storage folder 'todo-attachments' accepts image/*, application/pdf, not 'text/plain'."
    );
    let manifest = found("storage/todo-attachments.json", None);
    assert_eq!(
        messages(&manifest.into_iter().cloned().collect::<Vec<_>>()),
        [
            "rayfin/seed/storage/todo-attachments.json: \"notes.txt\": `owner` is `$developer`, \
             `$user:<username>` or null, not \"ana\".",
            "rayfin/seed/storage/todo-attachments.json: \"gone.pdf\" isn't a file in \
             rayfin/seed/storage/todo-attachments.",
        ]
    );
    // What is valid is read all the same.
    assert_eq!(seed.folders.len(), 2);
    assert_eq!(seed.folders[1].files[0].path, "me.png");

    // Rows and files need their services.
    let off = read(
        &h.seed_dir(),
        &h.model,
        Services {
            data: false,
            storage: false,
        },
    );
    assert!(off.tables.is_empty() && off.folders.is_empty());
    let message = |file: &str| {
        let file = format!("rayfin/seed/{file}");
        off.problems
            .iter()
            .find(|problem| problem.file == file)
            .map(|problem| problem.message.clone())
            .unwrap_or_default()
    };
    assert!(message("data/Todo.json").contains("data service is off"));
    assert!(message("storage/user-avatars").contains("storage service isn't running"));
}

#[tokio::test]
async fn loading_replaces_the_rows_and_files_with_the_seed() {
    let h = harness().await;
    h.write(
        "data/Category.json",
        &format!(r#"[{{"id": "{C2}", "name": "Old"}}]"#),
    );
    h.write("storage/user-avatars/old.png", "old");
    h.load().await.unwrap();
    let old = h.blobs();
    assert_eq!(old.len(), 1);

    // Rows that refer to each other in any order, rows left without an id, files with owners and
    // fields.
    h.clear_seed();
    h.write(
        "data/Comment.csv",
        &format!(
            "author_id,authorEmail,body,createdAt,todo\n\
             $user:bob@example.com,bob@example.com,Nice,2026-01-03,{T1}\n"
        ),
    );
    h.write(
        "data/Todo.json",
        &format!(
            r#"[{{"id": "{T1}", "user_id": "$developer", "title": "Buy milk", "status": "doing",
                 "cost": 1.5, "createdAt": "2026-01-01", "category": "{C1}"}}]"#
        ),
    );
    h.write(
        "data/Category.json",
        &format!(r#"[{{"id": "{C1}", "name": "Home"}}, {{"name": "Work", "parent_id": "{C1}"}}]"#),
    );
    h.write("storage/todo-attachments/receipts/r1.pdf", "%PDF-1.7");
    h.write(
        "storage/todo-attachments.json",
        r#"{"receipts/r1.pdf": {"owner": "$user:bob@example.com",
            "fields": {"team_id": "t1", "pages": 2}}}"#,
    );
    h.write("storage/user-avatars/me.png", "png");

    // A dry run says what loading does, and changes nothing.
    let checked = h.check().await.unwrap();
    assert_eq!(checked.describe(), "4 rows in 3 entities and 2 files");
    assert!(checked.users.is_empty());
    assert_eq!(h.rows("Category").await.len(), 1, "the old category stays");
    assert_eq!(h.blobs(), old, "no bytes are copied");
    let bob = h.auth.find_user("bob@example.com").await.unwrap();
    assert!(bob.is_none(), "no user is created");

    let loaded = h.load().await.unwrap();

    assert_eq!(loaded.describe(), "4 rows in 3 entities and 2 files");
    let developer = h.user_id(rl_auth::LOCAL_DEVELOPER).await;
    let bob = h.user_id("bob@example.com").await;
    let users: Vec<(String, String)> = loaded
        .users
        .iter()
        .map(|(user, account)| (user.placeholder(), account.id.clone()))
        .collect();
    assert_eq!(
        users,
        [
            ("$developer".to_owned(), developer.clone()),
            ("$user:bob@example.com".to_owned(), bob.clone()),
        ]
    );

    let categories = h.rows("Category").await;
    assert_eq!(categories.len(), 2, "the old category is gone");
    let work = categories.iter().find(|row| row["name"] == "Work").unwrap();
    assert_eq!(work["parent_id"], C1);
    assert_eq!(work["sortOrder"], 0, "a field left out has its default");
    assert_eq!(
        work["id"].as_str().map(str::len),
        Some(36),
        "and `id` a new uuid"
    );
    let todos = h.rows("Todo").await;
    assert_eq!(todos[0]["user_id"], developer.as_str());
    assert_eq!(todos[0]["category_id"], C1);
    assert_eq!(h.rows("Comment").await[0]["author_id"], bob.as_str());

    let attachments = h.files("todo-attachments").await;
    assert_eq!(attachments.len(), 1);
    let receipt = &attachments[0];
    assert_eq!(receipt["path"], "receipts/r1.pdf");
    assert_eq!(receipt["name"], "r1.pdf");
    assert_eq!(receipt["contentType"], "application/pdf");
    assert_eq!(receipt["ownerId"], bob.as_str());
    assert_eq!(receipt["fields"], json!({"team_id": "t1", "pages": 2}));
    assert_eq!(receipt["size"], 8);
    let avatars = h.files("user-avatars").await;
    assert_eq!(avatars.len(), 1, "the old avatar is gone");
    assert_eq!(avatars[0]["ownerId"], developer.as_str());
    let stored = storage_admin::file(&h.db, &h.storage_dir(), "user-avatars", "me.png")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fs::read(&stored.bytes).unwrap(), b"png");
    let blobs = h.blobs();
    assert_eq!(blobs.len(), 2);
    assert!(!blobs.contains(&old[0]), "the replaced bytes are deleted");
    assert!(!h.seeding_left());
}

#[tokio::test]
async fn stored_files_stay_while_the_storage_service_is_off() {
    let h = harness().await;
    h.write("storage/user-avatars/me.png", "png");
    h.load().await.unwrap();
    h.clear_seed();
    h.write(
        "data/Category.json",
        &format!(r#"[{{"id": "{C1}", "name": "Home"}}]"#),
    );
    let loaded = h.run(false, false).await.unwrap();
    assert_eq!(loaded.describe(), "1 row in 1 entity and 0 files");
    assert_eq!(h.rows("Category").await.len(), 1);
    assert_eq!(
        h.files("user-avatars").await.len(),
        1,
        "the stored file stays"
    );
}

#[tokio::test]
async fn nothing_changes_when_the_database_refuses_the_rows() {
    let h = harness().await;
    h.write(
        "data/Category.json",
        &format!(r#"[{{"id": "{C1}", "name": "Home"}}]"#),
    );
    h.write("storage/user-avatars/me.png", "png");
    h.load().await.unwrap();
    let blobs = h.blobs();

    h.write(
        "data/Category.json",
        &format!(
            r#"[{{"id": "{C1}", "name": "Home"}}, {{"id": "{C2}", "name": "home"}},
                {{"name": "X"}}]"#
        ),
    );
    h.write(
        "data/Todo.csv",
        &format!(
            "id,user_id,title,status,cost,createdAt,contactEmail\n\
             {T1},$developer,Buy milk,someday,1,2026-01-01,\n\
             {T2},$developer,Walk dog,done,1,2026-01-01,not-an-email\n"
        ),
    );
    h.write("storage/user-avatars/new.png", "new");
    let Err(LoadError::Invalid(problems)) = h.load().await else {
        panic!("the seed loaded");
    };
    assert_eq!(
        messages(&problems),
        [
            "rayfin/seed/data/Category.json, row 2: `name` must be unique, and another row has \
             the same value.",
            "rayfin/seed/data/Category.json, row 3: `name` must be at least 2 characters long.",
            "rayfin/seed/data/Todo.csv, row 2: `status` must be one of: its new, doing, done, its \
             archived.",
            "rayfin/seed/data/Todo.csv, row 3: `contactEmail` must be an email address \
             (name@domain.tld, no spaces).",
        ]
    );
    assert_eq!(problems[0].field.as_deref(), Some("name"));
    let Err(LoadError::Invalid(checked)) = h.check().await else {
        panic!("the dry run passed");
    };
    assert_eq!(checked, problems, "a dry run finds the same problems");

    // Once each row is valid on its own, the references between them are checked.
    h.write(
        "data/Category.json",
        &format!(r#"[{{"id": "{C1}", "name": "Home"}}]"#),
    );
    h.write(
        "data/Todo.csv",
        &format!(
            "id,user_id,title,status,cost,createdAt,category\n\
             {T1},$developer,Buy milk,done,1,2026-01-01,{C2}\n"
        ),
    );
    let Err(LoadError::Invalid(problems)) = h.load().await else {
        panic!("the seed loaded");
    };
    assert_eq!(
        messages(&problems),
        [format!(
            "rayfin/seed/data/Todo.csv, row 2: `category_id` is {C2}, but no Category has that id."
        )]
    );
    assert_eq!(problems[0].field.as_deref(), Some("category_id"));

    // The local data is as it was, and the copies of the seed's files are gone.
    assert_eq!(h.rows("Category").await.len(), 1);
    assert!(h.rows("Todo").await.is_empty());
    assert_eq!(h.files("user-avatars").await.len(), 1);
    assert_eq!(h.blobs(), blobs);
    assert!(!h.seeding_left());
}

#[tokio::test]
async fn exported_local_data_loads_back_as_it_was() {
    // An optional field with a default: its null must stay null.
    let h = harness_with(|model| {
        let category = model.entities.iter_mut().find(|e| e.name == "Category");
        let columns = &mut category.unwrap().columns;
        let sort_order = columns.iter_mut().find(|c| c.field == "sortOrder").unwrap();
        sort_order.nullable = true;
    })
    .await;
    h.write(
        "data/Category.json",
        &format!(r#"[{{"id": "{C1}", "name": "Home", "sortOrder": null}}, {{"id": "{C2}", "name": "Work"}}]"#),
    );
    h.write(
        "data/Todo.json",
        &format!(
            r#"[
              {{"id": "{T1}", "user_id": "$user:ana@example.com", "title": "$$developer",
                "status": "doing", "cost": 1.5, "isPublic": true,
                "createdAt": "2026-01-01T10:00:00Z"}},
              {{"id": "{T2}", "user_id": "$developer", "title": "Walk dog", "status": "done",
                "cost": 2.5, "createdAt": "2026-01-02", "slug": "$5 off",
                "secretNote": " $$user:ana@example.com "}}
            ]"#
        ),
    );
    h.write("storage/todo-attachments/a.pdf", "%PDF");
    h.write("storage/todo-attachments/b.pdf", "bytes");
    h.write(
        "storage/todo-attachments.json",
        r#"{"a.pdf": {"owner": null, "fields": {"team_id": "t1"}},
            "b.pdf": {"contentType": "image/png"}}"#,
    );
    h.write("storage/user-avatars/me.png", "png");
    h.load().await.unwrap();
    // Stored files whose paths can't be seed files' everywhere, or that loading would skip.
    h.store("user-avatars", "CON.txt", b"device").await;
    h.store("user-avatars", "notes/.hidden", b"hidden").await;
    h.store("user-avatars", "Pics/a.png", b"a").await;
    h.store("user-avatars", "pics/b.png", b"b").await;

    let exported = h.export().await;
    assert_eq!(
        exported["data"],
        json!([
            {
                "entity": "Category",
                "file": "data/Category.json",
                "rows": [
                    {"id": C1, "name": "Home", "sortOrder": null},
                    {"id": C2, "name": "Work", "sortOrder": 0},
                ],
            },
            {
                "entity": "Todo",
                "file": "data/Todo.json",
                "rows": [
                    {"id": T1, "user_id": "$user:ana@example.com", "title": "$$developer",
                     "status": "doing", "cost": 1.5, "isPublic": true,
                     "createdAt": "2026-01-01T10:00:00.000Z"},
                    {"id": T2, "user_id": "$developer", "title": "Walk dog", "slug": "$5 off",
                     "status": "done", "cost": 2.5, "isPublic": false,
                     "createdAt": "2026-01-02T00:00:00.000Z",
                     "secretNote": " $$user:ana@example.com "},
                ],
            },
        ])
    );
    assert_eq!(
        exported["skipped"],
        json!([
            {"folder": "user-avatars", "path": "CON.txt",
             "message": "`CON.txt` is a name Windows keeps for a device."},
            {"folder": "user-avatars", "path": "notes/.hidden",
             "message": "`.hidden` is the name of a hidden or system file, which loading seed data skips."},
            {"folder": "user-avatars", "path": "pics/b.png",
             "message": "Its folder `pics` differs only in case from `Pics`, and on Windows and macOS they'd be one."},
        ])
    );
    assert_eq!(
        exported["storage"],
        json!([
            {
                "folder": "todo-attachments",
                "dir": "storage/todo-attachments",
                "manifestFile": "storage/todo-attachments.json",
                "manifest": {
                    "a.pdf": {"owner": null, "fields": {"team_id": "t1"}},
                    "b.pdf": {"contentType": "image/png"},
                },
                "files": [{"path": "a.pdf", "size": 4}, {"path": "b.pdf", "size": 5}],
            },
            {
                "folder": "user-avatars",
                "dir": "storage/user-avatars",
                "manifestFile": "storage/user-avatars.json",
                "manifest": {"Pics/a.png": {"owner": null, "contentType": "text/plain"}},
                "files": [{"path": "Pics/a.png", "size": 1}, {"path": "me.png", "size": 3}],
            },
        ])
    );

    // Written out as a host writes it, it loads back to the same local data.
    h.clear_seed();
    for table in exported["data"].as_array().unwrap() {
        let rows = serde_json::to_string_pretty(&table["rows"]).unwrap();
        h.write(table["file"].as_str().unwrap(), &rows);
    }
    for folder in exported["storage"].as_array().unwrap() {
        let name = folder["folder"].as_str().unwrap();
        if !folder["manifest"].is_null() {
            let manifest = folder["manifest"].to_string();
            h.write(folder["manifestFile"].as_str().unwrap(), &manifest);
        }
        for file in folder["files"].as_array().unwrap() {
            let path = file["path"].as_str().unwrap();
            let stored = storage_admin::file(&h.db, &h.storage_dir(), name, path)
                .await
                .unwrap()
                .unwrap();
            let bytes = fs::read_to_string(stored.bytes).unwrap();
            h.write(
                &format!("{}/{path}", folder["dir"].as_str().unwrap()),
                &bytes,
            );
        }
    }
    h.load().await.unwrap();
    let again = h.export().await;
    assert_eq!(
        (&again["data"], &again["storage"]),
        (&exported["data"], &exported["storage"])
    );
    assert_eq!(
        again["skipped"],
        json!([]),
        "the files left out aren't stored now"
    );
}
