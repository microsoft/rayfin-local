//! `@role` policies (DAB `policy.database`) compiled to SQL predicates.
//!
//! DAB substitutes the caller's claims into the policy and adds it to the statement's `WHERE`,
//! so a policy filters rows with SQL semantics. Here [`compile`] turns a [`PolicyExpr`] into a
//! [`Pred`] with the claims (and literals) bound as parameters, converted to the storage format
//! of the field they are compared with (`rl_db::codec`): a claim compared with a uuid field is
//! parsed as a uuid, a `Date` literal as a date-time, and `true` becomes `1`. Unsupported
//! conversions fail policy parsing with `BadRequest`; null literals become SQL null tests.
//!
//! - **read** (this crate): [`read_filter`] gives the predicate ANDed into every `SELECT` of the
//!   entity (lists, by-key reads, nested relationships, relationship filters).
//! - **update/delete** (mutations): `compile` the grant's policy against the table alias and
//!   add it to the statement's `WHERE`.
//! - **create** (mutations): [`create_filter`] rejects omitted policy fields before defaults
//!   are generated, then checks the supplied row in an `INSERT … SELECT … WHERE`.
//!
//! A policy that reads a claim the caller does not have fails the whole operation with DAB's
//! [`PolicyError::MissingClaim`] (`Forbidden`), anonymous callers included.

use rl_db::codec;
use rl_model::{Action, Caller, Column, CompareOp, Entity, FieldKind, Literal, Operand as Op};
use rl_model::{Policy, PolicyExpr};
use rusqlite::types::Value as SqlValue;
use serde_json::Value as Json;

use crate::access;
use crate::error::{GraphQlError, codes, messages};
use crate::sql::{CmpOp, Operand, Pred};

/// Why a policy cannot be applied for this caller.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    /// The policy reads `claims.<0>`, which the caller's token does not have.
    #[error("{}", messages::MISSING_CLAIMS)]
    MissingClaim(String),
    /// Create policies cannot read a field omitted from the request, even with a default.
    #[error("The create policy requires field `{0}` to be supplied in the item.")]
    MissingField(String),
    /// DAB's OData resolver cannot promote an operand to the comparison's type.
    #[error("The database policy has incompatible operand types; {0}.")]
    InvalidComparison(String),
    /// The policy names a field the entity does not have (the model rejects this).
    #[error("the policy reads `item.{0}`, which is not a field of the entity")]
    UnknownField(String),
}

impl From<PolicyError> for GraphQlError {
    fn from(error: PolicyError) -> Self {
        match error {
            PolicyError::MissingClaim(_) | PolicyError::MissingField(_) => {
                GraphQlError::dab(error.to_string(), codes::FORBIDDEN)
            }
            PolicyError::InvalidComparison(_) => {
                GraphQlError::dab(error.to_string(), codes::BAD_REQUEST)
            }
            PolicyError::UnknownField(_) => {
                GraphQlError::dab(messages::DATABASE_ERROR, codes::UNEXPECTED_ERROR)
            }
        }
    }
}

/// A create policy sees only supplied fields, not database defaults or generated values.
pub fn create_filter(
    entity: &Entity,
    caller: &Caller,
    alias: &str,
    supplied: &[usize],
) -> Result<Option<Pred>, PolicyError> {
    let Some(policy) = access::grant(entity, caller.role(), Action::Create)
        .and_then(|grant| grant.policy.as_ref())
    else {
        return Ok(None);
    };
    require_claims(&policy.expr, caller)?;
    for field in policy.expr.fields() {
        if !supplied
            .iter()
            .any(|index| entity.columns[*index].field == field)
        {
            return Err(PolicyError::MissingField(field.to_owned()));
        }
    }
    compile_policy(policy, entity, caller, &|column| {
        Operand::column(alias, &column.name)
    })
    .map(Some)
}

/// The read predicate of `entity` for `caller` against the rows of table alias `alias`:
/// `Ok(None)` when the role may read every row. Callers check first that the role may read the
/// entity at all ([`access::grant`]).
pub fn read_filter(
    entity: &Entity,
    caller: &Caller,
    alias: &str,
) -> Result<Option<Pred>, PolicyError> {
    action_filter(entity, caller, Action::Read, alias)
}

/// The predicate of the grant `caller` acts under for `action`, against table alias `alias`.
pub fn action_filter(
    entity: &Entity,
    caller: &Caller,
    action: Action,
    alias: &str,
) -> Result<Option<Pred>, PolicyError> {
    let Some(policy) = access::grant(entity, caller.role(), action).and_then(|g| g.policy.as_ref())
    else {
        return Ok(None);
    };
    compile_policy(policy, entity, caller, &|column: &Column| {
        Operand::column(alias, &column.name)
    })
    .map(Some)
}

/// [`compile`] for a model [`Policy`].
pub fn compile_policy(
    policy: &Policy,
    entity: &Entity,
    caller: &Caller,
    row: &dyn Fn(&Column) -> Operand,
) -> Result<Pred, PolicyError> {
    require_claims(&policy.expr, caller)?;
    // The check AST serializes both Date objects and strings as JSON strings. Only the
    // accompanying DAB text retains their distinction (unquoted ISO versus quoted text).
    let mut dates = string_literal_types(&policy.text).into_iter();
    compile_expr(&policy.expr, entity, caller, row, &mut dates)
}

/// Compiles `expr` for `caller`; `row` gives the SQL operand of an `@item.<field>` (a column of
/// the statement's table alias, or a candidate value).
pub fn compile(
    expr: &PolicyExpr,
    entity: &Entity,
    caller: &Caller,
    row: &dyn Fn(&Column) -> Operand,
) -> Result<Pred, PolicyError> {
    require_claims(expr, caller)?;
    compile_expr(expr, entity, caller, row, &mut std::iter::empty())
}

fn require_claims(expr: &PolicyExpr, caller: &Caller) -> Result<(), PolicyError> {
    for name in expr.claims() {
        if caller.claim(name).is_none() {
            return Err(PolicyError::MissingClaim(name.to_owned()));
        }
    }
    Ok(())
}

fn compile_expr(
    expr: &PolicyExpr,
    entity: &Entity,
    caller: &Caller,
    row: &dyn Fn(&Column) -> Operand,
    dates: &mut dyn Iterator<Item = bool>,
) -> Result<Pred, PolicyError> {
    match expr {
        PolicyExpr::And(args) | PolicyExpr::Or(args) => {
            let preds = args
                .iter()
                .map(|arg| compile_expr(arg, entity, caller, row, dates))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(if matches!(expr, PolicyExpr::And(_)) {
                Pred::all(preds)
            } else {
                Pred::Or(preds)
            })
        }
        PolicyExpr::Compare { op, left, right } => {
            let left = side(left, entity, caller, dates)?;
            let right = side(right, entity, caller, dates)?;
            let cmp = match op {
                CompareOp::Eq => CmpOp::Eq,
                CompareOp::Ne => CmpOp::Ne,
            };
            Ok(match (left, right) {
                (Side::Field(a), Side::Field(b)) => {
                    if !compatible_fields(&a.kind, &b.kind) {
                        return Err(PolicyError::InvalidComparison(
                            "compare fields of compatible types".into(),
                        ));
                    }
                    Pred::Cmp(row(a), cmp, row(b))
                }
                (Side::Field(column), Side::Value(value)) => {
                    field_comparison(row(column), &column.kind, &value, cmp)?
                }
                (Side::Value(value), Side::Field(column)) => {
                    field_comparison(row(column), &column.kind, &value, cmp)?
                }
                (Side::Value(a), Side::Value(b)) => constants_compare(&a, &b, cmp)?,
            })
        }
    }
}

enum Side<'m> {
    Field(&'m Column),
    Value(Value),
}

struct Value {
    json: Json,
    date: bool,
}

fn side<'m>(
    operand: &Op,
    entity: &'m Entity,
    caller: &Caller,
    dates: &mut dyn Iterator<Item = bool>,
) -> Result<Side<'m>, PolicyError> {
    Ok(match operand {
        Op::Field(name) => Side::Field(
            entity
                .field(name)
                .ok_or_else(|| PolicyError::UnknownField(name.clone()))?,
        ),
        Op::Claim(name) => Side::Value(Value {
            json: caller
                .claim(name)
                .cloned()
                .ok_or_else(|| PolicyError::MissingClaim(name.clone()))?,
            date: false,
        }),
        Op::Literal(literal) => Side::Value(Value {
            json: match literal {
                Literal::Null => Json::Null,
                Literal::Bool(value) => Json::Bool(*value),
                Literal::Number(value) => Json::Number(value.clone()),
                Literal::String(value) => Json::String(value.clone()),
            },
            date: matches!(literal, Literal::String(_)) && dates.next().unwrap_or(false),
        }),
    })
}

fn field_comparison(
    field: Operand,
    kind: &FieldKind,
    value: &Value,
    op: CmpOp,
) -> Result<Pred, PolicyError> {
    if value.json.is_null() {
        return Ok(Pred::IsNull {
            operand: field,
            negated: op == CmpOp::Ne,
        });
    }
    Ok(Pred::Cmp(field, op, Operand::Value(stored(kind, value)?)))
}

fn compatible_fields(a: &FieldKind, b: &FieldKind) -> bool {
    let text = |kind: &FieldKind| {
        matches!(
            kind,
            FieldKind::Text { .. } | FieldKind::Email { .. } | FieldKind::Set { .. }
        )
    };
    std::mem::discriminant(a) == std::mem::discriminant(b)
        || (text(a) && text(b))
        || matches!(
            (a, b),
            (FieldKind::Int { .. }, FieldKind::Decimal { .. })
                | (FieldKind::Decimal { .. }, FieldKind::Int { .. })
        )
}

/// DAB's ClaimsTypeDataUriResolver promotes Guid, Int32, Boolean and String properties.
/// Date/decimal strings are deliberately not passed through the general storage codec.
fn stored(kind: &FieldKind, value: &Value) -> Result<SqlValue, PolicyError> {
    let invalid = || {
        PolicyError::InvalidComparison(format!(
            "use a value compatible with {}",
            kind.graphql_scalar()
        ))
    };
    let json = &value.json;
    let converted = match (kind, json) {
        (FieldKind::Date, _) if !value.date => None,
        (_, _) if value.date && !matches!(kind, FieldKind::Date) => None,
        (FieldKind::Uuid | FieldKind::Int { .. }, Json::String(text)) => {
            codec::parse_text(kind, text).ok()
        }
        (FieldKind::Boolean, Json::String(text)) => match text.trim().to_ascii_lowercase().as_str()
        {
            "true" => Some(SqlValue::Integer(1)),
            "false" => Some(SqlValue::Integer(0)),
            _ => None,
        },
        (FieldKind::Text { .. } | FieldKind::Email { .. } | FieldKind::Set { .. }, _) => match json
        {
            Json::String(text) => Some(SqlValue::Text(text.clone())),
            Json::Bool(flag) => Some(SqlValue::Text(if *flag { "True" } else { "False" }.into())),
            Json::Number(number) => Some(SqlValue::Text(number.to_string())),
            _ => None,
        },
        _ => codec::encode_operand(kind, json).ok(),
    };
    converted.ok_or_else(invalid)
}

fn constants_compare(a: &Value, b: &Value, op: CmpOp) -> Result<Pred, PolicyError> {
    let raw = |value: &Value| match &value.json {
        Json::Null => Some(SqlValue::Null),
        Json::String(text) => Some(SqlValue::Text(text.clone())),
        Json::Bool(flag) => Some(SqlValue::Integer(i64::from(*flag))),
        Json::Number(number) => number
            .as_i64()
            .map(SqlValue::Integer)
            .or_else(|| number.as_f64().map(SqlValue::Real)),
        _ => None,
    };
    let invalid = || PolicyError::InvalidComparison("compare constants of the same type".into());
    let left = raw(a).ok_or_else(invalid)?;
    let right = raw(b).ok_or_else(invalid)?;
    if a.json.is_null() || b.json.is_null() {
        return Ok(Pred::IsNull {
            operand: Operand::Value(if a.json.is_null() { right } else { left }),
            negated: op == CmpOp::Ne,
        });
    }
    if a.date != b.date || std::mem::discriminant(&a.json) != std::mem::discriminant(&b.json) {
        return Err(invalid());
    }
    // Let the database compare constants using its own collation rather than lowercasing.
    Ok(Pred::Cmp(Operand::Value(left), op, Operand::Value(right)))
}

fn string_literal_types(text: &str) -> Vec<bool> {
    let mut types = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\'' {
            while let Some(ch) = chars.next() {
                if ch == '\'' {
                    if chars.peek() == Some(&'\'') {
                        chars.next();
                    } else {
                        break;
                    }
                }
            }
            types.push(false);
        } else if !ch.is_whitespace() && ch != '(' && ch != ')' {
            let mut token = String::from(ch);
            while chars
                .peek()
                .is_some_and(|ch| !ch.is_whitespace() && *ch != '(' && *ch != ')')
            {
                token.push(chars.next().unwrap());
            }
            if token.contains('T')
                && codec::encode_operand(&FieldKind::Date, &Json::String(token)).is_ok()
            {
                types.push(true);
            }
        }
    }
    types
}

#[cfg(test)]
mod tests {
    use rl_model::{AppModel, Session};
    use serde_json::json;

    use super::*;
    use crate::sql::Sql;

    fn basic() -> AppModel {
        let json = include_str!("../../rl-model/tests/fixtures/basic.analysis.json");
        AppModel::from_analysis(&serde_json::from_str(json).unwrap()).unwrap()
    }

    fn caller(claims: Json) -> Caller {
        let Json::Object(claims) = claims else {
            panic!()
        };
        Caller::authenticated(Session {
            user_id: "u".into(),
            email: None,
            claims,
            access_token: "t".into(),
        })
    }

    fn render(pred: &Pred) -> Sql {
        let mut sql = Sql::default();
        pred.render(&mut sql);
        sql
    }

    #[test]
    fn read_policies_bind_typed_claims_and_literals() {
        let model = basic();
        let todo = model.entity("Todo").unwrap();
        let pred = read_filter(todo, &caller(json!({"sub": "abc"})), "t0")
            .unwrap()
            .unwrap();
        let sql = render(&pred);
        assert_eq!(sql.text, "((t0.\"user_id\" = ?) OR (t0.\"isPublic\" = ?))");
        assert_eq!(
            sql.params,
            [SqlValue::Text("abc".into()), SqlValue::Integer(1)]
        );

        // Anonymous callers act under the anonymous grant; this one reads no claims.
        let pred = read_filter(todo, &Caller::anonymous(), "t1")
            .unwrap()
            .unwrap();
        assert_eq!(render(&pred).text, "(t1.\"isPublic\" = ?)");

        // The text retains the Date literal's type, lost in the JSON check AST.
        let project = model.entity("Project").unwrap();
        let pred = read_filter(project, &Caller::anonymous(), "t0")
            .unwrap()
            .unwrap();
        let sql = render(&pred);
        assert_eq!(
            sql.text,
            "((t0.\"startsOn\" != ?) AND (t0.\"budget\" != ?))"
        );
        assert_eq!(
            sql.params[0],
            SqlValue::Text("2000-01-01T00:00:00.0000000Z".into())
        );
        let pred = read_filter(project, &caller(json!({"sub": "u1"})), "t0").unwrap();
        assert_eq!(
            render(&pred.unwrap()).text,
            "((t0.\"owner_id\" = ?) OR (t0.\"closedAt\" IS NULL))"
        );
    }

    #[test]
    fn missing_claims_and_constant_comparisons() {
        let model = basic();
        let comment = model.entity("Comment").unwrap();
        let grant = access::grant(comment, "authenticated", Action::Create).unwrap();
        let policy = grant.policy.as_ref().unwrap();
        let row = |c: &Column| Operand::column("t0", &c.name);
        let error = compile_policy(policy, comment, &caller(json!({"sub": "u1"})), &row);
        assert_eq!(error, Err(PolicyError::MissingClaim("email".into())));

        // Constants stay typed and are compared by the database, without case folding.
        let claims = json!({"sub": "u1", "email": "a@b.c", "role": "Authenticated"});
        let pred = compile_policy(policy, comment, &caller(claims), &row).unwrap();
        let sql = render(&pred);
        assert!(sql.text.ends_with("OR (? = ?)))"), "{}", sql.text);
        assert_eq!(
            &sql.params[2..],
            [
                SqlValue::Text("Authenticated".into()),
                SqlValue::Text("authenticated".into()),
            ]
        );

        let anonymous = Caller::anonymous();
        assert_eq!(
            compile_policy(policy, comment, &anonymous, &row),
            Err(PolicyError::MissingClaim("email".into()))
        );
    }

    #[test]
    fn unconvertible_claims_are_bad_requests() {
        let model = basic();
        let person = model.entity("Person").unwrap();
        let error = read_filter(person, &caller(json!({"sub": "not-a-uuid"})), "t0").unwrap_err();
        assert_eq!(GraphQlError::from(error).extensions["code"], "BadRequest");
        let id = "0D7F4C1E-9A43-4B8F-B2A6-3F0C5E8D1A22";
        let pred = read_filter(person, &caller(json!({"sub": id})), "t0").unwrap();
        let sql = render(&pred.unwrap());
        assert_eq!(sql.params, [SqlValue::Text(id.to_lowercase())]);
    }

    fn policy(check: Json) -> Policy {
        let expr = PolicyExpr::from_check(&check).unwrap();
        Policy {
            text: expr.to_string(),
            expr,
        }
    }

    fn evaluate(pred: &Pred, row: &str) -> Option<i64> {
        let sql = render(pred);
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.query_row(
            &format!("SELECT {} FROM ({row}) AS t0", sql.text),
            rusqlite::params_from_iter(sql.params.iter()),
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn null_tests_and_ordinary_inequality_follow_sql_logic() {
        let model = basic();
        let todo = model.entity("Todo").unwrap();
        for (op, value, expected) in [
            ("eq", Json::Null, [Some(1), Some(0), Some(0)]),
            ("neq", Json::Null, [Some(0), Some(1), Some(1)]),
            ("neq", json!(3), [None, Some(0), Some(1)]),
        ] {
            let policy = policy(json!({"op": op, "lhs": {"field": "estimate"}, "rhs": value}));
            let pred = compile_policy(&policy, todo, &Caller::anonymous(), &|c| {
                Operand::column("t0", &c.name)
            })
            .unwrap();
            for (row, expected) in [
                "SELECT NULL AS estimate",
                "SELECT 3 AS estimate",
                "SELECT 4 AS estimate",
            ]
            .into_iter()
            .zip(expected)
            {
                assert_eq!(evaluate(&pred, row), expected, "{op}: {row}");
            }
        }
    }

    #[test]
    fn missing_claim_in_a_true_or_branch_still_forbids_the_request() {
        let model = basic();
        let policy = policy(json!({"op": "or", "args": [
            {"op": "eq", "lhs": {"field": "isPublic"}, "rhs": true},
            {"op": "eq", "lhs": {"claim": "absent"}, "rhs": "x"}
        ]}));
        let error = compile_policy(
            &policy,
            model.entity("Todo").unwrap(),
            &Caller::anonymous(),
            &|c| Operand::column("t0", &c.name),
        )
        .unwrap_err();
        assert_eq!(error, PolicyError::MissingClaim("absent".into()));
        assert_eq!(GraphQlError::from(error).extensions["code"], "Forbidden");
    }

    #[test]
    fn string_promotion_is_limited_to_dab_supported_property_types() {
        let model = basic();
        let todo = model.entity("Todo").unwrap();
        for (field, claim, expected) in [
            (
                "id",
                json!("0D7F4C1E-9A43-4B8F-B2A6-3F0C5E8D1A22"),
                Some(SqlValue::Text(
                    "0d7f4c1e-9a43-4b8f-b2a6-3f0c5e8d1a22".into(),
                )),
            ),
            ("estimate", json!(" 42 "), Some(SqlValue::Integer(42))),
            ("isPublic", json!("TrUe"), Some(SqlValue::Integer(1))),
            ("isPublic", json!("false"), Some(SqlValue::Integer(0))),
            ("id", json!("not-a-guid"), None),
            ("estimate", json!("2147483648"), None),
            ("estimate", json!("4.2"), None),
            ("isPublic", json!("1"), None),
            ("isPublic", json!(1), None),
            ("cost", json!("10.5"), None),
            ("createdAt", json!("2026-01-01T00:00:00Z"), None),
        ] {
            let policy = policy(json!({"op": "eq", "lhs": {"field": field},
                "rhs": {"claim": "value"}}));
            let result = compile_policy(&policy, todo, &caller(json!({"value": claim})), &|c| {
                Operand::column("t0", &c.name)
            });
            match expected {
                Some(expected) => assert_eq!(render(&result.unwrap()).params, [expected]),
                None => assert!(
                    matches!(result, Err(PolicyError::InvalidComparison(_))),
                    "{field}: {result:?}"
                ),
            }
        }
    }

    #[test]
    fn date_literals_and_iso_strings_are_distinct_even_with_identical_text() {
        let model = basic();
        let todo = model.entity("Todo").unwrap();
        let mut policy = policy(json!({"op": "eq", "lhs": {"field": "createdAt"},
            "rhs": "2026-01-01T00:00:00.000Z"}));
        let row = |c: &Column| Operand::column("t0", &c.name);
        assert!(compile_policy(&policy, todo, &Caller::anonymous(), &row).is_err());
        policy.text = "@item.createdAt eq 2026-01-01T00:00:00.000Z".into();
        let pred = compile_policy(&policy, todo, &Caller::anonymous(), &row).unwrap();
        assert_eq!(
            render(&pred).params,
            [SqlValue::Text("2026-01-01T00:00:00.0000000Z".into())]
        );
        assert_eq!(
            string_literal_types(
                "(@item.title eq 'it''s 2026-01-01T00:00:00.000Z') or \
             (@item.createdAt eq 2026-01-01T00:00:00.000Z)"
            ),
            [false, true]
        );
    }

    #[test]
    fn text_uses_column_collation_and_constants_are_not_stringified() {
        let model = basic();
        let todo = model.entity("Todo").unwrap();
        let policy = policy(json!({"op": "eq", "lhs": {"field": "title"}, "rhs": "alice"}));
        let pred = compile_policy(&policy, todo, &Caller::anonymous(), &|c| {
            Operand::column("t0", &c.name)
        })
        .unwrap();
        assert_eq!(
            evaluate(&pred, "SELECT 'Alice' COLLATE BINARY AS title"),
            Some(0)
        );
        assert_eq!(
            evaluate(&pred, "SELECT 'Alice' COLLATE NOCASE AS title"),
            Some(1)
        );
        let constant = |json| Value { json, date: false };
        assert!(constants_compare(&constant(json!("1")), &constant(json!(1)), CmpOp::Eq).is_err());
        let pred = constants_compare(
            &constant(json!("Alice")),
            &constant(json!("alice")),
            CmpOp::Eq,
        )
        .unwrap();
        assert_eq!(evaluate(&pred, "SELECT 1"), Some(0));
    }
}
