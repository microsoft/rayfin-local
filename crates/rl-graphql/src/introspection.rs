//! Standard introspection, reflected lazily from the same schema the validator and planner use.

use serde_json::{Value as Json, json};

use crate::error::GraphQlError;
use crate::plan::{Collected, Ctx, collect_fields};
use crate::schema::{
    EnumDef, FieldDef, InputValueDef, ObjectDef, Resolver, Schema, TypeDef, TypeRef, field, input,
};

const TYPE_KINDS: &[&str] = &[
    "SCALAR",
    "OBJECT",
    "INTERFACE",
    "UNION",
    "ENUM",
    "INPUT_OBJECT",
    "LIST",
    "NON_NULL",
];
const DIRECTIVE_LOCATIONS: &[&str] = &[
    "QUERY",
    "MUTATION",
    "SUBSCRIPTION",
    "FIELD",
    "FRAGMENT_DEFINITION",
    "FRAGMENT_SPREAD",
    "INLINE_FRAGMENT",
    "VARIABLE_DEFINITION",
    "SCHEMA",
    "SCALAR",
    "OBJECT",
    "FIELD_DEFINITION",
    "ARGUMENT_DEFINITION",
    "INTERFACE",
    "UNION",
    "ENUM",
    "ENUM_VALUE",
    "INPUT_OBJECT",
    "INPUT_FIELD_DEFINITION",
];

/// Registers metadata types and query meta-fields before validation can use them.
pub(crate) fn extend_schema(schema: &mut Schema) {
    for (name, values) in [
        ("__TypeKind", TYPE_KINDS),
        ("__DirectiveLocation", DIRECTIVE_LOCATIONS),
    ] {
        let def = TypeDef::Enum(EnumDef {
            name: name.into(),
            description: None,
            values: values.iter().map(|s| (*s).into()).collect(),
        });
        schema.types.insert(name.into(), def);
    }
    let string = TypeRef::named("String");
    let boolean = TypeRef::named("Boolean");
    let ty = TypeRef::named("__Type");
    for (name, fields) in [
        (
            "__Schema",
            vec![
                metadata("description", string.clone()),
                metadata("types", ty.clone().non_null().list().non_null()),
                metadata("queryType", ty.clone().non_null()),
                metadata("mutationType", ty.clone()),
                metadata("subscriptionType", ty.clone()),
                metadata(
                    "directives",
                    TypeRef::named("__Directive").non_null().list().non_null(),
                ),
            ],
        ),
        (
            "__Type",
            vec![
                metadata("kind", TypeRef::named("__TypeKind").non_null()),
                metadata("name", string.clone()),
                metadata("description", string.clone()),
                metadata("specifiedByURL", string.clone()),
                deprecated_filter(metadata(
                    "fields",
                    TypeRef::named("__Field").non_null().list(),
                )),
                metadata("interfaces", ty.clone().non_null().list()),
                metadata("possibleTypes", ty.clone().non_null().list()),
                deprecated_filter(metadata(
                    "enumValues",
                    TypeRef::named("__EnumValue").non_null().list(),
                )),
                deprecated_filter(metadata(
                    "inputFields",
                    TypeRef::named("__InputValue").non_null().list(),
                )),
                metadata("ofType", ty.clone()),
                metadata("isOneOf", boolean.clone()),
            ],
        ),
        (
            "__Field",
            vec![
                metadata("name", string.clone().non_null()),
                metadata("description", string.clone()),
                deprecated_filter(metadata(
                    "args",
                    TypeRef::named("__InputValue").non_null().list().non_null(),
                )),
                metadata("type", ty.clone().non_null()),
                metadata("isDeprecated", boolean.clone().non_null()),
                metadata("deprecationReason", string.clone()),
            ],
        ),
        (
            "__InputValue",
            vec![
                metadata("name", string.clone().non_null()),
                metadata("description", string.clone()),
                metadata("type", ty.non_null()),
                metadata("defaultValue", string.clone()),
                metadata("isDeprecated", boolean.clone().non_null()),
                metadata("deprecationReason", string.clone()),
            ],
        ),
        (
            "__EnumValue",
            vec![
                metadata("name", string.clone().non_null()),
                metadata("description", string.clone()),
                metadata("isDeprecated", boolean.clone().non_null()),
                metadata("deprecationReason", string.clone()),
            ],
        ),
        (
            "__Directive",
            vec![
                metadata("name", string.clone().non_null()),
                metadata("description", string),
                metadata("isRepeatable", boolean.non_null()),
                metadata(
                    "locations",
                    TypeRef::named("__DirectiveLocation")
                        .non_null()
                        .list()
                        .non_null(),
                ),
                deprecated_filter(metadata(
                    "args",
                    TypeRef::named("__InputValue").non_null().list().non_null(),
                )),
            ],
        ),
    ] {
        schema.types.insert(
            name.into(),
            TypeDef::Object(ObjectDef {
                name: name.into(),
                description: None,
                fields: fields.into_iter().map(|f| (f.name.clone(), f)).collect(),
            }),
        );
    }
    let mut by_name = metadata("__type", TypeRef::named("__Type"));
    by_name.args.insert(
        "name".into(),
        input(
            "name",
            "The type's name.",
            TypeRef::named("String").non_null(),
        ),
    );
    let query = schema.types.get_mut(&schema.query_type).unwrap();
    let TypeDef::Object(query) = query else {
        unreachable!("the query root is an object")
    };
    for def in [
        metadata("__schema", TypeRef::named("__Schema").non_null()),
        by_name,
    ] {
        query.fields.insert(def.name.clone(), def);
    }
}

fn metadata(name: &str, ty: TypeRef) -> FieldDef {
    field(name, None, ty, Resolver::Extension)
}

fn deprecated_filter(mut field: FieldDef) -> FieldDef {
    let mut arg = input(
        "includeDeprecated",
        "Include deprecated members.",
        TypeRef::named("Boolean"),
    );
    arg.default = Some("false".into());
    field.args.insert(arg.name.clone(), arg);
    field
}

/// Standard directives: executable directives match the validator's skip/include behavior.
fn directives() -> Vec<Directive> {
    let conditional = |name| Directive {
        name,
        description: "Conditionally includes or skips the selected field or fragment.",
        locations: &["FIELD", "FRAGMENT_SPREAD", "INLINE_FRAGMENT"],
        args: vec![input(
            "if",
            "The condition.",
            TypeRef::named("Boolean").non_null(),
        )],
    };
    let mut reason = input(
        "reason",
        "Why this member is deprecated.",
        TypeRef::named("String"),
    );
    reason.default = Some("\"No longer supported\"".into());
    vec![
        conditional("skip"),
        conditional("include"),
        Directive {
            name: "deprecated",
            description: "Marks a schema member as deprecated.",
            locations: &[
                "FIELD_DEFINITION",
                "ARGUMENT_DEFINITION",
                "INPUT_FIELD_DEFINITION",
                "ENUM_VALUE",
            ],
            args: vec![reason],
        },
        Directive {
            name: "specifiedBy",
            description: "Specifies a custom scalar's specification URL.",
            locations: &["SCALAR"],
            args: vec![input(
                "url",
                "The specification URL.",
                TypeRef::named("String").non_null(),
            )],
        },
        Directive {
            name: "oneOf",
            description: "Requires exactly one non-null input field.",
            locations: &["INPUT_OBJECT"],
            args: Vec::new(),
        },
    ]
}

struct Directive {
    name: &'static str,
    description: &'static str,
    locations: &'static [&'static str],
    args: Vec<InputValueDef>,
}

enum Node<'a> {
    Schema,
    Type(TypeRef),
    Field(&'a FieldDef),
    Input(&'a InputValueDef),
    EnumValue(&'a str),
    Directive(Directive),
    DirectiveInput(InputValueDef),
}

enum Value<'a> {
    Scalar(Json),
    Object(Node<'a>),
    List(Vec<Node<'a>>),
}

impl From<Json> for Value<'_> {
    fn from(value: Json) -> Self {
        Self::Scalar(value)
    }
}

pub(crate) fn execute(ctx: &Ctx<'_>, field: &Collected<'_>) -> Result<Json, GraphQlError> {
    let root = ctx.schema.object(&ctx.schema.query_type).unwrap();
    let def = &root.fields[field.name()];
    let node = if field.name() == "__schema" {
        Node::Schema
    } else {
        let args = ctx.arguments(def, &field.fields[0].node)?;
        let name = match args.get("name") {
            Some(crate::Input::Scalar(Json::String(name))) => name.as_str(),
            _ => "",
        };
        if !ctx.schema.types.contains_key(name) {
            return Ok(Json::Null);
        }
        Node::Type(TypeRef::named(name))
    };
    render(ctx, def.ty.name(), &node, &field.sub_sets())
}

fn render(
    ctx: &Ctx<'_>,
    parent: &str,
    node: &Node<'_>,
    sets: &[&async_graphql_parser::types::SelectionSet],
) -> Result<Json, GraphQlError> {
    let mut out = serde_json::Map::new();
    for selected in collect_fields(ctx, parent, sets) {
        let value = if selected.name() == "__typename" {
            json!(parent)
        } else {
            let def = &ctx.schema.object(parent).unwrap().fields[selected.name()];
            let value = reflect(ctx.schema, node, selected.name());
            match value {
                Value::Scalar(value) => value,
                Value::Object(child) => render(ctx, def.ty.name(), &child, &selected.sub_sets())?,
                Value::List(children) => Json::Array(
                    children
                        .iter()
                        .map(|child| render(ctx, def.ty.name(), child, &selected.sub_sets()))
                        .collect::<Result<_, _>>()?,
                ),
            }
        };
        out.insert(selected.key, value);
    }
    Ok(Json::Object(out))
}

fn reflect<'a>(schema: &'a Schema, node: &Node<'a>, name: &str) -> Value<'a> {
    match node {
        Node::Schema => match name {
            "types" => Value::List(
                schema
                    .types
                    .keys()
                    .map(|name| Node::Type(TypeRef::named(name)))
                    .collect(),
            ),
            "queryType" => Value::Object(Node::Type(TypeRef::named(&schema.query_type))),
            "mutationType" => schema.mutation_type.as_ref().map_or_else(
                || Json::Null.into(),
                |name| Value::Object(Node::Type(TypeRef::named(name))),
            ),
            "directives" => Value::List(directives().into_iter().map(Node::Directive).collect()),
            _ => Json::Null.into(),
        },
        Node::Type(ty) => reflect_type(schema, ty, name),
        Node::Field(field) => match name {
            "name" => json!(field.name).into(),
            "description" => json!(field.description).into(),
            "args" => Value::List(field.args.values().map(Node::Input).collect()),
            "type" => Value::Object(Node::Type(field.ty.clone())),
            "isDeprecated" => json!(false).into(),
            _ => Json::Null.into(),
        },
        Node::Input(input) => reflect_input(input, name),
        Node::DirectiveInput(input) => reflect_input(input, name),
        Node::EnumValue(value) => match name {
            "name" => json!(value).into(),
            "isDeprecated" => json!(false).into(),
            _ => Json::Null.into(),
        },
        Node::Directive(directive) => match name {
            "name" => json!(directive.name).into(),
            "description" => json!(directive.description).into(),
            "isRepeatable" => json!(false).into(),
            "locations" => json!(directive.locations).into(),
            "args" => Value::List(
                directive
                    .args
                    .iter()
                    .cloned()
                    .map(Node::DirectiveInput)
                    .collect(),
            ),
            _ => Json::Null.into(),
        },
    }
}

fn reflect_input<'a>(input: &InputValueDef, name: &str) -> Value<'a> {
    match name {
        "name" => json!(input.name).into(),
        "description" => json!(input.description).into(),
        "type" => Value::Object(Node::Type(input.ty.clone())),
        "defaultValue" => json!(input.default).into(),
        "isDeprecated" => json!(false).into(),
        _ => Json::Null.into(),
    }
}

fn reflect_type<'a>(schema: &'a Schema, ty: &TypeRef, name: &str) -> Value<'a> {
    let TypeRef::Named(type_name) = ty else {
        return match (name, ty) {
            ("kind", TypeRef::NonNull(_)) => json!("NON_NULL").into(),
            ("kind", _) => json!("LIST").into(),
            ("ofType", TypeRef::NonNull(inner) | TypeRef::List(inner)) => {
                Value::Object(Node::Type((**inner).clone()))
            }
            _ => Json::Null.into(),
        };
    };
    let def = &schema.types[type_name];
    match name {
        "kind" => json!(match def {
            TypeDef::Scalar(..) => "SCALAR",
            TypeDef::Object(_) => "OBJECT",
            TypeDef::InputObject(_) => "INPUT_OBJECT",
            TypeDef::Enum(_) => "ENUM",
        })
        .into(),
        "name" => json!(type_name).into(),
        "description" => json!(match def {
            TypeDef::Scalar(_, description) => description,
            TypeDef::Object(def) => &def.description,
            TypeDef::InputObject(def) => &def.description,
            TypeDef::Enum(def) => &def.description,
        })
        .into(),
        "fields" => match def {
            TypeDef::Object(object) => Value::List(
                object
                    .fields
                    .values()
                    .filter(|f| !f.name.starts_with("__"))
                    .map(Node::Field)
                    .collect(),
            ),
            _ => Json::Null.into(),
        },
        "inputFields" => match def {
            TypeDef::InputObject(input) => {
                Value::List(input.fields.values().map(Node::Input).collect())
            }
            _ => Json::Null.into(),
        },
        "enumValues" => match def {
            TypeDef::Enum(enumeration) => Value::List(
                enumeration
                    .values
                    .iter()
                    .map(|s| Node::EnumValue(s))
                    .collect(),
            ),
            _ => Json::Null.into(),
        },
        "interfaces" if matches!(def, TypeDef::Object(_)) => Value::List(Vec::new()),
        "isOneOf" if matches!(def, TypeDef::InputObject(_)) => json!(false).into(),
        _ => Json::Null.into(),
    }
}
