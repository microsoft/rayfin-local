//! The T-SQL side of the GraphQL API ([`rl_graphql::tsql`]): the schema DAB generates for a
//! connector's entities, and the statements their documents become, on a fake database that
//! records them. `rl-connectors`' `sql_server.rs` runs the same documents on a real SQL Server.

use std::sync::{Arc, Mutex};

use rl_graphql::schema::{Mutations, Schema, SchemaOptions};
use rl_graphql::tsql::backend::{
    BoxFuture, Param, Row, SqlBackend, SqlError, SqlTransaction, Statement, Value,
};
use rl_graphql::tsql::{Target, TsqlApi};
use rl_graphql::{DataApiSettings, Outcome, Request};
use rl_model::{Action, AppModel, Caller, Policy, PolicyExpr, Session};
use serde_json::{Map, Value as Json, json};

fn inventory() -> AppModel {
    let analysis = include_str!("../../rl-model/tests/fixtures/inventory.connector.json");
    let metadata = include_str!(
        "../../../tests/fixtures/apps/connectors/rayfin/connectors/inventory/metadata.json"
    );
    let mut model =
        AppModel::from_connector_analysis(&serde_json::from_str(analysis).unwrap()).unwrap();
    model.apply_source_metadata(&serde_json::from_str(metadata).unwrap());
    model
}

/// Records the statements it runs and answers each with the next canned rows.
#[derive(Default)]
struct Recorder {
    statements: Mutex<Vec<Statement>>,
    answers: Mutex<Vec<Result<Vec<Row>, SqlError>>>,
    commits: Mutex<usize>,
}

impl Recorder {
    fn answering(answers: Vec<Result<Vec<Row>, SqlError>>) -> Arc<Self> {
        Arc::new(Self {
            statements: Mutex::default(),
            answers: Mutex::new(answers),
            commits: Mutex::default(),
        })
    }

    fn statements(&self) -> Vec<Statement> {
        self.statements.lock().unwrap().clone()
    }

    fn answer(&self, statement: &Statement) -> Result<Vec<Row>, SqlError> {
        self.statements.lock().unwrap().push(statement.clone());
        let mut answers = self.answers.lock().unwrap();
        if answers.is_empty() {
            Ok(Vec::new())
        } else {
            answers.remove(0)
        }
    }
}

struct Tx(Arc<Recorder>);

impl SqlTransaction for Tx {
    fn query<'a>(
        &'a mut self,
        statement: &'a Statement,
    ) -> BoxFuture<'a, Result<Vec<Row>, SqlError>> {
        Box::pin(async move { self.0.answer(statement) })
    }

    fn execute<'a>(&'a mut self, statement: &'a Statement) -> BoxFuture<'a, Result<u64, SqlError>> {
        Box::pin(async move { self.0.answer(statement).map(|rows| rows.len() as u64) })
    }

    fn commit(self: Box<Self>) -> BoxFuture<'static, Result<(), SqlError>> {
        Box::pin(async move {
            *self.0.commits.lock().unwrap() += 1;
            Ok(())
        })
    }
}

struct Backend(Arc<Recorder>);

impl SqlBackend for Backend {
    fn query<'a>(&'a self, statement: &'a Statement) -> BoxFuture<'a, Result<Vec<Row>, SqlError>> {
        Box::pin(async move { self.0.answer(statement) })
    }

    fn begin(&self) -> BoxFuture<'_, Result<Box<dyn SqlTransaction>, SqlError>> {
        let tx: Box<dyn SqlTransaction> = Box::new(Tx(Arc::clone(&self.0)));
        Box::pin(async move { Ok(tx) })
    }
}

fn alice() -> Caller {
    let mut claims = Map::new();
    claims.insert("sub".into(), json!("alice-id"));
    claims.insert("email".into(), json!("alice@contoso.com"));
    Caller::authenticated(Session {
        user_id: "alice-id".into(),
        email: Some("alice@contoso.com".into()),
        claims,
        access_token: "t".into(),
    })
}

fn row(json: Json) -> Row {
    vec![Value::Text(json.to_string())]
}

async fn run(target: Target, recorder: &Arc<Recorder>, query: &str) -> Json {
    run_model(inventory(), target, recorder, query).await
}

async fn run_model(model: AppModel, target: Target, recorder: &Arc<Recorder>, query: &str) -> Json {
    run_as(model, &alice(), target, recorder, query).await
}

async fn run_as(
    model: AppModel,
    caller: &Caller,
    target: Target,
    recorder: &Arc<Recorder>,
    query: &str,
) -> Json {
    let api = TsqlApi::new(Arc::new(model), target, DataApiSettings::default());
    let request = Request {
        query: query.to_owned(),
        ..Request::default()
    };
    match api
        .execute(caller, request, &Backend(Arc::clone(recorder)))
        .await
    {
        Outcome::Graphql { status, body } => {
            assert_eq!(status, 200);
            body
        }
        Outcome::AuthenticationRequired => panic!("401"),
    }
}

#[test]
fn the_schema_follows_the_connectors_keys_and_dialect() {
    let model = inventory();
    let schema = Schema::build_with(&model, SchemaOptions::default());
    let query = schema.object("Query").unwrap();
    let args = |field: &str| -> Vec<String> {
        query.fields[field]
            .args
            .values()
            .map(|arg| format!("{}: {}", arg.name, arg.ty))
            .collect()
    };
    assert_eq!(args("product_by_pk"), ["productId: UUID!"]);
    assert_eq!(
        args("stockLevel_by_pk"),
        ["productId: UUID!", "warehouseCode: String!"]
    );
    assert!(query.fields.contains_key("auditEntries"));
    assert!(!query.fields.contains_key("auditEntry_by_pk"));

    let mutation = schema.object("Mutation").unwrap();
    assert_eq!(mutation.fields["createProduct"].ty.to_string(), "Product");
    let delete: Vec<_> = mutation.fields["deleteStockLevel"].args.keys().collect();
    assert_eq!(delete, ["productId", "warehouseCode"]);
    // Server-generated columns are optional on create (defaults) or left out (identity).
    let create = schema.input_object("CreateProductInput").unwrap();
    assert_eq!(create.fields["productId"].ty.to_string(), "UUID");
    assert_eq!(create.fields["createdAt"].ty.to_string(), "DateTime");
    assert_eq!(create.fields["name"].ty.to_string(), "String!");

    let warehouse = Schema::build_with(
        &model,
        SchemaOptions {
            mutations: Mutations::OperationResult,
        },
    );
    let mutation = warehouse.object("Mutation").unwrap();
    assert_eq!(
        mutation.fields["updateProduct"].ty.to_string(),
        "DbOperationResult"
    );
    assert!(warehouse.object("DbOperationResult").is_some());

    let analytics = Schema::build_with(
        &model,
        SchemaOptions {
            mutations: Mutations::None,
        },
    );
    assert!(analytics.mutation_type.is_none());
}

#[tokio::test]
async fn a_list_becomes_one_statement_with_applies_and_typed_parameters() {
    let recorder = Recorder::answering(vec![Ok(vec![row(json!([
        {"name": "Hammer", "createdAt": "2024-01-15T10:30:00.1234567Z", "category": {"name": "Tools"}},
        ["2024-01-15T10:30:00.1234567Z", "11111111-1111-1111-1111-111111111111"]
    ]))])]);
    let body = run(
        Target::SqlDatabase,
        &recorder,
        r#"{ products(first: 1, orderBy: {createdAt: DESC},
             filter: {name: {contains: "50%"}, createdAt: {gte: "2024-01-01T00:00:00Z"}})
             { items { name createdAt category { name } } endCursor } }"#,
    )
    .await;
    let statements = recorder.statements();
    let statement = &statements[0];
    assert_eq!(
        statement.text,
        "SELECT TOP (2) (CAST(N'[' AS NVARCHAR(MAX)) + (CAST(N'{' AS NVARCHAR(MAX)) + \
         N'\"name\":' + ISNULL(N'\"' + STRING_ESCAPE(CONVERT(NVARCHAR(MAX), t0.[name]), 'json') + \
         N'\"', N'null') + N',\"createdAt\":' + ISNULL(N'\"' + CONVERT(NVARCHAR(40), \
         CAST(t0.[created_at] AS DATETIME2(7)), 126) + N'Z' + N'\"', N'null') + \
         N',\"category\":' + ISNULL([a1].[j], N'null') + N'}') + N',' + N'[' + \
         ISNULL(N'\"' + CONVERT(NVARCHAR(40), CAST(t0.[created_at] AS DATETIME2(7)), 126) + \
         N'Z' + N'\"', N'null') + N',' + ISNULL(N'\"' + LOWER(CONVERT(NVARCHAR(36), \
         t0.[product_id])) + N'\"', N'null') + N']' + N']') AS [j] FROM [sales].[products] AS t0 \
         OUTER APPLY (SELECT TOP (1) (CAST(N'{' AS NVARCHAR(MAX)) + N'\"name\":' + \
         ISNULL(N'\"' + STRING_ESCAPE(CONVERT(NVARCHAR(MAX), t1.[name]), 'json') + N'\"', \
         N'null') + N'}') AS [j] FROM [dbo].[categories] AS t1 WHERE (t1.[category_id] = \
         t0.[category_id])) AS [a1] WHERE ((t0.[name] LIKE @P1 ESCAPE '\\') AND \
         (t0.[created_at] >= CAST(@P2 AS DATETIME2(7)))) ORDER BY t0.[created_at] DESC, \
         t0.[product_id] ASC"
    );
    assert_eq!(
        statement.params,
        [
            Param::Text("%50\\%%".into()),
            Param::DateTime("2024-01-01T00:00:00.0000000".into()),
        ]
    );
    // Dates come back in DAB's format; the cursor keeps their full precision.
    let products = &body["data"]["products"];
    assert_eq!(
        products["items"],
        json!([{"name": "Hammer", "createdAt": "2024-01-15T10:30:00.123Z",
                "category": {"name": "Tools"}}])
    );
    assert!(products["endCursor"].is_null(), "one row: no next page");
}

#[tokio::test]
async fn keys_policies_and_writes_bind_their_columns_types() {
    let recorder = Recorder::answering(vec![
        Ok(vec![vec![Value::Uuid(
            "11111111-1111-1111-1111-111111111111".into(),
        )]]),
        Ok(vec![row(json!({"price": 13}))]),
    ]);
    let body = run(
        Target::SqlDatabase,
        &recorder,
        r#"mutation { updateProduct(productId: "11111111-1111-1111-1111-111111111111",
             item: {price: 13, inStock: false}) { price } }"#,
    )
    .await;
    assert_eq!(body, json!({"data": {"updateProduct": {"price": 13}}}));
    let statements = recorder.statements();
    assert_eq!(
        statements[0].text,
        "UPDATE [sales].[products] SET [price] = @P3, [in_stock] = @P4 OUTPUT \
         INSERTED.[product_id] WHERE (([sales].[products].[product_id] = @P1) AND \
         ([sales].[products].[owner_email] = @P2))"
    );
    assert_eq!(
        statements[0].params,
        [
            Param::Text("11111111-1111-1111-1111-111111111111".into()),
            Param::Text("alice@contoso.com".into()),
            Param::Decimal {
                value: 13,
                scale: 0
            },
            Param::Bool(false),
        ]
    );
    // The row is read back by the key the write returned.
    assert!(
        statements[1]
            .text
            .ends_with("WHERE (t0.[product_id] = @P1)"),
        "{}",
        statements[1].text
    );

    let recorder = Recorder::answering(vec![Ok(vec![vec![Value::Int(1)]])]);
    let body = run(
        Target::Warehouse,
        &recorder,
        r#"mutation { deleteStockLevel(productId: "11111111-1111-1111-1111-111111111111",
             warehouseCode: "NYC") { result } }"#,
    )
    .await;
    assert_eq!(
        body,
        json!({"data": {"deleteStockLevel": {"result": "success"}}})
    );
    assert_eq!(
        recorder.statements()[0].text,
        "DELETE FROM [sales].[stock_levels] WHERE (([sales].[stock_levels].[product_id] = @P1) \
         AND ([sales].[stock_levels].[warehouse_code] = @P2))"
    );
}

fn product_policy(model: &mut AppModel, action: Action, check: Json) {
    let entity = model
        .entities
        .iter_mut()
        .find(|e| e.name == "Product")
        .unwrap();
    let grant = entity
        .permissions
        .iter_mut()
        .find(|p| p.role == "authenticated")
        .unwrap()
        .actions
        .iter_mut()
        .rev()
        .find(|g| g.action.covers(action))
        .unwrap();
    let expr = PolicyExpr::from_check(&check).unwrap();
    grant.policy = Some(Policy {
        text: expr.to_string(),
        expr,
    });
}

#[tokio::test]
async fn connector_create_policy_rejects_omitted_defaulted_columns_without_running_sql() {
    let mut model = inventory();
    product_policy(
        &mut model,
        Action::Create,
        json!({"op": "neq", "lhs": {"field": "createdAt"}, "rhs": null}),
    );
    let recorder = Recorder::default();
    let recorder = Arc::new(recorder);
    let body = run_model(
        model,
        Target::SqlDatabase,
        &recorder,
        r#"mutation { createProduct(item: {name: "Hammer", price: 1, inStock: true,
             categoryId: 1,
             ownerEmail: "alice@contoso.com"}) { name } }"#,
    )
    .await;
    assert_eq!(
        body["errors"][0]["extensions"]["code"], "Forbidden",
        "{body}"
    );
    assert!(recorder.statements().is_empty());
    assert_eq!(*recorder.commits.lock().unwrap(), 0);
}

#[tokio::test]
async fn connector_null_tests_and_policy_type_errors_share_data_semantics() {
    for (check, expected_sql, code) in [
        (
            json!({"op": "eq", "lhs": {"field": "ownerEmail"}, "rhs": null}),
            Some("t0.[owner_email] IS NULL"),
            None,
        ),
        (
            json!({"op": "neq", "lhs": {"field": "ownerEmail"}, "rhs": null}),
            Some("t0.[owner_email] IS NOT NULL"),
            None,
        ),
        (
            json!({"op": "eq", "lhs": {"field": "productId"}, "rhs": {"claim": "sub"}}),
            None,
            Some("BadRequest"),
        ),
        (
            json!({"op": "or", "args": [
                {"op": "eq", "lhs": {"field": "inStock"}, "rhs": true},
                {"op": "eq", "lhs": {"claim": "absent"}, "rhs": "x"}
            ]}),
            None,
            Some("Forbidden"),
        ),
    ] {
        let mut model = inventory();
        product_policy(&mut model, Action::Read, check);
        let recorder = Recorder::answering(vec![Ok(vec![])]);
        let body = run_model(
            model,
            Target::SqlDatabase,
            &recorder,
            "{ products { items { name } } }",
        )
        .await;
        if let Some(code) = code {
            assert_eq!(body["errors"][0]["extensions"]["code"], code);
            assert!(recorder.statements().is_empty());
        } else {
            assert!(body.get("errors").is_none(), "{body}");
            assert!(
                recorder.statements()[0]
                    .text
                    .contains(expected_sql.unwrap())
            );
        }
    }
}

#[tokio::test]
async fn connector_mutation_visibility_does_not_undo_successful_writes() {
    const ID: &str = "11111111-1111-1111-1111-111111111111";
    for read_grant in [true, false] {
        let mut model = inventory();
        if read_grant {
            product_policy(
                &mut model,
                Action::Read,
                json!({"op": "eq", "lhs": {"field": "ownerEmail"}, "rhs": {"claim": "email"}}),
            );
        } else {
            let entity = model
                .entities
                .iter_mut()
                .find(|e| e.name == "Product")
                .unwrap();
            entity
                .permissions
                .iter_mut()
                .find(|p| p.role == "anonymous")
                .unwrap()
                .actions = vec![rl_model::ActionPermission {
                action: Action::Update,
                fields: None,
                policy: None,
            }];
        }
        let recorder =
            Recorder::answering(vec![Ok(vec![vec![Value::Uuid(ID.into())]]), Ok(vec![])]);
        let caller = if read_grant {
            alice()
        } else {
            Caller::anonymous()
        };
        let body = run_as(
            model,
            &caller,
            Target::SqlDatabase,
            &recorder,
            &format!(
                "mutation {{ updateProduct(productId: \"{ID}\", item: {{price: 2}}) {{ name }} }}"
            ),
        )
        .await;
        assert_eq!(body["data"]["updateProduct"], Json::Null);
        assert_eq!(*recorder.commits.lock().unwrap(), 1);
        if read_grant {
            assert!(body.get("errors").is_none(), "{body}");
            assert_eq!(recorder.statements().len(), 2);
        } else {
            assert_eq!(body["errors"][0]["extensions"]["code"], "Forbidden");
            assert_eq!(recorder.statements().len(), 1);
        }
    }
}

#[tokio::test]
async fn connector_filtered_update_is_bad_request_but_filtered_delete_is_null() {
    for (query, expected_code) in [
        (
            r#"mutation { updateProduct(productId: "11111111-1111-1111-1111-111111111111",
              item: {price: 2}) { name } }"#,
            Some("BadRequest"),
        ),
        (
            r#"mutation { deleteProduct(productId: "11111111-1111-1111-1111-111111111111")
              { name } }"#,
            None,
        ),
    ] {
        let recorder = Recorder::answering(vec![Ok(vec![]), Ok(vec![])]);
        let body = run(Target::SqlDatabase, &recorder, query).await;
        if let Some(code) = expected_code {
            assert_eq!(body["errors"][0]["extensions"]["code"], code);
        } else {
            assert_eq!(body, json!({"data": {"deleteProduct": null}}));
        }
        assert_eq!(*recorder.commits.lock().unwrap(), 0);
    }
}

#[tokio::test]
async fn keyless_reads_have_no_order_and_failures_say_why() {
    let recorder = Recorder::answering(vec![Err(SqlError::Unavailable(
        "Run `az login`, then try again.".into(),
    ))]);
    let body = run(
        Target::SqlAnalyticsEndpoint,
        &recorder,
        "{ auditEntries { items { action } } }",
    )
    .await;
    let text = &recorder.statements()[0].text;
    assert!(!text.contains("ORDER BY"), "{text}");
    assert_eq!(
        body["errors"][0]["message"],
        "Run `az login`, then try again."
    );
    assert_eq!(body["errors"][0]["path"], json!(["auditEntries"]));
    assert_eq!(body["data"], Json::Null);

    let recorder = Recorder::answering(Vec::new());
    let body = run(
        Target::SqlDatabase,
        &recorder,
        "{ products { groupBy(fields: [inStock]) { fields { inStock } } } }",
    )
    .await;
    assert!(recorder.statements().is_empty());
    assert!(
        body["errors"][0]["message"]
            .as_str()
            .unwrap()
            .starts_with("Aggregations (groupBy) on connector entities don't run"),
        "{body}"
    );
}
