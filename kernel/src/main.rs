//! Kernel daemon entrypoint: boot identity, p2p, proofs, and demo flows.

use anyhow::{Context, Result, bail};
use clap::Parser;
use libp2p::Multiaddr;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};
use tokio::time::{Duration, interval};
use tracing::{error, info, warn};

use wattetheria_kernel::admission::{
    AdmissionConfig, AdmissionVerdict, NonceTracker, validate_gossip_packet_with_nonce,
};
use wattetheria_kernel::audit::AuditLog;
use wattetheria_kernel::capabilities::CapabilityPolicy;
use wattetheria_kernel::control_plane::{ControlPlaneState, RateLimiter, serve_control_plane};
use wattetheria_kernel::data_ops::recover_if_corrupt_with_sources;
use wattetheria_kernel::event_log::{EventLog, EventRecord};
use wattetheria_kernel::governance::{GovernanceEngine, PlanetCreationRequest};
use wattetheria_kernel::hashcash;
use wattetheria_kernel::identity::Identity;
use wattetheria_kernel::mailbox::CrossSubnetMailbox;
use wattetheria_kernel::online_proof::OnlineProofManager;
use wattetheria_kernel::oracle::{OracleFeed, OracleRegistry};
use wattetheria_kernel::p2p::{P2PConfig, P2PNode};
use wattetheria_kernel::policy_engine::PolicyEngine;
use wattetheria_kernel::signing::sign_payload;
use wattetheria_kernel::task_engine::TaskEngine;
use wattetheria_kernel::trust::{TrustConfig, WebOfTrust};
use wattetheria_kernel::types::{ActionEnvelope, Reward, Sla, VerificationMode, VerificationSpec};

#[derive(Debug, Clone, Serialize)]
struct SignedEnvelope<T: Serialize> {
    r#type: String,
    version: String,
    agent_id: String,
    payload: T,
    signature: String,
}

#[derive(Debug, Parser)]
#[command(name = "wattetheria-kernel")]
#[allow(clippy::struct_excessive_bools)]
struct Cli {
    #[arg(long, default_value = ".wattetheria")]
    data_dir: PathBuf,
    #[arg(long, default_value = "wattetheria.v0.1")]
    topic: String,
    #[arg(long, default_value = "/ip4/0.0.0.0/tcp/0")]
    listen: String,
    #[arg(long = "bootstrap")]
    bootstrap: Vec<String>,
    #[arg(long = "recovery-source")]
    recovery_sources: Vec<String>,
    #[arg(long, default_value_t = false)]
    run_demo_task: bool,
    #[arg(long, default_value_t = false)]
    ignite_demo_planet: bool,
    #[arg(long, default_value_t = false)]
    enable_hashcash: bool,
    #[arg(long, default_value_t = false)]
    require_hashcash_inbound: bool,
    #[arg(long, default_value_t = false)]
    require_hashcash_broadcast: bool,
    #[arg(long, default_value_t = 64)]
    p2p_max_peers: usize,
    #[arg(long, default_value_t = 240)]
    p2p_peer_rate_limit: usize,
    #[arg(long, default_value_t = 1200)]
    p2p_topic_rate_limit: usize,
    #[arg(long, default_value_t = 300)]
    p2p_publish_rate_limit: usize,
    #[arg(long, default_value_t = 1)]
    p2p_topic_shards: usize,
    #[arg(long, default_value_t = 120)]
    p2p_dedupe_ttl_sec: i64,
    #[arg(long, default_value_t = 300)]
    p2p_message_ttl_sec: i64,
    #[arg(long, default_value = "127.0.0.1:7777")]
    control_plane_bind: String,
    #[arg(long, default_value_t = 60)]
    control_plane_rate_limit: usize,
}

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();
    std::fs::create_dir_all(&cli.data_dir).context("create data dir")?;

    let identity_path = cli.data_dir.join("identity.json");
    let events_path = cli.data_dir.join("events.jsonl");
    let snapshots_path = cli.data_dir.join("snapshots");
    std::fs::create_dir_all(&snapshots_path).context("create snapshots dir")?;

    startup_recover_events(&events_path, &snapshots_path, &cli.recovery_sources).await?;

    let identity = Identity::load_or_create(identity_path)?;
    let event_log = EventLog::new(events_path)?;
    let audit_log = AuditLog::new(cli.data_dir.join("audit/control_plane.jsonl"))?;
    let control_token = load_or_create_control_token(cli.data_dir.join("control.token"))?;
    let control_bind = parse_control_bind(&cli.control_plane_bind)?;
    let policy_state_path = cli.data_dir.join("policy/state.json");
    let policy_engine = PolicyEngine::load_or_new(
        policy_state_path,
        uuid::Uuid::new_v4().to_string(),
        CapabilityPolicy::default(),
    )?;

    let online_proof_path = cli.data_dir.join("online_proof.json");
    let mut online_proof = OnlineProofManager::load_or_new(&online_proof_path).unwrap_or_default();
    online_proof.create_lease(&identity.agent_id, 300, 20);
    let mut web_of_trust = WebOfTrust::new(TrustConfig {
        blacklist_weight_threshold: 3,
    });

    let mut oracle_registry = OracleRegistry::load_or_new(cli.data_dir.join("oracle/state.json"))?;
    let oracle_state_path = cli.data_dir.join("oracle/state.json");

    let ledger_path = cli.data_dir.join("ledger.json");
    let mut task_engine =
        TaskEngine::new_with_ledger(event_log.clone(), identity.clone(), &ledger_path)?;
    let governance_state_path = cli.data_dir.join("governance/state.json");
    let governance_engine = Arc::new(Mutex::new(GovernanceEngine::load_or_new(
        &governance_state_path,
    )?));
    let mailbox_state_path = cli.data_dir.join("mailbox/state.json");
    let mailbox = CrossSubnetMailbox::load_or_new(&mailbox_state_path)?;

    let (listen_addr, bootstrap_addrs) = parse_multiaddrs(&cli)?;
    let p2p_config = P2PConfig {
        max_connected_peers: cli.p2p_max_peers,
        per_peer_msgs_per_minute: cli.p2p_peer_rate_limit,
        per_topic_msgs_per_minute: cli.p2p_topic_rate_limit,
        per_topic_publish_per_minute: cli.p2p_publish_rate_limit,
        topic_shards: cli.p2p_topic_shards.max(1),
        dedupe_ttl_sec: cli.p2p_dedupe_ttl_sec,
        message_ttl_sec: cli.p2p_message_ttl_sec,
        ..P2PConfig::default()
    };
    let mut p2p = P2PNode::new_with_config(&cli.topic, listen_addr, &bootstrap_addrs, p2p_config)?;

    let handshake = build_signed_handshake(&identity, &online_proof, cli.enable_hashcash)?;
    p2p.publish_json(&handshake)?;

    info!(agent_id = %identity.agent_id, "kernel started");
    log_listeners(&p2p);

    if cli.run_demo_task {
        run_demo_task(&mut task_engine, &mut p2p, &identity, &ledger_path)?;
    }

    if cli.ignite_demo_planet {
        let mut governance = governance_engine.lock().await;
        ignite_demo_planet(&mut governance, &identity)?;
        governance.persist(&governance_state_path)?;
    }

    let (stream_tx, _) = broadcast::channel(128);
    let control_state = ControlPlaneState {
        agent_id: identity.agent_id.clone(),
        identity: identity.clone(),
        started_at: chrono::Utc::now().timestamp(),
        auth_token: control_token,
        event_log: event_log.clone(),
        task_engine: Arc::new(Mutex::new(task_engine)),
        task_ledger_path: ledger_path,
        governance_engine,
        governance_state_path,
        policy_engine: Arc::new(Mutex::new(policy_engine)),
        mailbox: Arc::new(Mutex::new(mailbox)),
        mailbox_state_path,
        audit_log: audit_log.clone(),
        rate_limiter: Arc::new(RateLimiter::new(cli.control_plane_rate_limit, 60)),
        stream_tx,
    };

    info!(bind = %control_bind, "starting control plane");
    let control_task = tokio::spawn(async move {
        if let Err(error) = serve_control_plane(control_state, control_bind).await {
            error!(%error, "control plane terminated");
        }
    });

    let admission_config = AdmissionConfig {
        max_time_drift_sec: 180,
        min_hashcash_bits: 12,
        require_hashcash_for_handshake: cli.require_hashcash_inbound,
        require_hashcash_for_broadcast: cli.require_hashcash_broadcast,
    };

    let mut nonce_tracker = NonceTracker::new(admission_config.max_time_drift_sec * 2);

    let run_result = run_loop(
        &mut p2p,
        LoopContext {
            online_proof: &mut online_proof,
            online_proof_path: &online_proof_path,
            identity: &identity,
            admission_config: &admission_config,
            nonce_tracker: &mut nonce_tracker,
            web_of_trust: &mut web_of_trust,
            event_log: &event_log,
            oracle_registry: &mut oracle_registry,
            oracle_state_path: &oracle_state_path,
            enable_hashcash_broadcast: cli.enable_hashcash,
        },
    )
    .await;
    control_task.abort();
    run_result
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .without_time()
        .init();
}

fn parse_multiaddrs(cli: &Cli) -> Result<(Multiaddr, Vec<Multiaddr>)> {
    let listen_addr = cli.listen.parse().context("parse listen multiaddr")?;
    let bootstrap_addrs = cli
        .bootstrap
        .iter()
        .map(|addr| addr.parse())
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("parse bootstrap multiaddr")?;
    Ok((listen_addr, bootstrap_addrs))
}

fn parse_control_bind(value: &str) -> Result<SocketAddr> {
    value
        .parse()
        .with_context(|| format!("parse control plane bind address: {value}"))
}

fn load_or_create_control_token(path: PathBuf) -> Result<String> {
    if path.exists() {
        let token = std::fs::read_to_string(&path).context("read control token")?;
        let token = token.trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }

    let token = uuid::Uuid::new_v4().to_string();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("create token directory")?;
    }
    std::fs::write(path, &token).context("write control token")?;
    Ok(token)
}

async fn startup_recover_events(
    events_path: &Path,
    snapshots_path: &Path,
    recovery_sources: &[String],
) -> Result<()> {
    let mut local_sources = Vec::new();
    let mut http_sources = Vec::new();

    for source in recovery_sources {
        if source.starts_with("http://") || source.starts_with("https://") {
            http_sources.push(source.clone());
        } else {
            local_sources.push(PathBuf::from(source));
        }
    }

    match recover_if_corrupt_with_sources(events_path, snapshots_path, &local_sources) {
        Ok(Some(snapshot)) => {
            warn!(
                snapshot_id = %snapshot.id,
                events = snapshot.event_count,
                "event log recovered from local snapshot/source during startup"
            );
        }
        Ok(None) => {}
        Err(error) => {
            if http_sources.is_empty() {
                return Err(error).context("startup event-log recovery check");
            }
            warn!(%error, "local recovery path failed; trying remote recovery sources");
        }
    }

    if !event_log_chain_is_valid(events_path)
        && !http_sources.is_empty()
        && let Some(source) = recover_events_from_http_sources(events_path, &http_sources).await?
    {
        warn!(source = %source, "event log recovered from remote source during startup");
    }

    if !event_log_chain_is_valid(events_path) {
        bail!("event log remains invalid after startup recovery attempts");
    }

    Ok(())
}

async fn recover_events_from_http_sources(
    events_path: &Path,
    sources: &[String],
) -> Result<Option<String>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .context("build recovery http client")?;

    for source in sources {
        let response = match client.get(source).send().await {
            Ok(response) => response,
            Err(error) => {
                warn!(source = %source, %error, "failed to fetch recovery source");
                continue;
            }
        };

        if !response.status().is_success() {
            warn!(source = %source, status = %response.status(), "recovery source returned non-success status");
            continue;
        }

        let body = match response.text().await {
            Ok(body) => body,
            Err(error) => {
                warn!(source = %source, %error, "failed to read recovery response body");
                continue;
            }
        };

        let rows = match parse_recovery_rows(&body) {
            Ok(rows) => rows,
            Err(error) => {
                warn!(source = %source, %error, "recovery source payload parse failed");
                continue;
            }
        };

        if rows.is_empty() {
            continue;
        }

        if write_candidate_events(events_path, &rows).is_err() {
            continue;
        }
        if event_log_chain_is_valid(events_path) {
            return Ok(Some(source.clone()));
        }
    }

    Ok(None)
}

fn parse_recovery_rows(raw: &str) -> Result<Vec<EventRecord>> {
    if let Ok(rows) = serde_json::from_str::<Vec<EventRecord>>(raw) {
        return Ok(rows);
    }

    if let Ok(value) = serde_json::from_str::<Value>(raw)
        && let Some(events) = value.get("events")
    {
        let rows: Vec<EventRecord> = serde_json::from_value(events.clone())
            .context("parse events array from recovery payload")?;
        return Ok(rows);
    }

    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<EventRecord>(line).context("parse recovery jsonl row"))
        .collect()
}

fn write_candidate_events(events_path: &Path, rows: &[EventRecord]) -> Result<()> {
    let mut content = String::new();
    for row in rows {
        content.push_str(&serde_json::to_string(row)?);
        content.push('\n');
    }
    std::fs::write(events_path, content).context("write candidate recovered events")
}

fn event_log_chain_is_valid(events_path: &Path) -> bool {
    EventLog::new(events_path)
        .and_then(|log| log.verify_chain())
        .is_ok_and(|(ok, _)| ok)
}

fn build_handshake_payload(identity: &Identity, enable_hashcash: bool) -> Option<Value> {
    if !enable_hashcash {
        return None;
    }
    hashcash::mint(&identity.agent_id, 12, 200_000)
        .map(|stamp| json!({"stamp": stamp, "bits": 12, "resource": identity.agent_id}))
}

fn build_signed_handshake(
    identity: &Identity,
    online_proof: &OnlineProofManager,
    enable_hashcash: bool,
) -> Result<SignedEnvelope<Value>> {
    let online_payload = online_proof
        .get_proof(&identity.agent_id)
        .context("online proof missing")?;
    let hashcash_value = build_handshake_payload(identity, enable_hashcash);
    let payload = json!({
        "version": "0.1",
        "agent_id": identity.agent_id,
        "nonce": uuid::Uuid::new_v4().to_string(),
        "timestamp": chrono::Utc::now().timestamp(),
        "capabilities_summary": {
            "fs": {"read": ["/data"], "write": []},
            "net": {"outbound": [], "rate_limit": 60},
            "proc": {"exec": false},
            "wallet": {"sign": false, "send": false},
            "mcp": {"call": []},
            "model": {"invoke": {"tpm": 0}},
            "p2p": {"publish": {"rate_limit": 120}}
        },
        "online_proof": online_payload,
        "hashcash": hashcash_value,
    });
    Ok(SignedEnvelope {
        r#type: "HANDSHAKE".to_string(),
        version: "0.1".to_string(),
        agent_id: identity.agent_id.clone(),
        signature: sign_payload(&payload, identity)?,
        payload,
    })
}

fn log_listeners(p2p: &P2PNode) {
    for listener in p2p.listeners() {
        info!(%listener, "listening");
    }
}

fn run_demo_task(
    task_engine: &mut TaskEngine,
    p2p: &mut P2PNode,
    identity: &Identity,
    ledger_path: &Path,
) -> Result<()> {
    let task = task_engine.publish_task(
        "market.match",
        "T0",
        json!({
            "buy_orders": [
                {"id":"b-1", "price":120, "qty":5},
                {"id":"b-2", "price":115, "qty":4}
            ],
            "sell_orders": [
                {"id":"s-1", "price":100, "qty":2},
                {"id":"s-2", "price":110, "qty":6}
            ]
        }),
        VerificationSpec {
            mode: VerificationMode::Deterministic,
            witnesses: None,
        },
        Reward {
            watt: 10,
            reputation: 2,
            capacity: 3,
        },
        Sla { timeout_sec: 120 },
    )?;
    task_engine.claim_task(&task.task_id, &identity.agent_id)?;
    let result = task_engine.execute_task(&task.task_id)?;
    task_engine.submit_task_result(&task.task_id, &result, &identity.agent_id)?;
    task_engine.verify_task(&task.task_id)?;
    let settled = task_engine.settle_task(&task.task_id)?;
    info!(task_id = %task.task_id, watt = settled.watt, "demo task settled");
    task_engine.persist_ledger(ledger_path)?;

    let action = ActionEnvelope {
        r#type: "ACTION".to_string(),
        version: "0.1".to_string(),
        action: "TASK_RESULT".to_string(),
        action_id: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now().timestamp(),
        sender: identity.agent_id.clone(),
        recipient: None,
        payload: json!({"task_id": task.task_id, "status":"SETTLED"}),
        signature: sign_payload(
            &json!({"kind":"TASK_RESULT","task_id":task.task_id}),
            identity,
        )?,
    };
    p2p.publish_json(&action)?;
    Ok(())
}

fn ignite_demo_planet(governance: &mut GovernanceEngine, identity: &Identity) -> Result<()> {
    let signer_a = Identity::new_random();
    let signer_b = Identity::new_random();
    governance.issue_license(&identity.agent_id, &identity.agent_id, "task-proof", 7);
    governance.lock_bond(&identity.agent_id, 100, 30);
    let created_at = chrono::Utc::now().timestamp();
    let approvals = vec![
        GovernanceEngine::sign_genesis(
            "planet-main",
            "Planet Main",
            &identity.agent_id,
            created_at,
            &signer_a,
        )?,
        GovernanceEngine::sign_genesis(
            "planet-main",
            "Planet Main",
            &identity.agent_id,
            created_at,
            &signer_b,
        )?,
    ];
    let request = PlanetCreationRequest {
        subnet_id: "planet-main".to_string(),
        name: "Planet Main".to_string(),
        creator: identity.agent_id.clone(),
        created_at,
        tax_rate: 0.05,
        min_bond: 50,
        min_approvals: 2,
    };
    let planet = governance.create_planet(&request, &approvals)?;
    info!(subnet = %planet.subnet_id, "demo planet created");
    Ok(())
}

fn build_oracle_feed_packet(feed: &OracleFeed, include_hashcash: bool) -> Value {
    let hashcash_value = if include_hashcash {
        hashcash::mint(&feed.publisher, 12, 80_000)
            .map(|stamp| json!({"stamp": stamp, "bits": 12, "resource": feed.publisher}))
    } else {
        None
    };

    json!({
        "type": "ORACLE_FEED",
        "version": "0.1",
        "feed": feed,
        "hashcash": hashcash_value,
    })
}

fn parse_oracle_feed_packet(bytes: &[u8]) -> Result<Option<OracleFeed>> {
    let value: Value = match serde_json::from_slice(bytes) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };

    if value["type"].as_str() != Some("ORACLE_FEED") {
        return Ok(None);
    }

    let feed: OracleFeed = serde_json::from_value(value["feed"].clone())
        .context("parse ORACLE_FEED packet payload")?;
    Ok(Some(feed))
}

fn sync_and_publish_local_oracle_feeds(
    p2p: &mut P2PNode,
    oracle_registry: &mut OracleRegistry,
    oracle_state_path: &Path,
    enable_hashcash_broadcast: bool,
    known_oracle_signatures: &mut BTreeSet<String>,
) -> Result<()> {
    *oracle_registry = OracleRegistry::load_or_new(oracle_state_path)?;
    for feed in oracle_registry.all_feeds() {
        if !known_oracle_signatures.insert(feed.signature.clone()) {
            continue;
        }
        p2p.publish_json(&build_oracle_feed_packet(&feed, enable_hashcash_broadcast))?;
    }
    Ok(())
}

fn handle_oracle_feed_packet(
    bytes: &[u8],
    oracle_registry: &mut OracleRegistry,
    oracle_state_path: &Path,
    event_log: &EventLog,
    identity: &Identity,
    known_oracle_signatures: &mut BTreeSet<String>,
) -> Result<()> {
    let Some(feed) = parse_oracle_feed_packet(bytes)? else {
        return Ok(());
    };

    let inserted = oracle_registry.ingest_feed(&feed, Some(identity), Some(event_log))?;
    if inserted {
        known_oracle_signatures.insert(feed.signature);
        oracle_registry.persist(oracle_state_path)?;
    }

    Ok(())
}

struct LoopContext<'a> {
    online_proof: &'a mut OnlineProofManager,
    online_proof_path: &'a Path,
    identity: &'a Identity,
    admission_config: &'a AdmissionConfig,
    nonce_tracker: &'a mut NonceTracker,
    web_of_trust: &'a mut WebOfTrust,
    event_log: &'a EventLog,
    oracle_registry: &'a mut OracleRegistry,
    oracle_state_path: &'a Path,
    enable_hashcash_broadcast: bool,
}

async fn run_loop(p2p: &mut P2PNode, ctx: LoopContext<'_>) -> Result<()> {
    let mut heartbeat = interval(Duration::from_secs(20));
    let mut oracle_sync = interval(Duration::from_secs(15));
    let mut known_oracle_signatures: BTreeSet<String> = BTreeSet::new();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                break;
            }
            _ = heartbeat.tick() => {
                let _ = ctx.online_proof.heartbeat(&ctx.identity.agent_id);
                let _ = ctx.online_proof.persist(ctx.online_proof_path);
            }
            _ = oracle_sync.tick() => {
                sync_and_publish_local_oracle_feeds(
                    p2p,
                    ctx.oracle_registry,
                    ctx.oracle_state_path,
                    ctx.enable_hashcash_broadcast,
                    &mut known_oracle_signatures,
                )?;
            }
            msg = p2p.poll_once() => {
                if let Some(packet) = msg? {
                    match validate_gossip_packet_with_nonce(&packet.data, ctx.admission_config, ctx.nonce_tracker) {
                        AdmissionVerdict::Accept => {
                            info!(
                                size = packet.data.len(),
                                topic = %packet.topic,
                                peer = %packet.source_peer.clone().unwrap_or_else(|| "unknown".to_string()),
                                "received gossip packet"
                            );
                            handle_oracle_feed_packet(
                                &packet.data,
                                ctx.oracle_registry,
                                ctx.oracle_state_path,
                                ctx.event_log,
                                ctx.identity,
                                &mut known_oracle_signatures,
                            )?;
                        }
                        AdmissionVerdict::Reject(reason) => {
                            if let Some(peer) = &packet.source_peer {
                                let report = ctx.web_of_trust.report_peer(peer, &ctx.identity.agent_id, &reason);
                                if ctx.web_of_trust.is_blacklisted(peer) {
                                    warn!(
                                        peer = %peer,
                                        reason = %report.reason,
                                        "peer entered local web-of-trust blacklist"
                                    );
                                }
                            }
                            warn!(
                                peer = %packet.source_peer.clone().unwrap_or_else(|| "unknown".to_string()),
                                topic = %packet.topic,
                                %reason,
                                "dropped gossip packet by admission policy"
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
