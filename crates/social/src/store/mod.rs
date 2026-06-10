mod deferred_agent_events;
mod reliability_tasks;
mod schema;

use crate::domain::blocks::SocialBlock;
use crate::domain::friend_requests::{FriendRequest, FriendRequestDirection, FriendRequestState};
use crate::domain::friendships::{Friendship, FriendshipState};
use crate::domain::identities::RemoteIdentityProfile;
use crate::domain::messages::{
    DeliveryState, DirectMessage, MessageDirection, MessageKind, ReadState,
};
use crate::domain::receipts::{MessageReceipt, ReceiptKind};
use crate::domain::threads::{DirectThread, ThreadState};
use crate::domain::transport_bindings::{RemoteTransportBinding, TransportKind};
use crate::policy::decisions::{PolicyDecision, PolicyDecisionLog};
use crate::policy::rules::{PolicyRule, PolicyRuleType, PolicyScope};
use crate::ports::repositories::{
    BlockRepository, FriendRequestRepository, FriendshipRepository, MessageReceiptRepository,
    MessageRepository, PolicyDecisionLogRepository, PolicyRuleRepository, RemoteIdentityRepository,
    ThreadRepository, TransportBindingRepository,
};
use crate::types::{SocialError, SocialResult};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

pub struct SocialStore {
    conn: Mutex<Connection>,
}

impl SocialStore {
    pub fn open(path: impl AsRef<Path>) -> SocialResult<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| SocialError::Storage(format!("create store dir: {error}")))?;
        }
        let conn = Connection::open(path.as_ref())
            .map_err(|error| SocialError::Storage(format!("open sqlite: {error}")))?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn import_legacy_db(&self, path: impl AsRef<Path>) -> SocialResult<()> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(());
        }
        let path = path.to_string_lossy().to_string();
        let conn = self.conn()?;
        conn.execute("ATTACH DATABASE ?1 AS legacy_social", [path.as_str()])
            .map_err(|error| SocialError::Storage(format!("attach legacy social db: {error}")))?;
        let result = import_legacy_social_tables(&conn);
        let detach_result = conn.execute("DETACH DATABASE legacy_social", []);
        if let Err(error) = detach_result {
            return Err(SocialError::Storage(format!(
                "detach legacy social db: {error}"
            )));
        }
        result
    }

    pub fn open_in_memory() -> SocialResult<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|error| SocialError::Storage(format!("open in-memory sqlite: {error}")))?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> SocialResult<()> {
        let conn = self.conn()?;
        schema::migrate(&conn)
    }

    fn conn(&self) -> SocialResult<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| SocialError::Storage("sqlite mutex poisoned".to_owned()))
    }
}

fn import_legacy_social_tables(conn: &Connection) -> SocialResult<()> {
    for table in [
        "public_identities",
        "public_transport_bindings",
        "friend_requests",
        "friendships",
        "public_blocks",
        "dm_threads",
        "dm_messages",
        "dm_message_receipts",
        "policy_rules",
        "policy_decision_logs",
    ] {
        if !legacy_table_exists(conn, table)? {
            continue;
        }
        conn.execute(
            &format!("INSERT OR IGNORE INTO {table} SELECT * FROM legacy_social.{table}"),
            [],
        )
        .map_err(|error| {
            SocialError::Storage(format!("import legacy social table {table}: {error}"))
        })?;
    }
    Ok(())
}

fn legacy_table_exists(conn: &Connection, table: &str) -> SocialResult<bool> {
    conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM legacy_social.sqlite_master
            WHERE type = 'table' AND name = ?1
        )",
        [table],
        |row| row.get::<_, i64>(0),
    )
    .map(|exists| exists == 1)
    .map_err(|error| SocialError::Storage(format!("query legacy table {table}: {error}")))
}

impl RemoteIdentityRepository for SocialStore {
    fn upsert_remote_identity(&self, identity: &RemoteIdentityProfile) -> SocialResult<()> {
        let capabilities = serde_json::to_string(&identity.capabilities)
            .map_err(|error| SocialError::Storage(format!("serialize capabilities: {error}")))?;
        let skills = serde_json::to_string(&identity.skills)
            .map_err(|error| SocialError::Storage(format!("serialize skills: {error}")))?;
        self.conn()?
            .execute(
                "INSERT INTO public_identities (
                    public_id, agent_did, display_name, description, capabilities_json, skills_json,
                    did_document_json, active, last_profile_fetched_at, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(public_id) DO UPDATE SET
                    agent_did = excluded.agent_did,
                    display_name = excluded.display_name,
                    description = excluded.description,
                    capabilities_json = excluded.capabilities_json,
                    skills_json = excluded.skills_json,
                    did_document_json = excluded.did_document_json,
                    active = excluded.active,
                    last_profile_fetched_at = excluded.last_profile_fetched_at,
                    updated_at = excluded.updated_at",
                params![
                    identity.public_id,
                    identity.agent_did,
                    identity.display_name,
                    identity.description,
                    capabilities,
                    skills,
                    identity
                        .did_document_json
                        .as_ref()
                        .map(serde_json::Value::to_string),
                    bool_to_sqlite(identity.active),
                    identity.last_profile_fetched_at,
                    identity.created_at,
                    identity.updated_at
                ],
            )
            .map_err(|error| SocialError::Storage(format!("upsert remote identity: {error}")))?;
        Ok(())
    }

    fn list_remote_identities(&self) -> SocialResult<Vec<RemoteIdentityProfile>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT public_id, agent_did, display_name, description, capabilities_json,
                        skills_json, did_document_json, active, last_profile_fetched_at, created_at, updated_at
                 FROM public_identities
                 ORDER BY updated_at DESC, public_id ASC",
            )
            .map_err(|error| {
                SocialError::Storage(format!("prepare remote identities query: {error}"))
            })?;
        let rows = stmt
            .query_map([], row_to_remote_identity)
            .map_err(|error| SocialError::Storage(format!("query remote identities: {error}")))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| SocialError::Storage(format!("collect remote identities: {error}")))
    }

    fn get_remote_identity(&self, public_id: &str) -> SocialResult<Option<RemoteIdentityProfile>> {
        self.conn()?
            .query_row(
                "SELECT public_id, agent_did, display_name, description, capabilities_json,
                        skills_json, did_document_json, active, last_profile_fetched_at, created_at, updated_at
                 FROM public_identities
                 WHERE public_id = ?1",
                params![public_id],
                row_to_remote_identity,
            )
            .optional()
            .map_err(|error| SocialError::Storage(format!("query remote identity: {error}")))
    }
}

impl TransportBindingRepository for SocialStore {
    fn upsert_transport_binding(&self, binding: &RemoteTransportBinding) -> SocialResult<()> {
        self.conn()?
            .execute(
                "INSERT INTO public_transport_bindings (
                    public_id, transport_node_id, transport_kind, agent_did, binding_source,
                    binding_confidence, binding_proof_json, binding_verified, binding_verified_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(public_id, transport_node_id) DO UPDATE SET
                    transport_kind = excluded.transport_kind,
                    agent_did = excluded.agent_did,
                    binding_source = excluded.binding_source,
                    binding_confidence = excluded.binding_confidence,
                    binding_proof_json = excluded.binding_proof_json,
                    binding_verified = excluded.binding_verified,
                    binding_verified_at = excluded.binding_verified_at,
                    updated_at = excluded.updated_at",
                params![
                    binding.public_id,
                    binding.transport_node_id,
                    transport_kind_to_str(binding.transport_kind),
                    binding.agent_did,
                    binding.binding_source,
                    binding.binding_confidence,
                    binding
                        .binding_proof_json
                        .as_ref()
                        .map(serde_json::Value::to_string),
                    bool_to_sqlite(binding.binding_verified),
                    binding.binding_verified_at,
                    binding.updated_at,
                ],
            )
            .map_err(|error| SocialError::Storage(format!("upsert transport binding: {error}")))?;
        Ok(())
    }

    fn list_transport_bindings(&self) -> SocialResult<Vec<RemoteTransportBinding>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT public_id, transport_node_id, transport_kind, agent_did, binding_source,
                        binding_confidence, binding_proof_json, binding_verified, binding_verified_at, updated_at
                 FROM public_transport_bindings
                 ORDER BY updated_at DESC, public_id ASC, transport_node_id ASC",
            )
            .map_err(|error| {
                SocialError::Storage(format!("prepare transport bindings query: {error}"))
            })?;
        let rows = stmt
            .query_map([], row_to_transport_binding)
            .map_err(|error| SocialError::Storage(format!("query transport bindings: {error}")))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| SocialError::Storage(format!("collect transport bindings: {error}")))
    }

    fn list_transport_bindings_for_public_id(
        &self,
        public_id: &str,
    ) -> SocialResult<Vec<RemoteTransportBinding>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT public_id, transport_node_id, transport_kind, agent_did, binding_source,
                        binding_confidence, binding_proof_json, binding_verified, binding_verified_at, updated_at
                 FROM public_transport_bindings
                 WHERE public_id = ?1
                 ORDER BY updated_at DESC, transport_node_id ASC",
            )
            .map_err(|error| {
                SocialError::Storage(format!(
                    "prepare transport bindings by public_id query: {error}"
                ))
            })?;
        let rows = stmt
            .query_map(params![public_id], row_to_transport_binding)
            .map_err(|error| {
                SocialError::Storage(format!("query transport bindings by public_id: {error}"))
            })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|error| {
            SocialError::Storage(format!("collect transport bindings by public_id: {error}"))
        })
    }
}

impl PolicyRuleRepository for SocialStore {
    fn upsert_policy_rule(&self, rule: &PolicyRule) -> SocialResult<()> {
        self.conn()?
            .execute(
                "INSERT INTO policy_rules (
                    rule_id, owner_public_id, rule_type, scope, matcher_json, config_json, priority,
                    enabled, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(rule_id) DO UPDATE SET
                    owner_public_id = excluded.owner_public_id,
                    rule_type = excluded.rule_type,
                    scope = excluded.scope,
                    matcher_json = excluded.matcher_json,
                    config_json = excluded.config_json,
                    priority = excluded.priority,
                    enabled = excluded.enabled,
                    updated_at = excluded.updated_at",
                params![
                    rule.rule_id,
                    rule.owner_public_id,
                    policy_rule_type_to_str(rule.rule_type),
                    policy_scope_to_str(rule.scope),
                    rule.matcher_json.to_string(),
                    rule.config_json.to_string(),
                    rule.priority,
                    bool_to_sqlite(rule.enabled),
                    rule.created_at,
                    rule.updated_at,
                ],
            )
            .map_err(|error| SocialError::Storage(format!("upsert policy rule: {error}")))?;
        Ok(())
    }

    fn list_policy_rules(&self, owner_public_id: Option<&str>) -> SocialResult<Vec<PolicyRule>> {
        let conn = self.conn()?;
        let mut rules = Vec::new();
        let sql = if owner_public_id.is_some() {
            "SELECT rule_id, owner_public_id, rule_type, scope, matcher_json, config_json, priority, enabled,
                    created_at, updated_at
             FROM policy_rules
             WHERE owner_public_id = ?1
             ORDER BY priority ASC, rule_id ASC"
        } else {
            "SELECT rule_id, owner_public_id, rule_type, scope, matcher_json, config_json, priority, enabled,
                    created_at, updated_at
             FROM policy_rules
             ORDER BY priority ASC, rule_id ASC"
        };
        let mut stmt = conn.prepare(sql).map_err(|error| {
            SocialError::Storage(format!("prepare policy rules query: {error}"))
        })?;
        let mut rows = if let Some(owner_public_id) = owner_public_id {
            stmt.query(params![owner_public_id])
        } else {
            stmt.query([])
        }
        .map_err(|error| SocialError::Storage(format!("query policy rules: {error}")))?;

        while let Some(row) = rows
            .next()
            .map_err(|error| SocialError::Storage(format!("iterate policy rules: {error}")))?
        {
            rules.push(row_to_policy_rule(row)?);
        }

        Ok(rules)
    }
}

impl PolicyDecisionLogRepository for SocialStore {
    fn append_policy_decision_log(&self, log: &PolicyDecisionLog) -> SocialResult<()> {
        self.conn()?
            .execute(
                "INSERT INTO policy_decision_logs (
                    decision_id, owner_public_id, scope, target_public_id, target_node_id,
                    rule_id, decision, reason, context_json, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    log.decision_id,
                    log.owner_public_id,
                    policy_scope_to_str(log.scope),
                    log.target_public_id,
                    log.target_node_id,
                    log.rule_id,
                    policy_decision_to_str(log.decision),
                    log.reason,
                    log.context_json.to_string(),
                    log.created_at
                ],
            )
            .map_err(|error| {
                SocialError::Storage(format!("append policy decision log: {error}"))
            })?;
        Ok(())
    }

    fn list_policy_decision_logs(
        &self,
        owner_public_id: &str,
    ) -> SocialResult<Vec<PolicyDecisionLog>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT decision_id, owner_public_id, scope, target_public_id, target_node_id,
                        rule_id, decision, reason, context_json, created_at
                 FROM policy_decision_logs
                 WHERE owner_public_id = ?1
                 ORDER BY created_at DESC, decision_id ASC",
            )
            .map_err(|error| {
                SocialError::Storage(format!("prepare policy decision logs query: {error}"))
            })?;
        let rows = stmt
            .query_map(params![owner_public_id], row_to_policy_decision_log)
            .map_err(|error| {
                SocialError::Storage(format!("query policy decision logs: {error}"))
            })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| SocialError::Storage(format!("collect policy decision logs: {error}")))
    }
}

impl FriendRequestRepository for SocialStore {
    fn upsert_friend_request(&self, request: &FriendRequest) -> SocialResult<()> {
        self.conn()?
            .execute(
                "INSERT INTO friend_requests (
                    request_id, local_public_id, remote_public_id, remote_node_id, direction, state,
                    decision_reason, correlation_id, created_at, updated_at, expires_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(request_id) DO UPDATE SET
                    local_public_id = excluded.local_public_id,
                    remote_public_id = excluded.remote_public_id,
                    remote_node_id = excluded.remote_node_id,
                    direction = excluded.direction,
                    state = excluded.state,
                    decision_reason = excluded.decision_reason,
                    correlation_id = excluded.correlation_id,
                    updated_at = excluded.updated_at,
                    expires_at = excluded.expires_at",
                params![
                    request.request_id,
                    request.local_public_id,
                    request.remote_public_id,
                    request.remote_node_id,
                    friend_request_direction_to_str(request.direction),
                    friend_request_state_to_str(request.state),
                    request.decision_reason,
                    request.correlation_id,
                    request.created_at,
                    request.updated_at,
                    request.expires_at,
                ],
            )
            .map_err(|error| SocialError::Storage(format!("upsert friend request: {error}")))?;
        Ok(())
    }

    fn list_friend_requests(&self, local_public_id: &str) -> SocialResult<Vec<FriendRequest>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT request_id, local_public_id, remote_public_id, remote_node_id, direction, state,
                        decision_reason, correlation_id, created_at, updated_at, expires_at
                 FROM friend_requests
                 WHERE local_public_id = ?1
                 ORDER BY updated_at DESC, request_id ASC",
            )
            .map_err(|error| {
                SocialError::Storage(format!("prepare friend requests query: {error}"))
            })?;
        let rows = stmt
            .query_map(params![local_public_id], row_to_friend_request)
            .map_err(|error| SocialError::Storage(format!("query friend requests: {error}")))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| SocialError::Storage(format!("collect friend requests: {error}")))
    }
}

impl FriendshipRepository for SocialStore {
    fn upsert_friendship(&self, friendship: &Friendship) -> SocialResult<()> {
        self.conn()?
            .execute(
                "INSERT INTO friendships (
                    friendship_id, local_public_id, remote_public_id, state,
                    established_from_request_id, thread_id, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(friendship_id) DO UPDATE SET
                    local_public_id = excluded.local_public_id,
                    remote_public_id = excluded.remote_public_id,
                    state = excluded.state,
                    established_from_request_id = excluded.established_from_request_id,
                    thread_id = excluded.thread_id,
                    updated_at = excluded.updated_at",
                params![
                    friendship.friendship_id,
                    friendship.local_public_id,
                    friendship.remote_public_id,
                    friendship_state_to_str(friendship.state),
                    friendship.established_from_request_id,
                    friendship.thread_id,
                    friendship.created_at,
                    friendship.updated_at,
                ],
            )
            .map_err(|error| SocialError::Storage(format!("upsert friendship: {error}")))?;
        Ok(())
    }

    fn list_friendships(&self, local_public_id: &str) -> SocialResult<Vec<Friendship>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT friendship_id, local_public_id, remote_public_id, state,
                        established_from_request_id, thread_id, created_at, updated_at
                 FROM friendships
                 WHERE local_public_id = ?1
                 ORDER BY updated_at DESC, friendship_id ASC",
            )
            .map_err(|error| SocialError::Storage(format!("prepare friendships query: {error}")))?;
        let rows = stmt
            .query_map(params![local_public_id], row_to_friendship)
            .map_err(|error| SocialError::Storage(format!("query friendships: {error}")))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| SocialError::Storage(format!("collect friendships: {error}")))
    }

    fn find_friendship(
        &self,
        local_public_id: &str,
        remote_public_id: &str,
    ) -> SocialResult<Option<Friendship>> {
        self.conn()?
            .query_row(
                "SELECT friendship_id, local_public_id, remote_public_id, state,
                        established_from_request_id, thread_id, created_at, updated_at
                 FROM friendships
                 WHERE local_public_id = ?1 AND remote_public_id = ?2",
                params![local_public_id, remote_public_id],
                row_to_friendship,
            )
            .optional()
            .map_err(|error| SocialError::Storage(format!("query friendship: {error}")))
    }
}

impl BlockRepository for SocialStore {
    fn upsert_block(&self, block: &SocialBlock) -> SocialResult<()> {
        self.conn()?
            .execute(
                "INSERT INTO public_blocks (
                    block_id, owner_public_id, blocked_public_id, blocked_node_id, reason, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(block_id) DO UPDATE SET
                    owner_public_id = excluded.owner_public_id,
                    blocked_public_id = excluded.blocked_public_id,
                    blocked_node_id = excluded.blocked_node_id,
                    reason = excluded.reason,
                    updated_at = excluded.updated_at",
                params![
                    block.block_id,
                    block.owner_public_id,
                    block.blocked_public_id,
                    block.blocked_node_id,
                    block.reason,
                    block.created_at,
                    block.updated_at,
                ],
            )
            .map_err(|error| SocialError::Storage(format!("upsert social block: {error}")))?;
        Ok(())
    }

    fn remove_block(&self, owner_public_id: &str, blocked_public_id: &str) -> SocialResult<()> {
        self.conn()?
            .execute(
                "DELETE FROM public_blocks
                 WHERE owner_public_id = ?1 AND blocked_public_id = ?2",
                params![owner_public_id, blocked_public_id],
            )
            .map_err(|error| SocialError::Storage(format!("remove social block: {error}")))?;
        Ok(())
    }

    fn list_blocks(&self, owner_public_id: &str) -> SocialResult<Vec<SocialBlock>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT block_id, owner_public_id, blocked_public_id, blocked_node_id, reason, created_at, updated_at
                 FROM public_blocks
                 WHERE owner_public_id = ?1
                 ORDER BY updated_at DESC, block_id ASC",
            )
            .map_err(|error| {
                SocialError::Storage(format!("prepare social blocks query: {error}"))
            })?;
        let rows = stmt
            .query_map(params![owner_public_id], row_to_social_block)
            .map_err(|error| SocialError::Storage(format!("query social blocks: {error}")))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| SocialError::Storage(format!("collect social blocks: {error}")))
    }

    fn find_block(
        &self,
        owner_public_id: &str,
        blocked_public_id: &str,
    ) -> SocialResult<Option<SocialBlock>> {
        self.conn()?
            .query_row(
                "SELECT block_id, owner_public_id, blocked_public_id, blocked_node_id, reason, created_at, updated_at
                 FROM public_blocks
                 WHERE owner_public_id = ?1 AND blocked_public_id = ?2",
                params![owner_public_id, blocked_public_id],
                row_to_social_block,
            )
            .optional()
            .map_err(|error| SocialError::Storage(format!("query social block: {error}")))
    }
}

impl ThreadRepository for SocialStore {
    fn upsert_thread(&self, thread: &DirectThread) -> SocialResult<()> {
        self.conn()?
            .execute(
                "INSERT INTO dm_threads (
                    thread_id, local_public_id, remote_public_id, transport_thread_id, state,
                    last_message_at, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(thread_id) DO UPDATE SET
                    local_public_id = excluded.local_public_id,
                    remote_public_id = excluded.remote_public_id,
                    transport_thread_id = excluded.transport_thread_id,
                    state = excluded.state,
                    last_message_at = excluded.last_message_at,
                    updated_at = excluded.updated_at",
                params![
                    thread.thread_id,
                    thread.local_public_id,
                    thread.remote_public_id,
                    thread.transport_thread_id,
                    thread_state_to_str(thread.state),
                    thread.last_message_at,
                    thread.created_at,
                    thread.updated_at,
                ],
            )
            .map_err(|error| SocialError::Storage(format!("upsert thread: {error}")))?;
        Ok(())
    }

    fn list_threads(&self, local_public_id: &str) -> SocialResult<Vec<DirectThread>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT thread_id, local_public_id, remote_public_id, transport_thread_id, state,
                        last_message_at, created_at, updated_at
                 FROM dm_threads
                 WHERE local_public_id = ?1
                 ORDER BY updated_at DESC, thread_id ASC",
            )
            .map_err(|error| SocialError::Storage(format!("prepare thread query: {error}")))?;
        let rows = stmt
            .query_map(params![local_public_id], row_to_thread)
            .map_err(|error| SocialError::Storage(format!("query dm_threads: {error}")))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| SocialError::Storage(format!("collect dm_threads: {error}")))
    }

    fn find_thread(
        &self,
        local_public_id: &str,
        remote_public_id: &str,
    ) -> SocialResult<Option<DirectThread>> {
        self.conn()?
            .query_row(
                "SELECT thread_id, local_public_id, remote_public_id, transport_thread_id, state,
                        last_message_at, created_at, updated_at
                 FROM dm_threads
                 WHERE local_public_id = ?1 AND remote_public_id = ?2",
                params![local_public_id, remote_public_id],
                row_to_thread,
            )
            .optional()
            .map_err(|error| SocialError::Storage(format!("query thread: {error}")))
    }

    fn list_threads_by_state(
        &self,
        local_public_id: &str,
        state: ThreadState,
    ) -> SocialResult<Vec<DirectThread>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT thread_id, local_public_id, remote_public_id, transport_thread_id, state,
                        last_message_at, created_at, updated_at
                 FROM dm_threads
                 WHERE local_public_id = ?1 AND state = ?2
                 ORDER BY updated_at DESC, thread_id ASC",
            )
            .map_err(|error| {
                SocialError::Storage(format!("prepare thread by state query: {error}"))
            })?;
        let rows = stmt
            .query_map(
                params![local_public_id, thread_state_to_str(state)],
                row_to_thread,
            )
            .map_err(|error| SocialError::Storage(format!("query dm_threads by state: {error}")))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| SocialError::Storage(format!("collect dm_threads by state: {error}")))
    }
}

impl MessageRepository for SocialStore {
    fn upsert_message(&self, message: &DirectMessage) -> SocialResult<()> {
        self.conn()?
            .execute(
                "INSERT INTO dm_messages (
                    thread_id, message_id, transport_message_id, local_public_id, remote_public_id,
                    direction, message_kind, content_json, encrypted_body, content_encoding,
                    agent_envelope_json, agent_signature, delivery_state, read_state, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
                 ON CONFLICT(thread_id, message_id) DO UPDATE SET
                    transport_message_id = excluded.transport_message_id,
                    local_public_id = excluded.local_public_id,
                    remote_public_id = excluded.remote_public_id,
                    direction = excluded.direction,
                    message_kind = excluded.message_kind,
                    content_json = excluded.content_json,
                    encrypted_body = excluded.encrypted_body,
                    content_encoding = excluded.content_encoding,
                    agent_envelope_json = excluded.agent_envelope_json,
                    agent_signature = excluded.agent_signature,
                    delivery_state = excluded.delivery_state,
                    read_state = excluded.read_state,
                    updated_at = excluded.updated_at",
                params![
                    message.thread_id,
                    message.message_id,
                    message.transport_message_id,
                    message.local_public_id,
                    message.remote_public_id,
                    message_direction_to_str(message.direction),
                    message_kind_to_str(message.message_kind),
                    message.content_json.to_string(),
                    message.encrypted_body,
                    message.content_encoding,
                    message.agent_envelope_json.as_ref().map(serde_json::Value::to_string),
                    message.agent_signature,
                    delivery_state_to_str(message.delivery_state),
                    read_state_to_str(message.read_state),
                    message.created_at,
                    message.updated_at,
                ],
            )
            .map_err(|error| SocialError::Storage(format!("upsert message: {error}")))?;
        Ok(())
    }

    fn list_thread_messages(&self, thread_id: &str) -> SocialResult<Vec<DirectMessage>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT thread_id, message_id, transport_message_id, local_public_id, remote_public_id,
                        direction, message_kind, content_json, encrypted_body, content_encoding,
                        agent_envelope_json, agent_signature, delivery_state, read_state, created_at, updated_at
                 FROM dm_messages
                 WHERE thread_id = ?1
                 ORDER BY created_at ASC, message_id ASC",
            )
            .map_err(|error| SocialError::Storage(format!("prepare message query: {error}")))?;
        let rows = stmt
            .query_map(params![thread_id], row_to_message)
            .map_err(|error| SocialError::Storage(format!("query dm_messages: {error}")))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| SocialError::Storage(format!("collect dm_messages: {error}")))
    }

    fn get_message(
        &self,
        thread_id: &str,
        message_id: &str,
    ) -> SocialResult<Option<DirectMessage>> {
        self.conn()?
            .query_row(
                "SELECT thread_id, message_id, transport_message_id, local_public_id, remote_public_id,
                        direction, message_kind, content_json, encrypted_body, content_encoding,
                        agent_envelope_json, agent_signature, delivery_state, read_state, created_at, updated_at
                 FROM dm_messages
                 WHERE thread_id = ?1 AND message_id = ?2",
                params![thread_id, message_id],
                row_to_message,
            )
            .optional()
            .map_err(|error| SocialError::Storage(format!("query message: {error}")))
    }
}

impl MessageReceiptRepository for SocialStore {
    fn upsert_message_receipt(&self, receipt: &MessageReceipt) -> SocialResult<()> {
        self.conn()?
            .execute(
                "INSERT INTO dm_message_receipts (
                    message_id, receipt_kind, recorded_at, detail
                 ) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(message_id, receipt_kind, recorded_at) DO UPDATE SET
                    detail = excluded.detail",
                params![
                    receipt.message_id,
                    receipt_kind_to_str(receipt.receipt_kind),
                    receipt.recorded_at,
                    receipt.detail,
                ],
            )
            .map_err(|error| SocialError::Storage(format!("upsert message receipt: {error}")))?;
        Ok(())
    }

    fn list_message_receipts(&self, message_id: &str) -> SocialResult<Vec<MessageReceipt>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT message_id, receipt_kind, recorded_at, detail
                 FROM dm_message_receipts
                 WHERE message_id = ?1
                 ORDER BY recorded_at ASC, receipt_kind ASC",
            )
            .map_err(|error| {
                SocialError::Storage(format!("prepare message receipts query: {error}"))
            })?;
        let rows = stmt
            .query_map(params![message_id], row_to_message_receipt)
            .map_err(|error| SocialError::Storage(format!("query message receipts: {error}")))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| SocialError::Storage(format!("collect message receipts: {error}")))
    }
}

fn row_to_remote_identity(row: &rusqlite::Row<'_>) -> rusqlite::Result<RemoteIdentityProfile> {
    Ok(RemoteIdentityProfile {
        public_id: row.get(0)?,
        agent_did: row.get(1)?,
        display_name: row.get(2)?,
        description: row.get(3)?,
        capabilities: parse_json_array(row.get::<_, String>(4)?)?,
        skills: parse_json_array(row.get::<_, String>(5)?)?,
        did_document_json: row
            .get::<_, Option<String>>(6)?
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?,
        active: sqlite_to_bool(row.get::<_, i64>(7)?),
        last_profile_fetched_at: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

fn row_to_policy_rule(row: &rusqlite::Row<'_>) -> SocialResult<PolicyRule> {
    Ok(PolicyRule {
        rule_id: row
            .get(0)
            .map_err(|error| SocialError::Storage(format!("read rule_id: {error}")))?,
        owner_public_id: row
            .get(1)
            .map_err(|error| SocialError::Storage(format!("read owner_public_id: {error}")))?,
        rule_type: policy_rule_type_from_str(
            &row.get::<_, String>(2)
                .map_err(|error| SocialError::Storage(format!("read rule_type: {error}")))?,
        )?,
        scope: policy_scope_from_str(
            &row.get::<_, String>(3)
                .map_err(|error| SocialError::Storage(format!("read scope: {error}")))?,
        )?,
        matcher_json: serde_json::from_str(
            &row.get::<_, String>(4)
                .map_err(|error| SocialError::Storage(format!("read matcher_json: {error}")))?,
        )
        .map_err(|error| SocialError::Storage(format!("parse matcher_json: {error}")))?,
        config_json: serde_json::from_str(
            &row.get::<_, String>(5)
                .map_err(|error| SocialError::Storage(format!("read config_json: {error}")))?,
        )
        .map_err(|error| SocialError::Storage(format!("parse config_json: {error}")))?,
        priority: row
            .get(6)
            .map_err(|error| SocialError::Storage(format!("read priority: {error}")))?,
        enabled: sqlite_to_bool(
            row.get::<_, i64>(7)
                .map_err(|error| SocialError::Storage(format!("read enabled: {error}")))?,
        ),
        created_at: row
            .get(8)
            .map_err(|error| SocialError::Storage(format!("read created_at: {error}")))?,
        updated_at: row
            .get(9)
            .map_err(|error| SocialError::Storage(format!("read updated_at: {error}")))?,
    })
}

fn row_to_policy_decision_log(row: &rusqlite::Row<'_>) -> rusqlite::Result<PolicyDecisionLog> {
    Ok(PolicyDecisionLog {
        decision_id: row.get(0)?,
        owner_public_id: row.get(1)?,
        scope: policy_scope_from_str(&row.get::<_, String>(2)?)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?,
        target_public_id: row.get(3)?,
        target_node_id: row.get(4)?,
        rule_id: row.get(5)?,
        decision: policy_decision_from_str(&row.get::<_, String>(6)?)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?,
        reason: row.get(7)?,
        context_json: serde_json::from_str(&row.get::<_, String>(8)?)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?,
        created_at: row.get(9)?,
    })
}

fn row_to_friend_request(row: &rusqlite::Row<'_>) -> rusqlite::Result<FriendRequest> {
    Ok(FriendRequest {
        request_id: row.get(0)?,
        local_public_id: row.get(1)?,
        remote_public_id: row.get(2)?,
        remote_node_id: row.get(3)?,
        direction: friend_request_direction_from_str(&row.get::<_, String>(4)?)
            .map_err(to_row_error)?,
        state: friend_request_state_from_str(&row.get::<_, String>(5)?).map_err(to_row_error)?,
        decision_reason: row.get(6)?,
        correlation_id: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        expires_at: row.get(10)?,
    })
}

fn row_to_friendship(row: &rusqlite::Row<'_>) -> rusqlite::Result<Friendship> {
    Ok(Friendship {
        friendship_id: row.get(0)?,
        local_public_id: row.get(1)?,
        remote_public_id: row.get(2)?,
        state: friendship_state_from_str(&row.get::<_, String>(3)?).map_err(to_row_error)?,
        established_from_request_id: row.get(4)?,
        thread_id: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

fn row_to_social_block(row: &rusqlite::Row<'_>) -> rusqlite::Result<SocialBlock> {
    Ok(SocialBlock {
        block_id: row.get(0)?,
        owner_public_id: row.get(1)?,
        blocked_public_id: row.get(2)?,
        blocked_node_id: row.get(3)?,
        reason: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn row_to_thread(row: &rusqlite::Row<'_>) -> rusqlite::Result<DirectThread> {
    let state = row.get::<_, String>(4)?;
    Ok(DirectThread {
        thread_id: row.get(0)?,
        local_public_id: row.get(1)?,
        remote_public_id: row.get(2)?,
        transport_thread_id: row.get(3)?,
        state: thread_state_from_str(&state).map_err(to_row_error)?,
        last_message_at: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

fn row_to_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<DirectMessage> {
    Ok(DirectMessage {
        thread_id: row.get(0)?,
        message_id: row.get(1)?,
        transport_message_id: row.get(2)?,
        local_public_id: row.get(3)?,
        remote_public_id: row.get(4)?,
        direction: message_direction_from_str(&row.get::<_, String>(5)?).map_err(to_row_error)?,
        message_kind: message_kind_from_str(&row.get::<_, String>(6)?).map_err(to_row_error)?,
        content_json: serde_json::from_str(&row.get::<_, String>(7)?)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?,
        encrypted_body: row.get(8)?,
        content_encoding: row.get(9)?,
        agent_envelope_json: row
            .get::<_, Option<String>>(10)?
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?,
        agent_signature: row.get(11)?,
        delivery_state: delivery_state_from_str(&row.get::<_, String>(12)?)
            .map_err(to_row_error)?,
        read_state: read_state_from_str(&row.get::<_, String>(13)?).map_err(to_row_error)?,
        created_at: row.get(14)?,
        updated_at: row.get(15)?,
    })
}

fn row_to_transport_binding(row: &rusqlite::Row<'_>) -> rusqlite::Result<RemoteTransportBinding> {
    Ok(RemoteTransportBinding {
        public_id: row.get(0)?,
        transport_node_id: row.get(1)?,
        transport_kind: transport_kind_from_str(&row.get::<_, String>(2)?).map_err(to_row_error)?,
        agent_did: row.get(3)?,
        binding_source: row.get(4)?,
        binding_confidence: row.get(5)?,
        binding_proof_json: row
            .get::<_, Option<String>>(6)?
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?,
        binding_verified: sqlite_to_bool(row.get::<_, i64>(7)?),
        binding_verified_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

fn row_to_message_receipt(row: &rusqlite::Row<'_>) -> rusqlite::Result<MessageReceipt> {
    Ok(MessageReceipt {
        message_id: row.get(0)?,
        receipt_kind: receipt_kind_from_str(&row.get::<_, String>(1)?).map_err(to_row_error)?,
        recorded_at: row.get(2)?,
        detail: row.get(3)?,
    })
}

fn to_row_error(error: SocialError) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(error))
}

fn parse_json_array(raw: String) -> rusqlite::Result<Vec<String>> {
    serde_json::from_str(&raw)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
}

fn bool_to_sqlite(value: bool) -> i64 {
    if value { 1 } else { 0 }
}

fn sqlite_to_bool(value: i64) -> bool {
    value != 0
}

fn policy_rule_type_to_str(value: PolicyRuleType) -> &'static str {
    match value {
        PolicyRuleType::RejectBlockedAgent => "reject_blocked_agent",
        PolicyRuleType::RejectDuplicatePendingRequest => "reject_duplicate_pending_request",
        PolicyRuleType::RejectActiveFriendship => "reject_active_friendship",
        PolicyRuleType::AllowDirectMessageForFriends => "allow_direct_message_for_friends",
        PolicyRuleType::DenyDirectMessageWhenBlocked => "deny_direct_message_when_blocked",
        PolicyRuleType::DenyDirectMessageWhenNotFriends => "deny_direct_message_when_not_friends",
    }
}

fn policy_rule_type_from_str(value: &str) -> SocialResult<PolicyRuleType> {
    match value {
        "reject_blocked_agent" => Ok(PolicyRuleType::RejectBlockedAgent),
        "reject_duplicate_pending_request" => Ok(PolicyRuleType::RejectDuplicatePendingRequest),
        "reject_active_friendship" => Ok(PolicyRuleType::RejectActiveFriendship),
        "allow_direct_message_for_friends" => Ok(PolicyRuleType::AllowDirectMessageForFriends),
        "deny_direct_message_when_blocked" => Ok(PolicyRuleType::DenyDirectMessageWhenBlocked),
        "deny_direct_message_when_not_friends" => {
            Ok(PolicyRuleType::DenyDirectMessageWhenNotFriends)
        }
        other => Err(SocialError::Storage(format!(
            "unknown policy rule type: {other}"
        ))),
    }
}

fn policy_scope_to_str(value: PolicyScope) -> &'static str {
    match value {
        PolicyScope::Global => "global",
        PolicyScope::FriendRequestsInbound => "friend_requests_inbound",
        PolicyScope::FriendRequestsOutbound => "friend_requests_outbound",
        PolicyScope::DirectMessagesInbound => "direct_dm_messages_inbound",
        PolicyScope::DirectMessagesOutbound => "direct_dm_messages_outbound",
        PolicyScope::Blocks => "blocks",
    }
}

fn policy_scope_from_str(value: &str) -> SocialResult<PolicyScope> {
    match value {
        "global" => Ok(PolicyScope::Global),
        "friend_requests_inbound" => Ok(PolicyScope::FriendRequestsInbound),
        "friend_requests_outbound" => Ok(PolicyScope::FriendRequestsOutbound),
        "direct_dm_messages_inbound" => Ok(PolicyScope::DirectMessagesInbound),
        "direct_dm_messages_outbound" => Ok(PolicyScope::DirectMessagesOutbound),
        "blocks" => Ok(PolicyScope::Blocks),
        other => Err(SocialError::Storage(format!(
            "unknown policy scope: {other}"
        ))),
    }
}

fn policy_decision_to_str(value: PolicyDecision) -> &'static str {
    match value {
        PolicyDecision::Allow => "allow",
        PolicyDecision::Deny => "deny",
    }
}

fn policy_decision_from_str(value: &str) -> SocialResult<PolicyDecision> {
    match value {
        "allow" => Ok(PolicyDecision::Allow),
        "deny" => Ok(PolicyDecision::Deny),
        other => Err(SocialError::Storage(format!(
            "unknown policy decision: {other}"
        ))),
    }
}

fn friend_request_direction_to_str(value: FriendRequestDirection) -> &'static str {
    match value {
        FriendRequestDirection::Inbound => "inbound",
        FriendRequestDirection::Outbound => "outbound",
    }
}

fn friend_request_direction_from_str(value: &str) -> SocialResult<FriendRequestDirection> {
    match value {
        "inbound" => Ok(FriendRequestDirection::Inbound),
        "outbound" => Ok(FriendRequestDirection::Outbound),
        other => Err(SocialError::Storage(format!(
            "unknown friend request direction: {other}"
        ))),
    }
}

fn friend_request_state_to_str(value: FriendRequestState) -> &'static str {
    match value {
        FriendRequestState::Pending => "pending",
        FriendRequestState::Accepted => "accepted",
        FriendRequestState::Rejected => "rejected",
        FriendRequestState::Blocked => "blocked",
        FriendRequestState::Cancelled => "cancelled",
        FriendRequestState::Expired => "expired",
    }
}

fn friend_request_state_from_str(value: &str) -> SocialResult<FriendRequestState> {
    match value {
        "pending" => Ok(FriendRequestState::Pending),
        "accepted" => Ok(FriendRequestState::Accepted),
        "rejected" => Ok(FriendRequestState::Rejected),
        "blocked" => Ok(FriendRequestState::Blocked),
        "cancelled" => Ok(FriendRequestState::Cancelled),
        "expired" => Ok(FriendRequestState::Expired),
        other => Err(SocialError::Storage(format!(
            "unknown friend request state: {other}"
        ))),
    }
}

fn friendship_state_to_str(value: FriendshipState) -> &'static str {
    match value {
        FriendshipState::Active => "active",
        FriendshipState::Removed => "removed",
        FriendshipState::Blocked => "blocked",
    }
}

fn friendship_state_from_str(value: &str) -> SocialResult<FriendshipState> {
    match value {
        "active" => Ok(FriendshipState::Active),
        "removed" => Ok(FriendshipState::Removed),
        "blocked" => Ok(FriendshipState::Blocked),
        other => Err(SocialError::Storage(format!(
            "unknown friendship state: {other}"
        ))),
    }
}

fn transport_kind_to_str(value: TransportKind) -> &'static str {
    match value {
        TransportKind::Wattswarm => "wattswarm",
    }
}

fn transport_kind_from_str(value: &str) -> SocialResult<TransportKind> {
    match value {
        "wattswarm" => Ok(TransportKind::Wattswarm),
        other => Err(SocialError::Storage(format!(
            "unknown transport kind: {other}"
        ))),
    }
}

fn thread_state_to_str(value: ThreadState) -> &'static str {
    match value {
        ThreadState::Pending => "pending",
        ThreadState::Ready => "ready",
        ThreadState::Closed => "closed",
        ThreadState::Blocked => "blocked",
    }
}

fn thread_state_from_str(value: &str) -> SocialResult<ThreadState> {
    match value {
        "pending" => Ok(ThreadState::Pending),
        "ready" => Ok(ThreadState::Ready),
        "closed" => Ok(ThreadState::Closed),
        "blocked" => Ok(ThreadState::Blocked),
        other => Err(SocialError::Storage(format!(
            "unknown thread state: {other}"
        ))),
    }
}

fn message_direction_to_str(value: MessageDirection) -> &'static str {
    match value {
        MessageDirection::Inbound => "inbound",
        MessageDirection::Outbound => "outbound",
    }
}

fn message_direction_from_str(value: &str) -> SocialResult<MessageDirection> {
    match value {
        "inbound" => Ok(MessageDirection::Inbound),
        "outbound" => Ok(MessageDirection::Outbound),
        other => Err(SocialError::Storage(format!(
            "unknown message direction: {other}"
        ))),
    }
}

fn message_kind_to_str(value: MessageKind) -> &'static str {
    match value {
        MessageKind::Message => "message",
        MessageKind::RelationshipEstablished => "relationship_established",
        MessageKind::SessionInit => "session_init",
    }
}

fn message_kind_from_str(value: &str) -> SocialResult<MessageKind> {
    match value {
        "message" => Ok(MessageKind::Message),
        "relationship_established" => Ok(MessageKind::RelationshipEstablished),
        "session_init" => Ok(MessageKind::SessionInit),
        other => Err(SocialError::Storage(format!(
            "unknown message kind: {other}"
        ))),
    }
}

fn delivery_state_to_str(value: DeliveryState) -> &'static str {
    match value {
        DeliveryState::Pending => "pending",
        DeliveryState::Delivered => "delivered",
        DeliveryState::Acknowledged => "acknowledged",
        DeliveryState::Failed => "failed",
    }
}

fn delivery_state_from_str(value: &str) -> SocialResult<DeliveryState> {
    match value {
        "pending" => Ok(DeliveryState::Pending),
        "delivered" => Ok(DeliveryState::Delivered),
        "acknowledged" => Ok(DeliveryState::Acknowledged),
        "failed" => Ok(DeliveryState::Failed),
        other => Err(SocialError::Storage(format!(
            "unknown delivery state: {other}"
        ))),
    }
}

fn read_state_to_str(value: ReadState) -> &'static str {
    match value {
        ReadState::Unread => "unread",
        ReadState::Read => "read",
    }
}

fn read_state_from_str(value: &str) -> SocialResult<ReadState> {
    match value {
        "unread" => Ok(ReadState::Unread),
        "read" => Ok(ReadState::Read),
        other => Err(SocialError::Storage(format!("unknown read state: {other}"))),
    }
}

fn receipt_kind_to_str(value: ReceiptKind) -> &'static str {
    match value {
        ReceiptKind::Sent => "sent",
        ReceiptKind::Delivered => "delivered",
        ReceiptKind::Acknowledged => "acknowledged",
        ReceiptKind::Read => "read",
    }
}

fn receipt_kind_from_str(value: &str) -> SocialResult<ReceiptKind> {
    match value {
        "sent" => Ok(ReceiptKind::Sent),
        "delivered" => Ok(ReceiptKind::Delivered),
        "acknowledged" => Ok(ReceiptKind::Acknowledged),
        "read" => Ok(ReceiptKind::Read),
        other => Err(SocialError::Storage(format!(
            "unknown receipt kind: {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::policy_service::ensure_default_policy_rules;
    use crate::domain::deferred_agent_events::DeferredAgentEvent;
    use crate::domain::messages::{
        DeliveryState, DirectMessage, MessageDirection, MessageKind, ReadState,
    };
    use crate::domain::receipts::{MessageReceipt, ReceiptKind};
    use crate::domain::threads::{DirectThread, ThreadState};
    use crate::domain::transport_bindings::{RemoteTransportBinding, TransportKind};
    use crate::policy::decisions::{PolicyDecision, PolicyDecisionLog};
    use crate::ports::repositories::PolicyDecisionLogRepository;
    use std::path::PathBuf;

    fn unique_test_db_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "wattetheria-social-{name}-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        path
    }

    #[test]
    fn store_bootstraps_policy_rules_and_is_idempotent() {
        let store = SocialStore::open_in_memory().expect("open store");

        let first =
            ensure_default_policy_rules(&store, "did:key:alice", 100).expect("bootstrap rules");
        let second =
            ensure_default_policy_rules(&store, "did:key:alice", 200).expect("bootstrap rules");

        assert_eq!(first.len(), 5);
        assert_eq!(first, second);
    }

    #[test]
    fn store_tracks_deferred_agent_events_until_replayed() {
        let store = SocialStore::open_in_memory().expect("open store");
        let event = DeferredAgentEvent {
            event_id: "evt-dm-1".to_owned(),
            local_public_id: "agent-local".to_owned(),
            remote_public_id: "agent-remote".to_owned(),
            remote_node_id: Some("node-remote".to_owned()),
            source_agent_id: Some("did:key:remote".to_owned()),
            status: "waiting_for_friendship".to_owned(),
            event_json: serde_json::json!({"event_id":"evt-dm-1"}),
            reason: Some("waiting_for_friendship".to_owned()),
            created_at: 10,
            updated_at: 10,
            replayed_at: None,
        };

        store.defer_agent_event(&event).expect("defer event");
        store
            .defer_agent_event(&event)
            .expect("defer event idempotently");

        let waiting = store
            .list_waiting_deferred_agent_events("agent-local", "agent-remote", 10)
            .expect("list waiting events");
        assert_eq!(waiting, vec![event.clone()]);

        store
            .mark_deferred_agent_event_replayed("evt-dm-1", 20)
            .expect("mark replayed");
        let stored = store
            .get_deferred_agent_event("evt-dm-1")
            .expect("get deferred event")
            .expect("deferred event exists");
        assert_eq!(stored.status, "replayed");
        assert_eq!(stored.replayed_at, Some(20));
        assert!(
            store
                .list_waiting_deferred_agent_events("agent-local", "agent-remote", 10)
                .expect("list waiting after replay")
                .is_empty()
        );
    }

    #[test]
    fn store_persists_policy_decision_logs() {
        let store = SocialStore::open_in_memory().expect("open store");
        store
            .append_policy_decision_log(&PolicyDecisionLog {
                decision_id: "decision-1".to_string(),
                owner_public_id: "did:key:alice".to_string(),
                scope: PolicyScope::DirectMessagesOutbound,
                target_public_id: "did:key:bob".to_string(),
                target_node_id: Some("node-bob".to_string()),
                rule_id: Some("allow-direct-message-for-friends".to_string()),
                decision: PolicyDecision::Allow,
                reason: "active_friendship".to_string(),
                context_json: serde_json::json!({"scope":"direct_dm_messages_outbound"}),
                created_at: 42,
            })
            .expect("append policy decision log");

        let logs = store
            .list_policy_decision_logs("did:key:alice")
            .expect("list policy decision logs");
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].decision, PolicyDecision::Allow);
        assert_eq!(logs[0].target_public_id, "did:key:bob");
    }

    #[test]
    fn store_persists_dm_threads_and_dm_messages() {
        let store = SocialStore::open_in_memory().expect("open store");
        let thread = DirectThread {
            thread_id: "thread-1".to_owned(),
            local_public_id: "did:key:alice".to_owned(),
            remote_public_id: "did:key:bob".to_owned(),
            transport_thread_id: "dm:alice:bob".to_owned(),
            state: ThreadState::Ready,
            last_message_at: Some(3),
            created_at: 1,
            updated_at: 3,
        };
        store.upsert_thread(&thread).expect("save thread");

        let message = DirectMessage {
            thread_id: thread.thread_id.clone(),
            message_id: "message-1".to_owned(),
            transport_message_id: Some("transport-1".to_owned()),
            local_public_id: thread.local_public_id.clone(),
            remote_public_id: thread.remote_public_id.clone(),
            direction: MessageDirection::Outbound,
            message_kind: MessageKind::Message,
            content_json: serde_json::json!({"text":"hello"}),
            encrypted_body: None,
            content_encoding: None,
            agent_envelope_json: Some(serde_json::json!({"protocol":"google_a2a"})),
            agent_signature: Some("sig-1".to_owned()),
            delivery_state: DeliveryState::Pending,
            read_state: ReadState::Unread,
            created_at: 2,
            updated_at: 2,
        };
        store.upsert_message(&message).expect("save message");

        let dm_threads = store
            .list_threads("did:key:alice")
            .expect("list dm_threads");
        let dm_messages = store
            .list_thread_messages("thread-1")
            .expect("list thread dm_messages");

        assert_eq!(dm_threads, vec![thread]);
        assert_eq!(dm_messages, vec![message]);
    }

    #[test]
    fn store_persists_friend_requests_friendships_and_blocks() {
        let store = SocialStore::open_in_memory().expect("open store");
        let request = FriendRequest {
            request_id: "request-1".to_owned(),
            local_public_id: "did:key:alice".to_owned(),
            remote_public_id: "did:key:bob".to_owned(),
            remote_node_id: Some("peer-bob".to_owned()),
            direction: FriendRequestDirection::Outbound,
            state: FriendRequestState::Pending,
            decision_reason: None,
            correlation_id: Some("correlation-1".to_owned()),
            created_at: 1,
            updated_at: 1,
            expires_at: Some(10),
        };
        store
            .upsert_friend_request(&request)
            .expect("save friend request");

        let friendship = Friendship {
            friendship_id: "friendship-1".to_owned(),
            local_public_id: "did:key:alice".to_owned(),
            remote_public_id: "did:key:bob".to_owned(),
            state: FriendshipState::Active,
            established_from_request_id: Some("request-1".to_owned()),
            thread_id: Some("thread-1".to_owned()),
            created_at: 2,
            updated_at: 2,
        };
        store
            .upsert_friendship(&friendship)
            .expect("save friendship");

        let block = SocialBlock {
            block_id: "block-1".to_owned(),
            owner_public_id: "did:key:alice".to_owned(),
            blocked_public_id: "did:key:mallory".to_owned(),
            blocked_node_id: Some("peer-mallory".to_owned()),
            reason: Some("spam".to_owned()),
            created_at: 3,
            updated_at: 3,
        };
        store.upsert_block(&block).expect("save social block");

        assert_eq!(
            store
                .list_friend_requests("did:key:alice")
                .expect("list friend requests"),
            vec![request]
        );
        assert_eq!(
            store
                .list_friendships("did:key:alice")
                .expect("list friendships"),
            vec![friendship]
        );
        assert_eq!(
            store.list_blocks("did:key:alice").expect("list blocks"),
            vec![block]
        );
    }

    #[test]
    fn store_tracks_due_outbound_friend_request_reliability_tasks() {
        let store = SocialStore::open_in_memory().expect("open store");
        let request = FriendRequest {
            request_id: "request-1".to_owned(),
            local_public_id: "did:key:alice".to_owned(),
            remote_public_id: "did:key:bob".to_owned(),
            remote_node_id: Some("node-bob".to_owned()),
            direction: FriendRequestDirection::Outbound,
            state: FriendRequestState::Pending,
            decision_reason: None,
            correlation_id: Some("correlation-1".to_owned()),
            created_at: 100,
            updated_at: 100,
            expires_at: None,
        };
        store
            .upsert_friend_request(&request)
            .expect("save friend request");
        let mut duplicate = request.clone();
        duplicate.request_id = "request-older-duplicate".to_owned();
        duplicate.updated_at = 99;
        store
            .upsert_friend_request(&duplicate)
            .expect("save duplicate friend request");

        assert!(
            store
                .due_outbound_pending_friend_requests(399, 300, 10)
                .expect("query not due")
                .is_empty()
        );
        assert_eq!(
            store
                .due_outbound_pending_friend_requests(400, 300, 10)
                .expect("query due")
                .len(),
            1
        );
        assert_eq!(
            store
                .due_outbound_pending_friend_requests(400, 300, 10)
                .expect("query due")[0]
                .request_id,
            "request-1"
        );

        store
            .defer_reliability_task("friend_request", "request-1", 400, 700, Some("offline"))
            .expect("defer task");
        assert!(
            store
                .due_outbound_pending_friend_requests(699, 300, 10)
                .expect("query deferred")
                .is_empty()
        );
        let deferred = store
            .get_reliability_task("friend_request", "request-1")
            .expect("get deferred task")
            .expect("deferred task");
        assert_eq!(deferred.attempt_count, 0);
        assert_eq!(deferred.next_attempt_at, 700);

        store
            .record_reliability_attempt("friend_request", "request-1", 700, 1600, None)
            .expect("record attempt");
        let attempted = store
            .get_reliability_task("friend_request", "request-1")
            .expect("get attempted task")
            .expect("attempted task");
        assert_eq!(attempted.attempt_count, 1);
        assert_eq!(attempted.last_attempt_at, Some(700));
        assert_eq!(attempted.next_attempt_at, 1600);

        store
            .upsert_friendship(&Friendship {
                friendship_id: "friendship-1".to_owned(),
                local_public_id: "did:key:alice".to_owned(),
                remote_public_id: "did:key:bob".to_owned(),
                state: FriendshipState::Active,
                established_from_request_id: Some("request-1".to_owned()),
                thread_id: None,
                created_at: 800,
                updated_at: 800,
            })
            .expect("save active friendship");
        assert!(
            store
                .due_outbound_pending_friend_requests(2000, 300, 10)
                .expect("query active friendship")
                .is_empty()
        );
    }

    #[test]
    fn store_persists_public_identities_bindings_and_receipts() {
        let store = SocialStore::open_in_memory().expect("open store");
        let identity = RemoteIdentityProfile {
            public_id: "did:key:bob".to_owned(),
            agent_did: "did:key:bob".to_owned(),
            display_name: "Bob".to_owned(),
            description: Some("remote identity".to_owned()),
            capabilities: vec!["dm".to_owned(), "friend_request".to_owned()],
            skills: vec!["chat".to_owned()],
            did_document_json: Some(serde_json::json!({"id":"did:key:bob"})),
            active: true,
            last_profile_fetched_at: Some(5),
            created_at: 1,
            updated_at: 5,
        };
        store
            .upsert_remote_identity(&identity)
            .expect("save remote identity");

        let binding = RemoteTransportBinding {
            public_id: "did:key:bob".to_owned(),
            agent_did: Some("did:key:bob".to_owned()),
            transport_kind: TransportKind::Wattswarm,
            transport_node_id: "peer-bob".to_owned(),
            binding_source: "friend_request".to_owned(),
            binding_confidence: 90,
            binding_proof_json: Some(serde_json::json!({"proof":"placeholder"})),
            binding_verified: false,
            binding_verified_at: None,
            updated_at: 6,
        };
        store
            .upsert_transport_binding(&binding)
            .expect("save transport binding");

        let receipt = MessageReceipt {
            message_id: "message-1".to_owned(),
            receipt_kind: ReceiptKind::Delivered,
            recorded_at: 7,
            detail: Some("transport delivered".to_owned()),
        };
        store
            .upsert_message_receipt(&receipt)
            .expect("save message receipt");

        assert_eq!(
            store
                .list_remote_identities()
                .expect("list remote identities"),
            vec![identity.clone()]
        );
        assert_eq!(
            store
                .list_transport_bindings()
                .expect("list transport bindings"),
            vec![binding.clone()]
        );
        assert_eq!(
            store
                .list_message_receipts("message-1")
                .expect("list message receipts"),
            vec![receipt]
        );
        assert_eq!(
            store
                .get_remote_identity("did:key:bob")
                .expect("get remote identity"),
            Some(identity)
        );
        assert_eq!(
            store
                .list_transport_bindings_for_public_id("did:key:bob")
                .expect("list transport bindings by public_id"),
            vec![binding]
        );
    }

    #[test]
    fn store_imports_legacy_social_tables_into_unified_db() {
        let legacy_path = unique_test_db_path("legacy");
        let unified_path = unique_test_db_path("unified");
        let identity = RemoteIdentityProfile {
            public_id: "did:key:bob".to_owned(),
            agent_did: "did:key:bob".to_owned(),
            display_name: "Bob".to_owned(),
            description: Some("remote identity".to_owned()),
            capabilities: vec!["dm".to_owned()],
            skills: vec!["chat".to_owned()],
            did_document_json: None,
            active: true,
            last_profile_fetched_at: None,
            created_at: 1,
            updated_at: 1,
        };

        {
            let legacy = SocialStore::open(&legacy_path).expect("open legacy store");
            legacy
                .upsert_remote_identity(&identity)
                .expect("save legacy identity");
        }

        let unified = SocialStore::open(&unified_path).expect("open unified store");
        unified
            .import_legacy_db(&legacy_path)
            .expect("import legacy social db");
        unified
            .import_legacy_db(&legacy_path)
            .expect("import legacy social db idempotently");

        assert_eq!(
            unified
                .list_remote_identities()
                .expect("list imported identities"),
            vec![identity]
        );

        let _ = std::fs::remove_file(legacy_path);
        let _ = std::fs::remove_file(unified_path);
    }

    #[test]
    fn store_uses_namespaced_schema_version_for_unified_db() {
        let unified_path = unique_test_db_path("schema-version");
        {
            let conn = Connection::open(&unified_path).expect("open sqlite");
            conn.execute_batch(
                "CREATE TABLE schema_version (version INTEGER NOT NULL);
                 INSERT INTO schema_version (version) VALUES (3);",
            )
            .expect("seed local db schema version");
        }

        let store = SocialStore::open(&unified_path).expect("open social store");
        drop(store);

        let conn = Connection::open(&unified_path).expect("open sqlite");
        let local_version: i64 = conn
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .expect("read local schema version");
        let social_version: i64 = conn
            .query_row("SELECT version FROM social_schema_version", [], |row| {
                row.get(0)
            })
            .expect("read social schema version");
        assert_eq!(local_version, 3);
        assert_eq!(social_version, schema::SCHEMA_VERSION);

        let _ = std::fs::remove_file(unified_path);
    }

    #[test]
    fn store_supports_filtered_friendship_block_thread_and_message_queries() {
        let store = SocialStore::open_in_memory().expect("open store");
        let friendship = Friendship {
            friendship_id: "friendship-1".to_owned(),
            local_public_id: "did:key:alice".to_owned(),
            remote_public_id: "did:key:bob".to_owned(),
            state: FriendshipState::Active,
            established_from_request_id: Some("request-1".to_owned()),
            thread_id: Some("thread-1".to_owned()),
            created_at: 1,
            updated_at: 1,
        };
        store
            .upsert_friendship(&friendship)
            .expect("save friendship");

        let block = SocialBlock {
            block_id: "block-1".to_owned(),
            owner_public_id: "did:key:alice".to_owned(),
            blocked_public_id: "did:key:mallory".to_owned(),
            blocked_node_id: Some("peer-mallory".to_owned()),
            reason: Some("spam".to_owned()),
            created_at: 2,
            updated_at: 2,
        };
        store.upsert_block(&block).expect("save block");

        let thread = DirectThread {
            thread_id: "thread-1".to_owned(),
            local_public_id: "did:key:alice".to_owned(),
            remote_public_id: "did:key:bob".to_owned(),
            transport_thread_id: "dm:alice:bob".to_owned(),
            state: ThreadState::Ready,
            last_message_at: Some(3),
            created_at: 1,
            updated_at: 3,
        };
        store.upsert_thread(&thread).expect("save thread");

        let message = DirectMessage {
            thread_id: "thread-1".to_owned(),
            message_id: "message-1".to_owned(),
            transport_message_id: Some("transport-1".to_owned()),
            local_public_id: "did:key:alice".to_owned(),
            remote_public_id: "did:key:bob".to_owned(),
            direction: MessageDirection::Outbound,
            message_kind: MessageKind::Message,
            content_json: serde_json::json!({"text":"hello"}),
            encrypted_body: None,
            content_encoding: None,
            agent_envelope_json: Some(serde_json::json!({"protocol":"google_a2a"})),
            agent_signature: Some("sig-1".to_owned()),
            delivery_state: DeliveryState::Pending,
            read_state: ReadState::Unread,
            created_at: 3,
            updated_at: 3,
        };
        store.upsert_message(&message).expect("save message");

        assert_eq!(
            store
                .find_friendship("did:key:alice", "did:key:bob")
                .expect("find friendship"),
            Some(friendship)
        );
        assert_eq!(
            store
                .find_block("did:key:alice", "did:key:mallory")
                .expect("find block"),
            Some(block)
        );
        assert_eq!(
            store
                .find_thread("did:key:alice", "did:key:bob")
                .expect("find thread"),
            Some(thread.clone())
        );
        assert_eq!(
            store
                .list_threads_by_state("did:key:alice", ThreadState::Ready)
                .expect("list dm_threads by state"),
            vec![thread]
        );
        assert_eq!(
            store
                .get_message("thread-1", "message-1")
                .expect("get message"),
            Some(message)
        );
    }
}
