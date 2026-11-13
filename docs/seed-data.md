# Seed data

A project can declare rows and files for local development in `rayfin/seed`.
`rayfin-local serve --seed` loads them in place of the local data, and so do programs that run rayfin-local through the [control API](embedding.md#control-api).
Everyone who opens the project starts from the same data, including rows that belong to a signed-in user.
[Seed scripts](https://rayfin.ai/docs/data/seeding) can't write those: they can only act as an anonymous caller.

Only rayfin-local reads `rayfin/seed`; seed data never goes to Fabric.

```text
rayfin/seed/
  data/
    Category.csv
    Todo.json
  storage/
    todo-attachments/
      receipts/2025-01.pdf
    todo-attachments.json
    user-avatars/
      me.png
```

## Rows

`rayfin/seed/data` holds a `.json` or a `.csv` file per entity, named after the entity (`Todo.json`) or its table (`Todos.csv`), whatever the case.

- **JSON:** an array of objects, keyed by field name.
- **CSV:** a header row of field names, then a row per line.
  An empty cell leaves its field out.
  Files are UTF-8, with or without a byte order mark (Excel's "CSV UTF-8").

```json
[
  {
    "id": "6f9619ff-8b86-4011-b42d-00c04fc964ff",
    "user_id": "$developer",
    "title": "Buy milk",
    "status": "doing",
    "dueDate": "2025-02-01",
    "category": "0e5f2b2a-3b8c-4d3e-9f6a-1c2d3e4f5a6b"
  }
]
```

```csv
id,name,sortOrder
0e5f2b2a-3b8c-4d3e-9f6a-1c2d3e4f5a6b,Home,1
```

- A key is a field (in any case), its column's name, or a `@one` relationship, which sets the relationship's key column: `"category"` sets `category_id` to the related row's id.
- A field left out gets what a create gives it: its default, or a new uuid for `id`.
  Give a row an `id` when other rows refer to it.
- JSON values are as the data API takes them: numbers, `true` and `false`, `null` and strings.
  Text (a CSV cell, or a JSON string) converts to the field's type:
  - numbers;
  - `true`, `false`, `1` and `0`;
  - uuids;
  - dates, as `2025-01-31` (midnight UTC) or an RFC 3339 date and time (`2025-01-31T09:30:00Z`, `2025-01-31T09:30:00+02:00`).
- Rows may come in any order, across files.
  References between them are checked once all of them are in.

## Users

Row policies compare a row's owner with the signed-in user (`@claims.sub eq @item.user_id`).
These placeholders stand for a user's id, so that the right user sees each row:

| Placeholder | The user |
| --- | --- |
| `$developer` | You, as rayfin-local signs you in: your Entra identity when it signs you in with it, else `developer@example.com`. |
| `$user:<username>` | A local user such as `$user:ana@example.com`, created without a password when missing. Preview the app as them with `--sign-in-as user:<username>`. |

- Placeholders fill text and uuid fields.
- A value that is a placeholder with one more `$` is text: `$$developer` is the text `$developer`.

## Files

`rayfin/seed/storage/<folder>/` holds a storage folder's files, at their paths in the folder.
`<folder>` is the folder's name (`todo-attachments`) or its `@blob` class's name (`Attachments`).

By default, a file:

- belongs to you (`$developer`);
- has the content type its extension suggests;
- has no fields.

`rayfin/seed/storage/<folder>.json` says otherwise, by path:

```json
{
  "receipts/2025-01.pdf": { "owner": "$user:ana@example.com", "fields": { "team_id": "t1" } },
  "logo": { "contentType": "image/svg+xml", "owner": null }
}
```

| Key | Meaning |
| --- | --- |
| `owner` | `$developer`, `$user:<username>`, or `null` for no owner (as after an anonymous upload). |
| `contentType` | The file's MIME type. |
| `fields` | The folder's app fields. |

Files follow the folder's rules as uploads do: its content types and its size limit.
Each file can be at most 25 MiB, and all files together at most 1 GiB.

Some files are ignored:

- hidden files and folders, such as `.gitkeep` and `.DS_Store`;
- `Thumbs.db` and `desktop.ini`;
- `README.md` files in `rayfin/seed/data` and `rayfin/seed/storage`.

## Loading

- `rayfin-local serve --seed` loads the seed data once the backend is up.
  `serve --reset --seed` starts from the seed data alone.
- Programs that run rayfin-local load it with `POST /seed` ([embedding.md](embedding.md#control-api)).
  `GET /seed` checks the files without loading them, and a dry run (`POST /seed` with `"dryRun": true`) also finds what the database would refuse, without changing anything.

Loading replaces the local data in one transaction: every entity's rows, including entities without a seed file, and every stored file.
Users and their sessions stay.
The users the placeholders name are created when missing.
While the storage service is off, the stored files stay as they are.

When the seed data has a problem, nothing changes.
Problems include:

- a file that can't be read;
- an unknown entity or field;
- a value of the wrong type;
- a row the database refuses: a missing required field, a duplicate unique value, a value outside its `@set`, or a reference to a row that isn't there.

Each problem names its file, row and field.
The row is a JSON array's item (counted from 1), or a CSV file's line.

## Saving the local data as seed data

`GET /seed/export` ([embedding.md](embedding.md#control-api)) returns the local data in this form, for programs to write to `rayfin/seed`:

- each entity's rows, as `data/<Entity>.json`, without the `null` values that leaving a field out gives too;
- each storage folder's files;
- a `<folder>.json` for the files that need one.

User ids become placeholders: yours `$developer`, others `$user:<username>`.

Stored files whose paths can't be file paths everywhere are left out and listed with why:

- names Windows doesn't allow, such as `CON.txt` or `a:b.png`;
- paths that differ only in case from another file's, such as `Pics/a.png` beside `pics/b.png`, which are one on Windows and macOS;
- hidden files, which loading skips.
