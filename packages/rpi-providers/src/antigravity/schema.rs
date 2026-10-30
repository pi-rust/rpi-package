//! The Gemini protobuf schema subset used by the original Pi request conversion.
use super::json::{describe, entries, finite_number, set, string, truthy};
use indexmap::IndexSet;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Options {
    pub move_numeric_constraints_to_description: bool,
    pub claude_validated: bool,
}
fn merge_all_of(input: &Map<String, Value>, hints: &mut Vec<String>) -> Map<String, Value> {
    let Some(branches) = input.get("allOf").and_then(Value::as_array) else {
        return input.clone();
    };
    let mut base = input.clone();
    base.shift_remove("allOf");
    let mut properties = Map::new();
    let mut required = IndexSet::new();
    let mut descriptions = Vec::new();
    let mut metadata = Map::new();
    let mut unambiguous = !branches.is_empty();
    for branch in std::iter::once(Value::Object(base.clone())).chain(branches.iter().cloned()) {
        let Some(object) = branch.as_object() else {
            unambiguous = false;
            break;
        };
        if object
            .get("type")
            .is_some_and(|value| string(value).to_uppercase() != "OBJECT")
            || object
                .get("properties")
                .is_some_and(|value| !value.is_object())
        {
            unambiguous = false;
            break;
        }
        for (key, value) in entries(&branch) {
            if !matches!(key.as_str(), "properties" | "required" | "description")
                && !metadata.contains_key(&key)
            {
                metadata.insert(key, value.clone());
            }
        }
        if let Some(description) = object.get("description").and_then(Value::as_str) {
            descriptions.push(description.to_owned());
        }
        for (name, value) in entries(&branch["properties"]) {
            if name == "__proto__" {
                continue;
            }
            if properties.get(&name).is_some_and(|prior| prior != value) {
                unambiguous = false;
            }
            properties.insert(name, value.clone());
        }
        if let Some(names) = object.get("required").and_then(Value::as_array) {
            required.extend(names.iter().filter_map(Value::as_str).map(str::to_owned));
        }
    }
    if unambiguous {
        set(&mut metadata, "type", "object");
        set(&mut metadata, "properties", Value::Object(properties));
        if !required.is_empty() {
            set(
                &mut metadata,
                "required",
                json!(required.into_iter().collect::<Vec<_>>()),
            );
        }
        if !descriptions.is_empty() {
            set(&mut metadata, "description", descriptions.join(" "));
        }
        return metadata;
    }
    hints.push("allOf: using the first branch; other intersection constraints omitted.".into());
    let description = base
        .get("description")
        .filter(|value| truthy(value))
        .cloned();
    if let Some(first) = branches.first().and_then(Value::as_object) {
        base.extend(first.clone());
    }
    if let Some(description) = description {
        set(&mut base, "description", description);
    }
    base
}
fn type_name(branch: &Map<String, Value>) -> String {
    if branch.get("properties").is_some_and(truthy) {
        "object".into()
    } else if branch.get("items").is_some_and(truthy) {
        "array".into()
    } else {
        string(branch.get("type").unwrap_or(&Value::Null)).to_lowercase()
    }
}
fn flatten_union(input: Map<String, Value>, hints: &mut Vec<String>) -> Map<String, Value> {
    let Some(any) = input
        .get("anyOf")
        .and_then(Value::as_array)
        .filter(|any| !any.is_empty())
        .cloned()
    else {
        return input;
    };
    let mut base = input;
    base.shift_remove("anyOf");
    let branches: Vec<_> = any.iter().filter_map(Value::as_object).collect();
    if branches.is_empty() {
        return base;
    }
    if branches.len() == any.len()
        && branches.iter().all(|branch| {
            branch
                .get("enum")
                .and_then(Value::as_array)
                .is_some_and(|values| !values.is_empty() && values.iter().all(Value::is_string))
                && branch.get("type").map_or(true, |value| value == "STRING")
                && !branch.get("properties").is_some_and(truthy)
                && !branch.get("items").is_some_and(truthy)
        })
    {
        let values: IndexSet<_> = branches
            .iter()
            .flat_map(|branch| branch["enum"].as_array().unwrap())
            .map(|value| value.as_str().unwrap())
            .collect();
        set(&mut base, "type", "STRING");
        set(
            &mut base,
            "enum",
            json!(values.into_iter().collect::<Vec<_>>()),
        );
        return base;
    }
    let score = |branch: &Map<String, Value>| match type_name(branch).as_str() {
        "object" => 3,
        "array" => 2,
        "null" => 0,
        _ => 1,
    };
    let mut selected = branches[0];
    for branch in &branches[1..] {
        if score(branch) > score(selected) {
            selected = branch;
        }
    }
    let description = base
        .get("description")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    base.extend(selected.clone());
    if let Some(description) = description {
        let text = match selected
            .get("description")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && *value != description)
        {
            Some(other) => format!("{description} ({other})"),
            None => description,
        };
        set(&mut base, "description", text);
    }
    if branches.len() > 1 {
        let types: IndexSet<_> = branches.iter().map(|branch| type_name(branch)).collect();
        hints.push(format!(
            "Accepts: {}",
            types.into_iter().collect::<Vec<_>>().join(" | ")
        ));
    }
    base
}
fn numeric(key: &str) -> bool {
    matches!(
        key,
        "minLength"
            | "maxLength"
            | "minItems"
            | "maxItems"
            | "minProperties"
            | "maxProperties"
            | "minimum"
            | "maximum"
            | "exclusiveMinimum"
            | "exclusiveMaximum"
            | "multipleOf"
    )
}
fn allowed(key: &str) -> bool {
    matches!(
        key,
        "type"
            | "properties"
            | "items"
            | "required"
            | "description"
            | "format"
            | "pattern"
            | "title"
            | "nullable"
            | "propertyOrdering"
            | "example"
            | "default"
            | "enum"
            | "anyOf"
            | "minimum"
            | "maximum"
            | "minLength"
            | "maxLength"
            | "minItems"
            | "maxItems"
            | "minProperties"
            | "maxProperties"
    )
}

fn type_name_value(value: &Value) -> Option<&str> {
    match value {
        Value::String(value) => Some(value),
        Value::Array(values) => values
            .iter()
            .filter_map(Value::as_str)
            .find(|value| !value.eq_ignore_ascii_case("null")),
        _ => None,
    }
}

pub fn to_gemini_schema(schema: &Value, options: Options) -> Value {
    let Some(schema) = schema.as_object() else {
        return schema.clone();
    };
    let mut hints = Vec::new();
    let input = Value::Object(merge_all_of(schema, &mut hints));
    let property_names: IndexSet<_> = entries(&input["properties"])
        .into_iter()
        .map(|(key, _)| key)
        .collect();
    let mut result = Map::new();
    for (key, value) in entries(&input) {
        if options.claude_validated {
            if key == "title" {
                continue;
            }
            if key == "additionalProperties" && value == false {
                hints.push("No extra properties allowed".into());
            }
            if matches!(
                key.as_str(),
                "minLength"
                    | "maxLength"
                    | "minItems"
                    | "maxItems"
                    | "pattern"
                    | "format"
                    | "default"
            ) {
                if value.is_string() || value.is_number() || value.is_boolean() {
                    hints.push(format!("{key}: {}", string(value)));
                }
                continue;
            }
        }
        if key == "const" {
            if value.is_string() {
                set(&mut result, "enum", json!([value]));
            } else {
                hints.push(format!("Must be {}.", describe(value)));
            }
            continue;
        }
        if numeric(&key) {
            let exclusive = matches!(key.as_str(), "exclusiveMinimum" | "exclusiveMaximum");
            if exclusive
                && !options.move_numeric_constraints_to_description
                && string(&input["type"]).to_uppercase() == "INTEGER"
            {
                if let Some(value) = value.as_f64() {
                    let lower = key == "exclusiveMinimum";
                    let bound = if lower {
                        value.floor() + 1.0
                    } else {
                        value.ceil() - 1.0
                    };
                    let inclusive = if lower { "minimum" } else { "maximum" };
                    if bound.is_finite() && bound.abs() <= 9007199254740991.0 {
                        let bounded = input[inclusive].as_f64().map_or(bound, |existing| {
                            if lower {
                                existing.max(bound)
                            } else {
                                existing.min(bound)
                            }
                        });
                        set(&mut result, inclusive, finite_number(bounded));
                        continue;
                    }
                }
            }
            if options.move_numeric_constraints_to_description || exclusive || key == "multipleOf" {
                if value.is_string() || value.is_number() || value.is_boolean() {
                    hints.push(format!("{key}: {}", string(value)));
                }
                continue;
            }
        }
        if key == "oneOf" {
            if let Some(values) = value.as_array() {
                if !input["anyOf"].is_array() {
                    set(
                        &mut result,
                        "anyOf",
                        Value::Array(
                            values
                                .iter()
                                .map(|value| to_gemini_schema(value, options))
                                .collect(),
                        ),
                    );
                } else {
                    hints.push("oneOf: additional union constraints omitted.".into());
                }
                continue;
            }
        }
        if !allowed(&key) {
            continue;
        }
        match key.as_str() {
            "type" => {
                if let Some(type_name) = type_name_value(value) {
                    set(&mut result, &key, type_name.to_uppercase());
                } else if !value.is_array() {
                    set(&mut result, &key, value.clone());
                }
            }
            "properties" if value.is_object() || value.is_array() => {
                set(
                    &mut result,
                    &key,
                    Value::Object(
                        entries(value)
                            .into_iter()
                            // JavaScript assignment to this setter creates no own property.
                            .filter(|(key, _)| key != "__proto__")
                            .map(|(key, value)| (key, to_gemini_schema(value, options)))
                            .collect(),
                    ),
                );
            }
            "items" if value.is_object() || value.is_array() || value.is_null() => {
                set(&mut result, &key, to_gemini_schema(value, options));
            }
            "anyOf" if value.is_array() => {
                set(
                    &mut result,
                    &key,
                    Value::Array(
                        value
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|value| to_gemini_schema(value, options))
                            .collect(),
                    ),
                );
            }
            "enum" if value.is_array() => {
                let values = value.as_array().unwrap();
                if values.iter().all(Value::is_string) {
                    if !input["const"].is_string() {
                        set(&mut result, "enum", value.clone());
                    }
                } else {
                    hints.push(format!(
                        "Allowed values: {}.",
                        values.iter().map(describe).collect::<Vec<_>>().join(", ")
                    ));
                }
            }
            "required" if value.is_array() && !property_names.is_empty() => {
                let required: Vec<_> = value
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|value| {
                        value
                            .as_str()
                            .is_some_and(|value| property_names.contains(value))
                    })
                    .cloned()
                    .collect();
                if !required.is_empty() {
                    set(&mut result, &key, Value::Array(required));
                }
            }
            "minimum" | "maximum"
                if value.is_number() && result.get(&key).is_some_and(Value::is_number) =>
            {
                let old = result[&key].as_f64().unwrap();
                let new = value.as_f64().unwrap();
                set(
                    &mut result,
                    &key,
                    finite_number(if key == "minimum" {
                        old.max(new)
                    } else {
                        old.min(new)
                    }),
                );
            }
            _ => {
                set(&mut result, &key, value.clone());
            }
        }
    }
    if options.claude_validated {
        result = flatten_union(result, &mut hints);
        if let (Some(properties), Some(required)) = (
            result.get("properties").and_then(Value::as_object),
            result.get("required").and_then(Value::as_array),
        ) {
            let required: Vec<_> = required
                .iter()
                .filter(|value| {
                    value
                        .as_str()
                        .is_some_and(|name| properties.contains_key(name))
                })
                .cloned()
                .collect();
            if required.is_empty() {
                result.shift_remove("required");
            } else {
                set(&mut result, "required", Value::Array(required));
            }
        }
    }
    if !hints.is_empty() {
        let hint = hints.join(", ");
        let description = match result
            .get("description")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            Some(description) => format!("{description} ({hint})"),
            None => hint,
        };
        set(&mut result, "description", description);
    }
    if result.get("type").is_some_and(|value| value == "ARRAY")
        && !result.get("items").is_some_and(truthy)
    {
        set(&mut result, "items", json!({"type":"STRING"}));
    }
    Value::Object(result)
}
