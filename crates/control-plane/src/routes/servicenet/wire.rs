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

pub(super) fn agent_envelope(params: &Value) -> Result<Option<Value>, String> {
    let Some(envelope) = metadata_value(params, "agent_envelope")
        .or_else(|| params.pointer("/extensions/agent_envelope"))
    else {
        return Ok(None);
    };
    match envelope {
        Value::String(encoded) => serde_json::from_str(encoded)
            .map(Some)
            .map_err(|error| format!("invalid encoded A2A agent_envelope: {error}")),
        value => Ok(Some(value.clone())),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn signed_agent_envelope_accepts_opaque_json_metadata() {
        let envelope = json!({
            "source_agent_id": "did:key:zCaller",
            "extensions": {"issued_at_ms": 1_784_708_528_589_u64}
        });
        let params = json!({
            "metadata": {
                "agent_envelope": serde_json::to_string(&envelope).unwrap()
            }
        });

        assert_eq!(agent_envelope(&params).unwrap(), Some(envelope));
    }

    #[test]
    fn signed_agent_envelope_keeps_legacy_object_metadata_compatibility() {
        let envelope = json!({"source_agent_id": "did:key:zCaller"});
        let params = json!({"metadata": {"agent_envelope": envelope}});

        assert_eq!(agent_envelope(&params).unwrap(), Some(envelope));
    }

    #[test]
    fn signed_agent_envelope_rejects_invalid_opaque_json_metadata() {
        let params = json!({"metadata": {"agent_envelope": "not-json"}});

        let error = agent_envelope(&params).expect_err("invalid envelope JSON must fail");
        assert!(error.contains("invalid encoded A2A agent_envelope"));
    }
}
