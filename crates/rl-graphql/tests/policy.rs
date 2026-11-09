//! BaaS decision-table cases: supplied create values, pre-image writes and response visibility.

mod common;

use common::{App, basic_model};
use rl_model::{Action, ActionPermission, AppModel, Policy, PolicyExpr};
use serde_json::{Value, json};

const ID: &str = "00000000-0000-4000-8000-000000000001";

fn actions<'m>(model: &'m mut AppModel, entity: &str) -> &'m mut Vec<ActionPermission> {
    &mut model
        .entities
        .iter_mut()
        .find(|e| e.name == entity)
        .unwrap()
        .permissions
        .iter_mut()
        .find(|p| p.role == "authenticated")
        .unwrap()
        .actions
}

fn policy(check: Value) -> Policy {
    let expr = PolicyExpr::from_check(&check).unwrap();
    Policy {
        text: expr.to_string(),
        expr,
    }
}

fn count(app: &App, sql: &str) -> i64 {
    app.db
        .call_blocking(|conn| conn.query_row(sql, [], |row| row.get(0)))
        .unwrap()
}

#[tokio::test]
async fn create_checks_supplied_fields_never_defaults_generated_values_or_missing_variables() {
    for (field, rhs, supplied, expected_code) in [
        ("isPublic", json!(false), "", Some("Forbidden")),
        ("isPublic", json!(false), "isPublic: false,", None),
        (
            "isPublic",
            json!(false),
            "isPublic: true,",
            Some("BadRequest"),
        ),
        ("estimate", Value::Null, "", Some("Forbidden")),
        ("estimate", Value::Null, "estimate: null,", None),
        ("id", json!(ID), "", Some("Forbidden")),
        (
            "id",
            json!(ID),
            "id: \"00000000-0000-4000-8000-000000000001\",",
            None,
        ),
        (
            "isPublic",
            json!(false),
            "isPublic: $missing,",
            Some("Forbidden"),
        ),
    ] {
        let mut model = basic_model();
        let grants = actions(&mut model, "Todo");
        let create = grants
            .iter_mut()
            .find(|g| g.action == Action::Create)
            .unwrap();
        create.policy = Some(policy(
            json!({"op": "eq", "lhs": {"field": field}, "rhs": rhs}),
        ));
        let app = App::new(model).await;
        let (owner, token) = app.user("owner@example.test").await;
        let caller = app.caller(Some(&token));
        let variables = if supplied.contains('$') {
            "($missing: Boolean)"
        } else {
            ""
        };
        let response = app
            .run(
                &caller,
                &format!(
                    "mutation{variables} {{ createTodo(item: {{ {supplied} user_id: \"{owner}\", \
             title: \"candidate\", status: \"doing\", cost: 1, \
             createdAt: \"2026-01-01T00:00:00Z\" }}) {{ id }} }}"
                ),
            )
            .await;
        assert_eq!(response.status, 200);
        match expected_code {
            Some(code) => {
                assert_eq!(response.codes(), [code], "{field}: {}", response.body);
                assert_eq!(count(&app, "SELECT count(*) FROM Todos"), 0);
            }
            None => {
                assert!(response.data()["createTodo"]["id"].is_string());
                assert_eq!(count(&app, "SELECT count(*) FROM Todos"), 1);
            }
        }
    }
}

#[tokio::test]
async fn update_uses_preimage_and_commits_even_when_postimage_is_unreadable() {
    let mut model = basic_model();
    actions(&mut model, "Todo")
        .iter_mut()
        .find(|g| g.action == Action::Update)
        .unwrap()
        .fields = None;
    let app = App::new(model).await;
    let (owner, token) = app.user("owner@example.test").await;
    let caller = app.caller(Some(&token));
    app.seed(&format!(
        "INSERT INTO Todos (id, user_id, title, status, cost, createdAt) VALUES \
         ('{ID}', '{owner}', 'before', 'doing', 1, '2026-01-01T00:00:00.0000000Z')"
    ));
    let response = app.run(&caller, &format!(
        "mutation {{ updateTodo(id: \"{ID}\", item: {{ user_id: \"new-owner\" }}) {{ id }} }}"
    )).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.data(), &json!({"updateTodo": null}));
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM Todos WHERE user_id = 'new-owner'"
        ),
        1
    );

    let response = app
        .run(
            &caller,
            &format!(
                "mutation {{ updateTodo(id: \"{ID}\", item: {{ user_id: \"{owner}\" }}) {{ id }} }}"
            ),
        )
        .await;
    assert_eq!(response.codes(), ["BadRequest"]);
    assert!(response.messages()[0].starts_with("Could not find item with <"));

    let response = app
        .run(&caller, &format!("{{ todo_by_pk(id: \"{ID}\") {{ id }} }}"))
        .await;
    assert_eq!(response.data(), &json!({"todo_by_pk": null}));
    let response = app
        .run(
            &caller,
            &format!("mutation {{ deleteTodo(id: \"{ID}\") {{ id }} }}"),
        )
        .await;
    assert_eq!(response.data(), &json!({"deleteTodo": null}));
    assert_eq!(count(&app, "SELECT count(*) FROM Todos"), 1);
}

#[tokio::test]
async fn no_read_grant_reports_forbidden_after_update_and_delete_commit() {
    let mut model = basic_model();
    let category = model
        .entities
        .iter_mut()
        .find(|e| e.name == "Category")
        .unwrap();
    category
        .permissions
        .iter_mut()
        .find(|p| p.role == "anonymous")
        .unwrap()
        .actions = [Action::Update, Action::Delete]
        .into_iter()
        .map(|action| ActionPermission {
            action,
            fields: None,
            policy: None,
        })
        .collect();
    let app = App::new(model).await;
    let caller = app.caller(None);
    app.seed(&format!(
        "INSERT INTO Categories (id, name) VALUES ('{ID}', 'before')"
    ));
    let response = app
        .run(
            &caller,
            &format!(
                "mutation {{ updateCategory(id: \"{ID}\", item: {{ name: \"after\" }}) {{ id }} }}"
            ),
        )
        .await;
    assert_eq!(response.status, 200);
    assert_eq!(response.codes(), ["Forbidden"]);
    assert_eq!(
        count(&app, "SELECT count(*) FROM Categories WHERE name = 'after'"),
        1
    );
    let response = app
        .run(
            &caller,
            &format!("mutation {{ deleteCategory(id: \"{ID}\") {{ id }} }}"),
        )
        .await;
    assert_eq!(response.codes(), ["Forbidden"]);
    assert_eq!(count(&app, "SELECT count(*) FROM Categories"), 0);
}

#[tokio::test]
async fn a_missing_read_claim_in_a_true_or_branch_is_forbidden_not_a_hidden_row() {
    let mut model = basic_model();
    let read = actions(&mut model, "Todo")
        .iter_mut()
        .find(|g| g.action == Action::Read)
        .unwrap();
    read.policy = Some(policy(json!({"op": "or", "args": [
        {"op": "eq", "lhs": {"field": "isPublic"}, "rhs": true},
        {"op": "eq", "lhs": {"claim": "absent"}, "rhs": "x"}
    ]})));
    let app = App::new(model).await;
    let (_, token) = app.user("owner@example.test").await;
    let response = app
        .run(&app.caller(Some(&token)), "{ todos { items { id } } }")
        .await;
    assert_eq!(response.status, 200);
    assert_eq!(response.codes(), ["Forbidden"]);
}

#[tokio::test]
async fn create_policy_uses_the_same_text_collation_as_the_stored_column() {
    for (postgresql, expected) in [(false, None), (true, Some("BadRequest"))] {
        let mut model = basic_model();
        if postgresql {
            model.dialect = rl_model::Dialect::Postgresql;
            let todo = model
                .entities
                .iter_mut()
                .find(|e| e.name == "Todo")
                .unwrap();
            todo.columns
                .iter_mut()
                .find(|c| c.primary_key)
                .unwrap()
                .db_type = "UUID".into();
        }
        actions(&mut model, "Todo")
            .iter_mut()
            .find(|g| g.action == Action::Create)
            .unwrap()
            .policy = Some(policy(
            json!({"op": "eq", "lhs": {"field": "title"}, "rhs": "alice"}),
        ));
        let app = App::new(model).await;
        let (owner, token) = app.user("owner@example.test").await;
        let response = app
            .run(
                &app.caller(Some(&token)),
                &format!(
                    "mutation {{ createTodo(item: {{ user_id: \"{owner}\", title: \"ALICE\", \
             status: \"doing\", cost: 1, createdAt: \"2026-01-01T00:00:00Z\" }}) {{ title }} }}"
                ),
            )
            .await;
        if let Some(code) = expected {
            assert_eq!(response.codes(), [code]);
            assert_eq!(count(&app, "SELECT count(*) FROM Todos"), 0);
        } else {
            assert_eq!(response.data(), &json!({"createTodo": {"title": "ALICE"}}));
            assert_eq!(count(&app, "SELECT count(*) FROM Todos"), 1);
        }
    }
}

#[tokio::test]
async fn callers_cannot_select_an_authenticated_role_with_a_request_header() {
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let app = App::new(basic_model()).await;
    let request = Request::post("/graphql")
        .header("X-MS-API-ROLE", "authenticated")
        .body(Body::from(
            json!({"query": "{ milestones { items { id } } }"}).to_string(),
        ))
        .unwrap();
    let response = app.api.clone().router().oneshot(request).await.unwrap();
    assert_eq!(response.status(), 200);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        body["errors"][0]["extensions"]["code"],
        "AUTH_NOT_AUTHORIZED"
    );
}
