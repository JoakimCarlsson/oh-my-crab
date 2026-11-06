//! A small JSON Schema checker, for the arguments of tool calls and the structured results of subagents.
//!
//! It covers the keywords results are usually described with: `type`, `enum`, `const`, `properties`, `required`, `additionalProperties`, `items`, `minItems`, `maxItems`, `minLength`, `maxLength`, `minimum`, `maximum`, `exclusiveMinimum`, `exclusiveMaximum`, `allOf`, `anyOf`, `oneOf` and `not`. Other keywords, such as `$ref`, `pattern` and `format`, are accepted and not checked.

use serde_json::{Map, Value};

/// Checks that `schema` can describe a result: a JSON object or a boolean, whose `type`, when it has one, names types.
///
/// # Errors
///
/// Returns why the schema cannot be used.
pub fn check_schema(schema: &Value) -> Result<(), String> {
    let rules = match schema {
        Value::Bool(_) => return Ok(()),
        Value::Object(rules) => rules,
        _ => return Err("a JSON Schema must be an object or a boolean".to_owned()),
    };
    match rules.get("type") {
        None | Some(Value::String(_)) => Ok(()),
        Some(Value::Array(names)) if names.iter().all(Value::is_string) => Ok(()),
        Some(_) => Err("`type` must be a type name or a list of them".to_owned()),
    }
}

/// The ways `value` breaks `schema`, each starting with where in `value` it is, counted from `root`; empty when the value fits.
pub fn violations(schema: &Value, value: &Value, root: &str) -> Vec<String> {
    let mut found = Vec::new();
    walk(schema, value, root, &mut found);
    found
}

/// Whether `value` fits `schema`.
pub fn fits(schema: &Value, value: &Value) -> bool {
    violations(schema, value, "").is_empty()
}

/// Adds to `found` the ways `value`, found at `at`, breaks `schema`.
fn walk(schema: &Value, value: &Value, at: &str, found: &mut Vec<String>) {
    let rules = match schema {
        Value::Bool(false) => {
            found.push(format!("{at}: no value is allowed here"));
            return;
        }
        Value::Object(rules) => rules,
        _ => return,
    };
    if let Some(expected) = rules.get("type")
        && !type_fits(expected, value)
    {
        found.push(format!(
            "{at}: expected {}, got {}",
            type_names(expected),
            type_of(value)
        ));
        return;
    }
    if let Some(Value::Array(allowed)) = rules.get("enum")
        && !allowed.contains(value)
    {
        found.push(format!(
            "{at}: must be one of {}",
            Value::Array(allowed.clone())
        ));
    }
    if let Some(constant) = rules.get("const")
        && constant != value
    {
        found.push(format!("{at}: must be {constant}"));
    }
    match value {
        Value::Object(fields) => object(rules, fields, at, found),
        Value::Array(items) => array(rules, items, at, found),
        Value::String(text) => string(rules, text, at, found),
        Value::Number(_) => number(rules, value, at, found),
        _ => {}
    }
    combined(rules, value, at, found);
}

/// Whether `value` is of the type, or one of the types, `expected` names.
fn type_fits(expected: &Value, value: &Value) -> bool {
    match expected {
        Value::String(name) => is_type(name, value),
        Value::Array(names) => names
            .iter()
            .filter_map(Value::as_str)
            .any(|name| is_type(name, value)),
        _ => true,
    }
}

/// Whether `value` is of the type called `name`; an unknown name fits anything.
pub(crate) fn is_type(name: &str, value: &Value) -> bool {
    match name {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => {
            value.is_i64() || value.is_u64() || value.as_f64().is_some_and(|n| n.fract() == 0.0)
        }
        _ => true,
    }
}

/// The type or types `expected` names, for a message.
fn type_names(expected: &Value) -> String {
    match expected {
        Value::String(name) => name.clone(),
        Value::Array(names) => names
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" or "),
        other => other.to_string(),
    }
}

/// The type of `value`, for a message.
fn type_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Adds to `found` the ways the object `fields`, found at `at`, breaks the object keywords of `rules`.
fn object(
    rules: &Map<String, Value>,
    fields: &Map<String, Value>,
    at: &str,
    found: &mut Vec<String>,
) {
    let properties = rules.get("properties").and_then(Value::as_object);
    if let Some(Value::Array(required)) = rules.get("required") {
        for name in required.iter().filter_map(Value::as_str) {
            if !fields.contains_key(name) {
                found.push(format!("{at}: missing required property `{name}`"));
            }
        }
    }
    for (name, field) in fields {
        let place = format!("{at}.{name}");
        match properties.and_then(|known| known.get(name)) {
            Some(schema) => walk(schema, field, &place, found),
            None => match rules.get("additionalProperties") {
                Some(Value::Bool(false)) => {
                    found.push(format!("{at}: unexpected property `{name}`"));
                }
                Some(schema @ Value::Object(_)) => walk(schema, field, &place, found),
                _ => {}
            },
        }
    }
}

/// Adds to `found` the ways the array `items`, found at `at`, breaks the array keywords of `rules`.
fn array(rules: &Map<String, Value>, items: &[Value], at: &str, found: &mut Vec<String>) {
    if let Some(least) = rules.get("minItems").and_then(Value::as_u64)
        && (items.len() as u64) < least
    {
        found.push(format!(
            "{at}: needs at least {least} items, has {}",
            items.len()
        ));
    }
    if let Some(most) = rules.get("maxItems").and_then(Value::as_u64)
        && (items.len() as u64) > most
    {
        found.push(format!(
            "{at}: allows at most {most} items, has {}",
            items.len()
        ));
    }
    if let Some(schema) = rules.get("items").filter(|schema| !schema.is_array()) {
        for (index, item) in items.iter().enumerate() {
            walk(schema, item, &format!("{at}[{index}]"), found);
        }
    }
}

/// Adds to `found` the ways the string `text`, found at `at`, breaks the string keywords of `rules`.
fn string(rules: &Map<String, Value>, text: &str, at: &str, found: &mut Vec<String>) {
    let length = text.chars().count() as u64;
    if let Some(least) = rules.get("minLength").and_then(Value::as_u64)
        && length < least
    {
        found.push(format!("{at}: needs at least {least} characters"));
    }
    if let Some(most) = rules.get("maxLength").and_then(Value::as_u64)
        && length > most
    {
        found.push(format!("{at}: allows at most {most} characters"));
    }
}

/// Adds to `found` the ways the number `value`, found at `at`, breaks the number keywords of `rules`.
fn number(rules: &Map<String, Value>, value: &Value, at: &str, found: &mut Vec<String>) {
    let Some(number) = value.as_f64() else {
        return;
    };
    let bound = |key: &str| rules.get(key).and_then(Value::as_f64);
    if let Some(least) = bound("minimum")
        && number < least
    {
        found.push(format!("{at}: must be at least {least}"));
    }
    if let Some(most) = bound("maximum")
        && number > most
    {
        found.push(format!("{at}: must be at most {most}"));
    }
    if let Some(floor) = bound("exclusiveMinimum")
        && number <= floor
    {
        found.push(format!("{at}: must be greater than {floor}"));
    }
    if let Some(ceiling) = bound("exclusiveMaximum")
        && number >= ceiling
    {
        found.push(format!("{at}: must be less than {ceiling}"));
    }
}

/// Adds to `found` the ways `value`, found at `at`, breaks the `allOf`, `anyOf`, `oneOf` and `not` keywords of `rules`.
fn combined(rules: &Map<String, Value>, value: &Value, at: &str, found: &mut Vec<String>) {
    let list = |key: &str| {
        rules
            .get(key)
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
    };
    for schema in list("allOf") {
        walk(schema, value, at, found);
    }
    let any = list("anyOf");
    if !any.is_empty() && !any.iter().any(|schema| fits(schema, value)) {
        found.push(format!("{at}: matches none of the `anyOf` alternatives"));
    }
    let one = list("oneOf");
    if !one.is_empty() {
        let matched = one.iter().filter(|schema| fits(schema, value)).count();
        if matched != 1 {
            found.push(format!(
                "{at}: must match exactly one `oneOf` alternative, matches {matched}"
            ));
        }
    }
    if let Some(schema) = rules.get("not")
        && fits(schema, value)
    {
        found.push(format!("{at}: matches the schema under `not`"));
    }
}

#[cfg(test)]
/// Tests for the schema checker.
mod tests {
    use serde_json::json;

    use super::*;

    /// A schema for a list of findings, used by several tests.
    fn findings() -> Value {
        json!({
            "type": "object",
            "properties": {
                "files": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                "count": { "type": "integer", "minimum": 0 },
                "level": { "enum": ["low", "high"] }
            },
            "required": ["files", "count"],
            "additionalProperties": false
        })
    }

    /// A value that fits has no violations.
    #[test]
    fn a_fitting_value_passes() {
        let value = json!({ "files": ["a.rs"], "count": 2, "level": "low" });
        assert!(violations(&findings(), &value, "result").is_empty());
    }

    /// Each broken rule is reported with where it is.
    #[test]
    fn violations_name_their_place() {
        let value = json!({ "files": [3], "count": -1.5, "level": "mid", "extra": true });
        let found = violations(&findings(), &value, "result");
        assert!(found.contains(&"result.files[0]: expected string, got number".to_owned()));
        assert!(found.contains(&"result.count: expected integer, got number".to_owned()));
        assert!(
            found
                .iter()
                .any(|v| v.starts_with("result.level: must be one of"))
        );
        assert!(found.contains(&"result: unexpected property `extra`".to_owned()));
        let missing = violations(&findings(), &json!({ "files": [] }), "result");
        assert!(missing.contains(&"result: missing required property `count`".to_owned()));
        assert!(missing.contains(&"result.files: needs at least 1 items, has 0".to_owned()));
    }

    /// The combining keywords and type lists are honoured.
    #[test]
    fn combinators_and_type_lists() {
        let schema = json!({ "anyOf": [{ "type": "string" }, { "type": "null" }] });
        assert!(fits(&schema, &json!(null)));
        assert!(!fits(&schema, &json!(1)));
        let schema = json!({ "type": ["string", "integer"], "not": { "const": "no" } });
        assert!(fits(&schema, &json!(3)));
        assert!(!fits(&schema, &json!("no")));
        assert!(!fits(&json!({ "oneOf": [{}, {}] }), &json!(1)));
    }

    /// Only objects and booleans are schemas.
    #[test]
    fn schemas_are_checked() {
        assert!(check_schema(&json!({ "type": "object" })).is_ok());
        assert!(check_schema(&json!(true)).is_ok());
        assert!(check_schema(&json!("object")).is_err());
        assert!(check_schema(&json!({ "type": 3 })).is_err());
    }
}
