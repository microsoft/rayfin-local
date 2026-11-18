//! The JSON fallback: `executeQueries` for developer tokens `executeDaxQueries` refuses.
//!
//! Power BI answers `executeDaxQueries` (Arrow) only to tokens with a dataset scope
//! (`Dataset.Read.All`), which the Azure CLI can't get; its tokens carry `user_impersonation`,
//! which `executeQueries` (JSON) accepts. The JSON answer goes back in the
//! `FabricSemanticModelTabularResponse` envelope (the shape it was designed for), which the SDK
//! runtime normalizes as it normalizes a decoded Arrow stream. What differs from the Arrow path:
//!
//! - columns' `dataType`s are read from their values, as `executeQueries` sends none: numbers are
//!   `Int64` when every one is whole, else `Double` (Power BI's `Decimal` reads as one of those),
//!   and a column of nulls or of mixed values has none (the SDK reports `'unknown'`);
//! - DateTimes have no milliseconds;
//! - an empty result has no columns;
//! - Power BI caps results at 100,000 rows: more is an `overflow` error, unless
//!   `resultSetRowCountLimit` asks for no more than that;
//! - an evaluation error is a `query` error (the SDK's Arrow decoder keeps the rows before it);
//! - `resultSetRowCountLimit` truncates the rows here; `culture`, `schemaOnly` and `queryTimeout`
//!   have no `executeQueries` equivalent.

use super::*;

/// Power BI's error codes in `results[0].error`: more than one `EVALUATE`, and too many rows.
const MORE_THAN_ONE_RESULT_SET: &str = "DaxResultsetCountNotSupported";
const TOO_MANY_ROWS: &str = "DaxRowCountNotSupported";
/// `Number.MAX_SAFE_INTEGER`: the SDK's Arrow decoder reports larger integers as `overflow`.
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

pub(super) async fn execute(
    power_bi: &PowerBi,
    connector: &str,
    target: &Target<'_>,
    query: &str,
    input: &Map<String, Value>,
    token: &str,
) -> Attempt {
    let client_request_id = Uuid::new_v4().to_string();
    let url = power_bi.dataset_url(target, "executeQueries");
    let timeout = total_timeout(input);
    let body = json!({
        "queries": [{ "query": query }],
        "serializerSettings": { "includeNulls": true },
    });
    let started = Instant::now();
    let sent = power_bi
        .post(
            &url,
            token,
            "application/json",
            &client_request_id,
            timeout,
            &body,
        )
        .await;
    let response = match sent {
        Ok(response) => response,
        Err(failure) => {
            tracing::warn!(connector, request_id = %client_request_id, "{}", failure.message);
            return Attempt::Answered(failed(&client_request_id, failure));
        }
    };
    let status = response.status();
    let request_id = answer_request_id(&response, client_request_id);
    tracing::info!(
        connector,
        status = status.as_u16(),
        latency_ms = started.elapsed().as_millis() as u64,
        request_id = %request_id,
        "semantic-model query (executeQueries)"
    );

    if !status.is_success() {
        let body = read_error_body(response).await;
        if let Some(query_error) = compile_error(&body) {
            return Attempt::Answered(succeeded(&request_id, Vec::new(), Some(query_error)));
        }
        if status == StatusCode::UNAUTHORIZED {
            return Attempt::Refused(Refusal { request_id, body });
        }
        tracing::warn!(
            connector,
            status = status.as_u16(),
            request_id = %request_id,
            "Power BI refused the query"
        );
        return Attempt::Answered(failed(&request_id, QueryFailure::api(body, status)));
    }
    let answer = match response.bytes().await {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes).map_err(|e| e.to_string()),
        Err(error) => Err(error.to_string()),
    };
    Attempt::Answered(match answer {
        Ok(answer) => {
            let (tables, query_error) = tables_of(answer, row_limit(input));
            succeeded(&request_id, tables, query_error)
        }
        Err(problem) => {
            let message = format!("Failed to read Power BI's executeQueries answer: {problem}");
            failed(&request_id, QueryFailure::new("unknown", message))
        }
    })
}

/// `results[0]` of an `executeQueries` answer as the envelope's `tables` and `queryError`.
fn tables_of(mut answer: Value, row_limit: Option<u64>) -> (Vec<Value>, Option<Value>) {
    let mut result = answer
        .pointer_mut("/results/0")
        .map(Value::take)
        .unwrap_or_default();
    let mut rows = match result.pointer_mut("/tables/0/rows").map(Value::take) {
        Some(Value::Array(rows)) => rows,
        _ => Vec::new(),
    };
    let error = &result["error"];
    if error.is_object() {
        let limit_covered = row_limit
            .and_then(|limit| usize::try_from(limit).ok())
            .is_some_and(|limit| limit <= rows.len());
        match error["code"].as_str() {
            // The Arrow path reads the first result set too.
            Some(MORE_THAN_ONE_RESULT_SET) => {}
            // Power BI stopped at its cap, but past the rows the caller asked for.
            Some(TOO_MANY_ROWS) if limit_covered => {}
            Some(TOO_MANY_ROWS) => {
                let error = json!({ "message": message_of(error), "code": TOO_MANY_ROWS });
                return (vec![json!({ "rows": [], "error": error })], None);
            }
            _ => return (Vec::new(), Some(query_error(error))),
        }
    }
    let columns = columns_of(&rows);
    if let Some(limit) = row_limit {
        rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    }
    if let Some(message) = unsafe_integer(&rows) {
        return (
            vec![json!({ "rows": [], "error": { "message": message } })],
            None,
        );
    }
    let mut table = json!({ "rows": rows });
    if !columns.is_empty() {
        table["columns"] = Value::Array(columns);
    }
    (vec![table], None)
}

/// The first row's columns, in Power BI's order, each with the `dataType` the SDK's Arrow decoder
/// reports when the column's values settle it. Read from every row Power BI sent, not only those
/// a row limit keeps.
fn columns_of(rows: &[Value]) -> Vec<Value> {
    let Some(Value::Object(first)) = rows.first() else {
        return Vec::new();
    };
    first
        .keys()
        .map(|name| {
            let mut column = json!({ "name": name });
            if let Some(data_type) = data_type(rows.iter().map(|row| &row[name])) {
                column["dataType"] = data_type.into();
            }
            column
        })
        .collect()
}

/// The data type `values` share, nulls aside; `None` when they share none or are all null.
fn data_type<'a>(values: impl Iterator<Item = &'a Value>) -> Option<&'static str> {
    let mut shared = None;
    for value in values {
        let this = match value {
            Value::Null => continue,
            Value::Bool(_) => "Boolean",
            Value::Number(number) if number.is_f64() => "Double",
            Value::Number(_) => "Int64",
            Value::String(text) if is_date_time(text) => "DateTime",
            Value::String(_) => "String",
            Value::Array(_) | Value::Object(_) => return None,
        };
        shared = Some(match (shared, this) {
            (None, _) => this,
            (Some(so_far), _) if so_far == this => so_far,
            // Whole numbers among fractions, and DateTime-like text among other text.
            (Some("Int64" | "Double"), "Int64" | "Double") => "Double",
            (Some("DateTime" | "String"), "DateTime" | "String") => "String",
            _ => return None,
        });
    }
    shared
}

/// Whether `text` is a DateTime as `executeQueries` writes one: `2024-08-02T00:00:00`, perhaps
/// with fractional seconds, and without a time zone (Analysis Services' DateTimes have none).
fn is_date_time(text: &str) -> bool {
    const SHAPE: &[u8] = b"0000-00-00T00:00:00";
    let Some((stamp, fraction)) = text.as_bytes().split_at_checked(SHAPE.len()) else {
        return false;
    };
    let stamp_fits = stamp.iter().zip(SHAPE).all(|(byte, shape)| match shape {
        b'0' => byte.is_ascii_digit(),
        _ => byte == shape,
    });
    let fraction_fits = match fraction {
        [] => true,
        [b'.', digits @ ..] => !digits.is_empty() && digits.iter().all(u8::is_ascii_digit),
        _ => false,
    };
    stamp_fits && fraction_fits
}

/// An in-band error (`results[0].error`), with Analysis Services' code as the Arrow error table
/// has it.
fn query_error(error: &Value) -> Value {
    let mut query_error = json!({ "message": message_of(error) });
    if let Some(code) = detail_value(&error["details"], "AnalysisServicesErrorCode")
        .or_else(|| error["code"].as_str().map(str::to_owned))
    {
        query_error["code"] = code.into();
    }
    query_error
}

fn message_of(error: &Value) -> String {
    error["message"]
        .as_str()
        .filter(|m| !m.is_empty())
        .unwrap_or("Power BI couldn't run the query.")
        .to_owned()
}

/// A DAX compile error: 400 `DatasetExecuteQueriesError` with the message and Analysis Services'
/// code in `pbi.error.details` (the Arrow path's in-band error table).
fn compile_error(body: &str) -> Option<Value> {
    let parsed: Value = serde_json::from_str(body).ok()?;
    let error = &parsed["error"];
    if error["code"] != "DatasetExecuteQueriesError" {
        return None;
    }
    let details = &error["pbi.error"]["details"];
    let mut query_error = json!({ "message": detail_value(details, "DetailsMessage")? });
    if let Some(code) = detail_value(details, "AnalysisServicesErrorCode") {
        query_error["code"] = code.into();
    }
    Some(query_error)
}

/// `details[] .detail.value` of the entry with `code`, as text.
fn detail_value(details: &Value, code: &str) -> Option<String> {
    let entry = details.as_array()?.iter().find(|d| d["code"] == code)?;
    match &entry["detail"]["value"] {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

/// The SDK's Arrow decoder message for an integer JavaScript can't hold exactly.
fn unsafe_integer(rows: &[Value]) -> Option<String> {
    rows.iter()
        .filter_map(Value::as_object)
        .flatten()
        .find_map(|(column, value)| {
            let Value::Number(number) = value else {
                return None;
            };
            let beyond = number.as_u64().is_some_and(|n| n > MAX_SAFE_INTEGER)
                || number
                    .as_i64()
                    .is_some_and(|n| n.unsigned_abs() > MAX_SAFE_INTEGER);
            beyond.then(|| {
                format!(
                    "Integer value {number} in column \"{column}\" exceeds \
                     Number.MAX_SAFE_INTEGER and cannot be safely represented."
                )
            })
        })
}

fn succeeded(request_id: &str, tables: Vec<Value>, query_error: Option<Value>) -> Response {
    let mut output = Map::new();
    output.insert("tables".into(), tables.into());
    output.insert("requestId".into(), request_id.into());
    if let Some(query_error) = query_error {
        output.insert("queryError".into(), query_error);
    }
    envelope(request_id, "Succeeded", Value::Object(output))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(result: Value) -> Value {
        json!({ "informationProtectionLabel": { "name": "Internal" }, "results": [result] })
    }

    #[test]
    fn rows_become_the_first_table() {
        let rows = json!([{ "[x]": 1 }, { "[x]": 2 }, { "[x]": null }]);
        let columns = json!([{ "name": "[x]", "dataType": "Int64" }]);
        let (tables, error) = tables_of(answer(json!({ "tables": [{ "rows": rows }] })), None);
        assert_eq!(tables, [json!({ "rows": rows, "columns": columns })]);
        assert_eq!(error, None);

        let (tables, _) = tables_of(answer(json!({ "tables": [{ "rows": rows }] })), Some(2));
        let kept = json!([{ "[x]": 1 }, { "[x]": 2 }]);
        assert_eq!(tables, [json!({ "rows": kept, "columns": columns })]);
    }

    #[test]
    fn columns_take_the_type_their_values_share() {
        let rows = json!([
            {
                "s[Year]": 2020, "[Sales]": 11.5, "[Units]": 3, "s[Date]": "2020-01-01T00:00:00",
                "s[Name]": "Asia", "[On]": true, "[Blank]": null, "[Variant]": 5,
            },
            {
                "s[Year]": 2021, "[Sales]": 12, "[Units]": null,
                "s[Date]": "2020-02-01T08:30:00.123", "s[Name]": "2020-01-01T00:00:00",
                "[On]": false, "[Blank]": null, "[Variant]": "five",
            },
        ]);
        let (tables, _) = tables_of(answer(json!({ "tables": [{ "rows": rows }] })), Some(1));
        assert_eq!(
            tables[0]["columns"],
            json!([
                { "name": "s[Year]", "dataType": "Int64" },
                { "name": "[Sales]", "dataType": "Double" },
                { "name": "[Units]", "dataType": "Int64" },
                { "name": "s[Date]", "dataType": "DateTime" },
                { "name": "s[Name]", "dataType": "String" },
                { "name": "[On]", "dataType": "Boolean" },
                { "name": "[Blank]" },
                { "name": "[Variant]" },
            ])
        );
        // The row limit keeps one row, but the types come from both.
        assert_eq!(tables[0]["rows"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn in_band_errors_are_categorized_as_on_the_arrow_path() {
        let evaluation = json!({
            "tables": [{ "rows": [] }],
            "error": {
                "code": "QueryUserError",
                "message": "Query (1, 19) Cannot convert value 'a' of type Text to type Number.",
                "details": [{ "code": "AnalysisServicesErrorCode", "detail": { "type": "1", "value": 3241541768u64 } }],
            },
        });
        let (tables, error) = tables_of(answer(evaluation), None);
        assert!(tables.is_empty());
        assert_eq!(
            error,
            Some(json!({
                "message": "Query (1, 19) Cannot convert value 'a' of type Text to type Number.",
                "code": "3241541768",
            }))
        );

        let two_result_sets = json!({
            "tables": [{ "rows": [{ "[a]": 1 }] }],
            "error": { "code": MORE_THAN_ONE_RESULT_SET, "message": "More than 1 result set." },
        });
        let (tables, error) = tables_of(answer(two_result_sets), None);
        let columns = json!([{ "name": "[a]", "dataType": "Int64" }]);
        assert_eq!(
            tables,
            [json!({ "rows": [{ "[a]": 1 }], "columns": columns })]
        );
        assert_eq!(error, None);

        let too_many_rows = json!({
            "tables": [{ "rows": [{ "[Value]": 1 }, { "[Value]": 2 }] }],
            "error": { "code": TOO_MANY_ROWS, "message": "More than 100000 rows." },
        });
        let (tables, error) = tables_of(answer(too_many_rows.clone()), None);
        let table_error = json!({ "message": "More than 100000 rows.", "code": TOO_MANY_ROWS });
        assert_eq!(tables, [json!({ "rows": [], "error": table_error })]);
        assert_eq!(error, None);
        // A row limit within the rows Power BI returned is honored instead.
        let (tables, error) = tables_of(answer(too_many_rows), Some(1));
        let columns = json!([{ "name": "[Value]", "dataType": "Int64" }]);
        assert_eq!(
            tables,
            [json!({ "rows": [{ "[Value]": 1 }], "columns": columns })]
        );
        assert_eq!(error, None);

        let big = json!({ "tables": [{ "rows": [{ "[big]": 9007199254740993u64 }] }] });
        let (tables, _) = tables_of(answer(big), None);
        assert_eq!(
            tables[0]["error"]["message"],
            "Integer value 9007199254740993 in column \"[big]\" exceeds Number.MAX_SAFE_INTEGER \
             and cannot be safely represented."
        );
    }

    #[test]
    fn compile_errors_are_query_errors() {
        let body = r#"{"error":{"code":"DatasetExecuteQueriesError","pbi.error":{"code":"DatasetExecuteQueriesError","parameters":{},"details":[{"code":"DetailsMessage","detail":{"type":1,"value":"The end of the input was reached."}},{"code":"AnalysisServicesErrorCode","detail":{"type":1,"value":"3238920193"}}]}}}"#;
        assert_eq!(
            compile_error(body),
            Some(json!({ "message": "The end of the input was reached.", "code": "3238920193" }))
        );
        assert_eq!(
            compile_error(r#"{"error":{"code":"BadRequest","message":"Bad Request"}}"#),
            None
        );
    }
}
