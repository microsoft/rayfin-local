mod common;

use common::{App, basic_model};
use rl_graphql::DataApiSettings;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn app() -> App {
    App::with_settings(
        basic_model(),
        DataApiSettings {
            allow_introspection: true,
            ..DataApiSettings::default()
        },
    )
    .await
}

fn member<'a>(members: &'a Value, name: &str) -> &'a Value {
    members
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["name"] == name)
        .unwrap_or_else(|| panic!("missing {name} in {members}"))
}

#[tokio::test]
async fn full_introspection_query_reflects_the_executable_schema() {
    let app = app().await;
    let response = app
        .api
        .clone()
        .router()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/graphql")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({"query": include_str!("introspection.graphql")}).to_string(),
                ))
                .unwrap(),
        )
        .await;
    let response = response.unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let response = common::Response {
        status: 200,
        body: serde_json::from_slice(&body).unwrap(),
    };
    let schema = &response.data()["__schema"];
    assert_eq!(schema["queryType"]["name"], "Query");
    assert_eq!(schema["mutationType"]["name"], "Mutation");
    assert_eq!(schema["subscriptionType"], Value::Null);
    let types = &schema["types"];
    let todo = member(types, "Todo");
    assert_eq!(todo["kind"], "OBJECT");
    assert_eq!(todo["interfaces"], json!([]));
    assert_eq!(todo["possibleTypes"], Value::Null);
    let title = member(&todo["fields"], "title");
    assert_eq!(title["type"]["kind"], "NON_NULL");
    assert_eq!(title["type"]["ofType"]["name"], "String");
    assert_eq!(title["isDeprecated"], false);
    assert_eq!(title["deprecationReason"], Value::Null);
    let items = member(&member(types, "TodoConnection")["fields"], "items");
    assert_eq!(items["type"]["kind"], "NON_NULL");
    assert_eq!(items["type"]["ofType"]["kind"], "LIST");
    assert_eq!(items["type"]["ofType"]["ofType"]["kind"], "NON_NULL");
    assert_eq!(items["type"]["ofType"]["ofType"]["ofType"]["name"], "Todo");
    let filter = member(types, "TodoFilterInput");
    assert_eq!(filter["kind"], "INPUT_OBJECT");
    assert_eq!(filter["isOneOf"], false);
    assert_eq!(
        member(&filter["inputFields"], "title")["type"]["name"],
        "StringFilterInput"
    );
    assert_eq!(member(types, "OrderBy")["enumValues"][0]["name"], "ASC");
    assert_eq!(member(types, "OrderBy")["enumValues"][1]["name"], "DESC");
    let query = member(types, "Query");
    let todos = member(&query["fields"], "todos");
    assert!(todos["description"].as_str().unwrap().contains("Todo"));
    assert_eq!(
        member(&todos["args"], "filter")["type"]["name"],
        "TodoFilterInput"
    );
    assert!(
        query["fields"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["name"] != "__schema")
    );
    assert_eq!(
        member(&member(types, "User")["fields"], "Id")["type"]["ofType"]["name"],
        "UUID"
    );
    assert_eq!(
        member(&member(types, "User")["fields"], "Email")["type"]["kind"],
        "NON_NULL"
    );
    assert_eq!(
        member(&member(types, "Query")["fields"], "user_by_pk")["args"][0]["name"],
        "Id"
    );
    let mutations = &member(types, "Mutation")["fields"];
    assert!(mutations.as_array().unwrap().iter().all(|f| {
        !matches!(
            f["name"].as_str(),
            Some("createUser" | "updateUser" | "deleteUser")
        )
    }));
    let skip = member(&schema["directives"], "skip");
    assert_eq!(
        skip["locations"],
        json!(["FIELD", "FRAGMENT_SPREAD", "INLINE_FRAGMENT"])
    );
    assert_eq!(skip["args"][0]["type"]["kind"], "NON_NULL");
    assert_eq!(skip["args"][0]["type"]["ofType"]["name"], "Boolean");
    assert_eq!(
        member(&schema["directives"], "deprecated")["args"][0]["defaultValue"],
        "\"No longer supported\""
    );
}

#[tokio::test]
async fn type_lookup_aliases_variables_fragments_and_directives() {
    let app = app().await;
    let response = app
        .run_with(
            &app.caller(None),
            "query($name: String!, $skip: Boolean!) {
               found: __type(name: $name) {
                 kind __typename ...T
                 fields @skip(if: $skip) { name }
               }
               absent: __type(name: \"Unknown\") { name }
               __typename
             }
             fragment T on __Type { n: name }",
            json!({"name": "User", "skip": true})
                .as_object()
                .unwrap()
                .clone(),
        )
        .await;
    assert_eq!(
        response.data(),
        &json!({
            "found": {"kind": "OBJECT", "__typename": "__Type", "n": "User"},
            "absent": null,
            "__typename": "Query"
        })
    );
    let response = app.run(&app.caller(None), "{ __type { name } }").await;
    assert_eq!(response.messages(), ["The argument `name` is required."]);
    let response = app
        .run(
            &app.caller(None),
            "{ __type(name: \"User\") { nonexistent } }",
        )
        .await;
    assert!(!response.errors().is_empty());
}

#[tokio::test]
async fn introspection_stays_disabled_by_default() {
    let app = App::new(basic_model()).await;
    for query in [
        "{ __schema { queryType { name } } }",
        "{ __type(name: \"User\") { name } }",
    ] {
        let response = app.run(&app.caller(None), query).await;
        assert_eq!(response.codes(), ["HC0046"]);
        assert_eq!(
            response.messages(),
            ["Introspection is not allowed for the current request."]
        );
        assert!(response.body.get("data").is_none());
    }
    assert_eq!(
        app.run(&app.caller(None), "{ __typename }").await.data(),
        &json!({"__typename": "Query"})
    );
}

#[tokio::test]
async fn introspection_updates_with_the_served_model() {
    let app = app().await;
    let mut model = basic_model();
    model.entities.retain(|e| e.name != "Todo");
    for entity in &mut model.entities {
        entity.relationships.retain(|r| r.target != "Todo");
    }

    app.api.set_model(std::sync::Arc::new(model));
    let response = app
        .run(
            &app.caller(None),
            "{ removed: __type(name: \"Todo\") { name } kept: __type(name: \"User\") { name } }",
        )
        .await;
    assert_eq!(
        response.data(),
        &json!({"removed": null, "kept": {"name": "User"}})
    );
    app.api.set_model(app.api.model());
    assert!(app.api.model().entity("User").is_none());
    let response = app
        .run(
            &app.caller(None),
            "{ __type(name: \"Query\") { fields { name } } }",
        )
        .await;
    let fields = response.data()["__type"]["fields"].as_array().unwrap();
    assert_eq!(
        fields
            .iter()
            .filter(|field| field["name"] == "users")
            .count(),
        1
    );
}

#[tokio::test]
async fn a_read_only_model_has_no_mutation_root() {
    let mut model = basic_model();
    model.entities.clear();
    let app = App::with_settings(
        model,
        DataApiSettings {
            allow_introspection: true,
            ..DataApiSettings::default()
        },
    )
    .await;
    let response = app
        .run(&app.caller(None), include_str!("introspection.graphql"))
        .await;
    assert_eq!(response.data()["__schema"]["mutationType"], Value::Null);
    assert_eq!(
        member(&response.data()["__schema"]["types"], "User")["kind"],
        "OBJECT"
    );
}
