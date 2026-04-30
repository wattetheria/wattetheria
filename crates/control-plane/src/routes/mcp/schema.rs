use serde_json::{Map, Value, json};

use super::{AgentTool, path_vars};

pub(super) fn input_schema(tool: &AgentTool) -> Value {
    client_schema(tool)
        .or_else(|| payment_schema(tool))
        .or_else(|| topic_schema(tool))
        .or_else(|| mission_schema(tool))
        .or_else(|| social_schema(tool))
        .or_else(|| mailbox_schema(tool))
        .or_else(|| servicenet_schema(tool))
        .unwrap_or_else(|| tool_schema(tool, &[], &[], true))
}

fn client_schema(tool: &AgentTool) -> Option<Value> {
    match tool.name {
        "client_task_activity" => Some(empty_tool_schema(tool)),
        "client_export" => Some(tool_schema(
            tool,
            &[
                string_field("agent_did", "Agent DID to scope the export."),
                string_field("public_id", "Public identity to scope the export."),
                integer_field("peer_limit", "Maximum number of peers to include."),
                integer_field("task_limit", "Maximum number of tasks to include."),
                integer_field(
                    "organization_limit",
                    "Maximum number of organizations to include.",
                ),
                integer_field("rpc_log_limit", "Maximum number of RPC logs to include."),
                integer_field(
                    "leaderboard_limit",
                    "Maximum number of leaderboard rows to include.",
                ),
                string_field("leaderboard_category", "Leaderboard category to include."),
            ],
            &[],
            false,
        )),
        _ => None,
    }
}

fn payment_schema(tool: &AgentTool) -> Option<Value> {
    match tool.name {
        "get_agent_payment" | "submit_agent_payment" | "cancel_agent_payment" => {
            Some(empty_tool_schema(tool))
        }
        "list_agent_payments" => Some(tool_schema(
            tool,
            &[
                string_field("public_id", "Local public identity filter."),
                string_field(
                    "counterpart_public_id",
                    "Counterpart public identity filter.",
                ),
                enum_field(
                    "status",
                    "Payment status filter.",
                    &[
                        "proposed",
                        "authorized",
                        "submitted",
                        "settled",
                        "rejected",
                        "expired",
                        "cancelled",
                    ],
                ),
                string_field("role", "Payment role filter, such as sender or receiver."),
                string_field("rail", "Settlement rail filter."),
                integer_field("limit", "Maximum number of payment sessions to return."),
            ],
            &[],
            false,
        )),
        "propose_agent_payment" => Some(tool_schema(
            tool,
            &[
                string_field("counterpart_public_id", "Recipient public identity."),
                string_field("amount", "Payment amount as a string."),
                string_field("currency", "Payment currency."),
                string_field("rail", "Settlement rail."),
                enum_field("layer", "Settlement layer.", &["web2", "web3"]),
                string_field("network", "Settlement network."),
                string_field("recipient_address", "Recipient settlement address."),
                string_field("mission_id", "Related mission ID."),
                string_field("task_id", "Related task ID."),
                string_field("description", "Payment description."),
                value_field("metadata", "Optional payment metadata."),
                integer_field("expires_at", "Unix timestamp expiry."),
            ],
            &["counterpart_public_id", "amount", "currency", "rail"],
            false,
        )),
        "authorize_agent_payment" => Some(tool_schema(
            tool,
            &[string_field("sender_address", "Sender settlement address.")],
            &[],
            false,
        )),
        "settle_agent_payment" => Some(tool_schema(
            tool,
            &[value_field(
                "settlement_receipt",
                "Settlement success receipt payload.",
            )],
            &["settlement_receipt"],
            false,
        )),
        "reject_agent_payment" => Some(tool_schema(
            tool,
            &[string_field(
                "reject_reason",
                "Reason for rejecting the payment.",
            )],
            &["reject_reason"],
            false,
        )),
        _ => None,
    }
}

fn topic_schema(tool: &AgentTool) -> Option<Value> {
    match tool.name {
        "list_topics" => Some(tool_schema(tool, &list_topic_fields(), &[], false)),
        "create_topic" => Some(tool_schema(
            tool,
            &create_topic_fields(),
            &["feed_key", "scope_hint", "display_name", "projection_kind"],
            false,
        )),
        "list_topic_messages" => Some(tool_schema(
            tool,
            &list_topic_message_fields(),
            &["feed_key", "scope_hint"],
            false,
        )),
        "post_topic_message" => Some(tool_schema(
            tool,
            &post_topic_message_fields(),
            &["feed_key", "scope_hint", "content"],
            false,
        )),
        "subscribe_topic" => Some(tool_schema(
            tool,
            &subscribe_topic_fields(true),
            &["feed_key", "scope_hint", "active"],
            false,
        )),
        "unsubscribe_topic" => Some(tool_schema(
            tool,
            &subscribe_topic_fields(false),
            &["feed_key", "scope_hint"],
            false,
        )),
        _ => None,
    }
}

fn topic_projection_kind_field(description: &str) -> (&'static str, Value) {
    enum_field(
        "projection_kind",
        description,
        &[
            "chat_room",
            "working_group",
            "guild",
            "organization",
            "mission_thread",
            "direct_conversation",
        ],
    )
}

fn list_topic_fields() -> Vec<(&'static str, Value)> {
    vec![
        integer_field("limit", "Maximum number of gateway Hives to return."),
        integer_field(
            "offset",
            "Zero-based client offset into the bounded gateway result window.",
        ),
        string_field("topic_id", "Network Hive topic ID filter."),
        string_field("organization_id", "Organization topic filter."),
        string_field("mission_id", "Mission topic filter."),
        topic_projection_kind_field("Topic projection kind filter."),
        bool_field(
            "include_inactive",
            "Whether inactive topics should be included.",
        ),
    ]
}

fn create_topic_fields() -> Vec<(&'static str, Value)> {
    vec![
        string_field("feed_key", "Topic feed key."),
        string_field("scope_hint", "Topic scope hint."),
        string_field("display_name", "Human-readable topic name."),
        string_field("summary", "Optional topic summary."),
        topic_projection_kind_field("Topic projection kind."),
        string_field("organization_id", "Organization linked to this topic."),
        string_field("mission_id", "Mission linked to this topic."),
        string_array_field("participant_public_ids", "Initial participant public IDs."),
        string_field("why_this_exists", "Reason this topic exists."),
        value_field("initial_message", "Optional first topic message payload."),
    ]
}

fn list_topic_message_fields() -> Vec<(&'static str, Value)> {
    vec![
        string_field("feed_key", "Topic feed key."),
        string_field("scope_hint", "Topic scope hint."),
        integer_field("limit", "Maximum number of messages to return."),
        integer_field("before_created_at", "Cursor timestamp boundary."),
        string_field("before_message_id", "Cursor message ID boundary."),
        string_field("subscriber_id", "Subscriber ID for cursor tracking."),
    ]
}

fn post_topic_message_fields() -> Vec<(&'static str, Value)> {
    vec![
        string_field("feed_key", "Topic feed key."),
        string_field("scope_hint", "Topic scope hint."),
        value_field("content", "Message content payload."),
        string_field("reply_to_message_id", "Message ID this post replies to."),
    ]
}

fn subscribe_topic_fields(include_active: bool) -> Vec<(&'static str, Value)> {
    let mut fields = vec![
        string_field("feed_key", "Topic feed key."),
        string_field("scope_hint", "Topic scope hint."),
    ];
    if include_active {
        fields.push(bool_field(
            "active",
            "Whether the subscription should be active.",
        ));
    }
    fields
}

fn mission_schema(tool: &AgentTool) -> Option<Value> {
    match tool.name {
        "list_missions" => Some(tool_schema(tool, &list_mission_fields(), &[], false)),
        "publish_mission" => Some(tool_schema(
            tool,
            &publish_mission_fields(),
            &["title", "description", "domain", "reward", "payload"],
            false,
        )),
        "claim_mission" => Some(tool_schema(
            tool,
            &claim_mission_fields(),
            &["mission_id", "agent_did"],
            false,
        )),
        "complete_mission" => Some(tool_schema(
            tool,
            &complete_mission_fields(),
            &["mission_id", "agent_did"],
            false,
        )),
        "settle_mission" => Some(tool_schema(
            tool,
            &settle_mission_fields(),
            &["mission_id"],
            false,
        )),
        _ => None,
    }
}

fn list_mission_fields() -> Vec<(&'static str, Value)> {
    vec![
        enum_field(
            "status",
            "Network mission status filter.",
            &[
                "published",
                "open",
                "claimed",
                "completed",
                "settled",
                "cancelled",
            ],
        ),
        integer_field("limit", "Maximum number of gateway missions to return."),
        integer_field(
            "offset",
            "Zero-based client offset into the bounded gateway result window.",
        ),
    ]
}

fn publish_mission_fields() -> Vec<(&'static str, Value)> {
    vec![
        string_field("title", "Mission title."),
        string_field("description", "Mission description."),
        enum_field(
            "domain",
            "Mission domain.",
            &["wealth", "power", "security", "trade", "culture"],
        ),
        string_field("subnet_id", "Optional target subnet."),
        string_field("zone_id", "Optional target zone."),
        enum_field(
            "required_role",
            "Required role path.",
            &["operator", "broker", "enforcer", "artificer"],
        ),
        enum_field(
            "required_faction",
            "Required faction.",
            &["order", "freeport", "raider"],
        ),
        reward_field(),
        value_field("payload", "Mission payload."),
    ]
}

fn claim_mission_fields() -> Vec<(&'static str, Value)> {
    vec![
        string_field("mission_id", "Mission ID."),
        string_field("agent_did", "Agent DID claiming the mission."),
        string_field(
            "task_id",
            "Wattswarm task ID from list_missions claim_route.",
        ),
        string_field(
            "mission_feed_key",
            "Mission feed key from list_missions claim_route.",
        ),
        string_field(
            "mission_scope_hint",
            "Wattswarm mission scope hint from list_missions claim_route.",
        ),
        string_field(
            "publisher_wattswarm_node_id",
            "Publisher Wattswarm node ID from list_missions claim_route.",
        ),
        value_field(
            "claim_route",
            "Claim route object returned by list_missions.",
        ),
    ]
}

fn complete_mission_fields() -> Vec<(&'static str, Value)> {
    let mut fields = claim_mission_fields();
    fields[1] = string_field("agent_did", "Agent DID completing the mission.");
    fields[6] = value_field(
        "claim_route",
        "Claim route object returned by list_missions for network missions.",
    );
    fields.push(value_field(
        "result",
        "Mission completion result to submit as the Wattswarm candidate output.",
    ));
    fields
}

fn settle_mission_fields() -> Vec<(&'static str, Value)> {
    vec![
        string_field("mission_id", "Mission ID to settle."),
        string_field(
            "task_id",
            "Wattswarm task ID to finalize before settling a local publisher mission.",
        ),
        string_field(
            "agent_did",
            "Completing agent DID used to derive the Wattswarm candidate ID.",
        ),
        string_field(
            "candidate_id",
            "Wattswarm candidate ID to accept before settling.",
        ),
    ]
}

fn social_schema(tool: &AgentTool) -> Option<Value> {
    match tool.name {
        "list_friends" => Some(tool_schema(
            tool,
            &[
                string_field("public_id", "Local public identity filter."),
                string_field(
                    "counterpart_public_id",
                    "Counterpart public identity filter.",
                ),
            ],
            &[],
            false,
        )),
        "upsert_friend" => Some(tool_schema(
            tool,
            &[
                string_field("counterpart_public_id", "Counterpart public identity."),
                enum_field("kind", "Relationship kind.", &["follow", "friend"]),
                bool_field("active", "Whether the relationship is active."),
            ],
            &["counterpart_public_id", "kind", "active"],
            false,
        )),
        _ => None,
    }
}

fn mailbox_schema(tool: &AgentTool) -> Option<Value> {
    match tool.name {
        "send_message" => Some(tool_schema(
            tool,
            &[
                string_field("to_agent", "Recipient agent DID."),
                string_field("from_subnet", "Sender subnet ID."),
                string_field("to_subnet", "Recipient subnet ID."),
                value_field("payload", "Message payload."),
            ],
            &["to_agent", "from_subnet", "to_subnet", "payload"],
            false,
        )),
        "fetch_messages" => Some(tool_schema(
            tool,
            &[string_field("subnet_id", "Subnet inbox ID to fetch.")],
            &["subnet_id"],
            false,
        )),
        "ack_message" => Some(tool_schema(
            tool,
            &[
                string_field("subnet_id", "Subnet inbox ID."),
                string_field("message_id", "Mailbox message ID to acknowledge."),
            ],
            &["subnet_id", "message_id"],
            false,
        )),
        _ => None,
    }
}

fn servicenet_schema(tool: &AgentTool) -> Option<Value> {
    match tool.name {
        "list_servicenet_agents" | "get_servicenet_agent" => Some(empty_tool_schema(tool)),
        "invoke_servicenet_agent" => Some(tool_schema(
            tool,
            &[
                string_field("task_id", "ServiceNet task ID."),
                string_field("context_id", "ServiceNet context ID."),
                string_field("message", "Message to send to the external agent."),
                value_field("input", "Structured input for the external agent."),
                string_field("skill_id", "External agent skill ID."),
                string_field("auth_token", "ServiceNet auth token."),
                string_field("auth_context_id", "ServiceNet auth context UUID."),
                string_field("region", "Requested ServiceNet region."),
                bool_field(
                    "confirm_risky",
                    "Whether risky external actions are confirmed.",
                ),
                integer_field("max_cost_units", "Maximum allowed ServiceNet cost units."),
                settlement_field(),
            ],
            &[],
            false,
        )),
        "get_servicenet_agent_task" => Some(tool_schema(
            tool,
            &[
                integer_field("history_length", "Task history length to retrieve."),
                string_field("auth_token", "ServiceNet auth token."),
                string_field("auth_context_id", "ServiceNet auth context UUID."),
            ],
            &[],
            false,
        )),
        _ => None,
    }
}

fn empty_tool_schema(tool: &AgentTool) -> Value {
    tool_schema(tool, &[], &[], false)
}

fn tool_schema(
    tool: &AgentTool,
    fields: &[(&str, Value)],
    body_required: &[&str],
    additional_properties: bool,
) -> Value {
    let path_vars = path_vars(tool.path);
    let mut properties = Map::new();
    for var in &path_vars {
        properties.insert(
            (*var).to_string(),
            json!({
                "type": "string",
                "description": format!("Path parameter `{var}` for {}", tool.path)
            }),
        );
    }
    for (name, schema) in fields {
        properties.insert((*name).to_string(), schema.clone());
    }

    if additional_properties {
        properties.insert(
            "query".to_string(),
            json!({
                "type": "object",
                "description": "Optional query parameters for GET endpoints.",
                "additionalProperties": true
            }),
        );
        properties.insert(
            "body".to_string(),
            json!({
                "type": "object",
                "description": "Optional JSON request body for POST endpoints.",
                "additionalProperties": true
            }),
        );
    }

    let required = path_vars
        .into_iter()
        .chain(body_required.iter().copied())
        .collect::<Vec<_>>();

    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": additional_properties
    })
}

fn string_field<'a>(name: &'a str, description: &str) -> (&'a str, Value) {
    (name, json!({"type": "string", "description": description}))
}

fn integer_field<'a>(name: &'a str, description: &str) -> (&'a str, Value) {
    (
        name,
        json!({"type": "integer", "minimum": 0, "description": description}),
    )
}

fn bool_field<'a>(name: &'a str, description: &str) -> (&'a str, Value) {
    (name, json!({"type": "boolean", "description": description}))
}

fn value_field<'a>(name: &'a str, description: &str) -> (&'a str, Value) {
    (name, json!({"description": description}))
}

fn enum_field<'a>(name: &'a str, description: &str, values: &[&str]) -> (&'a str, Value) {
    (
        name,
        json!({
            "type": "string",
            "enum": values,
            "description": description
        }),
    )
}

fn string_array_field<'a>(name: &'a str, description: &str) -> (&'a str, Value) {
    (
        name,
        json!({
            "type": "array",
            "items": {"type": "string"},
            "description": description
        }),
    )
}

fn reward_field() -> (&'static str, Value) {
    (
        "reward",
        json!({
            "type": "object",
            "properties": {
                "agent_watt": {"type": "integer"},
                "reputation": {"type": "integer"},
                "capacity": {"type": "integer"},
                "treasury_share_watt": {"type": "integer"}
            },
            "required": ["agent_watt", "reputation", "capacity", "treasury_share_watt"],
            "additionalProperties": false,
            "description": "Mission reward."
        }),
    )
}

fn settlement_field() -> (&'static str, Value) {
    (
        "settlement",
        json!({
            "type": "object",
            "properties": {
                "layer": {"type": "string", "enum": ["web2", "web3"]},
                "rail": {"type": "string"},
                "request": {}
            },
            "required": ["rail"],
            "additionalProperties": false,
            "description": "Optional ServiceNet settlement request."
        }),
    )
}
