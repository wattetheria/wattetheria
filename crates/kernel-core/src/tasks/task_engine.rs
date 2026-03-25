//! Deterministic T0 task engine with publish/claim/verify/settle lifecycle.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use crate::event_log::EventLog;
use crate::identity::{Identity, IdentityCompatView};
use crate::signing::{PayloadSigner, canonical_equal, sign_payload_with};
use crate::types::{AgentStats, Reward, Sla, Task, VerificationMode, VerificationSpec};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MarketOrder {
    pub id: String,
    pub price: i64,
    pub qty: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MarketInput {
    pub buy_orders: Vec<MarketOrder>,
    pub sell_orders: Vec<MarketOrder>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MarketTrade {
    pub buy_id: String,
    pub sell_id: String,
    pub qty: i64,
    pub price: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MarketResult {
    pub family: String,
    pub trades: Vec<MarketTrade>,
    pub cleared_volume: i64,
    pub trade_count: i64,
}

#[derive(Debug, Clone, Serialize)]
struct TaskSignable<'a> {
    task_id: &'a str,
    task_family: &'a str,
    tier: &'a str,
    input_spec: &'a Value,
    verification: &'a VerificationSpec,
    reward: &'a Reward,
    sla: &'a Sla,
    created_by: &'a str,
}

pub struct TaskEngine {
    event_log: EventLog,
    identity: IdentityCompatView,
    signer: Arc<dyn PayloadSigner>,
    tasks: HashMap<String, Task>,
    ledger: HashMap<String, AgentStats>,
}

impl TaskEngine {
    #[must_use]
    pub fn new(event_log: EventLog, identity: Identity) -> Self {
        let compat = identity.compat_view();
        let signer: Arc<dyn PayloadSigner> = Arc::new(identity);
        Self::new_with_signer(event_log, compat, signer)
    }

    #[must_use]
    pub fn new_with_signer(
        event_log: EventLog,
        identity: IdentityCompatView,
        signer: Arc<dyn PayloadSigner>,
    ) -> Self {
        Self {
            event_log,
            identity,
            signer,
            tasks: HashMap::new(),
            ledger: HashMap::new(),
        }
    }

    pub fn load_ledger(path: impl AsRef<Path>) -> Result<HashMap<String, AgentStats>> {
        if !path.as_ref().exists() {
            return Ok(HashMap::new());
        }
        let raw = fs::read_to_string(path.as_ref()).context("read task engine ledger")?;
        if raw.trim().is_empty() {
            return Ok(HashMap::new());
        }
        serde_json::from_str(&raw).context("parse task engine ledger")
    }

    pub fn new_with_ledger(
        event_log: EventLog,
        identity: Identity,
        ledger_path: impl AsRef<Path>,
    ) -> Result<Self> {
        let compat = identity.compat_view();
        let signer: Arc<dyn PayloadSigner> = Arc::new(identity);
        Self::new_with_ledger_and_signer(event_log, compat, signer, ledger_path)
    }

    pub fn new_with_ledger_and_signer(
        event_log: EventLog,
        identity: IdentityCompatView,
        signer: Arc<dyn PayloadSigner>,
        ledger_path: impl AsRef<Path>,
    ) -> Result<Self> {
        let ledger = Self::load_ledger(ledger_path)?;
        Ok(Self {
            event_log,
            identity,
            signer,
            tasks: HashMap::new(),
            ledger,
        })
    }

    pub fn persist_ledger(&self, path: impl AsRef<Path>) -> Result<()> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create ledger directory")?;
        }
        fs::write(path.as_ref(), serde_json::to_string_pretty(&self.ledger)?)
            .context("write task engine ledger")
    }

    pub fn publish_task(
        &mut self,
        task_family: &str,
        tier: &str,
        input_spec: Value,
        verification: VerificationSpec,
        reward: Reward,
        sla: Sla,
    ) -> Result<Task> {
        let created_by = self.identity.agent_did.clone();
        let task_id = uuid::Uuid::new_v4().to_string();

        let signable = TaskSignable {
            task_id: &task_id,
            task_family,
            tier,
            input_spec: &input_spec,
            verification: &verification,
            reward: &reward,
            sla: &sla,
            created_by: &created_by,
        };

        let signature = sign_payload_with(&signable, self.signer.as_ref())?;

        let task = Task {
            task_id: task_id.clone(),
            task_family: task_family.to_string(),
            tier: tier.to_string(),
            input_spec,
            verification,
            reward,
            sla,
            signature,
            created_by: Some(created_by.clone()),
            claimed_by: None,
            status: Some("PUBLISHED".to_string()),
            result: None,
        };

        self.tasks.insert(task_id.clone(), task.clone());
        self.event_log.append_signed_with_signer(
            "TASK_PUBLISHED",
            json!(task),
            self.signer.as_ref(),
        )?;
        Ok(task)
    }

    pub fn claim_task(&mut self, task_id: &str, worker_id: &str) -> Result<()> {
        let task = self.tasks.get_mut(task_id).context("task not found")?;
        if task.status.as_deref() != Some("PUBLISHED") {
            bail!("task is not claimable");
        }
        task.claimed_by = Some(worker_id.to_string());
        task.status = Some("CLAIMED".to_string());
        self.event_log.append_signed_with_signer(
            "TASK_CLAIMED",
            json!({"task_id": task_id, "worker_id": worker_id}),
            self.signer.as_ref(),
        )?;
        Ok(())
    }

    pub fn execute_task(&self, task_id: &str) -> Result<Value> {
        let task = self.tasks.get(task_id).context("task not found")?;
        if task.status.as_deref() != Some("CLAIMED") {
            bail!("task is not ready for execution");
        }
        deterministic_result(task)
    }

    pub fn submit_task_result(
        &mut self,
        task_id: &str,
        result: &Value,
        worker_id: &str,
    ) -> Result<()> {
        let task = self.tasks.get_mut(task_id).context("task not found")?;
        if task.status.as_deref() != Some("CLAIMED") {
            bail!("task is not ready for result submission");
        }
        task.result = Some(result.clone());
        task.status = Some("RESULT_SUBMITTED".to_string());
        self.event_log.append_signed_with_signer(
            "TASK_RESULT",
            json!({"task_id": task_id, "worker_id": worker_id, "result": result}),
            self.signer.as_ref(),
        )?;
        Ok(())
    }

    pub fn verify_task(&mut self, task_id: &str) -> Result<bool> {
        let task = self.tasks.get_mut(task_id).context("task not found")?;
        if task.status.as_deref() != Some("RESULT_SUBMITTED") {
            bail!("task is not ready for verification");
        }

        let Some(actual) = task.result.as_ref() else {
            bail!("missing task result");
        };

        let (accepted, required_witnesses, accepted_witnesses) = match task.verification.mode {
            VerificationMode::Deterministic => {
                let recomputed = deterministic_result(task)?;
                (canonical_equal(&recomputed, actual)?, 1_u8, 1_u8)
            }
            VerificationMode::Witness => {
                let required = task.verification.witnesses.unwrap_or(1).clamp(1, 8);
                let accepted_count = verify_witness_result(task, actual, required)?;
                (accepted_count == required, required, accepted_count)
            }
        };

        task.status = Some(if accepted { "VERIFIED" } else { "REJECTED" }.to_string());

        self.event_log.append_signed_with_signer(
            "TASK_VERIFIED",
            json!({
                "task_id": task_id,
                "accepted": accepted,
                "verification_mode": task.verification.mode,
                "required_witnesses": required_witnesses,
                "accepted_witnesses": accepted_witnesses,
            }),
            self.signer.as_ref(),
        )?;
        Ok(accepted)
    }

    pub fn settle_task(&mut self, task_id: &str) -> Result<AgentStats> {
        let task = self.tasks.get_mut(task_id).context("task not found")?;
        if task.status.as_deref() != Some("VERIFIED") {
            bail!("task is not ready for settlement");
        }

        let worker = task.claimed_by.clone().context("task has no worker")?;
        let entry = self.ledger.entry(worker.clone()).or_default();
        entry.watt += task.reward.watt;
        entry.reputation += task.reward.reputation;
        entry.capacity += task.reward.capacity;
        entry.power = (1 + (entry.capacity / 10)).max(1);

        task.status = Some("SETTLED".to_string());
        self.event_log.append_signed_with_signer(
            "TASK_SETTLED",
            json!({
                "task_id": task_id,
                "worker_id": worker,
                "reward": task.reward,
                "new_stats": entry,
            }),
            self.signer.as_ref(),
        )?;

        Ok(entry.clone())
    }

    #[must_use]
    pub fn get_ledger(&self, agent_did: &str) -> AgentStats {
        self.ledger.get(agent_did).cloned().unwrap_or_default()
    }

    #[must_use]
    pub fn get_task(&self, task_id: &str) -> Option<Task> {
        self.tasks.get(task_id).cloned()
    }
}

#[must_use]
pub fn run_market_match(input: &MarketInput) -> MarketResult {
    let mut buys = input.buy_orders.clone();
    let mut sells = input.sell_orders.clone();

    // Price-time style ordering with stable tie-break on order ID.
    buys.sort_by(|a, b| b.price.cmp(&a.price).then_with(|| a.id.cmp(&b.id)));
    sells.sort_by(|a, b| a.price.cmp(&b.price).then_with(|| a.id.cmp(&b.id)));

    let mut buy_idx = 0;
    let mut sell_idx = 0;
    let mut trades = Vec::new();

    while buy_idx < buys.len() && sell_idx < sells.len() {
        let buy = &mut buys[buy_idx];
        let sell = &mut sells[sell_idx];
        if buy.price < sell.price {
            break;
        }

        let qty = buy.qty.min(sell.qty);
        // Midpoint clearing price keeps matching deterministic and symmetric.
        let price = i64::midpoint(buy.price, sell.price);
        trades.push(MarketTrade {
            buy_id: buy.id.clone(),
            sell_id: sell.id.clone(),
            qty,
            price,
        });

        buy.qty -= qty;
        sell.qty -= qty;
        if buy.qty == 0 {
            buy_idx += 1;
        }
        if sell.qty == 0 {
            sell_idx += 1;
        }
    }

    let cleared_volume = trades.iter().map(|trade| trade.qty).sum::<i64>();
    MarketResult {
        family: "market.match".to_string(),
        trades: trades.clone(),
        cleared_volume,
        trade_count: i64::try_from(trades.len()).unwrap_or(i64::MAX),
    }
}

fn deterministic_result(task: &Task) -> Result<Value> {
    match task.task_family.as_str() {
        "market.match" => {
            let input: MarketInput = serde_json::from_value(task.input_spec.clone())
                .context("parse market input for deterministic task")?;
            Ok(serde_json::to_value(run_market_match(&input))?)
        }
        _ => bail!("unsupported task family: {}", task.task_family),
    }
}

fn verify_witness_result(task: &Task, actual: &Value, required: u8) -> Result<u8> {
    let mut accepted = 0_u8;
    for _ in 0..required {
        let recomputed = deterministic_result(task)?;
        if canonical_equal(&recomputed, actual)? {
            accepted = accepted.saturating_add(1);
        }
    }
    Ok(accepted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::EventLog;
    use crate::identity::Identity;
    use crate::types::VerificationMode;
    use tempfile::tempdir;

    #[test]
    fn market_match_is_deterministic() {
        let input = MarketInput {
            buy_orders: vec![
                MarketOrder {
                    id: "b2".to_string(),
                    price: 120,
                    qty: 2,
                },
                MarketOrder {
                    id: "b1".to_string(),
                    price: 110,
                    qty: 2,
                },
            ],
            sell_orders: vec![
                MarketOrder {
                    id: "s1".to_string(),
                    price: 100,
                    qty: 1,
                },
                MarketOrder {
                    id: "s2".to_string(),
                    price: 105,
                    qty: 2,
                },
            ],
        };
        let result = run_market_match(&input);
        assert_eq!(result.trade_count, 3);
        assert_eq!(result.cleared_volume, 3);
    }

    #[test]
    fn ledger_persistence_roundtrip() {
        let temp = tempdir().unwrap();
        let ledger_path = temp.path().join("ledger.json");
        let event_log = EventLog::new(temp.path().join("events.jsonl")).unwrap();
        let identity = Identity::new_random();
        let mut engine = TaskEngine::new(event_log, identity.clone());

        let task = engine
            .publish_task(
                "market.match",
                "T0",
                json!({
                    "buy_orders": [{"id":"b1", "price":120, "qty":3}],
                    "sell_orders": [{"id":"s1", "price":100, "qty":3}],
                }),
                VerificationSpec {
                    mode: VerificationMode::Deterministic,
                    witnesses: None,
                },
                Reward {
                    watt: 15,
                    reputation: 4,
                    capacity: 6,
                },
                Sla { timeout_sec: 120 },
            )
            .unwrap();

        engine
            .claim_task(&task.task_id, &identity.agent_did)
            .unwrap();
        let result = engine.execute_task(&task.task_id).unwrap();
        engine
            .submit_task_result(&task.task_id, &result, &identity.agent_did)
            .unwrap();
        engine.verify_task(&task.task_id).unwrap();
        engine.settle_task(&task.task_id).unwrap();

        engine.persist_ledger(&ledger_path).unwrap();

        let loaded_ledger = TaskEngine::load_ledger(&ledger_path).unwrap();
        let stats = loaded_ledger.get(&identity.agent_did).unwrap();
        assert_eq!(stats.watt, 15);
        assert_eq!(stats.reputation, 4);
        assert_eq!(stats.capacity, 6);
    }

    #[test]
    fn task_lifecycle_settles_rewards() {
        let temp = tempdir().unwrap();
        let event_log = EventLog::new(temp.path().join("events.jsonl")).unwrap();
        let identity = Identity::new_random();
        let mut engine = TaskEngine::new(event_log, identity.clone());

        let task = engine
            .publish_task(
                "market.match",
                "T0",
                json!({
                    "buy_orders": [{"id":"b1", "price":120, "qty":3}],
                    "sell_orders": [{"id":"s1", "price":100, "qty":3}],
                }),
                VerificationSpec {
                    mode: VerificationMode::Deterministic,
                    witnesses: None,
                },
                Reward {
                    watt: 20,
                    reputation: 5,
                    capacity: 8,
                },
                Sla { timeout_sec: 120 },
            )
            .unwrap();

        engine
            .claim_task(&task.task_id, &identity.agent_did)
            .unwrap();
        let result = engine.execute_task(&task.task_id).unwrap();
        engine
            .submit_task_result(&task.task_id, &result, &identity.agent_did)
            .unwrap();
        assert!(engine.verify_task(&task.task_id).unwrap());

        let ledger = engine.settle_task(&task.task_id).unwrap();
        assert_eq!(ledger.watt, 20);
        assert_eq!(ledger.reputation, 5);
    }

    #[test]
    fn witness_mode_accepts_matching_result() {
        let temp = tempdir().unwrap();
        let event_log = EventLog::new(temp.path().join("events.jsonl")).unwrap();
        let identity = Identity::new_random();
        let mut engine = TaskEngine::new(event_log, identity.clone());

        let task = engine
            .publish_task(
                "market.match",
                "T0",
                json!({
                    "buy_orders": [{"id":"b1", "price":120, "qty":3}],
                    "sell_orders": [{"id":"s1", "price":100, "qty":3}],
                }),
                VerificationSpec {
                    mode: VerificationMode::Witness,
                    witnesses: Some(2),
                },
                Reward {
                    watt: 10,
                    reputation: 2,
                    capacity: 3,
                },
                Sla { timeout_sec: 120 },
            )
            .unwrap();

        engine
            .claim_task(&task.task_id, &identity.agent_did)
            .unwrap();
        let result = engine.execute_task(&task.task_id).unwrap();
        engine
            .submit_task_result(&task.task_id, &result, &identity.agent_did)
            .unwrap();

        assert!(engine.verify_task(&task.task_id).unwrap());
    }

    #[test]
    fn witness_mode_rejects_mismatched_result() {
        let temp = tempdir().unwrap();
        let event_log = EventLog::new(temp.path().join("events.jsonl")).unwrap();
        let identity = Identity::new_random();
        let mut engine = TaskEngine::new(event_log, identity.clone());

        let task = engine
            .publish_task(
                "market.match",
                "T0",
                json!({
                    "buy_orders": [{"id":"b1", "price":120, "qty":3}],
                    "sell_orders": [{"id":"s1", "price":100, "qty":3}],
                }),
                VerificationSpec {
                    mode: VerificationMode::Witness,
                    witnesses: Some(3),
                },
                Reward {
                    watt: 10,
                    reputation: 2,
                    capacity: 3,
                },
                Sla { timeout_sec: 120 },
            )
            .unwrap();

        engine
            .claim_task(&task.task_id, &identity.agent_did)
            .unwrap();
        let mut result = engine.execute_task(&task.task_id).unwrap();
        result["trade_count"] = json!(999);
        engine
            .submit_task_result(&task.task_id, &result, &identity.agent_did)
            .unwrap();

        assert!(!engine.verify_task(&task.task_id).unwrap());
    }
}
