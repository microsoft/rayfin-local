//! A model without entities (a new app whose `rayfin/data` is still empty): the data API answers,
//! with nothing to query.

mod common;

use common::App;
use rl_model::AppModel;
use serde_json::json;

fn empty_model() -> AppModel {
    AppModel::from_analysis(&json!({
        "coreVersion": "1.36.2",
        "typescriptVersion": "5.9.3",
        "dialect": "mssql",
        "featureFlags": null,
        "entities": [],
        "config": { "entities": {} },
        "policies": [],
        "storage": null,
    }))
    .unwrap()
}

#[tokio::test]
async fn a_model_without_entities_answers_typename() {
    let app = App::new(empty_model()).await;
    let anonymous = app.caller(None);
    let response = app.run(&anonymous, "{ __typename }").await;
    assert_eq!(response.status, 200, "{}", response.body);
    assert_eq!(response.data(), &json!({ "__typename": "Query" }));

    // Without entities there is no Mutation type: a mutation is a clean GraphQL error.
    let (_, token) = app.user("ada@example.com").await;
    let signed_in = app.caller(Some(&token));
    let response = app.run(&signed_in, "mutation { __typename }").await;
    assert!(
        response.messages().iter().any(|m| m.contains("Mutation")),
        "{}",
        response.body
    );
}

#[tokio::test]
async fn a_model_without_entities_refuses_entity_fields() {
    let app = App::new(empty_model()).await;
    let anonymous = app.caller(None);
    let response = app.run(&anonymous, "{ todos { items { id } } }").await;
    assert!(!response.errors().is_empty(), "{}", response.body);
    assert!(
        response.messages().iter().any(|m| m.contains("todos")),
        "{}",
        response.body
    );
}

#[tokio::test]
async fn removing_every_entity_drops_their_tables() {
    let app = App::new(common::basic_model()).await;
    let report = rl_db::apply(&app.db, &empty_model(), false).await.unwrap();
    let dropped = report
        .steps
        .iter()
        .filter(|step| matches!(step, rl_db::migrate::MigrationStep::DropTable(_)))
        .count();
    assert_eq!(dropped, common::basic_model().entities.len(), "{report:?}");
    app.api.set_model(std::sync::Arc::new(empty_model()));
    let response = app.run(&app.caller(None), "{ __typename }").await;
    assert_eq!(response.data(), &json!({ "__typename": "Query" }));
}

#[tokio::test]
async fn removing_every_entity_is_refused_while_they_hold_rows() {
    let app = App::new(common::basic_model()).await;
    app.seed(
        "INSERT INTO \"Categories\" (id, name, display_label, sortOrder, parent_id) VALUES \
         ('00000000-0000-4000-8000-000000000001', 'Kept', 'Kept', 1, NULL)",
    );
    let err = rl_db::apply(&app.db, &empty_model(), false)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Categories"), "{err}");
    rl_db::apply(&app.db, &empty_model(), true).await.unwrap();
}
