//! Folder rules: a rule grants its `actions` to its `role` on objects matching its
//! `pathPattern` when its `check` (if any) holds for the caller and the object.
//!
//! Only authenticated grants with `pathPattern: "**"` apply. Checks are admitted against
//! BaaS's storage whitelist before serving. Equality requires two non-null values; inequality
//! also matches exactly one null. Missing allowed claims bind null, not an authorization error.
//! Strings compare exactly: BaaS emits SQL comparisons, never blanket lowercasing.

use rl_model::{
    Action, Caller, CompareOp, Literal, Operand, PolicyExpr, StorageFolder, StorageRule,
};
use serde_json::{Map, Value};

/// Whether some rule could grant `action` to `caller` (object checks not considered).
pub(crate) fn may(folder: &StorageFolder, caller: &Caller, action: Action) -> bool {
    folder.rules.iter().any(|rule| grants(rule, caller, action))
}

/// Whether a rule grants `action` on `item` (the object's fields, as [`crate::store`] builds
/// them).
pub(crate) fn allows(
    folder: &StorageFolder,
    caller: &Caller,
    action: Action,
    item: &Map<String, Value>,
) -> bool {
    folder.rules.iter().any(|rule| {
        grants(rule, caller, action)
            && rule
                .check
                .as_ref()
                .is_none_or(|check| eval(check, caller, item))
    })
}

fn grants(rule: &StorageRule, caller: &Caller, action: Action) -> bool {
    let role_applies = rule.role.eq_ignore_ascii_case("authenticated")
        && caller
            .session
            .as_ref()
            .is_some_and(|session| !session.user_id.trim().is_empty())
        && !claim(caller, "role")
            .and_then(|value| value.as_str().map(str::to_owned))
            .is_some_and(|role| role.eq_ignore_ascii_case("anonymous"));
    role_applies
        && rule.path_pattern == "**"
        && rule
            .actions
            .iter()
            .any(|granted| *granted == action || *granted == Action::All)
}

fn eval(expr: &PolicyExpr, caller: &Caller, item: &Map<String, Value>) -> bool {
    match expr {
        PolicyExpr::And(operands) => operands
            .iter()
            .fold(true, |allowed, e| eval(e, caller, item) & allowed),
        PolicyExpr::Or(operands) => operands
            .iter()
            .fold(false, |allowed, e| eval(e, caller, item) | allowed),
        PolicyExpr::Compare { op, left, right } => {
            match (resolve(left, caller, item), resolve(right, caller, item)) {
                (Some(left), Some(right)) => {
                    let equal = values_equal(&left, &right);
                    match op {
                        CompareOp::Eq => equal,
                        CompareOp::Ne => !equal,
                    }
                }
                (None, None) => false,
                _ => *op == CompareOp::Ne,
            }
        }
    }
}

/// An operand's value; `None` for a missing or null one.
fn resolve(operand: &Operand, caller: &Caller, item: &Map<String, Value>) -> Option<Value> {
    let value = match operand {
        Operand::Claim(name) => claim(caller, name),
        Operand::Field(name) => item.get(name).cloned(),
        Operand::Literal(Literal::Null) => None,
        Operand::Literal(Literal::Bool(b)) => Some(Value::Bool(*b)),
        Operand::Literal(Literal::Number(n)) => Some(Value::Number(n.clone())),
        Operand::Literal(Literal::String(s)) => Some(Value::String(s.clone())),
    };
    value.filter(|value| !value.is_null())
}

fn claim(caller: &Caller, name: &str) -> Option<Value> {
    let session = caller.session.as_ref()?;
    match name {
        "sub" => Some(Value::String(session.user_id.clone())),
        "email" => session
            .email
            .clone()
            .map(Value::String)
            .or_else(|| caller.claim(name).and_then(Value::as_str).map(|s| s.into())),
        "role" => caller.claim(name).and_then(Value::as_str).map(|s| s.into()),
        _ => None,
    }
}

fn values_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use rl_model::{OnConflict, Session};
    use serde_json::json;

    use super::*;

    fn user(sub: &str) -> Caller {
        Caller::authenticated(Session {
            user_id: sub.into(),
            email: None,
            claims: Map::new(),
            access_token: "t".into(),
        })
    }

    fn folder(rules: Vec<StorageRule>) -> StorageFolder {
        StorageFolder {
            name: "docs".into(),
            display_name: "Docs".into(),
            on_conflict: OnConflict::Error,
            max_size: None,
            allowed_content_types: vec![],
            fields: vec![],
            rules,
        }
    }

    fn rule(role: &str, actions: &[Action], check: Option<PolicyExpr>) -> StorageRule {
        StorageRule {
            path_pattern: "**".into(),
            role: role.into(),
            actions: actions.to_vec(),
            check,
        }
    }

    fn compare(op: CompareOp, left: Operand, right: Operand) -> PolicyExpr {
        PolicyExpr::Compare { op, left, right }
    }

    fn item(value: Value) -> Map<String, Value> {
        let Value::Object(mut map) = value else {
            unreachable!()
        };
        map.entry("path").or_insert("a.txt".into());
        map
    }

    #[test]
    fn owner_checks_compare_the_sub_claim_exactly() {
        let owner_only = compare(
            CompareOp::Eq,
            Operand::Claim("sub".into()),
            Operand::Field("owner_id".into()),
        );
        let folder = folder(vec![rule(
            "authenticated",
            &[Action::Read],
            Some(owner_only),
        )]);
        let mine = item(json!({ "owner_id": "user-1" }));
        assert!(allows(&folder, &user("user-1"), Action::Read, &mine));
        assert!(!allows(&folder, &user("User-1"), Action::Read, &mine));
        assert!(!allows(&folder, &user("user-2"), Action::Read, &mine));
        assert!(!allows(&folder, &user("user-1"), Action::Delete, &mine));
        assert!(!allows(&folder, &Caller::anonymous(), Action::Read, &mine));
        assert!(!allows(
            &folder,
            &user("user-1"),
            Action::Read,
            &item(json!({}))
        ));
    }

    #[test]
    fn comparison_decision_table() {
        for (fields, eq, neq) in [
            (json!({"a": "x", "b": "x"}), true, false),
            (json!({"a": "x", "b": "y"}), false, true),
            (json!({"a": "x", "b": "X"}), false, true),
            (json!({"a": null, "b": "x"}), false, true),
            (json!({"a": "x", "b": null}), false, true),
            (json!({"b": "x"}), false, true),
            (json!({"a": null, "b": null}), false, false),
            (json!({}), false, false),
            (json!({"a": 1, "b": 1.0}), true, false),
            (json!({"a": false, "b": false}), true, false),
        ] {
            let fields = item(fields);
            for (op, expected) in [(CompareOp::Eq, eq), (CompareOp::Ne, neq)] {
                let check = compare(op, Operand::Field("a".into()), Operand::Field("b".into()));
                assert_eq!(
                    eval(&check, &user("u"), &fields),
                    expected,
                    "{fields:?} {op:?}"
                );
            }
        }
        for (left, right) in [
            (json!(1), json!("1")),
            (json!(true), json!("true")),
            (json!(["x"]), json!("x")),
        ] {
            assert!(!values_equal(&left, &right));
        }
    }

    #[test]
    fn authenticated_rules_and_star_grants() {
        let folder = folder(vec![rule("authenticated", &[Action::All], None)]);
        let any = item(json!({}));
        assert!(!allows(&folder, &Caller::anonymous(), Action::Read, &any));
        assert!(!allows(&folder, &Caller::anonymous(), Action::Create, &any));
        assert!(allows(&folder, &user("u"), Action::Read, &any));
        assert!(allows(&folder, &user("u"), Action::Delete, &any));
        assert!(may(&folder, &user("u"), Action::Update));
        assert!(!may(&folder, &Caller::anonymous(), Action::Update));
    }

    #[test]
    fn only_baas_rule_roles_paths_and_actions_apply() {
        for (role, path, granted, expected) in [
            ("authenticated", "**", Action::Read, true),
            ("AUTHENTICATED", "**", Action::All, true),
            ("authenticated", "**", Action::Update, false),
            ("authenticated", "*.txt", Action::Read, false),
            ("authenticated", "", Action::Read, false),
            ("editor", "**", Action::Read, false),
        ] {
            let mut rule = rule(role, &[granted], None);
            rule.path_pattern = path.into();
            let folder = folder(vec![rule]);
            assert_eq!(may(&folder, &user("u"), Action::Read), expected);
            assert_eq!(
                allows(&folder, &user("u"), Action::Read, &item(json!({}))),
                expected
            );
        }
    }

    #[test]
    fn logical_checks_allow_missing_email_without_a_missing_claim_error() {
        let missing_email = compare(
            CompareOp::Eq,
            Operand::Claim("email".into()),
            Operand::Field("mail".into()),
        );
        let matches_sub = compare(
            CompareOp::Eq,
            Operand::Claim("sub".into()),
            Operand::Literal(Literal::String("u".into())),
        );
        for (args, expected_and, expected_or) in [
            (
                vec![matches_sub.clone(), missing_email.clone()],
                false,
                true,
            ),
            (vec![missing_email, matches_sub], false, true),
        ] {
            assert_eq!(
                eval(&PolicyExpr::And(args.clone()), &user("u"), &item(json!({}))),
                expected_and
            );
            assert_eq!(
                eval(&PolicyExpr::Or(args), &user("u"), &item(json!({}))),
                expected_or
            );
        }
    }

    #[test]
    fn missing_allowed_claims_bind_null() {
        let caller = user("u");
        for (fields, eq, neq) in [
            (json!({"mail": "u@example.test"}), false, true),
            (json!({"mail": null}), false, false),
            (json!({}), false, false),
        ] {
            for (op, expected) in [(CompareOp::Eq, eq), (CompareOp::Ne, neq)] {
                let check = compare(
                    op,
                    Operand::Claim("email".into()),
                    Operand::Field("mail".into()),
                );
                assert_eq!(eval(&check, &caller, &item(fields.clone())), expected);
            }
        }
    }

    #[test]
    fn matching_rules_form_a_permissive_union() {
        let check = compare(
            CompareOp::Eq,
            Operand::Claim("sub".into()),
            Operand::Literal(Literal::String("other".into())),
        );
        for (checks, expected) in [
            (vec![Some(check.clone())], false),
            (vec![Some(check.clone()), None], true),
            (vec![None, Some(check)], true),
        ] {
            let folder = folder(
                checks
                    .into_iter()
                    .map(|check| rule("authenticated", &[Action::Read], check))
                    .collect(),
            );
            assert_eq!(
                allows(&folder, &user("u"), Action::Read, &item(json!({}))),
                expected
            );
        }
    }

    #[test]
    fn allowed_claim_values_are_not_data_policy_roles() {
        let mut caller = user("u");
        caller.session.as_mut().unwrap().claims =
            json!({"role": "editor", "email": "u@example.test"})
                .as_object()
                .unwrap()
                .clone();
        assert_eq!(claim(&caller, "role"), Some(json!("editor")));
        assert_eq!(claim(&caller, "email"), Some(json!("u@example.test")));
        assert_eq!(claim(&caller, "custom"), None);
        let folder = folder(vec![rule("authenticated", &[Action::Read], None)]);
        assert!(allows(&folder, &caller, Action::Read, &item(json!({}))));
        caller
            .session
            .as_mut()
            .unwrap()
            .claims
            .insert("role".into(), json!("anonymous"));
        assert!(!allows(&folder, &caller, Action::Read, &item(json!({}))));
    }
}
