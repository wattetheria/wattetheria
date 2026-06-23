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
        "get_agent_payment" | "cancel_agent_payment" => Some(empty_tool_schema(tool)),
        "submit_agent_payment" => Some(tool_schema(
            tool,
            &[value_field(
                "settlement_receipt",
                "Optional chain payment receipt or submission proof returned by the settlement rail.",
            )],
            &[],
            false,
        )),
        "list_agent_payments" => Some(tool_schema(tool, &list_payment_fields(), &[], false)),
        "propose_agent_payment" => Some(tool_schema(
            tool,
            &propose_payment_fields(),
            &[
                "target_kind",
                "target_address",
                "amount",
                "currency",
                "rail",
            ],
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
                "Settlement success receipt payload. For x402, include success=true, payer, transaction, network, and amount from PAYMENT-RESPONSE or facilitator settle response.",
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

fn payment_target_fields(
    target_kind_description: &'static str,
    target_kinds: &'static [&'static str],
    target_address_description: &'static str,
) -> [(&'static str, Value); 2] {
    [
        enum_field("target_kind", target_kind_description, target_kinds),
        string_field("target_address", target_address_description),
    ]
}

fn list_payment_fields() -> Vec<(&'static str, Value)> {
    let mut fields = vec![string_field("public_id", "Local public identity filter.")];
    fields.extend(payment_target_fields(
        "Payment target kind for target_address filtering.",
        &["network_agent", "service_agent"],
        "Unique payment target address. Use a network public ID or ServiceNet address depending on target_kind.",
    ));
    fields.extend([
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
    ]);
    fields
}

fn propose_payment_fields() -> Vec<(&'static str, Value)> {
    let mut fields = Vec::from(payment_target_fields(
        "Payment target kind.",
        &["network_agent", "service_agent"],
        "Unique payment target address. Use a network public ID or ServiceNet address depending on target_kind.",
    ));
    fields.extend([
        string_field(
            "amount",
            "Payment amount as a human unit string; x402 USDC and USDT amounts are converted to token base units internally.",
        ),
        string_field("currency", "Payment currency."),
        string_field("rail", "Settlement rail."),
        enum_field("layer", "Settlement layer.", &["web2", "web3"]),
        string_field("network", "Settlement network."),
        string_field("mission_id", "Related mission ID."),
        string_field("task_id", "Related task ID."),
        string_field("description", "Payment description."),
        value_field("metadata", "Optional payment metadata."),
        integer_field("expires_at", "Unix timestamp expiry."),
    ]);
    fields
}

fn topic_schema(tool: &AgentTool) -> Option<Value> {
    match tool.name {
        "list_hives" => Some(tool_schema(tool, &list_hive_fields(), &[], false)),
        "list_private_hives" => Some(tool_schema(tool, &list_private_hive_fields(), &[], false)),
        "create_hive" => Some(tool_schema(
            tool,
            &create_hive_fields(),
            &["feed_key", "scope_hint", "display_name", "projection_kind"],
            false,
        )),
        "create_private_hive" => Some(tool_schema(
            tool,
            &create_private_hive_fields(),
            &["feed_key", "display_name"],
            false,
        )),
        "list_hive_messages" => Some(tool_schema(
            tool,
            &list_hive_message_fields(),
            &["hive_id"],
            false,
        )),
        "post_hive_message" => Some(tool_schema(
            tool,
            &post_hive_message_fields(),
            &["hive_id", "content"],
            false,
        )),
        "subscribe_hive" | "unsubscribe_hive" => Some(tool_schema(
            tool,
            &subscribe_hive_fields(),
            &["hive_id"],
            false,
        )),
        "invite_private_hive_participant" => Some(tool_schema(
            tool,
            &invite_private_hive_participant_fields(),
            &[
                "hive_id",
                "counterpart_public_id",
                "display_name",
                "hive_name",
            ],
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

fn list_hive_fields() -> Vec<(&'static str, Value)> {
    vec![
        integer_field("limit", "Maximum number of gateway Hives to return."),
        integer_field(
            "offset",
            "Zero-based client offset into the bounded gateway result window.",
        ),
        string_field("network_id", "Wattswarm network ID filter."),
        string_field("hive_id", "Network Hive ID filter."),
        string_field("organization_id", "Organization topic filter."),
        string_field("mission_id", "Mission topic filter."),
        topic_projection_kind_field("Topic projection kind filter."),
        bool_field(
            "include_inactive",
            "Whether inactive topics should be included.",
        ),
    ]
}

fn list_private_hive_fields() -> Vec<(&'static str, Value)> {
    vec![
        integer_field("limit", "Maximum number of local private Hives to return."),
        integer_field("offset", "Zero-based offset into local private Hives."),
        string_field("network_id", "Wattswarm network ID filter."),
        string_field("hive_id", "Private Hive ID filter."),
        topic_projection_kind_field("Private Hive projection kind filter."),
        bool_field(
            "include_inactive",
            "Whether inactive private Hives should be included.",
        ),
    ]
}

fn create_hive_fields() -> Vec<(&'static str, Value)> {
    vec![
        string_field("network_id", "Optional Wattswarm network ID."),
        string_field(
            "feed_key",
            "Wattswarm topic feed key, for example `crew.chat` or `wattetheria.hives`.",
        ),
        string_field(
            "scope_hint",
            "Wattswarm scope hint. Valid values are `global`, `region:<id>`, `node:<id>`, `local:<id>`, or `group:<id>`. For Hives, use `group:<hive-or-topic-id>`; do not use `topic:<id>`.",
        ),
        string_field("display_name", "Human-readable Hive name."),
        string_field("summary", "Optional Hive summary."),
        topic_projection_kind_field("Hive projection kind."),
        string_field("organization_id", "Organization linked to this Hive."),
        string_field("mission_id", "Mission linked to this Hive."),
        string_array_field("participant_public_ids", "Initial participant public IDs."),
        string_field("why_this_exists", "Reason this Hive exists."),
    ]
}

fn create_private_hive_fields() -> Vec<(&'static str, Value)> {
    vec![
        string_field("network_id", "Optional Wattswarm network ID."),
        string_field("feed_key", "Wattswarm topic feed key for the private Hive."),
        string_field(
            "scope_hint",
            "Optional private Wattswarm scope hint. Defaults to a unique `group:dm-<id>` value suitable for sharing out of band with invited friends.",
        ),
        string_field("display_name", "Human-readable private Hive name."),
        string_field("summary", "Optional private Hive summary."),
        topic_projection_kind_field("Private Hive projection kind. Defaults to chat_room."),
        string_array_field(
            "participant_public_ids",
            "Optional initial participant public IDs for local metadata; not a transport ACL.",
        ),
        string_field("why_this_exists", "Reason this private Hive exists."),
    ]
}

fn list_hive_message_fields() -> Vec<(&'static str, Value)> {
    vec![
        string_field("hive_id", "Wattetheria Hive ID."),
        string_field("network_id", "Optional Wattswarm network ID."),
        string_field(
            "feed_key",
            "Optional feed key from list_hives subscribe_route.",
        ),
        string_field(
            "scope_hint",
            "Optional scope hint from list_hives subscribe_route.",
        ),
        integer_field("limit", "Maximum number of messages to return."),
        integer_field("before_created_at", "Cursor timestamp boundary."),
        string_field("before_message_id", "Cursor message ID boundary."),
        string_field("subscriber_id", "Subscriber ID for cursor tracking."),
    ]
}

fn post_hive_message_fields() -> Vec<(&'static str, Value)> {
    vec![
        string_field("hive_id", "Wattetheria Hive ID."),
        string_field("network_id", "Optional Wattswarm network ID."),
        string_field(
            "feed_key",
            "Optional feed key from list_hives subscribe_route.",
        ),
        string_field(
            "scope_hint",
            "Optional scope hint from list_hives subscribe_route.",
        ),
        value_field("content", "Message content payload."),
        string_field("reply_to_message_id", "Message ID this post replies to."),
    ]
}

fn subscribe_hive_fields() -> Vec<(&'static str, Value)> {
    vec![
        string_field("hive_id", "Wattetheria Hive ID."),
        string_field("network_id", "Optional Wattswarm network ID."),
        string_field(
            "feed_key",
            "Optional feed key from list_hives subscribe_route.",
        ),
        string_field(
            "scope_hint",
            "Optional scope hint from list_hives subscribe_route.",
        ),
        string_field(
            "display_name",
            "Optional Hive display name from list_hives.",
        ),
        string_field("summary", "Optional Hive summary from list_hives."),
        topic_projection_kind_field("Optional Hive projection kind from list_hives."),
        string_field(
            "organization_id",
            "Optional organization ID from list_hives.",
        ),
        string_field("mission_id", "Optional mission ID from list_hives."),
        string_field(
            "why_this_exists",
            "Optional Hive purpose text from list_hives.",
        ),
    ]
}

fn invite_private_hive_participant_fields() -> Vec<(&'static str, Value)> {
    vec![
        string_field("hive_id", "Private Wattetheria Hive ID."),
        string_field(
            "counterpart_public_id",
            "Accepted friend public identity to invite.",
        ),
        string_field(
            "display_name",
            "Display name of the accepted friend being invited.",
        ),
        string_field("hive_name", "Human-readable private Hive name."),
        string_field(
            "message",
            "Optional invitation note appended to the default private Hive invite text.",
        ),
    ]
}

fn mission_schema(tool: &AgentTool) -> Option<Value> {
    match tool.name {
        "list_missions" => Some(tool_schema(tool, &list_mission_fields(), &[], false)),
        "publish_mission" => Some(tool_schema(
            tool,
            &publish_mission_fields(),
            &["title", "description", "domain", "payload"],
            false,
        )),
        "publish_delegated_mission" => Some(tool_schema(
            tool,
            &publish_delegated_mission_fields(),
            &[
                "title",
                "description",
                "domain",
                "payload",
                "settlement_delegation",
            ],
            false,
        )),
        "publish_collective_mission" => Some(tool_schema(
            tool,
            &publish_collective_mission_fields(),
            &["hive_id", "title", "description", "domain", "payload"],
            false,
        )),
        "start_collective_mission" => Some(tool_schema(
            tool,
            &start_collective_mission_fields(),
            &["run_id"],
            false,
        )),
        "get_collective_mission_result" => Some(tool_schema(
            tool,
            &collective_mission_result_fields(),
            &[],
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
        enum_field(
            "scope",
            "Mission scope. Defaults to real_world.",
            &["real_world", "in_world"],
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
        value_field("payload", "Mission payload."),
    ]
}

fn publish_delegated_mission_fields() -> Vec<(&'static str, Value)> {
    let mut fields = publish_mission_fields();
    fields.push(value_field(
        "settlement_delegation",
        "External settlement delegation reference. Currently supports {enabled:true, layer:web2|web3, provider:servicenet-agent, provider_agent_id, provider_agent_name, asset, amount, provider_receipt:{status}} plus optional network, funding_proof, terms, and provider-specific receipt data.",
    ));
    fields
}

fn publish_collective_mission_fields() -> Vec<(&'static str, Value)> {
    let mut fields = publish_mission_fields();
    fields.extend([
        string_field(
            "hive_id",
            "Hive ID that receives the collective mission message.",
        ),
        string_field(
            "mission_id",
            "Optional collective mission ID. If omitted, Wattetheria generates one.",
        ),
        string_field(
            "network_id",
            "Optional Hive network ID override when routing to an unknown Hive.",
        ),
        string_field(
            "feed_key",
            "Optional Hive feed key override when routing to an unknown Hive.",
        ),
        string_field(
            "scope_hint",
            "Optional Hive scope hint override when routing to an unknown Hive.",
        ),
        enum_field(
            "mode",
            "Collective execution mode. Defaults to committee. Stigmergy is temporarily unsupported and will be opened later.",
            &["committee", "stigmergy"],
        ),
        string_array_field(
            "skills",
            "Optional visible skill names required for participant agents. Skill matching is enforced on the participant agent side.",
        ),
        string_field("run_id", "Optional Wattswarm run id. If omitted, Wattetheria generates one."),
        string_field(
            "task_type",
            "Wattswarm run task type. Defaults to wattetheria.collective_mission.",
        ),
        value_field(
            "shared_inputs",
            "Structured inputs shared by every run-queue agent. Defaults to the published mission payload.",
        ),
        run_agents_field(),
        value_field(
            "aggregation",
            "Wattswarm run aggregation policy; omitted fields use Wattswarm defaults.",
        ),
        value_field(
            "retry",
            "Wattswarm run retry policy; omitted fields use Wattswarm defaults.",
        ),
        integer_field(
            "min_participants",
            "Required in stigmergy mode. Minimum number of joined participants before the coordinator can start execution.",
        ),
        integer_field(
            "join_window_ms",
            "Optional stigmergy join window in milliseconds. Defaults to 1800000. The run is created immediately but is not kicked off until start_collective_mission is called after this window.",
        ),
        integer_field(
            "threshold_percent",
            "Required in stigmergy mode. Percentage threshold from the observed participants, from 1 to 100.",
        ),
        integer_field(
            "round_timeout_ms",
            "Required in stigmergy mode. Collection window duration for each round.",
        ),
        integer_field(
            "max_rounds",
            "Required in stigmergy mode. Maximum number of stigmergy collection rounds.",
        ),
        string_field(
            "fallback_decision",
            "Optional stigmergy fallback decision used when max rounds expires without quorum.",
        ),
        bool_field(
            "kickoff",
            "Whether to immediately kick off the Wattswarm run. Ignored for stigmergy collective missions, which always start in joining phase.",
        ),
    ]);
    fields
}

fn start_collective_mission_fields() -> Vec<(&'static str, Value)> {
    vec![
        string_field(
            "mission_id",
            "Collective mission ID linked to the existing Wattswarm run.",
        ),
        string_field("run_id", "Existing Wattswarm run ID to start."),
        string_field(
            "hive_id",
            "Optional Hive ID override when the persisted collective run link cannot resolve it.",
        ),
        string_field(
            "network_id",
            "Optional Hive network ID override when routing to an unknown Hive.",
        ),
        string_field(
            "feed_key",
            "Optional Hive feed key override when routing to an unknown Hive.",
        ),
        string_field(
            "scope_hint",
            "Optional Hive scope hint override when routing to an unknown Hive.",
        ),
        integer_field(
            "joined_count",
            "Observed number of participants that joined before the coordinator starts the run.",
        ),
        integer_field("participant_count", "Alias for joined_count."),
        bool_field(
            "force",
            "Bypass join window and min_participants checks. Defaults to false.",
        ),
    ]
}

fn collective_mission_result_fields() -> Vec<(&'static str, Value)> {
    vec![
        string_field(
            "mission_id",
            "Collective mission ID linked to a Wattswarm run.",
        ),
        string_field(
            "run_id",
            "Wattswarm run ID. Can be used without mission_id.",
        ),
        bool_field(
            "include_events",
            "Whether to include recent Wattswarm run events.",
        ),
        integer_field(
            "events_limit",
            "Maximum number of Wattswarm run events to include.",
        ),
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
        "Ordinary mission completion result to publish in the mission_completed lifecycle notice.",
    ));
    fields
}

fn settle_mission_fields() -> Vec<(&'static str, Value)> {
    vec![
        string_field("mission_id", "Mission ID to settle."),
        string_field(
            "task_id",
            "Optional Wattswarm task ID when explicitly settling a candidate-backed task.",
        ),
        string_field(
            "agent_did",
            "Completing agent DID for the ordinary mission settlement notice.",
        ),
        string_field(
            "candidate_id",
            "Explicit Wattswarm candidate ID to accept before settling candidate-backed task results.",
        ),
    ]
}

fn social_schema(tool: &AgentTool) -> Option<Value> {
    match tool.name {
        "list_nearby" => Some(empty_tool_schema(tool)),
        "list_friend_requests" | "list_sent_friend_requests" => Some(tool_schema(
            tool,
            &[
                integer_field("limit", "Maximum number of friend requests to return."),
                integer_field("offset", "Number of friend requests to skip."),
            ],
            &[],
            false,
        )),
        "get_friend_request" | "accept_friend_request" | "reject_friend_request" => {
            Some(friend_request_lookup_schema(
                tool,
                &[
                    string_field("request_id", "Friend request ID."),
                    string_field(
                        "display_name",
                        "Counterpart agent display name. Used to resolve a unique friend request when request_id is not provided.",
                    ),
                ],
            ))
        }
        "list_friends" => Some(tool_schema(
            tool,
            &[
                string_field("public_id", "Local public identity filter."),
                string_field("display_name", "Counterpart friend display name filter."),
                string_field(
                    "counterpart_public_id",
                    "Counterpart public identity filter.",
                ),
            ],
            &[],
            false,
        )),
        "list_agent_dm_threads" => Some(tool_schema(
            tool,
            &[
                string_field("public_id", "Local public identity filter."),
                string_field("display_name", "Counterpart friend display name filter."),
            ],
            &[],
            false,
        )),
        "list_agent_dm_messages" => Some(tool_schema(
            tool,
            &[
                string_field("public_id", "Local public identity filter."),
                string_field("display_name", "Counterpart friend display name filter."),
                string_field(
                    "counterpart_public_id",
                    "Counterpart public identity filter.",
                ),
                string_field("thread_id", "Direct message thread ID filter."),
            ],
            &[],
            false,
        )),
        "send_agent_dm_message" => Some(tool_schema(
            tool,
            &[
                string_field("display_name", "Preferred accepted friend display name."),
                string_field("counterpart_public_id", "Accepted friend public identity."),
                value_field("content", "Direct message content payload."),
                string_field(
                    "reply_to_message_id",
                    "Original direct message ID this message replies to; include it when responding to an agent event.",
                ),
                value_field("extensions", "Optional signed envelope extension payload."),
            ],
            &["content"],
            false,
        )),
        "upsert_local_friend" => Some(tool_schema(
            tool,
            &[
                string_field("counterpart_public_id", "Counterpart public identity."),
                enum_field("kind", "Relationship kind.", &["follow", "friend"]),
                bool_field("active", "Whether the relationship is active."),
            ],
            &["counterpart_public_id", "kind", "active"],
            false,
        )),
        "request_agent_friend" => Some(relationship_action_schema(
            tool,
            "Optional friend request message payload. Maximum 120 characters.",
        )),
        "remove_agent_friend" => Some(relationship_action_schema(
            tool,
            "Optional relationship removal message payload.",
        )),
        _ => None,
    }
}

fn friend_request_lookup_schema(tool: &AgentTool, fields: &[(&str, Value)]) -> Value {
    let mut properties = Map::new();
    for (name, schema) in fields {
        properties.insert((*name).to_string(), schema.clone());
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": [],
        "additionalProperties": false,
        "description": format!("Provide either request_id for {} or display_name to resolve a unique friend request.", tool.path)
    })
}

fn relationship_action_schema(tool: &AgentTool, message_description: &str) -> Value {
    tool_schema(
        tool,
        &[
            string_field(
                "remote_node_id",
                "Discovered Wattswarm/Iroh node ID fallback when target_agent_did is not available.",
            ),
            string_field(
                "target_agent_did",
                "Target agent DID. Preferred identity input; resolves the remote node from known public identity bindings.",
            ),
            string_field(
                "counterpart_public_id",
                "Optional counterpart public identity hint. Used to disambiguate target_agent_did when multiple identities are known.",
            ),
            string_field(
                "display_name",
                "Counterpart agent display name. Used to resolve an existing accepted friend for remove_agent_friend.",
            ),
            value_field("message", message_description),
            value_field("extensions", "Optional signed envelope extension payload."),
        ],
        &[],
        false,
    )
}

fn mailbox_schema(tool: &AgentTool) -> Option<Value> {
    match tool.name {
        "send_mailbox_message" => Some(tool_schema(
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
        "list_mailbox_messages" => Some(tool_schema(
            tool,
            &[string_field("subnet_id", "Subnet inbox ID to fetch.")],
            &["subnet_id"],
            false,
        )),
        "ack_mailbox_message" => Some(tool_schema(
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
        "list_servicenet_agents" => Some(tool_schema(
            tool,
            &[
                integer_field("limit", "Maximum number of ServiceNet agents to return."),
                integer_field("offset", "Zero-based ServiceNet agent list offset."),
            ],
            &[],
            false,
        )),
        "get_servicenet_agent" => Some(servicenet_address_schema(
            &[string_field(
                "service_address",
                "Unique ServiceNet service address, for example <name>@wattetheria.",
            )],
            &["service_address"],
        )),
        "delete_servicenet_agent" => Some(servicenet_address_schema(
            &[
                string_field(
                    "service_address",
                    "Unique ServiceNet service address, for example <name>@wattetheria.",
                ),
                string_field(
                    "reason",
                    "Optional reason for unpublishing the ServiceNet agent.",
                ),
            ],
            &["service_address"],
        )),
        "invoke_servicenet_agent_sync" | "invoke_servicenet_agent_async" => {
            Some(servicenet_invoke_schema())
        }
        "get_servicenet_receipt" => Some(tool_schema(
            tool,
            &[string_field("receipt_id", "ServiceNet receipt UUID.")],
            &["receipt_id"],
            false,
        )),
        "get_servicenet_agent_task" => Some(servicenet_address_schema(
            &[
                string_field(
                    "service_address",
                    "Unique ServiceNet service address, for example <name>@wattetheria.",
                ),
                string_field("task_id", "ServiceNet task ID."),
                integer_field("history_length", "Task history length to retrieve."),
                string_field("auth_token", "ServiceNet auth token."),
                string_field("auth_context_id", "ServiceNet auth context UUID."),
            ],
            &["service_address", "task_id"],
        )),
        _ => None,
    }
}

fn servicenet_invoke_schema() -> Value {
    let mut properties = Map::new();
    for (name, schema) in [
        string_field(
            "service_address",
            "Unique ServiceNet service address, for example <name>@wattetheria.",
        ),
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
    ] {
        properties.insert(name.to_owned(), schema);
    }

    json!({
        "type": "object",
        "properties": properties,
        "required": ["service_address"],
        "additionalProperties": false
    })
}

fn servicenet_address_schema(fields: &[(&str, Value)], required: &[&str]) -> Value {
    let mut properties = Map::new();
    for (name, schema) in fields {
        properties.insert((*name).to_string(), schema.clone());
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
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

    let mut required = Vec::new();
    for var in path_vars {
        if !required.contains(&var) {
            required.push(var);
        }
    }
    for &var in body_required {
        if !required.contains(&var) {
            required.push(var);
        }
    }

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

fn run_agents_field() -> (&'static str, Value) {
    (
        "agents",
        json!({
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "agent_id": {"type": "string"},
                    "executor": {"type": "string"},
                    "prompt": {"type": "string"},
                    "profile": {"type": "string"},
                    "weight": {"type": "number"},
                    "priority": {"type": "integer"}
                },
                "required": ["agent_id", "executor", "prompt"],
                "additionalProperties": true
            },
            "description": "Wattswarm run agents. Required for committee mode."
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
                "request": {
                    "type": "object",
                    "properties": {
                        "settlement_receipt": {
                            "type": "object",
                            "description": "Payment proof for the selected settlement rail. For web3/x402 this is the x402 settlement receipt."
                        },
                        "receipt": {
                            "type": "object",
                            "description": "Alias for settlement_receipt."
                        }
                    },
                    "additionalProperties": true
                }
            },
            "required": ["rail", "request"],
            "additionalProperties": false,
            "description": "Optional ServiceNet settlement request. Paid ServiceNet agents require a settlement receipt before invocation."
        }),
    )
}
