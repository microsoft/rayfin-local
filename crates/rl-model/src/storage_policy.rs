//! Admission of the BaaS storage check language, independently of data policies.

use serde_json::Value;

use crate::{
    AppModel, CompareOp, Literal, ModelError, Operand, PolicyAstError, PolicyExpr,
    StorageFieldType, StorageFolder,
};

const PROFILE: &str = "BaaS (`rayfin up`) accepts authenticated storage grants with eq, neq, and, \
    or; claims sub, email, role; owner_id or declared metadata fields; matching string, number, \
    or boolean operand types, and no null literals";

impl AppModel {
    /// Rejects storage rules that BaaS would refuse at deploy, including models deserialized
    /// without going through the analyzer.
    pub fn validate_storage_rules(&self) -> Result<(), ModelError> {
        if let Some(storage) = &self.storage {
            for folder in &storage.folders {
                folder.validate_rules()?;
            }
        }
        Ok(())
    }
}

impl StorageFolder {
    /// Validates every branch of every check, even behind an unconditional grant.
    pub fn validate_rules(&self) -> Result<(), ModelError> {
        for (index, rule) in self.rules.iter().enumerate() {
            let result = if rule.role.eq_ignore_ascii_case("anonymous") {
                Err("anonymous storage grants are not supported".into())
            } else if let Some(check) = &rule.check {
                validate(check, self)
            } else {
                Ok(())
            };
            result.map_err(|reason| storage_rule_error(&self.name, index, &rule.role, reason))?;
        }
        Ok(())
    }
}

pub(crate) fn storage_rule_error(
    folder: &str,
    index: usize,
    role: &str,
    reason: String,
) -> ModelError {
    ModelError::Storage {
        folder: folder.into(),
        message: format!("rule[{index}] for `{role}`: {reason}. {PROFILE}; fix the folder rule"),
    }
}

fn validate(expr: &PolicyExpr, folder: &StorageFolder) -> Result<(), String> {
    match expr {
        PolicyExpr::And(args) | PolicyExpr::Or(args) => {
            if args.len() < 2 {
                return Err("and/or checks require at least two operands".into());
            }
            for arg in args {
                validate(arg, folder)?;
            }
        }
        PolicyExpr::Compare { left, right, .. } => {
            let left = operand_type(left, folder)?;
            let right = operand_type(right, folder)?;
            if left != right {
                return Err(format!("comparison type mismatch: {left} versus {right}"));
            }
        }
    }
    Ok(())
}

fn operand_type(operand: &Operand, folder: &StorageFolder) -> Result<&'static str, String> {
    match operand {
        Operand::Claim(name) if matches!(name.as_str(), "sub" | "email" | "role") => Ok("string"),
        Operand::Claim(name) => Err(format!("unsupported claim `{name}`")),
        Operand::Field(name) if name == "owner_id" => Ok("string"),
        Operand::Field(name) => {
            let field = folder
                .fields
                .iter()
                .find(|field| field.name == *name)
                .ok_or_else(|| format!("unsupported field `{name}` (not declared metadata)"))?;
            Ok(match field.kind {
                StorageFieldType::Number => "number",
                StorageFieldType::Boolean => "boolean",
                StorageFieldType::String | StorageFieldType::Date => "string",
            })
        }
        Operand::Literal(Literal::Null) => Err("null literals are not supported".into()),
        Operand::Literal(Literal::String(_)) => Ok("string"),
        Operand::Literal(Literal::Number(_)) => Ok("number"),
        Operand::Literal(Literal::Bool(_)) => Ok("boolean"),
    }
}

impl PolicyExpr {
    /// Reads the BaaS storage AST. Unlike data policies, either comparison operand may be a
    /// literal, binary logical nodes are accepted, and literal null is rejected.
    pub(crate) fn from_storage_check(node: &Value) -> Result<Self, PolicyAstError> {
        let invalid = |reason: &str| PolicyAstError {
            reason: reason.into(),
        };
        let op = node["op"]
            .as_str()
            .ok_or_else(|| invalid("a storage check needs an `op`"))?;
        match op {
            "and" | "or" => {
                let args = if let Some(args) = node["args"].as_array() {
                    if args.len() < 2 {
                        return Err(invalid("and/or checks require at least two operands"));
                    }
                    args.iter()
                        .map(Self::from_storage_check)
                        .collect::<Result<Vec<_>, _>>()
                } else {
                    [&node["lhs"], &node["rhs"]]
                        .into_iter()
                        .map(Self::from_storage_check)
                        .collect::<Result<Vec<_>, _>>()
                }?;
                Ok(if op == "and" {
                    Self::And(args)
                } else {
                    Self::Or(args)
                })
            }
            "eq" | "neq" => Ok(Self::Compare {
                op: if op == "eq" {
                    CompareOp::Eq
                } else {
                    CompareOp::Ne
                },
                left: storage_operand(&node["lhs"])?,
                right: storage_operand(&node["rhs"])?,
            }),
            _ => Err(PolicyAstError {
                reason: format!("unsupported storage check operator `{op}`"),
            }),
        }
    }
}

fn storage_operand(value: &Value) -> Result<Operand, PolicyAstError> {
    let operand = match value {
        Value::String(value) => Operand::Literal(Literal::String(value.clone())),
        Value::Number(value) => Operand::Literal(Literal::Number(value.clone())),
        Value::Bool(value) => Operand::Literal(Literal::Bool(*value)),
        Value::Object(object) => {
            let claim = object.get("claim");
            let field = object.get("field");
            match (claim, field) {
                (Some(Value::String(name)), None) if !name.trim().is_empty() => {
                    Operand::Claim(name.clone())
                }
                (None, Some(Value::String(name))) if !name.trim().is_empty() => {
                    Operand::Field(name.clone())
                }
                _ => {
                    return Err(PolicyAstError {
                        reason: "a storage operand needs exactly one nonempty claim or field"
                            .into(),
                    });
                }
            }
        }
        _ => {
            return Err(PolicyAstError {
                reason: "storage literals must be string, number, or boolean (not null)".into(),
            });
        }
    };
    Ok(operand)
}
