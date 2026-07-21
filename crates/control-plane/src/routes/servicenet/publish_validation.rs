use serde_json::{Map, Value, json};
use std::net::IpAddr;
use wattetheria_kernel::servicenet::validate_servicenet_agent_name;

pub(super) fn default_agent_card() -> Value {
    json!({
        "name": "",
        "description": "",
        "url": "",
        "preferredTransport": "JSONRPC",
        "protocolVersion": "1.0",
        "scope": "real_world",
        "origin": "custom_built",
        "domain": "GENERAL",
        "cost": 0,
        "currency": "USDC",
        "supportsTask": false,
        "skills": [{"name": "", "description": ""}],
        "securitySchemes": {"none": {"type": "none"}},
        "security": [{"none": []}],
    })
}

pub(super) fn normalize_agent_card_skills(mut card: Value) -> Value {
    if let Some(skills) = card.get_mut("skills").and_then(Value::as_array_mut) {
        for skill in skills {
            if let Some(skill_object) = skill.as_object_mut() {
                skill_object
                    .entry("description")
                    .or_insert_with(|| Value::String(String::new()));
            }
        }
    }
    card
}

pub(super) fn real_world_domains() -> Vec<&'static str> {
    vec![
        "GENERAL",
        "TRANSPORTATION",
        "FOOD",
        "CLOTHING",
        "HOUSING",
        "PAYMENTS",
        "COMMERCE",
        "MEDIA",
        "HEALTH",
        "EDUCATION",
        "TRAVEL",
    ]
}

pub(super) fn wattetheria_native_domains() -> Vec<&'static str> {
    vec![
        "GENERAL",
        "GOVERNANCE",
        "PRODUCTION",
        "TRADING",
        "AUTOMATION",
        "SECURITY",
        "EXPLORATION",
        "DISCOVERY",
        "SERVICENET",
    ]
}

pub(super) fn validate_agent_card(card: &Value) -> Result<(), String> {
    let object = card
        .as_object()
        .ok_or_else(|| "agent card must be a JSON object".to_owned())?;
    validate_agent_card_required_fields(object)?;
    validate_agent_card_strings(object)?;
    validate_public_adapter_url(
        object
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )?;
    if object.get("preferredTransport").and_then(Value::as_str) != Some("JSONRPC") {
        return Err("agent card `preferredTransport` must be `JSONRPC`".to_owned());
    }
    if object.get("protocolVersion").and_then(Value::as_str) != Some("1.0") {
        return Err("agent card `protocolVersion` must be `1.0`".to_owned());
    }
    validate_scope_origin_domain(
        object
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        object
            .get("origin")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        object
            .get("domain")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )?;
    validate_agent_card_pricing(object)?;
    if object
        .get("supportsTask")
        .and_then(Value::as_bool)
        .is_none()
    {
        return Err("agent card `supportsTask` must be a boolean".to_owned());
    }
    validate_agent_card_skills(object)?;
    validate_agent_card_no_secrets(card)
}

fn validate_agent_card_required_fields(object: &Map<String, Value>) -> Result<(), String> {
    for field in [
        "name",
        "description",
        "url",
        "preferredTransport",
        "protocolVersion",
        "scope",
        "origin",
        "domain",
        "cost",
        "currency",
        "supportsTask",
        "skills",
        "securitySchemes",
        "security",
    ] {
        if !object.contains_key(field) {
            return Err(format!("agent card is missing required field `{field}`"));
        }
    }
    Ok(())
}

fn validate_agent_card_strings(object: &Map<String, Value>) -> Result<(), String> {
    for field in ["name", "description"] {
        if object
            .get(field)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .is_empty()
        {
            return Err(format!("agent card `{field}` must not be empty"));
        }
    }
    validate_servicenet_agent_name(
        object
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn validate_agent_card_pricing(object: &Map<String, Value>) -> Result<(), String> {
    let cost = object
        .get("cost")
        .and_then(Value::as_u64)
        .ok_or_else(|| "agent card `cost` must be a non-negative integer".to_owned())?;
    if cost > u64::from(u32::MAX) {
        return Err(format!(
            "agent card `cost` must be a non-negative integer up to {}",
            u32::MAX
        ));
    }
    if !matches!(
        object.get("currency").and_then(Value::as_str),
        Some("USDC" | "USDT")
    ) {
        return Err("agent card `currency` must be `USDC` or `USDT`".to_owned());
    }
    Ok(())
}

fn validate_agent_card_skills(object: &Map<String, Value>) -> Result<(), String> {
    let skills = object
        .get("skills")
        .and_then(Value::as_array)
        .ok_or_else(|| "agent card `skills` must be an array".to_owned())?;
    if skills.is_empty() {
        return Err("agent card `skills` must list at least one skill".to_owned());
    }
    for (index, skill) in skills.iter().enumerate() {
        if skill
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .is_empty()
        {
            return Err(format!("skill[{index}] is missing required field `name`"));
        }
    }
    Ok(())
}

fn validate_agent_card_no_secrets(card: &Value) -> Result<(), String> {
    let card_text = serde_json::to_string(card).unwrap_or_default();
    if card_text.contains("sk-") || card_text.contains("BEGIN PRIVATE KEY") {
        return Err(
            "agent card appears to contain a secret; remove it before publishing".to_owned(),
        );
    }
    Ok(())
}

fn validate_public_adapter_url(endpoint: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(endpoint)
        .map_err(|error| format!("Adapter URL is not valid: {error}"))?;
    if url.scheme() != "https" {
        return Err(format!(
            "Adapter URL must use https:// (got scheme `{}`)",
            url.scheme()
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| "Adapter URL must include a host".to_owned())?;
    if host.eq_ignore_ascii_case("localhost") {
        return Err("Adapter URL host must not be localhost".to_owned());
    }
    if host.parse::<IpAddr>().is_ok() {
        return Err(format!(
            "Adapter URL host is an IP literal ({host}); use a DNS hostname instead"
        ));
    }
    Ok(())
}

fn validate_scope_origin_domain(scope: &str, origin: &str, domain: &str) -> Result<(), String> {
    match scope {
        "real_world" if !matches!(origin, "established_service" | "custom_built") => {
            return Err(
                "agent card `origin` must be `established_service` or `custom_built` for `real_world` scope"
                    .to_owned(),
            );
        }
        "wattetheria_native" if origin != "native_published" => {
            return Err(
                "agent card `origin` must be `native_published` for `wattetheria_native` scope"
                    .to_owned(),
            );
        }
        "real_world" | "wattetheria_native" => {}
        _ => {
            return Err(
                "agent card `scope` must be `real_world` or `wattetheria_native`".to_owned(),
            );
        }
    }
    let allowed = if scope == "real_world" {
        real_world_domains()
    } else {
        wattetheria_native_domains()
    };
    if !allowed.contains(&domain) {
        return Err(format!(
            "agent card `domain` is not supported for `{scope}` scope"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_url_is_an_exact_public_https_url() {
        assert!(validate_public_adapter_url("https://provider.example.com").is_ok());
        assert!(validate_public_adapter_url("https://provider.example.com/custom-route").is_ok());
        assert!(validate_public_adapter_url("http://provider.example.com").is_err());
        assert!(validate_public_adapter_url("https://127.0.0.1").is_err());
    }
}
