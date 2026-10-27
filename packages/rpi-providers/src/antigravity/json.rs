//! JavaScript value rules used by the pinned JSON transformations.
use serde_json::{Map, Value};

pub(crate) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        _ => true,
    }
}
pub(crate) fn trim(value: &str) -> &str {
    value.trim_matches(|c| matches!(c, '\u{9}'..='\u{d}'|'\u{20}'|'\u{a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}'))
}
fn number(value: f64) -> String {
    if value == 0.0 {
        return "0".into();
    }
    if value.abs() >= 1e21 || value.abs() < 1e-6 {
        let rendered = format!("{value:e}");
        let (base, exponent) = rendered.split_once('e').unwrap();
        return format!(
            "{base}e{}{exponent}",
            if exponent.starts_with('-') { "" } else { "+" }
        );
    }
    value.to_string()
}
pub(crate) fn string(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Number(value) => number(value.as_f64().unwrap()),
        Value::Array(values) => values
            .iter()
            .map(|value| {
                if value.is_null() {
                    String::new()
                } else {
                    string(value)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
        _ => value.to_string(),
    }
}
/// Object.entries visits canonical array-index keys before other insertion-ordered keys.
pub(crate) fn entries(value: &Value) -> Vec<(String, &Value)> {
    match value {
        Value::Array(values) => values
            .iter()
            .enumerate()
            .map(|(index, value)| (index.to_string(), value))
            .collect(),
        Value::Object(object) => {
            let mut entries: Vec<_> = object
                .iter()
                .map(|(key, value)| (key.clone(), value))
                .collect();
            entries.sort_by_key(|(key, _)| {
                let index = key
                    .parse::<u32>()
                    .ok()
                    .filter(|index| *index != u32::MAX && index.to_string() == *key);
                (index.is_none(), index.unwrap_or(0))
            });
            entries
        }
        _ => Vec::new(),
    }
}
/// JSON.stringify-compatible rendering with JavaScript key and number rules.
pub(crate) fn describe(value: &Value) -> String {
    match value {
        Value::Number(value) => number(value.as_f64().unwrap()),
        Value::Array(values) => format!(
            "[{}]",
            values.iter().map(describe).collect::<Vec<_>>().join(",")
        ),
        Value::Object(_) => format!(
            "{{{}}}",
            entries(value)
                .iter()
                .map(|(key, value)| format!(
                    "{}:{}",
                    serde_json::to_string(key).unwrap(),
                    describe(value)
                ))
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => value.to_string(),
    }
}
pub(crate) fn finite_number(value: f64) -> Value {
    if value.fract() == 0.0 && value.abs() <= 9007199254740991.0 {
        Value::from(value as i64)
    } else {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .unwrap_or(Value::Null)
    }
}
pub(crate) fn set(map: &mut Map<String, Value>, key: &str, value: impl Into<Value>) {
    map.insert(key.into(), value.into());
}
