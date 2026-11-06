//! The tools a script can call, written as compact TypeScript-like signatures for the model, made from their JSON schemas.
//!
//! The same tools always give the same text, so the description of the `script` tool stays the same from one request to the next and the provider's prompt cache keeps it.

use serde_json::Value;

/// How deep objects nested in arguments are spelled out before they are shown as `object`.
const MAX_DEPTH: usize = 3;

/// The signature of the tool `name`, whose arguments follow `schema` and whose calls resolve to `returns`, such as `tools.read(args: { path: string }): Promise<string>`.
pub(crate) fn signature(name: &str, schema: &Value, returns: Option<&str>) -> String {
    let access = if is_identifier(name) {
        format!("tools.{name}")
    } else {
        format!("tools[{}]", Value::String(name.to_owned()))
    };
    let args = type_of(schema, 0);
    let optional = required(schema).is_empty();
    let parameter = match (args.as_str(), optional) {
        ("{}" | "object", _) => "args?: object".to_owned(),
        (_, true) => format!("args?: {args}"),
        (_, false) => format!("args: {args}"),
    };
    format!(
        "{access}({parameter}): Promise<{}>",
        returns.unwrap_or("string")
    )
}

/// The names of the properties `schema` requires.
fn required(schema: &Value) -> Vec<&str> {
    schema["required"]
        .as_array()
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// Whether `name` can follow a dot in JavaScript.
fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_' || first == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// The TypeScript type that `schema` describes, at `depth` objects deep.
pub(crate) fn type_of(schema: &Value, depth: usize) -> String {
    if let Some(values) = schema["enum"].as_array() {
        return union(values.iter().map(Value::to_string).collect());
    }
    if let Some(value) = schema.get("const") {
        return value.to_string();
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(choices) = schema[key].as_array() {
            return union(
                choices
                    .iter()
                    .map(|choice| type_of(choice, depth))
                    .collect(),
            );
        }
    }
    match &schema["type"] {
        Value::String(kind) => named(kind, schema, depth),
        Value::Array(kinds) => union(
            kinds
                .iter()
                .filter_map(Value::as_str)
                .map(|kind| named(kind, schema, depth))
                .collect(),
        ),
        _ if schema.get("properties").is_some() => named("object", schema, depth),
        _ => "unknown".to_owned(),
    }
}

/// The TypeScript type of the JSON schema type `kind`, with the rest of `schema` filling in arrays and objects, at `depth` objects deep.
fn named(kind: &str, schema: &Value, depth: usize) -> String {
    match kind {
        "string" => "string".to_owned(),
        "integer" | "number" => "number".to_owned(),
        "boolean" => "boolean".to_owned(),
        "null" => "null".to_owned(),
        "array" => match schema.get("items") {
            Some(items) => {
                let item = type_of(items, depth);
                if item.contains(" | ") {
                    format!("({item})[]")
                } else {
                    format!("{item}[]")
                }
            }
            None => "unknown[]".to_owned(),
        },
        "object" => object(schema, depth),
        _ => "unknown".to_owned(),
    }
}

/// The TypeScript type of the object `schema` describes, at `depth` objects deep: its properties, those not required marked optional, or a record when it only describes its values.
fn object(schema: &Value, depth: usize) -> String {
    let Some(properties) = schema["properties"].as_object().filter(|p| !p.is_empty()) else {
        return match schema.get("additionalProperties") {
            Some(values) if values.is_object() => {
                format!("Record<string, {}>", type_of(values, depth + 1))
            }
            _ => "object".to_owned(),
        };
    };
    if depth >= MAX_DEPTH {
        return "object".to_owned();
    }
    let required = required(schema);
    let fields: Vec<String> = properties
        .iter()
        .map(|(key, value)| {
            let key_shown = if is_identifier(key) {
                key.clone()
            } else {
                Value::String(key.clone()).to_string()
            };
            let mark = if required.contains(&key.as_str()) {
                ""
            } else {
                "?"
            };
            format!("{key_shown}{mark}: {}", type_of(value, depth + 1))
        })
        .collect();
    format!("{{ {} }}", fields.join("; "))
}

/// `types` joined as a union, each once, in the order given.
fn union(types: Vec<String>) -> String {
    let mut seen: Vec<String> = Vec::new();
    for kind in types {
        if !seen.contains(&kind) {
            seen.push(kind);
        }
    }
    match seen.len() {
        0 => "unknown".to_owned(),
        _ => seen.join(" | "),
    }
}

#[cfg(test)]
/// Tests for the signatures.
mod tests {
    use serde_json::json;

    use super::*;

    /// Required and optional properties, enums, arrays and nested objects are spelled out, and the return type defaults to text.
    #[test]
    fn writes_arguments_as_typescript() {
        let schema = json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string" },
                "limit": { "type": "integer" },
                "mode": { "type": "string", "enum": ["a", "b"] },
                "files": { "type": "array", "items": { "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] } },
                "maybe": { "type": ["string", "null"] }
            },
            "required": ["pattern"]
        });
        assert_eq!(
            signature("grep", &schema, None),
            "tools.grep(args: { files?: { path: string }[]; limit?: number; maybe?: string | null; mode?: \"a\" | \"b\"; pattern: string }): Promise<string>"
        );
    }

    /// A tool without required arguments takes them optionally, a name that is not an identifier is quoted, and a structured result is shown.
    #[test]
    fn optional_arguments_quoted_names_and_results() {
        let schema = json!({ "type": "object", "properties": { "a": { "type": "boolean" } } });
        assert_eq!(
            signature("mcp-server.tool", &schema, Some("{ ok: boolean }")),
            "tools[\"mcp-server.tool\"](args?: { a?: boolean }): Promise<{ ok: boolean }>"
        );
        assert_eq!(
            signature("x", &json!({ "type": "object" }), None),
            "tools.x(args?: object): Promise<string>"
        );
    }

    /// Objects nested deeper than the limit are only named.
    #[test]
    fn deep_objects_are_cut_short() {
        let deep = json!({ "type": "object", "properties": { "a": { "type": "object", "properties": { "b": { "type": "object", "properties": { "c": { "type": "object", "properties": { "d": { "type": "string" } } } } } } } } });
        assert_eq!(type_of(&deep, 0), "{ a?: { b?: { c?: object } } }");
    }
}
