mod common;

use common::{App, basic_model};
use serde_json::json;

#[tokio::test]
async fn authenticated_users_can_read_every_user_and_navigate_real_foreign_keys() {
    let app = App::new(basic_model()).await;
    let (alice, token) = app.user("alice@example.test").await;
    let (bob, _) = app.user("bob@example.test").await;
    let caller = app.caller(Some(&token));
    assert_eq!(caller.claim("sub"), Some(&json!(alice)));
    let response = app
        .run(
            &caller,
            "{ users(orderBy: { Email: ASC }) { items { Id Email } } }",
        )
        .await;
    assert_eq!(
        response.items("users"),
        [
            json!({"Id": alice, "Email": "alice@example.test"}),
            json!({"Id": bob, "Email": "bob@example.test"}),
        ]
    );
    let response = app
        .run(
            &caller,
            &format!("{{ user_by_pk(Id: \"{bob}\") {{ Id Email }} }}"),
        )
        .await;
    assert_eq!(response.data()["user_by_pk"]["Id"], bob);
    let response = app
        .run(&caller, &format!("mutation {{ createPerson(item: {{ account_id: \"{alice}\" }}) {{ account {{ Id Email }} }} }}"))
        .await;
    assert_eq!(
        response.data()["createPerson"]["account"],
        json!({"Id": alice, "Email": "alice@example.test"})
    );
    let response = app
        .run(&caller, "{ people(filter: { account: { Email: { eq: \"alice@example.test\" } } }) { items { account { Id } } } }")
        .await;
    assert_eq!(
        response.items("people"),
        [json!({"account": {"Id": alice}})]
    );
    let missing = app.db.call_blocking(|conn| {
        conn.execute(
            "INSERT INTO People (account_id) VALUES ('00000000-0000-4000-8000-000000000001')",
            [],
        )
    });
    assert!(missing.is_err());
    assert!(app.api.model().entity("User").is_none());
}

#[tokio::test]
async fn user_access_is_read_only_and_never_anonymous() {
    let app = App::new(basic_model()).await;
    let (_, token) = app.user("alice@example.test").await;
    let response = app
        .run(&app.caller(None), "{ users { items { Id Email } } }")
        .await;
    assert!(response.status == 401 || !response.errors().is_empty());
    assert!(response.body["data"].is_null());
    let caller = app.caller(Some(&token));
    for query in [
        "mutation { createUser(item: { Email: \"x@example.test\" }) { Id } }",
        "mutation { updateUser(Id: \"00000000-0000-4000-8000-000000000001\", item: { Email: \"x\" }) { Id } }",
        "mutation { deleteUser(Id: \"00000000-0000-4000-8000-000000000001\") { Id } }",
    ] {
        let response = app.run(&caller, query).await;
        assert!(
            response
                .messages()
                .iter()
                .any(|m| m.contains("does not exist")),
            "{}",
            response.body
        );
        assert!(response.body.get("data").is_none());
    }
}

#[tokio::test]
async fn app_row_management_never_clears_or_seeds_system_identities() {
    let app = App::new(basic_model()).await;
    let (_, token) = app.user("retained@example.test").await;
    let caller = app.caller(Some(&token));
    app.api.clear_all_rows().await.unwrap();
    assert!(app.api.clear_rows("User").await.is_err());
    assert!(
        app.api
            .row_counts()
            .await
            .unwrap()
            .iter()
            .all(|(name, _)| name != "User")
    );
    let model = app.api.model();
    app.db.call_blocking(|conn| {
        let tx = conn.transaction().unwrap();
        rl_graphql::admin::replace_rows(&tx, &model, &[])
            .unwrap()
            .unwrap();
        tx.commit().unwrap();
    });
    let response = app.run(&caller, "{ users { items { Email } } }").await;
    assert_eq!(
        response.items("users"),
        [json!({"Email": "retained@example.test"})]
    );
}
