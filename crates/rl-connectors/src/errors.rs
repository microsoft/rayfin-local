//! The Fabric-facing error policy, separate from the SQL executor's diagnostic errors.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use rl_graphql::Outcome;
use serde_json::{Value, json};

pub(crate) const INTERNAL_MESSAGE: &str = "An internal execution error occured";

pub(crate) fn internal(status: StatusCode, code: &str, detail: &str) -> Response {
    tracing::error!(
        status = status.as_u16(),
        code,
        detail = %rl_tokens::redact_diagnostic(detail, []),
        "connector request failed"
    );
    (
        status,
        Json(json!({ "error": {
        "code": "InternalServerError", "message": INTERNAL_MESSAGE,
    } })),
    )
        .into_response()
}

/// Keep GraphQL HTTP status, partial data, locations and paths, but never SQL diagnostics.
pub(crate) fn mask_graphql(mut outcome: Outcome) -> Outcome {
    let Outcome::Graphql { body, .. } = &mut outcome else {
        return outcome;
    };
    if let Some(errors) = body.get_mut("errors").and_then(Value::as_array_mut) {
        for error in errors {
            let code = error.pointer("/extensions/code").and_then(Value::as_str);
            if matches!(code, Some("DatabaseOperationFailed" | "UnexpectedError")) {
                let detail = error.pointer("/extensions/detail").and_then(Value::as_str);
                let client_message = detail.filter(|message| is_client_message(message));
                let client = client_message.is_some();
                let message = client_message.unwrap_or(INTERNAL_MESSAGE).to_owned();
                if !client {
                    tracing::error!(
                        detail = %rl_tokens::redact_diagnostic(&error.to_string(), []),
                        "SQL connector GraphQL request failed"
                    );
                }
                error["message"] = message.into();
                error["extensions"] = json!({
                    "code": if client { "BadRequest" } else { "InternalServerError" },
                });
            }
        }
    }
    outcome
}

/// The vetted SQL-number mapping used by Fabric's GraphQL workload, not the OSS host's smaller
/// mapping. Unknown numbers remain system errors, rather than exposing their SQL messages.
pub(crate) fn sql_client_message(number: u32) -> Option<&'static str> {
    Some(match number {
        2 | 53 | 11001 | 40532 | 40615 => {
            "The request to data source failed. This may due to firewall or network access rules or incorrect connection details"
        }
        21 | 1205 | 15813 | 24596 => "The request to the data source failed unexpectedly.",
        201 | 313 => "The request failed due to invalid or missing parameters.",
        207 | 208 | 4502 | 8114 => {
            "The destination, destination table, column, or view could not be found. Please contact the API owner to verify the data source."
        }
        229 | 230 | 373 => {
            "The request to data source failed with permission error on the objects. Ensure the access to data source objects is configured correctly."
        }
        245 | 515 | 544 | 547 => "The request failed in data source due to constraint violation.",
        258 => "The request to data source timed out.",
        511 | 2628 | 7139 => "The data in the request exceeds the limits in data source.",
        916 | 10054 | 18456 | 19732 => {
            "The request to data source failed with authentication error. Ensure the access to data source is configured correctly."
        }
        2601 | 2627 => "The request failed in data source. Cannot insert duplicate key in object.",
        2714 => "The request failed due to attempt to create already existing object",
        6005 | 10928 | 24800..=24802 | 40613 | 42109 | 42119 => {
            "The data source may be temporarily unavailable. Please try again."
        }
        8003 => "The request failed in datasource due to too many input parameters.",
        8623 => "The request failed in data source.",
        8711 => {
            "The request failed in the data source due to mismatched WITHIN GROUP (ORDER BY ...) clauses in ordered aggregate functions. Ensure that the ORDER BY lists match, or separate them into subqueries."
        }
        40925 | 42131 => "The data source is unavailable.",
        11509..=11541 => {
            "Only those stored procedures whose metadata for the first result set described by sys.dm_exec_describe_first_result_set are supported."
        }
        50000.. => "The request failed in data source.",
        _ => return None,
    })
}

fn is_client_message(message: &str) -> bool {
    const REPRESENTATIVE_NUMBERS: &[u32] = &[
        2, 21, 201, 207, 229, 245, 258, 511, 916, 2601, 2714, 6005, 8003, 8623, 8711, 40925, 11509,
    ];
    REPRESENTATIVE_NUMBERS
        .iter()
        .any(|number| sql_client_message(*number) == Some(message))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(error: Value) -> Value {
        let outcome = Outcome::Graphql {
            status: 200,
            body: json!({ "data": {"other": 1}, "errors": [error] }),
        };
        let Outcome::Graphql { status, body } = mask_graphql(outcome) else {
            panic!()
        };
        assert_eq!(status, 200);
        assert_eq!(body["data"], json!({"other": 1}));
        body
    }

    #[test]
    fn masks_internal_details_preserving_locations_and_partial_data() {
        let body = response(json!({
            "message": "database private-host failed",
            "locations": [{"line": 2, "column": 3}], "path": ["products"],
            "extensions": {"code": "DatabaseOperationFailed", "detail": "private SQL statement"},
        }));
        assert_eq!(
            body["errors"][0],
            json!({
                "message": INTERNAL_MESSAGE,
                "locations": [{"line": 2, "column": 3}], "path": ["products"],
                "extensions": {"code": "InternalServerError"},
            })
        );
        assert!(!body.to_string().contains("private"));
    }

    #[test]
    fn retains_curated_sql_client_errors_and_validation_errors() {
        for number in [207, 245, 515, 547, 2601, 2627] {
            let message = sql_client_message(number).unwrap();
            let body = response(json!({
                "message": "database error",
                "extensions": {"code": "DatabaseOperationFailed", "detail": message},
            }));
            assert_eq!(
                body["errors"][0],
                json!({
                    "message": message, "extensions": {"code": "BadRequest"},
                })
            );
        }
        let original = json!({"message": "unknown field", "extensions": {"code": "HC0016"}});
        assert_eq!(response(original.clone())["errors"][0], original);
    }
}
