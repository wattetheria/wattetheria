use serde_json::Value;

pub(super) fn message_field<'a>(params: &'a Value, field: &str) -> Option<&'a Value> {
    params
        .get("message")
        .and_then(|message| message.get(field))
        .or_else(|| params.get(field))
}

pub(super) fn metadata_value<'a>(params: &'a Value, key: &str) -> Option<&'a Value> {
    params
        .get("metadata")
        .and_then(|metadata| metadata.get(key))
}

pub(super) fn agent_envelope(params: &Value) -> Option<&Value> {
    metadata_value(params, "agent_envelope")
        .or_else(|| params.pointer("/extensions/agent_envelope"))
}

pub(super) fn settlement(params: &Value) -> Option<&Value> {
    metadata_value(params, "settlement").or_else(|| params.pointer("/extensions/settlement"))
}

pub(super) fn skill_id(params: &Value) -> Option<&Value> {
    metadata_value(params, "skillId").or_else(|| params.get("skillId"))
}

pub(super) fn uses_standard_metadata(params: &Value) -> bool {
    metadata_value(params, "agent_envelope").is_some()
}
