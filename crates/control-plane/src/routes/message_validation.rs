use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value, json};

pub(crate) const MISSION_TITLE_MAX_CHARS: usize = 256;
pub(crate) const MISSION_DESCRIPTION_MAX_CHARS: usize = 8_192;
pub(crate) const MISSION_PAYLOAD_MAX_BYTES: usize = 64 * 1024;
pub(crate) const SOCIAL_MESSAGE_MAX_CHARS: usize = 4_096;

pub(crate) fn validate_mission_inputs(
    title: &str,
    description: &str,
    payload: &Value,
) -> Result<(), String> {
    validate_text("title", title, MISSION_TITLE_MAX_CHARS)?;
    validate_text("description", description, MISSION_DESCRIPTION_MAX_CHARS)?;
    validate_json_size("payload", payload, MISSION_PAYLOAD_MAX_BYTES)
}

pub(crate) fn validate_mission_arguments(arguments: &Value) -> Result<(), String> {
    let object = argument_object(arguments)?;
    let title = object
        .get("title")
        .and_then(Value::as_str)
        .ok_or_else(|| "title is required and must be a string".to_owned())?;
    let description = object
        .get("description")
        .and_then(Value::as_str)
        .ok_or_else(|| "description is required and must be a string".to_owned())?;
    let payload = object
        .get("payload")
        .ok_or_else(|| "payload is required".to_owned())?;
    validate_mission_inputs(title, description, payload)
}

pub(crate) fn validate_social_message_content(content: &Value) -> Result<(), String> {
    validate_value("content", content, SOCIAL_MESSAGE_MAX_CHARS)
}

pub(crate) fn validation_error_response(error: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": error.into()})),
    )
        .into_response()
}

fn argument_object(arguments: &Value) -> Result<&Map<String, Value>, String> {
    arguments
        .get("body")
        .and_then(Value::as_object)
        .or_else(|| arguments.as_object())
        .ok_or_else(|| "MCP arguments must be a JSON object".to_owned())
}

fn validate_text(field: &str, value: &str, max_chars: usize) -> Result<(), String> {
    if value_contains_disallowed_control_chars(&Value::String(value.to_owned())) {
        return Err(format!("{field} contains unsupported control characters"));
    }
    let actual_chars = value.chars().count();
    if actual_chars > max_chars {
        return Err(format!(
            "{field} must be at most {max_chars} characters (received {actual_chars})"
        ));
    }
    Ok(())
}

fn validate_value(field: &str, value: &Value, max_chars: usize) -> Result<(), String> {
    if value_contains_disallowed_control_chars(value) {
        return Err(format!("{field} contains unsupported control characters"));
    }
    let actual_chars = value_string_char_count(value);
    if actual_chars > max_chars {
        return Err(format!(
            "{field} must be at most {max_chars} characters (received {actual_chars})"
        ));
    }
    Ok(())
}

fn validate_json_size(field: &str, value: &Value, max_bytes: usize) -> Result<(), String> {
    let serialized = serde_json::to_vec(value)
        .map_err(|error| format!("{field} could not be serialized as JSON: {error}"))?;
    let actual_bytes = serialized.len();
    if actual_bytes > max_bytes {
        return Err(format!(
            "{field} must be at most {max_bytes} bytes when serialized as JSON (received {actual_bytes})"
        ));
    }
    Ok(())
}

fn value_string_char_count(value: &Value) -> usize {
    match value {
        Value::String(value) => value.chars().count(),
        Value::Array(values) => values.iter().map(value_string_char_count).sum(),
        Value::Object(values) => values.values().map(value_string_char_count).sum(),
        Value::Null | Value::Bool(_) | Value::Number(_) => 0,
    }
}

fn value_contains_disallowed_control_chars(value: &Value) -> bool {
    match value {
        Value::String(value) => string_contains_disallowed_control_chars(value),
        Value::Array(values) => values.iter().any(value_contains_disallowed_control_chars),
        Value::Object(values) => {
            values
                .keys()
                .any(|key| string_contains_disallowed_control_chars(key))
                || values.values().any(value_contains_disallowed_control_chars)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

fn string_contains_disallowed_control_chars(value: &str) -> bool {
    value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_all_language_scripts_and_common_symbols() {
        let content =
            json!("中文 English العربية 日本語 हिन्दी 한국어 🙂 !@#$%^&*()_+-=[]{};:',.<>/?");

        assert!(validate_social_message_content(&content).is_ok());
    }

    #[test]
    fn allows_line_breaks_tabs_and_carriage_returns_but_rejects_other_controls() {
        assert!(validate_social_message_content(&json!("line\nnext\trow\r")).is_ok());
        assert!(validate_social_message_content(&json!("bad\u{0000}value")).is_err());
    }

    #[test]
    fn counts_nested_string_values_as_unicode_characters() {
        let content = json!({"text": "中文🙂", "parts": ["é", "ok"]});

        assert!(validate_value("content", &content, 6).is_ok());
        assert!(validate_value("content", &content, 5).is_err());
    }

    #[test]
    fn validates_mission_text_lengths_without_language_restrictions() {
        let title = "中".repeat(MISSION_TITLE_MAX_CHARS);
        let description = "🙂".repeat(MISSION_DESCRIPTION_MAX_CHARS);

        assert!(validate_mission_inputs(&title, &description, &Value::Null).is_ok());
        assert!(
            validate_mission_inputs(&format!("{title}中"), &description, &Value::Null,).is_err()
        );
    }

    #[test]
    fn validates_direct_and_wrapped_mcp_mission_arguments() {
        let accepted = json!({
            "title": "中文 mission",
            "description": "日本語 description",
            "payload": {"objective": "العربية"}
        });
        assert!(validate_mission_arguments(&accepted).is_ok());

        let rejected = json!({
            "body": {
                "title": "x".repeat(MISSION_TITLE_MAX_CHARS + 1),
                "description": "description",
                "payload": null
            }
        });
        assert!(validate_mission_arguments(&rejected).is_err());
    }

    #[test]
    fn validates_serialized_payload_size_in_bytes() {
        let payload_at_limit = json!("x".repeat(MISSION_PAYLOAD_MAX_BYTES - 2));
        let payload_over_limit = json!("x".repeat(MISSION_PAYLOAD_MAX_BYTES - 1));

        assert!(
            validate_json_size("payload", &payload_at_limit, MISSION_PAYLOAD_MAX_BYTES).is_ok()
        );
        assert!(
            validate_json_size("payload", &payload_over_limit, MISSION_PAYLOAD_MAX_BYTES).is_err()
        );
    }
}
