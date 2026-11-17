use serde_json::Value;

// Fail closed on unknown assertion keywords. Draft 7 normally permits unknown
// keywords as annotations, which could silently discard a host's constraint.
// References are deliberately unsupported in this first, self-contained slice.
pub(crate) fn supported(schema: &Value) -> bool {
    if schema.is_boolean() {
        return true;
    }
    let Some(object) = schema.as_object() else {
        return false;
    };
    object.iter().all(|(key, value)| match key.as_str() {
        "$schema" => matches!(
            value.as_str(),
            Some("http://json-schema.org/draft-07/schema#")
                | Some("http://json-schema.org/draft-07/schema")
        ),
        "$ref" | "$id" => false,
        "properties" | "patternProperties" | "definitions" => value
            .as_object()
            .is_some_and(|entries| entries.values().all(supported)),
        "allOf" | "anyOf" | "oneOf" => value
            .as_array()
            .is_some_and(|entries| entries.iter().all(supported)),
        "items" => match value.as_array() {
            Some(entries) => entries.iter().all(supported),
            None => supported(value),
        },
        "additionalItems"
        | "additionalProperties"
        | "contains"
        | "propertyNames"
        | "not"
        | "if"
        | "then"
        | "else" => supported(value),
        "dependencies" => value.as_object().is_some_and(|entries| {
            entries.values().all(|entry| match entry.as_array() {
                Some(names) => names.iter().all(Value::is_string),
                None => supported(entry),
            })
        }),
        "$comment" | "title" | "description" | "default" | "examples" | "readOnly"
        | "writeOnly" | "type" | "enum" | "const" | "multipleOf" | "maximum"
        | "exclusiveMaximum" | "minimum" | "exclusiveMinimum" | "maxLength" | "minLength"
        | "pattern" | "maxItems" | "minItems" | "uniqueItems" | "maxProperties"
        | "minProperties" | "required" | "format" => true,
        _ => false,
    })
}
