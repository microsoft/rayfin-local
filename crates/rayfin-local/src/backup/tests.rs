use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rl_auth::AuthService;
use rl_model::CallerResolver as _;
use rl_project::Project;

use super::*;
use crate::commands::users::open_auth;

const RAYFIN_YML: &str =
    "id: app\nname: App\nversion: 1.0.0\nservices:\n  auth:\n    enabled: true\n";

struct Fixture {
    dir: tempfile::TempDir,
    project: Project,
    state: LocalState,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("rayfin")).unwrap();
    std::fs::write(dir.path().join("rayfin").join("rayfin.yml"), RAYFIN_YML).unwrap();
    let project = Project::discover(dir.path()).unwrap();
    let state = LocalState::load_or_create(&project).unwrap();
    Fixture {
        dir,
        project,
        state,
    }
}

fn connect(state: &LocalState) -> Connection {
    let conn = Connection::open(state.db_path()).unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         CREATE TABLE IF NOT EXISTS todos (id INTEGER PRIMARY KEY, owner TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS _rl_storage_objects (
             id TEXT PRIMARY KEY NOT NULL, blob TEXT NOT NULL UNIQUE);
         CREATE TABLE IF NOT EXISTS _rl_storage_operations (id TEXT PRIMARY KEY NOT NULL);",
    )
    .unwrap();
    conn
}

fn add_object(state: &LocalState, conn: &Connection, blob: &str, contents: &[u8]) {
    let objects = state.storage_dir().join("objects");
    std::fs::create_dir_all(&objects).unwrap();
    std::fs::write(objects.join(blob), contents).unwrap();
    conn.execute(
        "INSERT INTO _rl_storage_objects (id, blob) VALUES (?1, ?1)",
        [blob],
    )
    .unwrap();
}

/// A user `ana` signed in, two of her rows, a stored file and a pending storage operation.
async fn seed(state: &LocalState) -> (AuthService, rl_auth::TokenResponse) {
    let auth = open_auth(state).await.unwrap();
    let ana = auth.create_user("ana", "ana-password").await.unwrap();
    let token = auth.issue_session(&ana).await.unwrap();
    let conn = connect(state);
    conn.execute(
        "INSERT INTO todos (owner) VALUES (?1), (?1)",
        [ana.id.as_str()],
    )
    .unwrap();
    conn.execute("INSERT INTO _rl_storage_operations (id) VALUES ('op')", [])
        .unwrap();
    add_object(state, &conn, "blob-1", b"first file");
    (auth, token)
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT count(*) FROM \"{table}\""), [], |row| {
        row.get(0)
    })
    .unwrap()
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn temp_entries(dir: &Path) -> Vec<String> {
    entries(dir)
        .into_iter()
        .filter(|name| name.starts_with(".rl-"))
        .collect()
}

fn file_paths(manifest: &Manifest) -> Vec<&str> {
    manifest
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect()
}

#[tokio::test]
async fn backs_up_the_data_with_the_users_but_without_sessions() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let f = fixture();
    let (_auth, _token) = seed(&f.state).await;
    std::fs::write(f.state.dir().join("secrets.json"), "{}").unwrap();
    let out = f.dir.path().join("backups").join("one");

    let manifest = create(f.state.dir(), &out).unwrap();

    assert_eq!(manifest.format, FORMAT);
    assert_eq!(manifest.format_version, FORMAT_VERSION);
    assert!(manifest.database);
    assert_eq!(
        file_paths(&manifest),
        ["db.sqlite", "storage/objects/blob-1"]
    );
    assert_eq!(entries(&out), ["db.sqlite", "manifest.json", "storage"]);
    assert_eq!(
        fs::read(out.join("storage/objects/blob-1")).unwrap(),
        b"first file"
    );
    let written: Manifest = serde_json::from_slice(&fs::read(out.join(MANIFEST)).unwrap()).unwrap();
    assert_eq!(written, manifest);
    for file in &manifest.files {
        assert_eq!(
            hash_file(&out.join(&file.path)).unwrap(),
            (file.size, file.sha256.clone())
        );
    }

    let copy = Connection::open(out.join("db.sqlite")).unwrap();
    let mode: String = copy
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "delete");
    assert_eq!(count(&copy, "_rl_auth_users"), 1);
    assert_eq!(count(&copy, "todos"), 2);
    assert_eq!(count(&copy, "_rl_storage_objects"), 1);
    assert!(count(&copy, "_rl_migrations") > 0);
    for table in RUNTIME_TABLES {
        assert_eq!(count(&copy, table), 0, "{table}");
    }
    // Nothing is left next to the backup.
    assert_eq!(entries(out.parent().unwrap()), ["one"]);
}

#[test]
fn without_a_database_writes_an_empty_backup_and_creates_none() {
    let f = fixture();
    let out = f.dir.path().join("empty");
    std::fs::create_dir(&out).unwrap();

    let manifest = create(f.state.dir(), &out).unwrap();

    assert!(!manifest.database);
    assert!(manifest.files.is_empty());
    assert_eq!(entries(&out), ["manifest.json"]);
    assert!(!f.state.db_path().exists());
    assert_eq!(read_manifest(&out).unwrap(), manifest);
}

#[test]
fn refuses_outputs_that_are_not_new_and_outside_the_state() {
    let f = fixture();
    let root = f.dir.path();
    let full = root.join("full");
    std::fs::create_dir(&full).unwrap();
    std::fs::write(full.join("keep.txt"), "mine").unwrap();
    let file = root.join("file");
    std::fs::write(&file, "mine").unwrap();

    let err = create(f.state.dir(), &full).unwrap_err();
    assert_eq!(err.code(), "output-not-empty", "{err}");
    assert_eq!(entries(&full), ["keep.txt"]);
    let err = create(f.state.dir(), &file).unwrap_err();
    assert_eq!(err.code(), "output-invalid", "{err}");
    for inside in [f.state.dir().to_path_buf(), f.state.dir().join("backup")] {
        let err = create(f.state.dir(), &inside).unwrap_err();
        assert_eq!(err.code(), "output-invalid", "{}: {err}", inside.display());
    }
    // A path through `..` that ends inside the state folder.
    let sneaky = full.join("..").join(".rayfin-local").join("b");
    assert_eq!(
        create(f.state.dir(), &sneaky).unwrap_err().code(),
        "output-invalid"
    );
    assert!(!f.state.dir().join("backup").exists());
    assert!(!f.state.dir().join("b").exists());
}

#[tokio::test]
async fn a_failed_backup_leaves_nothing() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let f = fixture();
    let (_auth, _token) = seed(&f.state).await;
    let parent = f.dir.path().join("backups");
    let out = parent.join("one");
    let objects = f.state.storage_dir().join("objects");

    // A file the database refers to is missing after every copy of the database: the backup
    // gives up after its attempts.
    let mut attempts = Vec::new();
    let err = create_with(f.state.dir(), &out, &mut |attempt| {
        attempts.push(attempt);
        let _ = fs::rename(objects.join("blob-1"), objects.join("aside"));
    })
    .unwrap_err();
    assert_eq!(err.code(), "storage-changed", "{err}");
    assert_eq!(attempts, [1, 2, 3]);
    assert!(!out.exists());
    assert!(entries(&parent).is_empty(), "{:?}", entries(&parent));
}

#[tokio::test]
async fn stored_files_match_the_database_copy_when_they_change_during_a_backup() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let f = fixture();
    let (_auth, _token) = seed(&f.state).await;
    let conn = connect(&f.state);
    let objects = f.state.storage_dir().join("objects");

    // After the database is copied `blob-1` is deleted (its row first, as storage does): the
    // backup starts over and copies what the new copy refers to.
    let out = f.dir.path().join("deleted");
    let manifest = create_with(f.state.dir(), &out, &mut |attempt| {
        if attempt == 1 {
            conn.execute("DELETE FROM _rl_storage_objects WHERE blob = 'blob-1'", [])
                .unwrap();
            fs::remove_file(objects.join("blob-1")).unwrap();
        }
    })
    .unwrap();
    assert_eq!(file_paths(&manifest), ["db.sqlite"]);

    // `blob-2` is uploaded after the copy of the database: it's in neither.
    let out = f.dir.path().join("uploaded");
    let manifest = create_with(f.state.dir(), &out, &mut |_| {
        add_object(&f.state, &conn, "blob-2", b"second file");
    })
    .unwrap();
    assert_eq!(file_paths(&manifest), ["db.sqlite"]);
    let copy = Connection::open(out.join("db.sqlite")).unwrap();
    assert_eq!(count(&copy, "_rl_storage_objects"), 0);
    assert!(!out.join("storage/objects/blob-2").exists());
}

/// While `serve` holds its lock and writes, backups neither wait for the lock nor hold up the
/// writes, and each is a consistent copy.
#[tokio::test]
async fn backs_up_while_serving_and_writing() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let f = fixture();
    let (_auth, _token) = seed(&f.state).await;
    let (_serving, lock) = LocalState::open_for_serve(&f.project).unwrap();
    assert!(lock.is_exclusive());

    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let stop = Arc::clone(&stop);
        let state = f.state.clone();
        std::thread::spawn(move || {
            let conn = connect(&state);
            let mut slowest = Duration::ZERO;
            let mut writes = 0;
            while !stop.load(Ordering::Relaxed) {
                let started = Instant::now();
                conn.execute("INSERT INTO todos (owner) VALUES ('writer')", [])
                    .unwrap();
                slowest = slowest.max(started.elapsed());
                writes += 1;
            }
            (writes, slowest)
        })
    };

    let mut previous = 0;
    for n in 0..10 {
        let out = f.dir.path().join("backups").join(n.to_string());
        let manifest = create(f.state.dir(), &out).unwrap();
        assert!(manifest.database);
        let copy = Connection::open(out.join("db.sqlite")).unwrap();
        let check: String = copy
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(check, "ok");
        let rows = count(&copy, "todos");
        assert!(rows >= previous, "{rows} < {previous}");
        previous = rows;
        std::thread::sleep(Duration::from_millis(20));
    }
    stop.store(true, Ordering::Relaxed);
    let (writes, slowest) = writer.join().unwrap();
    assert!(writes > 10, "only {writes} writes");
    assert!(previous > 2, "no backup saw a concurrent write");
    assert!(slowest < Duration::from_secs(2), "a write took {slowest:?}");
}

#[tokio::test]
async fn restores_the_data_and_signs_everyone_out() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let f = fixture();
    let (auth, before) = seed(&f.state).await;
    let stamp = auth.session_stamp().await.unwrap();
    let keys = entries(&f.state.keys_dir());
    let key_files: Vec<Vec<u8>> = keys
        .iter()
        .map(|name| fs::read(f.state.keys_dir().join(name)).unwrap())
        .collect();
    let out = f.dir.path().join("backup");
    create(f.state.dir(), &out).unwrap();

    // Changes after the backup.
    let bob = auth.create_user("bob", "bob-password").await.unwrap();
    let after = auth.issue_session(&bob).await.unwrap();
    let conn = connect(&f.state);
    conn.execute("DELETE FROM todos", []).unwrap();
    add_object(&f.state, &conn, "blob-2", b"second file");
    drop(conn);
    drop(auth);
    std::fs::create_dir(f.state.dir().join(".rl-restore-stale")).unwrap();
    std::fs::create_dir(f.state.dir().join(".rl-restore-kept-old")).unwrap();

    let (state, _lock) = LocalState::open_locked(&f.project).unwrap();
    let restored = restore(&state, &out).unwrap();
    assert_eq!(restored.signed_out, 2);
    assert!(restored.manifest.database);

    let auth = open_auth(&state).await.unwrap();
    let users: Vec<String> = auth
        .list_users()
        .await
        .unwrap()
        .iter()
        .map(|user| user.name().to_owned())
        .collect();
    assert_eq!(users, ["ana"]);
    auth.verify_password("ana", "ana-password").await.unwrap();
    for token in [&before, &after] {
        assert!(auth.resolve(Some(&token.access_token)).is_err());
    }
    // Pages drop the sessions they stored.
    assert_ne!(auth.session_stamp().await.unwrap(), stamp);
    let conn = connect(&state);
    assert_eq!(count(&conn, "todos"), 2);
    assert_eq!(count(&conn, "_rl_storage_operations"), 0);
    let objects = state.storage_dir().join("objects");
    assert_eq!(entries(&objects), ["blob-1"]);
    assert_eq!(fs::read(objects.join("blob-1")).unwrap(), b"first file");

    // The keys and state.json are kept; staging is cleaned, set-aside data never deleted.
    assert_eq!(state.data().app_id, f.state.data().app_id);
    assert_eq!(entries(&state.keys_dir()), keys);
    for (name, contents) in keys.iter().zip(&key_files) {
        assert_eq!(&fs::read(state.keys_dir().join(name)).unwrap(), contents);
    }
    assert_eq!(temp_entries(state.dir()), [".rl-restore-kept-old"]);

    // Signing in again works.
    let ana = auth.find_user("ana").await.unwrap().unwrap();
    let token = auth.issue_session(&ana).await.unwrap();
    auth.resolve(Some(&token.access_token)).unwrap();
}

#[tokio::test]
async fn refuses_bad_backups_and_keeps_the_data() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let f = fixture();
    let (auth, _token) = seed(&f.state).await;
    let good = f.dir.path().join("good");
    create(f.state.dir(), &good).unwrap();
    auth.create_user("bob", "bob-password").await.unwrap();

    let copy = |name: &str| {
        let to = f.dir.path().join(name);
        for entry in walkdir::WalkDir::new(&good) {
            let entry = entry.unwrap();
            let target = to.join(entry.path().strip_prefix(&good).unwrap());
            if entry.file_type().is_dir() {
                fs::create_dir_all(&target).unwrap();
            } else {
                fs::copy(entry.path(), &target).unwrap();
            }
        }
        to
    };
    let edit_manifest = |dir: &Path, edit: &dyn Fn(&mut serde_json::Value)| {
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.join(MANIFEST)).unwrap()).unwrap();
        edit(&mut manifest);
        fs::write(dir.join(MANIFEST), serde_json::to_vec(&manifest).unwrap()).unwrap();
    };

    let changed = copy("changed");
    fs::write(changed.join("storage/objects/blob-1"), b"other file").unwrap();
    let damaged = copy("damaged");
    let mut bytes = fs::read(damaged.join("db.sqlite")).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    fs::write(damaged.join("db.sqlite"), bytes).unwrap();
    let missing_file = copy("missing-file");
    fs::remove_file(missing_file.join("storage/objects/blob-1")).unwrap();
    let no_manifest = copy("no-manifest");
    fs::remove_file(no_manifest.join(MANIFEST)).unwrap();
    let corrupt = copy("corrupt");
    fs::write(corrupt.join(MANIFEST), "{ not json").unwrap();
    let newer = copy("newer");
    edit_manifest(&newer, &|m| {
        m["formatVersion"] = (FORMAT_VERSION + 1).into()
    });
    let other_format = copy("other-format");
    edit_manifest(&other_format, &|m| m["format"] = "something-else".into());
    let escaping = copy("escaping");
    edit_manifest(&escaping, &|m| m["files"][1]["path"] = "../outside".into());
    let no_db_flag = copy("no-db-flag");
    edit_manifest(&no_db_flag, &|m| m["database"] = false.into());
    let not_rayfin = copy("not-rayfin");
    {
        let path = not_rayfin.join("db.sqlite");
        fs::remove_file(&path).unwrap();
        Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE t (x)")
            .unwrap();
        let (size, sha256) = hash_file(&path).unwrap();
        edit_manifest(&not_rayfin, &|m| {
            m["files"][0]["size"] = size.into();
            m["files"][0]["sha256"] = sha256.clone().into();
        });
    }

    let (state, _lock) = LocalState::open_locked(&f.project).unwrap();
    for (from, code) in [
        (&changed, "backup-invalid"),
        (&damaged, "backup-invalid"),
        (&missing_file, "backup-invalid"),
        (&no_manifest, "backup-invalid"),
        (&corrupt, "backup-invalid"),
        (&newer, "backup-unsupported"),
        (&other_format, "backup-invalid"),
        (&escaping, "backup-invalid"),
        (&no_db_flag, "backup-invalid"),
        (&not_rayfin, "backup-invalid"),
    ] {
        let err = restore(&state, from).unwrap_err();
        assert_eq!(err.code(), code, "{}: {err}", from.display());
        assert!(state.db_path().is_file());
        assert!(state.storage_dir().join("objects/blob-1").is_file());
        assert!(
            temp_entries(state.dir()).is_empty(),
            "{:?}",
            entries(state.dir())
        );
        assert_eq!(
            auth.list_users().await.unwrap().len(),
            2,
            "{}",
            from.display()
        );
    }
}

#[tokio::test]
async fn restoring_an_empty_backup_is_a_reset() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let empty = fixture();
    let out = empty.dir.path().join("empty");
    create(empty.state.dir(), &out).unwrap();

    let restored = fixture();
    let reset = fixture();
    for f in [&restored, &reset] {
        let (auth, _token) = seed(&f.state).await;
        drop(auth);
    }
    let (state, _lock) = LocalState::open_locked(&restored.project).unwrap();
    let result = restore(&state, &out).unwrap();
    assert!(!result.manifest.database);
    crate::commands::serve::clear_data(&reset.state).unwrap();

    for f in [&restored, &reset] {
        assert!(
            !entries(f.state.dir())
                .iter()
                .any(|name| name.starts_with("db.sqlite")),
            "{:?}",
            entries(f.state.dir())
        );
        assert!(entries(&f.state.storage_dir().join("objects")).is_empty());
        assert!(f.state.keys_dir().is_dir());
        assert!(f.state.dir().join("state.json").is_file());
        assert!(temp_entries(f.state.dir()).is_empty());
    }
}

#[tokio::test]
async fn restores_into_a_new_state_folder() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let old = fixture();
    let (_auth, _token) = seed(&old.state).await;
    let out = old.dir.path().join("backup");
    create(old.state.dir(), &out).unwrap();

    let new = tempfile::tempdir().unwrap();
    std::fs::create_dir(new.path().join("rayfin")).unwrap();
    std::fs::write(new.path().join("rayfin").join("rayfin.yml"), RAYFIN_YML).unwrap();
    let project = Project::discover(new.path()).unwrap();
    assert!(!project.state_dir().exists());

    let (state, _lock) = LocalState::open_locked(&project).unwrap();
    let restored = restore(&state, &out).unwrap();
    assert_eq!(restored.signed_out, 0);
    assert_ne!(state.data().app_id, old.state.data().app_id);

    let auth = open_auth(&state).await.unwrap();
    auth.verify_password("ana", "ana-password").await.unwrap();
    auth.session_stamp().await.unwrap();
    assert_eq!(count(&connect(&state), "todos"), 2);
    assert_eq!(entries(&state.storage_dir().join("objects")), ["blob-1"]);
}
