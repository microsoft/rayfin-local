//! Built-in data entities, separate from the analyzer's app-owned entities.

use crate::{
    Action, ActionPermission, Column, ColumnOrigin, Dialect, Entity, FieldKind, GraphQlNames, Key,
    RolePermission,
};

/// Fabric's read-only `User` entity. Its id is the local access token's `sub`.
pub fn system_user(dialect: Dialect) -> Entity {
    let column = |name: &str, kind, db_type: &str, primary_key| Column {
        name: name.into(),
        field: name.into(),
        kind,
        db_type: db_type.into(),
        nullable: false,
        default: None,
        primary_key,
        unique: false,
        origin: ColumnOrigin::Declared,
        source: None,
    };
    Entity {
        name: "User".into(),
        table: "Users".into(),
        graphql: GraphQlNames::new("User", "Users"),
        columns: vec![
            column(
                "Id",
                FieldKind::Uuid,
                match dialect {
                    Dialect::Mssql => "uniqueidentifier",
                    Dialect::Postgresql => "uuid",
                },
                true,
            ),
            column(
                "Email",
                FieldKind::Text {
                    min_length: None,
                    max_length: Some(255),
                    pattern: None,
                },
                "varchar(255)",
                false,
            ),
        ],
        primary_key: Key {
            name: "PK_Users".into(),
            columns: vec!["Id".into()],
        },
        foreign_keys: Vec::new(),
        unique_constraints: Vec::new(),
        check_constraints: Vec::new(),
        relationships: Vec::new(),
        permissions: vec![RolePermission {
            role: "authenticated".into(),
            actions: vec![ActionPermission {
                action: Action::Read,
                fields: None,
                policy: None,
            }],
        }],
    }
}
