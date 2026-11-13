//! Storage admission is independent of the more permissive data-policy language.

use rl_model::{AppModel, ModelError, Operand, PolicyExpr};
use serde_json::{Value, json};

fn analysis(check: Value) -> Value {
    let mut analysis: Value =
        serde_json::from_str(include_str!("fixtures/basic.analysis.json")).unwrap();
    analysis["storage"]["config"]["folders"][0]["rules"][0]["check"] = check;
    analysis
}

fn comparison(lhs: Value, rhs: Value) -> Value {
    json!({"op": "eq", "lhs": lhs, "rhs": rhs})
}

#[test]
fn storage_admission_decision_table() {
    for (check, expected) in [
        (
            comparison(json!({"claim": "sub"}), json!({"field": "owner_id"})),
            Ok(()),
        ),
        (
            comparison(json!({"claim": "email"}), json!({"field": "team_id"})),
            Ok(()),
        ),
        (
            comparison(json!({"claim": "role"}), json!("editor")),
            Ok(()),
        ),
        (comparison(json!({"field": "pages"}), json!(2)), Ok(())),
        (
            comparison(json!("literal on left"), json!({"field": "team_id"})),
            Ok(()),
        ),
        (comparison(json!(true), json!(false)), Ok(())),
        (
            comparison(json!({"claim": "custom"}), json!("x")),
            Err("claim `custom`"),
        ),
        (
            comparison(json!({"claim": "Email"}), json!("x")),
            Err("claim `Email`"),
        ),
        (
            comparison(json!({"field": "team_id"}), Value::Null),
            Err("not null"),
        ),
        (
            comparison(Value::Null, json!({"field": "team_id"})),
            Err("not null"),
        ),
        (
            comparison(json!({"field": "pages"}), json!("2")),
            Err("type mismatch"),
        ),
        (
            comparison(json!({"claim": "sub"}), json!({"field": "pages"})),
            Err("type mismatch"),
        ),
        (
            comparison(json!({"field": "team_id"}), json!(false)),
            Err("type mismatch"),
        ),
        (
            json!({"op": "gt", "lhs": {"field": "pages"}, "rhs": 2}),
            Err("operator `gt`"),
        ),
        (json!({"op": "not", "args": []}), Err("operator `not`")),
        (json!({"op": "and", "args": []}), Err("at least two")),
    ] {
        let result = AppModel::from_analysis(&analysis(check.clone()));
        match expected {
            Ok(()) => assert!(result.is_ok(), "{check}: {result:?}"),
            Err(reason) => {
                let error = result.unwrap_err();
                assert!(matches!(error, ModelError::Storage { .. }));
                let error = error.to_string();
                for expected in [reason, "todo-attachments", "rule[0]", "BaaS", "rayfin up"] {
                    assert!(error.contains(expected), "{expected}: {error}");
                }
            }
        }
    }
}

#[test]
fn only_owner_id_is_intrinsic_and_metadata_names_are_case_sensitive() {
    for name in [
        "id",
        "folder",
        "path",
        "name",
        "size",
        "content_type",
        "content_disposition",
        "cache_control",
        "etag",
        "created_at",
        "updated_at",
        "last_accessed_at",
        "user_metadata",
        "Owner_id",
        "TEAM_ID",
    ] {
        let check = comparison(json!({"field": name}), json!("x"));
        let error = AppModel::from_analysis(&analysis(check))
            .unwrap_err()
            .to_string();
        assert!(error.contains(&format!("field `{name}`")), "{error}");
    }
}

#[test]
fn validates_all_branches_and_rules_before_serving() {
    let invalid = comparison(json!({"claim": "custom"}), json!("x"));
    let valid = comparison(json!({"claim": "sub"}), json!({"claim": "sub"}));
    for op in ["and", "or"] {
        for args in [
            vec![valid.clone(), invalid.clone()],
            vec![invalid.clone(), valid.clone()],
        ] {
            let check = json!({"op": op, "args": args});
            assert!(AppModel::from_analysis(&analysis(check)).is_err());
        }
    }
    let mut analysis = analysis(invalid);
    let rules = analysis["storage"]["config"]["folders"][0]["rules"]
        .as_array_mut()
        .unwrap();
    rules.insert(
        0,
        json!({"role": "authenticated", "pathPattern": "**", "actions": ["*"]}),
    );
    let error = AppModel::from_analysis(&analysis).unwrap_err().to_string();
    assert!(
        error.contains("rule[1]") && error.contains("custom"),
        "{error}"
    );
}

#[test]
fn accepts_both_logical_shapes_and_all_profile_operators() {
    for check in [
        json!({"op": "and", "lhs": {"op": "eq", "lhs": {"field": "pages"}, "rhs": 2},
            "rhs": {"op": "neq", "lhs": {"claim": "email"}, "rhs": "hidden@example.test"}}),
        json!({"op": "or", "args": [
            {"op": "eq", "lhs": {"claim": "sub"}, "rhs": {"field": "owner_id"}},
            {"op": "neq", "lhs": {"field": "team_id"}, "rhs": "private"}
        ]}),
    ] {
        AppModel::from_analysis(&analysis(check)).unwrap();
    }
}

#[test]
fn metadata_operand_types_are_checked_without_coercion() {
    for (kind, literal, accepted) in [
        ("boolean", json!(true), true),
        ("boolean", json!("true"), false),
        ("number", json!(2.5), true),
        ("number", json!("2.5"), false),
        ("string", json!("2.5"), true),
        ("string", json!(2.5), false),
        ("Date", json!("2026-11-09T00:00:00Z"), true),
        ("Date", json!(2.5), false),
    ] {
        let mut analysis = analysis(comparison(json!({"field": "pages"}), literal));
        analysis["storage"]["config"]["folders"][0]["fields"][1]["type"] = json!(kind);
        assert_eq!(
            AppModel::from_analysis(&analysis).is_ok(),
            accepted,
            "{kind}"
        );
    }
}

#[test]
fn rejects_anonymous_grants_even_when_their_check_is_absent() {
    for role in ["anonymous", "ANONYMOUS", "Anonymous"] {
        let mut analysis = analysis(Value::Null);
        analysis["storage"]["config"]["folders"][0]["rules"][0]["role"] = json!(role);
        let error = AppModel::from_analysis(&analysis).unwrap_err().to_string();
        assert!(
            error.contains("anonymous storage") && error.contains("rule[0]"),
            "{error}"
        );
    }
}

#[test]
fn typed_and_deserialized_models_cannot_skip_admission() {
    let model = AppModel::from_analysis(&analysis(Value::Null)).unwrap();
    let mut json = serde_json::to_value(model).unwrap();
    json["storage"]["folders"][0]["rules"][0]["check"] = json!({
        "compare": {"op": "eq", "left": {"claim": "custom"},
            "right": {"literal": "x"}}
    });
    let model: AppModel = serde_json::from_value(json).unwrap();
    assert!(model.validate_storage_rules().is_err());

    let mut model = AppModel::from_analysis(&analysis(Value::Null)).unwrap();
    model.storage.as_mut().unwrap().folders[0].rules[0].check = Some(PolicyExpr::Compare {
        op: rl_model::CompareOp::Eq,
        left: Operand::Field("team_id".into()),
        right: Operand::Literal(rl_model::Literal::Null),
    });
    assert!(model.validate_storage_rules().is_err());
}
