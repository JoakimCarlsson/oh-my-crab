//! Lenient reading of tool arguments: the common ways a model gets an argument's type wrong are put right against the tool's schema before the arguments are checked.
//!
//! A string holding a number or a boolean becomes that number or boolean where the schema wants one, a number or boolean becomes a string where the schema wants text, a string holding JSON becomes the object or array it encodes where the schema wants one, a single value becomes a one-element list where the schema wants a list, arguments that are themselves one item of the tool's required list of objects become that list, a whole number written with a fraction becomes an integer where the schema wants an integer, and `null` given for an optional property that does not take it is left out. The rules apply all the way down, to the properties of objects and the items of arrays.

use serde_json::{Map, Number, Value};

use crate::ToolError;
use crate::schema::{fits, is_type, violations};

/// The name the top of the arguments goes by in the reasons a call is turned away.
const ROOT: &str = "args";

/// The arguments `args`, put right against `schema` and checked against it.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] naming each place in the arguments that still breaks the schema, and what was expected there, along with the arguments as received.
pub(crate) fn prepare(schema: &Value, args: Value) -> Result<Value, ToolError> {
    let coerced = coerce(schema, args.clone());
    let broken = violations(schema, &coerced, ROOT);
    if broken.is_empty() {
        return Ok(coerced);
    }
    Err(ToolError::InvalidArguments(format!(
        "\n- {}\nreceived: {}",
        broken.join("\n- "),
        shortened(args)
    )))
}

/// Longest string, in characters, echoed whole in the arguments a refusal
/// quotes back.
const ECHO_CHARS: usize = 200;

/// `value` with every string longer than [`ECHO_CHARS`] characters cut to
/// its start and a note of its length, so that a refusal does not repeat a
/// whole file the model sent.
fn shortened(value: Value) -> Value {
    match value {
        Value::String(text) => match text.char_indices().nth(ECHO_CHARS) {
            Some((end, _)) => Value::String(format!(
                "{}… ({} chars)",
                &text[..end],
                text.chars().count()
            )),
            None => Value::String(text),
        },
        Value::Array(items) => Value::Array(items.into_iter().map(shortened).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .map(|(key, value)| (key, shortened(value)))
                .collect(),
        ),
        other => other,
    }
}

/// `value` put right against `schema`, as far as the rules of this module go; anything it cannot put right is left as it is, for the check to report.
pub(crate) fn coerce(schema: &Value, value: Value) -> Value {
    let Value::Object(rules) = schema else {
        return value;
    };
    let value = chosen_alternative(rules, value);
    let value = list(rules, "allOf")
        .iter()
        .fold(value, |value, part| coerce(part, value));
    let wanted = wanted_types(rules);
    let value = if wanted.is_empty() || wanted.iter().any(|name| exactly(name, &value)) {
        value
    } else {
        converted(&wanted, value)
    };
    match value {
        Value::Object(fields) => Value::Object(coerce_fields(rules, wrapped(rules, fields))),
        Value::Array(items) => Value::Array(coerce_items(rules, items)),
        other => other,
    }
}

/// The properties `fields` of an object, or, when they hold one item of a list the object must have and does not, the object with that list and the item in it: a model asked for `{"files": [{...}]}` that sends only the `{...}` of its one file means the list, and one that sends `{"path": ..., "op": ..., "line": ...}` for `{"path": ..., "ops": [{...}]}` means the one operation.
///
/// The properties the object's own schema names stay on the object; the others make the item. The object is changed only when exactly one of its required properties is missing, that property is a list of objects, and the item fits what the list holds once it is put right; otherwise it is left as it is, for the check to report.
fn wrapped(rules: &Map<String, Value>, fields: Map<String, Value>) -> Map<String, Value> {
    let missing: Vec<&str> = list(rules, "required")
        .iter()
        .filter_map(Value::as_str)
        .filter(|name| !fields.contains_key(*name))
        .collect();
    let [name] = missing.as_slice() else {
        return fields;
    };
    let properties = rules.get("properties").and_then(Value::as_object);
    let item = properties
        .and_then(|properties| properties.get(*name))
        .filter(|property| property["type"] == "array")
        .map(|property| &property["items"])
        .filter(|item| item["type"] == "object");
    let Some(item) = item else {
        return fields;
    };
    let own = |key: &String| properties.is_some_and(|properties| properties.contains_key(key));
    let (mut kept, inner): (Map<String, Value>, Map<String, Value>) =
        fields.clone().into_iter().partition(|(key, _)| own(key));
    let one = coerce(item, Value::Object(inner));
    if one.as_object().is_some_and(|one| !one.is_empty()) && fits(item, &one) {
        kept.insert((*name).to_owned(), Value::Array(vec![one]));
        kept
    } else {
        fields
    }
}

/// The schemas listed under `key` in `rules`; none when the key is missing or not a list.
fn list<'a>(rules: &'a Map<String, Value>, key: &str) -> &'a [Value] {
    rules
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

/// `value` as the first `anyOf` or `oneOf` alternative of `rules` it can be put right to fit, or unchanged when it already fits one or fits none even when put right.
fn chosen_alternative(rules: &Map<String, Value>, value: Value) -> Value {
    let alternatives: Vec<&Value> = list(rules, "anyOf")
        .iter()
        .chain(list(rules, "oneOf"))
        .collect();
    if alternatives.is_empty() || alternatives.iter().any(|schema| fits(schema, &value)) {
        return value;
    }
    alternatives
        .iter()
        .map(|schema| (schema, coerce(schema, value.clone())))
        .find(|(schema, coerced)| fits(schema, coerced))
        .map_or(value, |(_, coerced)| coerced)
}

/// The type names `rules` asks for: its `type`, or `object` or `array` when it has none and describes properties or items.
fn wanted_types(rules: &Map<String, Value>) -> Vec<&str> {
    match rules.get("type") {
        Some(Value::String(name)) => vec![name.as_str()],
        Some(Value::Array(names)) => names.iter().filter_map(Value::as_str).collect(),
        _ if rules.contains_key("properties") => vec!["object"],
        _ if rules.contains_key("items") => vec!["array"],
        _ => Vec::new(),
    }
}

/// Whether `value` is of the type called `name` as a strict reader takes it, where an integer is only a number written without a fraction.
fn exactly(name: &str, value: &Value) -> bool {
    match name {
        "integer" => value.is_i64() || value.is_u64(),
        _ => is_type(name, value),
    }
}

/// `value` converted to the first of the types `wanted` it converts to, or unchanged when it converts to none.
fn converted(wanted: &[&str], value: Value) -> Value {
    wanted
        .iter()
        .find_map(|name| converted_to(name, &value))
        .unwrap_or(value)
}

/// `value` converted to the type called `name`, when the conversion is one this module makes.
fn converted_to(name: &str, value: &Value) -> Option<Value> {
    match (name, value) {
        ("integer", Value::String(text)) => integer(text.trim()),
        ("integer", Value::Number(number)) => number.as_f64().and_then(whole),
        ("number", Value::String(text)) => number(text.trim()),
        ("boolean", Value::String(text)) => boolean(text.trim()),
        ("string", Value::Number(number)) => Some(Value::String(number.to_string())),
        ("string", Value::Bool(flag)) => Some(Value::String(flag.to_string())),
        ("object", Value::String(text)) => decoded(text).filter(Value::is_object),
        ("array", Value::String(text)) => decoded(text)
            .filter(Value::is_array)
            .or_else(|| Some(Value::Array(vec![value.clone()]))),
        ("array", Value::Null) => None,
        ("array", single) => Some(Value::Array(vec![single.clone()])),
        _ => None,
    }
}

/// The integer `text` writes, with or without a zero fraction.
fn integer(text: &str) -> Option<Value> {
    if let Ok(signed) = text.parse::<i64>() {
        return Some(Value::from(signed));
    }
    if let Ok(unsigned) = text.parse::<u64>() {
        return Some(Value::from(unsigned));
    }
    text.parse::<f64>().ok().and_then(whole)
}

/// `number` as an integer, when it is a whole number an integer can hold.
fn whole(number: f64) -> Option<Value> {
    let fits_i64 = number.fract() == 0.0 && number >= i64::MIN as f64 && number <= i64::MAX as f64;
    fits_i64.then(|| Value::from(number as i64))
}

/// The number `text` writes, kept an integer when it is written as one.
fn number(text: &str) -> Option<Value> {
    if let Ok(integer) = text.parse::<i64>() {
        return Some(Value::from(integer));
    }
    text.parse::<f64>()
        .ok()
        .and_then(Number::from_f64)
        .map(Value::Number)
}

/// The boolean `text` writes, in any case.
fn boolean(text: &str) -> Option<Value> {
    match text.to_ascii_lowercase().as_str() {
        "true" => Some(Value::Bool(true)),
        "false" => Some(Value::Bool(false)),
        _ => None,
    }
}

/// The JSON value `text` encodes, when it is JSON.
fn decoded(text: &str) -> Option<Value> {
    serde_json::from_str(text.trim()).ok()
}

/// The properties `fields` of an object, each put right against its schema in `rules`, with `null` left out where the property is optional and does not take it.
fn coerce_fields(rules: &Map<String, Value>, fields: Map<String, Value>) -> Map<String, Value> {
    let properties = rules.get("properties").and_then(Value::as_object);
    let required: Vec<&str> = list(rules, "required")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let extra = rules
        .get("additionalProperties")
        .filter(|schema| schema.is_object());
    fields
        .into_iter()
        .filter_map(|(name, field)| {
            let schema = properties.and_then(|known| known.get(&name)).or(extra);
            let Some(schema) = schema else {
                return Some((name, field));
            };
            let omitted =
                field.is_null() && !required.contains(&name.as_str()) && !fits(schema, &field);
            (!omitted).then(|| {
                let coerced = coerce(schema, field);
                (name, coerced)
            })
        })
        .collect()
}

/// The `items` of an array, each put right against the schema `rules` gives items in its place: the one schema for all of them, or the schema at its position in a list of them.
fn coerce_items(rules: &Map<String, Value>, items: Vec<Value>) -> Vec<Value> {
    let positional = list(rules, "prefixItems");
    let (positional, rest) = match rules.get("items") {
        Some(Value::Array(each)) => (each.as_slice(), rules.get("additionalItems")),
        other => (positional, other),
    };
    items
        .into_iter()
        .enumerate()
        .map(|(index, item)| match positional.get(index).or(rest) {
            Some(schema) => coerce(schema, item),
            None => item,
        })
        .collect()
}

#[cfg(test)]
/// Tests for putting arguments right.
mod tests {
    use serde_json::json;

    use super::*;

    /// The arguments of a tool with every kind of property, used by several tests.
    fn tool_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "limit": { "type": "integer", "minimum": 1 },
                "ratio": { "type": "number" },
                "dry_run": { "type": "boolean" },
                "paths": { "type": "array", "items": { "type": "string" } },
                "options": {
                    "type": "object",
                    "properties": { "depth": { "type": "integer" } }
                },
                "edits": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "line": { "type": "integer" },
                            "text": { "type": "string" }
                        },
                        "required": ["line"]
                    }
                }
            },
            "required": ["path"]
        })
    }

    /// Arguments that hold one item of the required list of objects get that list with the item in it, keeping the properties that are the object's own, and are left alone when what is left does not fit an item or the list is there.
    #[test]
    fn a_bare_item_becomes_the_required_list() {
        let schema = json!({
            "type": "object",
            "properties": {
                "files": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" },
                            "line": { "type": "integer" }
                        },
                        "required": ["path", "line"]
                    }
                }
            },
            "required": ["files"]
        });
        assert_eq!(
            prepare(&schema, json!({ "path": "a.rs", "line": "3" })).unwrap(),
            json!({ "files": [{ "path": "a.rs", "line": 3 }] })
        );
        assert!(prepare(&schema, json!({ "path": "a.rs" })).is_err());
        let nested = json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "ops": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": { "op": { "type": "string" }, "line": { "type": "integer" } },
                        "required": ["op"]
                    }
                }
            },
            "required": ["path", "ops"]
        });
        assert_eq!(
            prepare(
                &nested,
                json!({ "path": "a.rs", "op": "delete", "line": 3 })
            )
            .unwrap(),
            json!({ "path": "a.rs", "ops": [{ "op": "delete", "line": 3 }] })
        );
        assert!(prepare(&nested, json!({ "path": "a.rs" })).is_err());
        assert_eq!(
            prepare(&schema, json!({ "files": [{ "path": "a.rs", "line": 3 }] })).unwrap(),
            json!({ "files": [{ "path": "a.rs", "line": 3 }] })
        );
    }

    /// Numbers and booleans written as strings become numbers and booleans.
    #[test]
    fn strings_become_numbers_and_booleans() {
        let args = json!({ "path": "a", "limit": "20", "ratio": " 0.5 ", "dry_run": "TRUE" });
        assert_eq!(
            coerce(&tool_schema(), args),
            json!({ "path": "a", "limit": 20, "ratio": 0.5, "dry_run": true })
        );
    }

    /// Whole numbers written with a fraction become integers, and numbers become text where text is wanted.
    #[test]
    fn numbers_take_the_wanted_form() {
        let args = json!({ "path": 7, "limit": 5.0 });
        assert_eq!(
            coerce(&tool_schema(), args),
            json!({ "path": "7", "limit": 5 })
        );
    }

    /// A single value becomes a list, and a list written as JSON text becomes the list.
    #[test]
    fn single_values_and_encoded_lists_become_lists() {
        let schema = tool_schema();
        assert_eq!(
            coerce(&schema, json!({ "path": "a", "paths": "src/lib.rs" }))["paths"],
            json!(["src/lib.rs"])
        );
        assert_eq!(
            coerce(&schema, json!({ "path": "a", "paths": "[\"x\", \"y\"]" }))["paths"],
            json!(["x", "y"])
        );
    }

    /// Objects written as JSON text become objects, put right in turn, and the whole arguments may be text too.
    #[test]
    fn encoded_objects_become_objects() {
        let schema = tool_schema();
        assert_eq!(
            coerce(
                &schema,
                json!({ "path": "a", "options": "{\"depth\": \"2\"}" })
            )["options"],
            json!({ "depth": 2 })
        );
        assert_eq!(
            coerce(&schema, json!("{\"path\": \"a\", \"limit\": \"3\"}")),
            json!({ "path": "a", "limit": 3 })
        );
    }

    /// Items of lists and properties of nested objects are put right.
    #[test]
    fn nested_values_are_put_right() {
        let args = json!({ "path": "a", "edits": [{ "line": "4", "text": 12 }] });
        assert_eq!(
            coerce(&tool_schema(), args)["edits"],
            json!([{ "line": 4, "text": "12" }])
        );
        let single = json!({ "path": "a", "edits": { "line": "9" } });
        assert_eq!(
            coerce(&tool_schema(), single)["edits"],
            json!([{ "line": 9 }])
        );
    }

    /// `null` for an optional property is left out, and kept where the property takes it or is required.
    #[test]
    fn null_optional_properties_are_left_out() {
        let schema = tool_schema();
        assert_eq!(
            coerce(
                &schema,
                json!({ "path": "a", "limit": null, "unknown": null })
            ),
            json!({ "path": "a", "unknown": null })
        );
        assert_eq!(
            coerce(&schema, json!({ "path": null })),
            json!({ "path": null })
        );
        let nullable = json!({ "properties": { "note": { "type": ["string", "null"] } } });
        assert_eq!(
            coerce(&nullable, json!({ "note": null })),
            json!({ "note": null })
        );
    }

    /// Values that cannot be put right are left alone.
    #[test]
    fn hopeless_values_are_left_alone() {
        let args = json!({ "path": "a", "limit": "many", "dry_run": "maybe", "options": "{" });
        assert_eq!(coerce(&tool_schema(), args.clone()), args);
    }

    /// The first alternative a value can be put right to fit is taken.
    #[test]
    fn alternatives_are_tried_in_order() {
        let schema = json!({ "anyOf": [{ "type": "integer" }, { "type": "boolean" }] });
        assert_eq!(coerce(&schema, json!("false")), json!(false));
        assert_eq!(coerce(&schema, json!("8")), json!(8));
        assert_eq!(coerce(&schema, json!(3)), json!(3));
    }

    /// Fitting arguments come back put right, and others are turned away naming each bad place and what it wanted.
    #[test]
    fn prepare_checks_after_putting_right() {
        let schema = tool_schema();
        assert_eq!(
            prepare(&schema, json!({ "path": "a", "limit": "2" })).unwrap(),
            json!({ "path": "a", "limit": 2 })
        );
        let Err(ToolError::InvalidArguments(message)) = prepare(
            &schema,
            json!({ "limit": "many", "edits": [{ "text": "x" }] }),
        ) else {
            panic!("expected the arguments to be turned away");
        };
        assert!(message.contains("- args: missing required property `path`"));
        assert!(message.contains("- args.limit: expected integer, got string"));
        assert!(message.contains("- args.edits[0]: missing required property `line`"));
        assert!(message.contains("received: {"));
    }

    /// Long strings in the arguments quoted back are cut to their start and
    /// length; short ones are quoted whole.
    #[test]
    fn refusals_cut_long_arguments() {
        let schema = tool_schema();
        let long = "é".repeat(5000);
        let Err(ToolError::InvalidArguments(message)) = prepare(
            &schema,
            json!({ "limit": "many", "edits": [{ "text": long }], "note": "short" }),
        ) else {
            panic!("expected the arguments to be turned away");
        };
        assert!(
            message.contains(&format!("\"{}… (5000 chars)\"", "é".repeat(200))),
            "{message}"
        );
        assert!(!message.contains(&"é".repeat(201)), "{message}");
        assert!(message.contains("\"short\""), "{message}");
        assert!(message.len() < 1000, "{}", message.len());
    }

    /// A schema that says nothing lets anything through untouched.
    #[test]
    fn empty_schemas_let_anything_through() {
        assert_eq!(
            prepare(&Value::Null, json!({ "a": null })).unwrap(),
            json!({ "a": null })
        );
        assert_eq!(prepare(&json!({}), json!("x")).unwrap(), json!("x"));
    }
}
