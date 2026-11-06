//! The entity tables as a host's control API reads and clears them, whatever the permissions:
//! row counts, pages of rows with each value as the data API answers it (the same
//! `rl_db::codec::json_expr` the queries use), and clearing rows. [`replace_rows`] writes seed
//! data in place of every row.

use std::collections::HashMap;

use rl_db::codec::{self, ConstraintKind, ConstraintViolation};
use rl_db::schema::CheckSchema;
use rl_model::{AppModel, Column, Entity, FieldKind};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, params_from_iter};
use serde::Serialize;
use serde_json::{Map, Value as Json};

use crate::DataApi;
use crate::sql::quote_ident;

/// The most rows [`DataApi::rows`] reads at once.
pub const MAX_ROWS: u32 = 500;
/// The most problems [`replace_rows`] reports.
pub const MAX_ROW_PROBLEMS: usize = 100;

/// Which rows of an entity to read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowsQuery {
    pub offset: u64,
    /// At most [`MAX_ROWS`].
    pub limit: u32,
    /// A field (its GraphQL name) to order by; the primary key when `None`, and after the field.
    pub order_by: Option<String>,
    pub desc: bool,
}

/// A page of an entity's rows.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Rows {
    pub columns: Vec<RowColumn>,
    /// One object per row, keyed by field name, in the order of `columns`.
    pub rows: Vec<Json>,
    /// How many rows the entity holds.
    pub total: u64,
}

/// A field of the rows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RowColumn {
    /// The field's GraphQL name.
    pub name: String,
    /// Its GraphQL type: `UUID`, `String`, `Int`, `Decimal`, `Boolean` or `DateTime`.
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// Whether it is the primary key.
    pub key: bool,
}

/// Rows to insert into an entity's table, for [`replace_rows`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NewRows {
    /// The entity, by name, as [`DataApi::rows`] finds it.
    pub entity: String,
    /// Each row's values by column (its SQL name). A column left out gets the value a create
    /// gives it: its default, or a new uuid for `id`.
    pub rows: Vec<Vec<(String, SqlValue)>>,
}

/// A row [`replace_rows`] could not insert, or that refers to a row that isn't there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowProblem {
    /// The index of the row's [`NewRows`].
    pub table: usize,
    /// The row's index in it.
    pub row: usize,
    /// The field (its GraphQL name) at fault, when known.
    pub field: Option<String>,
    pub message: String,
}

/// Why rows could not be read or cleared.
#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    #[error("the app has no entity {0}")]
    UnknownEntity(String),
    #[error("{entity} has no field {field}; its fields are {fields}")]
    UnknownField {
        entity: String,
        field: String,
        fields: String,
    },
    /// Rows of other entities refer to the rows to clear: `by` names them, with how many rows
    /// of each (empty when the database refused without saying which).
    #[error("{entity}'s rows can't be cleared while {} refer to them", referrers(.by))]
    Referenced {
        entity: String,
        by: Vec<(String, u64)>,
    },
    #[error("the database failed: {0}")]
    Database(String),
}

fn referrers(by: &[(String, u64)]) -> String {
    if by.is_empty() {
        return "other rows".to_owned();
    }
    by.iter()
        .map(|(entity, rows)| {
            let noun = if *rows == 1 { "row" } else { "rows" };
            format!("{rows} {entity} {noun}")
        })
        .collect::<Vec<_>>()
        .join(", ")
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

impl DataApi {
    /// How many rows each entity of the served model holds, in the model's order.
    pub async fn row_counts(&self) -> Result<Vec<(String, u64)>, AdminError> {
        let model = self.model();
        let entities: Vec<(String, String)> = model
            .entities
            .iter()
            .map(|entity| (entity.name.clone(), entity.table.clone()))
            .collect();
        let counts = self
            .db
            .call(move |conn| {
                entities
                    .into_iter()
                    .map(|(name, table)| Ok((name, count_rows(conn, &table, None)?)))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .await??;
        Ok(counts)
    }

    /// A page of `entity`'s rows (by name, as in the model), whatever the permissions.
    pub async fn rows(&self, entity: &str, query: &RowsQuery) -> Result<Rows, AdminError> {
        let model = self.model();
        let entity = find_entity(&model, entity)?;
        let mut order = Vec::new();
        if let Some(field) = &query.order_by {
            let column = find_field(entity, field)?;
            order.push(format!(
                "{} {}",
                quote_ident(&column.name),
                if query.desc { "DESC" } else { "ASC" }
            ));
        }
        for key in &entity.primary_key.columns {
            order.push(format!("{} ASC", quote_ident(key)));
        }
        let values = entity
            .columns
            .iter()
            .map(|column| {
                // json_quote keeps the JSON a boolean's expression makes, and quotes the rest.
                format!(
                    "json_quote({})",
                    codec::json_expr(&column.kind, &quote_ident(&column.name))
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT {values} FROM {} ORDER BY {} LIMIT ?1 OFFSET ?2",
            quote_ident(&entity.table),
            order.join(", ")
        );
        let table = entity.table.clone();
        let limit = i64::from(query.limit.clamp(1, MAX_ROWS));
        let offset = i64::try_from(query.offset).unwrap_or(i64::MAX);
        let fields: Vec<String> = entity.columns.iter().map(|c| c.field.clone()).collect();
        let (texts, total) = self
            .db
            .call(move |conn| -> rusqlite::Result<_> {
                let mut statement = conn.prepare(&sql)?;
                let width = statement.column_count();
                let texts = statement
                    .query_map([limit, offset], |row| {
                        (0..width)
                            .map(|index| row.get::<_, String>(index))
                            .collect::<rusqlite::Result<Vec<_>>>()
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok((texts, count_rows(conn, &table, None)?))
            })
            .await??;
        let rows = texts
            .into_iter()
            .map(|values| {
                let row: Map<String, Json> = fields
                    .iter()
                    .zip(values)
                    .map(|(field, text)| {
                        (
                            field.clone(),
                            serde_json::from_str(&text).unwrap_or(Json::Null),
                        )
                    })
                    .collect();
                Json::Object(row)
            })
            .collect();
        Ok(Rows {
            columns: entity.columns.iter().map(row_column).collect(),
            rows,
            total,
        })
    }

    /// Deletes every row of `entity`; returns how many. Refused while rows of other entities
    /// refer to them (none cascade).
    pub async fn clear_rows(&self, entity: &str) -> Result<u64, AdminError> {
        let model = self.model();
        let entity = find_entity(&model, entity)?;
        let name = entity.name.clone();
        let table = entity.table.clone();
        let referring = referring(&model, entity);
        let outcome = self
            .db
            .call(
                move |conn| -> rusqlite::Result<Result<u64, Vec<(String, u64)>>> {
                    let tx = conn.transaction()?;
                    let mut by = Vec::new();
                    for (entity, table, columns) in &referring {
                        let rows = count_rows(&tx, table, Some(columns))?;
                        if rows > 0 {
                            by.push((entity.clone(), rows));
                        }
                    }
                    if !by.is_empty() {
                        return Ok(Err(by));
                    }
                    let deleted =
                        tx.execute(&format!("DELETE FROM {}", quote_ident(&table)), [])?;
                    tx.commit()?;
                    Ok(Ok(u64::try_from(deleted).unwrap_or_default()))
                },
            )
            .await?;
        match outcome {
            Ok(Ok(deleted)) => Ok(deleted),
            Ok(Err(by)) => Err(AdminError::Referenced { entity: name, by }),
            Err(error) if is_foreign_key_violation(&error) => Err(AdminError::Referenced {
                entity: name,
                by: Vec::new(),
            }),
            Err(error) => Err(error.into()),
        }
    }

    /// Deletes every row of every entity of the served model; returns how many.
    pub async fn clear_all_rows(&self) -> Result<u64, AdminError> {
        let model = self.model();
        let tables: Vec<String> = model
            .entities
            .iter()
            .map(|entity| entity.table.clone())
            .collect();
        let deleted = self
            .db
            .call(move |conn| -> rusqlite::Result<u64> {
                let tx = conn.transaction()?;
                // Rows refer to each other across tables: check the references once all are gone.
                tx.pragma_update(None, "defer_foreign_keys", true)?;
                let mut deleted = 0;
                for table in &tables {
                    let rows = tx.execute(&format!("DELETE FROM {}", quote_ident(table)), [])?;
                    deleted += u64::try_from(rows).unwrap_or_default();
                }
                tx.commit()?;
                Ok(deleted)
            })
            .await??;
        Ok(deleted)
    }
}

/// In `conn`'s transaction: deletes every row of every entity of `model`, then inserts the rows
/// of `tables`, whatever the permissions. References between rows are checked once all of them
/// are in, so rows may come in any order.
///
/// Returns how many rows each of `tables` inserted, or the problems (at most
/// [`MAX_ROW_PROBLEMS`], by table and row): the rows the database refuses, with the constraint
/// they break, else the rows that refer to rows that aren't there. The caller commits, or rolls
/// back after problems.
pub fn replace_rows(
    conn: &Connection,
    model: &AppModel,
    tables: &[NewRows],
) -> Result<Result<Vec<u64>, Vec<RowProblem>>, AdminError> {
    // Until the commit, as rows refer to each other across tables.
    conn.pragma_update(None, "defer_foreign_keys", true)?;
    for entity in &model.entities {
        conn.execute(&format!("DELETE FROM {}", quote_ident(&entity.table)), [])?;
    }
    let mut problems = Vec::new();
    // The table and rowid of each row inserted, for the references it breaks.
    let mut inserted: HashMap<(String, i64), (usize, usize)> = HashMap::new();
    let mut counts = Vec::with_capacity(tables.len());
    for (t, table) in tables.iter().enumerate() {
        let entity = find_entity(model, &table.entity)?;
        let checks = rl_db::schema::table_schema(entity)
            .map(|schema| schema.checks)
            .unwrap_or_default();
        let mut count = 0;
        for (r, values) in table.rows.iter().enumerate() {
            let row = complete_row(entity, values)?;
            let sql = insert_sql(entity, &row);
            let result = conn
                .prepare_cached(&sql)?
                .execute(params_from_iter(row.iter().map(|(_, value)| value)));
            match result {
                Ok(_) => {
                    let key = (entity.table.to_lowercase(), conn.last_insert_rowid());
                    inserted.insert(key, (t, r));
                    count += 1;
                }
                Err(error) => {
                    let Some(violation) = codec::constraint_violation(&error) else {
                        return Err(error.into());
                    };
                    let (field, message) = describe_violation(entity, &checks, &violation);
                    problems.push(RowProblem {
                        table: t,
                        row: r,
                        field,
                        message,
                    });
                    if problems.len() == MAX_ROW_PROBLEMS {
                        return Ok(Err(problems));
                    }
                }
            }
        }
        counts.push(count);
    }
    if problems.is_empty() {
        problems = broken_references(conn, model, &inserted)?;
    }
    Ok(if problems.is_empty() {
        Ok(counts)
    } else {
        Err(problems)
    })
}

/// `values` (by column name) and the values a create gives the columns left out, in the
/// entity's column order.
fn complete_row<'e>(
    entity: &'e Entity,
    values: &[(String, SqlValue)],
) -> Result<Vec<(&'e Column, SqlValue)>, AdminError> {
    let mut row: Vec<Option<SqlValue>> = vec![None; entity.columns.len()];
    for (name, value) in values {
        let index = entity
            .columns
            .iter()
            .position(|column| column.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| AdminError::UnknownField {
                entity: entity.name.clone(),
                field: name.clone(),
                fields: field_list(entity),
            })?;
        row[index] = Some(value.clone());
    }
    Ok(entity
        .columns
        .iter()
        .zip(row)
        .filter_map(|(column, value)| {
            value
                .or_else(|| codec::generated_default(column))
                .map(|value| (column, value))
        })
        .collect())
}

fn insert_sql(entity: &Entity, row: &[(&Column, SqlValue)]) -> String {
    let table = quote_ident(&entity.table);
    if row.is_empty() {
        return format!("INSERT INTO {table} DEFAULT VALUES");
    }
    let columns: Vec<String> = row
        .iter()
        .map(|(column, _)| quote_ident(&column.name))
        .collect();
    format!(
        "INSERT INTO {table} ({}) VALUES ({})",
        columns.join(", "),
        vec!["?"; row.len()].join(", ")
    )
}

/// The field a constraint violation is about, when known, and what is wrong. SQLite's message
/// names the columns (`NOT NULL constraint failed: Todos.title`; several, comma-separated, for
/// a unique key) or the CHECK (`CHECK constraint failed: CK_Todos_status`).
fn describe_violation(
    entity: &Entity,
    checks: &[CheckSchema],
    violation: &ConstraintViolation,
) -> (Option<String>, String) {
    let detail = violation
        .message
        .split_once(": ")
        .map_or("", |(_, detail)| detail.trim());
    let columns: Vec<&Column> = detail
        .split(',')
        .filter_map(|part| {
            let part = part.trim();
            let name = part.rsplit_once('.').map_or(part, |(_, column)| column);
            entity
                .columns
                .iter()
                .find(|column| column.name.eq_ignore_ascii_case(name))
        })
        .collect();
    match (violation.kind, columns.as_slice()) {
        (ConstraintKind::NotNull, [column]) => {
            return (
                Some(column.field.clone()),
                format!("`{}` is required.", column.field),
            );
        }
        (ConstraintKind::Unique | ConstraintKind::PrimaryKey, [column]) => {
            return (
                Some(column.field.clone()),
                format!(
                    "`{}` must be unique, and another row has the same value.",
                    column.field
                ),
            );
        }
        (ConstraintKind::Unique | ConstraintKind::PrimaryKey, [_, _, ..]) => {
            let fields: Vec<String> = columns
                .iter()
                .map(|column| format!("`{}`", column.field))
                .collect();
            return (
                None,
                format!(
                    "{} must be unique together, and another row has the same values.",
                    fields.join(", ")
                ),
            );
        }
        (ConstraintKind::Check, _) => {
            let column = checks
                .iter()
                .find(|check| check.name.eq_ignore_ascii_case(detail))
                .and_then(|check| entity.column(&check.column));
            if let Some(column) = column {
                return (
                    Some(column.field.clone()),
                    check_message(entity, column, detail),
                );
            }
        }
        _ => {}
    }
    (
        None,
        format!("The database refused the row: {}.", violation.message),
    )
}

/// What the CHECK `check` (`CK_<table>_<column><suffix>`, as `rl_db::schema` names them) of
/// `column` asks for.
fn check_message(entity: &Entity, column: &Column, check: &str) -> String {
    let prefix = format!("CK_{}_{}", entity.table, column.name);
    let suffix = check.get(prefix.len()..).unwrap_or_default();
    let field = format!("`{}`", column.field);
    match (&column.kind, suffix) {
        (FieldKind::Set { allowed, .. }, "") => {
            format!("{field} must be one of: {}.", allowed.join(", "))
        }
        (
            FieldKind::Text {
                max_length: Some(max),
                ..
            }
            | FieldKind::Email {
                max_length: Some(max),
                ..
            },
            "_maxlen",
        ) => format!("{field} is longer than {max} characters."),
        (
            FieldKind::Text {
                min_length: Some(min),
                ..
            },
            "_minlen",
        ) => format!("{field} must be at least {min} characters long."),
        (FieldKind::Email { min_length, .. }, "_email") => match min_length {
            Some(min) => format!(
                "{field} must be an email address (name@domain.tld, no spaces) of at least {min} \
                 characters."
            ),
            None => format!("{field} must be an email address (name@domain.tld, no spaces)."),
        },
        (FieldKind::Int { min, max } | FieldKind::Decimal { min, max, .. }, "_range") => {
            format!("{field} must be {}.", range_text(*min, *max))
        }
        (
            FieldKind::Decimal {
                precision, scale, ..
            },
            "_type",
        ) => format!(
            "{field} is too large: a DECIMAL({precision},{scale}) has at most {} digits before \
             the decimal point.",
            precision.saturating_sub(*scale)
        ),
        _ => format!("{field} breaks the database's check {check}."),
    }
}

fn range_text(min: Option<f64>, max: Option<f64>) -> String {
    let number = |n: f64| {
        if n.fract() == 0.0 && n.abs() < 1e15 {
            format!("{}", n as i64)
        } else {
            n.to_string()
        }
    };
    match (min, max) {
        (Some(min), Some(max)) => format!("from {} to {}", number(min), number(max)),
        (Some(min), None) => format!("at least {}", number(min)),
        (None, Some(max)) => format!("at most {}", number(max)),
        (None, None) => "in range".to_owned(),
    }
}

/// The rows of `inserted` whose foreign keys refer to rows that aren't there, by table and row.
fn broken_references(
    conn: &Connection,
    model: &AppModel,
    inserted: &HashMap<(String, i64), (usize, usize)>,
) -> rusqlite::Result<Vec<RowProblem>> {
    let violations = conn
        .prepare("SELECT \"table\", rowid, parent, fkid FROM pragma_foreign_key_check()")?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let entity_of = |table: &str| {
        model
            .entities
            .iter()
            .find(|entity| entity.table.eq_ignore_ascii_case(table))
    };
    let mut problems = Vec::new();
    for (table, rowid, parent, fkid) in violations {
        let Some(rowid) = rowid else { continue };
        let Some(&(t, r)) = inserted.get(&(table.to_lowercase(), rowid)) else {
            continue;
        };
        let key: Option<(String, String)> = conn
            .query_row(
                "SELECT \"from\", \"to\" FROM pragma_foreign_key_list(?1) WHERE id = ?2
                 ORDER BY seq LIMIT 1",
                rusqlite::params![table, fkid],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let parent_entity = entity_of(&parent);
        let parent_name = parent_entity.map_or(parent.as_str(), |entity| entity.name.as_str());
        let column = key.as_ref().and_then(|(from, _)| {
            entity_of(&table)?
                .columns
                .iter()
                .find(|column| column.name.eq_ignore_ascii_case(from))
        });
        let message = match (column, &key) {
            (Some(column), Some((_, to))) => {
                let value: SqlValue = conn.query_row(
                    &format!(
                        "SELECT {} FROM {} WHERE rowid = ?1",
                        quote_ident(&column.name),
                        quote_ident(&table)
                    ),
                    [rowid],
                    |row| row.get(0),
                )?;
                let value = match value {
                    SqlValue::Text(text) => text,
                    SqlValue::Integer(n) => n.to_string(),
                    SqlValue::Real(n) => n.to_string(),
                    SqlValue::Null | SqlValue::Blob(_) => String::new(),
                };
                let to = parent_entity
                    .and_then(|entity| {
                        entity
                            .columns
                            .iter()
                            .find(|column| column.name.eq_ignore_ascii_case(to))
                    })
                    .map_or(to.as_str(), |column| column.field.as_str());
                format!(
                    "`{}` is {value}, but no {parent_name} has that {to}.",
                    column.field
                )
            }
            _ => format!("The row refers to a {parent_name} that isn't there."),
        };
        problems.push(RowProblem {
            table: t,
            row: r,
            field: column.map(|column| column.field.clone()),
            message,
        });
    }
    problems.sort_by_key(|problem| (problem.table, problem.row));
    problems.truncate(MAX_ROW_PROBLEMS);
    Ok(problems)
}

fn find_entity<'a>(model: &'a AppModel, name: &str) -> Result<&'a Entity, AdminError> {
    let name = name.trim();
    model
        .entities
        .iter()
        .find(|entity| entity.name == name)
        .or_else(|| {
            model
                .entities
                .iter()
                .find(|entity| entity.name.eq_ignore_ascii_case(name))
        })
        .ok_or_else(|| AdminError::UnknownEntity(name.to_owned()))
}

fn find_field<'a>(entity: &'a Entity, field: &str) -> Result<&'a Column, AdminError> {
    entity
        .columns
        .iter()
        .find(|column| column.field == field)
        .or_else(|| {
            entity
                .columns
                .iter()
                .find(|column| column.field.eq_ignore_ascii_case(field))
        })
        .ok_or_else(|| AdminError::UnknownField {
            entity: entity.name.clone(),
            field: field.to_owned(),
            fields: field_list(entity),
        })
}

/// The entity's fields, for messages.
fn field_list(entity: &Entity) -> String {
    entity
        .columns
        .iter()
        .map(|column| column.field.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn row_column(column: &Column) -> RowColumn {
    RowColumn {
        name: column.field.clone(),
        kind: column.kind.graphql_scalar(),
        key: column.primary_key,
    }
}

/// The other entities whose foreign keys point at `entity`'s table: name, table and key columns.
fn referring(model: &AppModel, entity: &Entity) -> Vec<(String, String, Vec<String>)> {
    model
        .entities
        .iter()
        .filter(|other| other.name != entity.name)
        .flat_map(|other| {
            other
                .foreign_keys
                .iter()
                .filter(|key| key.referenced_table.eq_ignore_ascii_case(&entity.table))
                .map(|key| (other.name.clone(), other.table.clone(), key.columns.clone()))
        })
        .collect()
}

/// The rows of `table`; with `set`, only those where each of these columns has a value.
fn count_rows(
    conn: &rusqlite::Connection,
    table: &str,
    set: Option<&[String]>,
) -> rusqlite::Result<u64> {
    let mut sql = format!("SELECT count(*) FROM {}", quote_ident(table));
    if let Some(columns) = set.filter(|columns| !columns.is_empty()) {
        let conditions: Vec<String> = columns
            .iter()
            .map(|column| format!("{} IS NOT NULL", quote_ident(column)))
            .collect();
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    let count: i64 = conn.query_row(&sql, [], |row| row.get(0))?;
    Ok(u64::try_from(count).unwrap_or_default())
}

fn is_foreign_key_violation(error: &rusqlite::Error) -> bool {
    codec::constraint_violation(error)
        .is_some_and(|violation| violation.kind == codec::ConstraintKind::ForeignKey)
}
