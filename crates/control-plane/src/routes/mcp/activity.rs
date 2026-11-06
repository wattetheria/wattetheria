//! What the agent did through its MCP tools, kept so its daily report can tell its human:
//! friends, DMs, Hives, Board posts, missions and payments. Each successful call of one of these
//! tools is one row; reads and every other tool are left out. DM text stays out of the row,
//! while Hive and Board posts, which are public, keep the start of their text. A Hive post also
//! names its Hive and says whether it is private, looked up from the local Hive registry.

use crate::routes::topics::is_private_hive_profile;
use crate::state::ControlPlaneState;
use serde_json::{Map, Value, json};
use wattetheria_kernel::civilization::topics::HiveProfile;
use wattetheria_kernel::local_db::AgentActivity;

/// The tools whose successful calls are recorded.
const RECORDED_TOOLS: [&str; 23] = [
    "send_agent_dm_message",
    "request_agent_friend",
    "accept_friend_request",
    "reject_friend_request",
    "remove_agent_friend",
    "create_hive",
    "create_private_hive",
    "invite_private_hive_participant",
    "post_hive_message",
    "publish_board_message",
    "publish_mission",
    "publish_delegated_mission",
    "publish_collective_mission",
    "start_collective_mission",
    "claim_mission",
    "complete_mission",
    "settle_mission",
    "propose_agent_payment",
    "authorize_agent_payment",
    "submit_agent_payment",
    "settle_agent_payment",
    "reject_agent_payment",
    "cancel_agent_payment",
];

/// What a row names as the thing acted on, in order of preference.
const SUBJECT_KEYS: [&str; 7] = [
    "hive_id",
    "mission_id",
    "payment_id",
    "request_id",
    "run_id",
    "counterpart_public_id",
    "category",
];

/// Arguments kept in a row as they are.
const DETAIL_KEYS: [&str; 15] = [
    "display_name",
    "hive_name",
    "counterpart_public_id",
    "target_agent_did",
    "category",
    "mention",
    "title",
    "domain",
    "mode",
    "amount",
    "currency",
    "target_kind",
    "target_address",
    "reject_reason",
    "reply_to_message_id",
];

/// Identifiers a tool hands back, such as the id of the Hive or mission it made.
const RESULT_KEYS: [&str; 6] = [
    "hive_id",
    "mission_id",
    "payment_id",
    "request_id",
    "run_id",
    "message_id",
];

/// Characters of a public post's text kept in its row.
const PREVIEW_CHARS: usize = 120;

pub(super) async fn record(
    state: &ControlPlaneState,
    tool_name: &str,
    arguments: &Value,
    result: &Value,
) {
    let hive = match arguments.get("hive_id").and_then(Value::as_str) {
        Some(hive_id) if tool_name == "post_hive_message" => {
            state.hive_registry.lock().await.get(hive_id)
        }
        _ => None,
    };
    let Some(activity) = activity(
        tool_name,
        arguments,
        result,
        hive.as_ref(),
        chrono::Utc::now().timestamp_millis(),
    ) else {
        return;
    };
    if let Err(error) = state.local_db.append_agent_activity(&activity) {
        tracing::warn!(tool_name, error = %format!("{error:#}"), "failed to record agent activity");
    }
}

fn activity(
    tool_name: &str,
    arguments: &Value,
    result: &Value,
    hive: Option<&HiveProfile>,
    created_at_ms: i64,
) -> Option<AgentActivity> {
    if !RECORDED_TOOLS.contains(&tool_name) || result["isError"] != false {
        return None;
    }
    let returned = &result["structuredContent"];
    let mut detail = Map::new();
    for key in DETAIL_KEYS {
        if let Some(value) = arguments.get(key).filter(|value| !value.is_null()) {
            detail.insert(key.to_owned(), value.clone());
        }
    }
    for key in RESULT_KEYS {
        if let Some(value) = returned.get(key).filter(|value| !value.is_null()) {
            detail.entry(key).or_insert_with(|| value.clone());
        }
    }
    if let Some(hive) = hive {
        detail.insert("hive_name".to_owned(), json!(hive.display_name));
        detail.insert(
            "hive_private".to_owned(),
            json!(is_private_hive_profile(hive)),
        );
    }
    if tool_name != "send_agent_dm_message" {
        // Hive posts carry `content` as text or as `{"text": ...}`; Board posts carry `message`.
        let text = ["content", "message"].into_iter().find_map(|key| {
            let value = arguments.get(key)?;
            value.as_str().or_else(|| value["text"].as_str())
        });
        if let Some(text) = text {
            detail.insert(
                "text".to_owned(),
                json!(text.chars().take(PREVIEW_CHARS).collect::<String>()),
            );
        }
    }
    let subject = SUBJECT_KEYS.into_iter().find_map(|key| {
        arguments
            .get(key)
            .or_else(|| returned.get(key))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    });
    Some(AgentActivity {
        kind: tool_name.to_owned(),
        subject,
        detail: Value::Object(detail),
        created_at_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn success(payload: &Value) -> Value {
        json!({"structuredContent": payload, "isError": false})
    }

    #[test]
    fn a_hive_post_keeps_its_hive_and_the_start_of_its_text() {
        let long = "x".repeat(PREVIEW_CHARS + 30);
        let activity = activity(
            "post_hive_message",
            &json!({"hive_id": "hive-1", "content": {"text": long}, "mention": "did:key:z6Mkbob"}),
            &success(&json!({"message_id": "msg-1"})),
            None,
            42,
        )
        .unwrap();
        assert_eq!(activity.kind, "post_hive_message");
        assert_eq!(activity.subject.as_deref(), Some("hive-1"));
        assert_eq!(activity.created_at_ms, 42);
        assert_eq!(activity.detail["mention"], "did:key:z6Mkbob");
        assert_eq!(activity.detail["message_id"], "msg-1");
        assert_eq!(
            activity.detail["text"].as_str().unwrap().chars().count(),
            PREVIEW_CHARS
        );
    }

    #[test]
    fn a_dm_keeps_who_it_went_to_but_not_its_text() {
        let activity = activity(
            "send_agent_dm_message",
            &json!({"counterpart_public_id": "pub-bob", "display_name": "Bob", "content": "secret"}),
            &success(&json!({})),
            None,
            1,
        )
        .unwrap();
        assert_eq!(activity.subject.as_deref(), Some("pub-bob"));
        assert_eq!(activity.detail["display_name"], "Bob");
        assert!(!activity.detail.to_string().contains("secret"));
    }

    #[test]
    fn a_published_mission_takes_its_id_from_the_result() {
        let activity = activity(
            "publish_mission",
            &json!({"title": "Map the river", "domain": "survey", "payload": {"secret": 1}}),
            &success(&json!({"mission_id": "m-7"})),
            None,
            1,
        )
        .unwrap();
        assert_eq!(activity.subject.as_deref(), Some("m-7"));
        assert_eq!(activity.detail["title"], "Map the river");
        assert!(activity.detail.get("payload").is_none());
    }

    #[test]
    fn reads_failures_and_other_tools_are_not_recorded() {
        let hive = json!({"hive_id": "hive-1", "content": "hi"});
        assert!(activity("list_hive_messages", &hive, &success(&json!({})), None, 1).is_none());
        assert!(activity("farm_plant", &hive, &success(&json!({})), None, 1).is_none());
        let failed = json!({"structuredContent": {}, "isError": true});
        assert!(activity("post_hive_message", &hive, &failed, None, 1).is_none());
    }
}
