use crate::civilization::missions::{CivilMission, MissionBoard, MissionStatus};
use crate::types::AgentStats;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const DEFAULT_ECONOMIC_POLICY_VERSION: u64 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EconomicPolicy {
    pub version: u64,
    pub enabled: bool,
    pub genesis_supply_watt: i64,
    pub per_agent_daily_cap_watt: i64,
    pub rewards: FixedRewardSchedule,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FixedRewardSchedule {
    pub mission_publish_watt: i64,
    pub mission_settle_publisher_watt: i64,
    pub topic_post_watt: i64,
    pub topic_reply_watt: i64,
    pub custom_agent_publish_watt: i64,
    pub external_agent_call_success_watt: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WalletBoundBalance {
    pub policy_version: u64,
    pub watt: i64,
    pub reputation: i64,
    pub capacity: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WalletBalanceRecord {
    pub controller_id: String,
    pub public_id: Option<String>,
    pub policy_version: u64,
    pub watt_balance: i64,
    pub reputation: i64,
    pub capacity: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WalletBalanceState {
    pub balances: BTreeMap<String, WalletBalanceRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RewardDelta {
    timestamp: i64,
    source_id: String,
    watt: i64,
    reputation: i64,
    capacity: i64,
}

impl Default for EconomicPolicy {
    fn default() -> Self {
        Self {
            version: DEFAULT_ECONOMIC_POLICY_VERSION,
            enabled: true,
            genesis_supply_watt: 1_000_000_000,
            per_agent_daily_cap_watt: 10_000,
            rewards: FixedRewardSchedule::default(),
        }
    }
}

impl Default for FixedRewardSchedule {
    fn default() -> Self {
        Self {
            mission_publish_watt: 1,
            mission_settle_publisher_watt: 2,
            topic_post_watt: 1,
            topic_reply_watt: 1,
            custom_agent_publish_watt: 5,
            external_agent_call_success_watt: 1,
        }
    }
}

impl WalletBoundBalance {
    #[must_use]
    pub fn stats(&self) -> AgentStats {
        AgentStats {
            power: (1 + (self.capacity / 10)).max(0),
            watt: self.watt,
            reputation: self.reputation,
            capacity: self.capacity,
        }
    }
}

impl WalletBalanceState {
    #[must_use]
    pub fn get(
        &self,
        controller_id: &str,
        public_id: Option<&str>,
    ) -> Option<&WalletBalanceRecord> {
        self.balances
            .get(&wallet_balance_key(controller_id, public_id))
    }

    pub fn upsert(
        &mut self,
        controller_id: &str,
        public_id: Option<&str>,
        balance: &WalletBoundBalance,
        updated_at: i64,
    ) -> WalletBalanceRecord {
        let record = WalletBalanceRecord {
            controller_id: controller_id.to_string(),
            public_id: public_id.map(str::to_string),
            policy_version: balance.policy_version,
            watt_balance: balance.watt,
            reputation: balance.reputation,
            capacity: balance.capacity,
            updated_at,
        };
        self.balances
            .insert(wallet_balance_key(controller_id, public_id), record.clone());
        record
    }
}

impl WalletBalanceRecord {
    #[must_use]
    pub fn balance(&self) -> WalletBoundBalance {
        WalletBoundBalance {
            policy_version: self.policy_version,
            watt: self.watt_balance,
            reputation: self.reputation,
            capacity: self.capacity,
        }
    }
}

fn wallet_balance_key(controller_id: &str, public_id: Option<&str>) -> String {
    format!("{}::{}", controller_id, public_id.unwrap_or(""))
}

#[must_use]
pub fn wallet_bound_balance_from_missions(
    policy: &EconomicPolicy,
    missions: &MissionBoard,
    controller_id: &str,
    public_id: Option<&str>,
) -> WalletBoundBalance {
    if !policy.enabled {
        return WalletBoundBalance {
            policy_version: policy.version,
            watt: 0,
            reputation: 0,
            capacity: 0,
        };
    }

    let mut deltas = Vec::new();
    for mission in missions.list(None) {
        collect_mission_deltas(policy, &mission, controller_id, public_id, &mut deltas);
    }
    deltas.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.source_id.cmp(&right.source_id))
    });

    let mut watt_by_day = BTreeMap::<i64, i64>::new();
    let mut balance = WalletBoundBalance {
        policy_version: policy.version,
        watt: 0,
        reputation: 0,
        capacity: 0,
    };
    for delta in deltas {
        balance.watt += capped_watt_delta(policy, &mut watt_by_day, &delta);
        balance.reputation += delta.reputation;
        balance.capacity += delta.capacity;
    }
    balance
}

fn collect_mission_deltas(
    policy: &EconomicPolicy,
    mission: &CivilMission,
    controller_id: &str,
    public_id: Option<&str>,
    deltas: &mut Vec<RewardDelta>,
) {
    if mission.status == MissionStatus::Cancelled {
        return;
    }
    if owns_identifier(&mission.publisher, controller_id, public_id) {
        deltas.push(RewardDelta {
            timestamp: mission.created_at,
            source_id: format!("mission.publish:{}", mission.mission_id),
            watt: policy.rewards.mission_publish_watt,
            reputation: 0,
            capacity: 0,
        });
        if mission.status == MissionStatus::Settled {
            deltas.push(RewardDelta {
                timestamp: mission.settled_at.unwrap_or(mission.created_at),
                source_id: format!("mission.settle.publisher:{}", mission.mission_id),
                watt: policy.rewards.mission_settle_publisher_watt,
                reputation: 0,
                capacity: 0,
            });
        }
    }

    if mission.status == MissionStatus::Settled
        && mission.completed_by.as_deref() == Some(controller_id)
    {
        deltas.push(RewardDelta {
            timestamp: mission.settled_at.unwrap_or(mission.created_at),
            source_id: format!("mission.settle.executor:{}", mission.mission_id),
            watt: mission.reward.agent_watt,
            reputation: mission.reward.reputation,
            capacity: mission.reward.capacity,
        });
    }
}

fn owns_identifier(value: &str, controller_id: &str, public_id: Option<&str>) -> bool {
    value == controller_id || public_id == Some(value)
}

fn capped_watt_delta(
    policy: &EconomicPolicy,
    watt_by_day: &mut BTreeMap<i64, i64>,
    delta: &RewardDelta,
) -> i64 {
    if delta.watt <= 0 {
        return 0;
    }
    if policy.per_agent_daily_cap_watt <= 0 {
        return delta.watt;
    }
    let day = delta.timestamp.div_euclid(86_400);
    let used = watt_by_day.entry(day).or_insert(0);
    let remaining = policy.per_agent_daily_cap_watt.saturating_sub(*used);
    let applied = delta.watt.min(remaining);
    *used += applied;
    applied
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::civilization::missions::{MissionDomain, MissionPublisherKind, MissionReward};
    use serde_json::json;

    #[test]
    fn default_policy_is_fixed_by_action_type() {
        let policy = EconomicPolicy::default();

        assert!(policy.enabled);
        assert_eq!(policy.version, 1);
        assert_eq!(policy.rewards.mission_publish_watt, 1);
        assert_eq!(policy.rewards.mission_settle_publisher_watt, 2);
        assert_eq!(policy.rewards.custom_agent_publish_watt, 5);
    }

    #[test]
    fn mission_rewards_are_wallet_bound_and_deterministic() {
        let mut board = MissionBoard::default();
        let mission = board.publish(
            "Publish market task",
            "Find an executor.",
            "captain-public",
            MissionPublisherKind::Player,
            MissionDomain::Trade,
            None,
            None,
            None,
            None,
            MissionReward {
                agent_watt: 40,
                reputation: 4,
                capacity: 1,
                treasury_share_watt: 0,
            },
            json!({}),
        );

        let published = wallet_bound_balance_from_missions(
            &EconomicPolicy::default(),
            &board,
            "agent-a",
            Some("captain-public"),
        );
        assert_eq!(published.watt, 1);

        board.claim(&mission.mission_id, "agent-a").unwrap();
        board.complete(&mission.mission_id, "agent-a").unwrap();
        board.settle(&mission.mission_id).unwrap();

        let settled = wallet_bound_balance_from_missions(
            &EconomicPolicy::default(),
            &board,
            "agent-a",
            Some("captain-public"),
        );
        assert_eq!(settled.watt, 43);
        assert_eq!(settled.reputation, 4);
        assert_eq!(settled.capacity, 1);
    }

    #[test]
    fn disabled_policy_yields_no_balance() {
        let mut board = MissionBoard::default();
        let policy = EconomicPolicy {
            enabled: false,
            ..EconomicPolicy::default()
        };
        board.publish(
            "No reward",
            "Policy disabled.",
            "agent-a",
            MissionPublisherKind::Player,
            MissionDomain::Trade,
            None,
            None,
            None,
            None,
            MissionReward {
                agent_watt: 40,
                reputation: 4,
                capacity: 1,
                treasury_share_watt: 0,
            },
            json!({}),
        );

        let balance = wallet_bound_balance_from_missions(&policy, &board, "agent-a", None);
        assert_eq!(balance.watt, 0);
        assert_eq!(balance.reputation, 0);
        assert_eq!(balance.capacity, 0);
    }

    #[test]
    fn per_agent_daily_cap_limits_watt_only() {
        let mut board = MissionBoard::default();
        let policy = EconomicPolicy {
            per_agent_daily_cap_watt: 10,
            rewards: FixedRewardSchedule {
                mission_publish_watt: 8,
                ..FixedRewardSchedule::default()
            },
            ..EconomicPolicy::default()
        };
        let first = board.publish(
            "Task one",
            "One.",
            "agent-a",
            MissionPublisherKind::Player,
            MissionDomain::Trade,
            None,
            None,
            None,
            None,
            MissionReward {
                agent_watt: 40,
                reputation: 4,
                capacity: 1,
                treasury_share_watt: 0,
            },
            json!({}),
        );
        let second = board.publish(
            "Task two",
            "Two.",
            "agent-a",
            MissionPublisherKind::Player,
            MissionDomain::Trade,
            None,
            None,
            None,
            None,
            MissionReward {
                agent_watt: 40,
                reputation: 4,
                capacity: 1,
                treasury_share_watt: 0,
            },
            json!({}),
        );
        board.claim(&first.mission_id, "agent-a").unwrap();
        board.complete(&first.mission_id, "agent-a").unwrap();
        board.settle(&first.mission_id).unwrap();
        board.claim(&second.mission_id, "agent-a").unwrap();
        board.complete(&second.mission_id, "agent-a").unwrap();
        board.settle(&second.mission_id).unwrap();

        let balance = wallet_bound_balance_from_missions(&policy, &board, "agent-a", None);
        assert_eq!(balance.watt, 10);
        assert_eq!(balance.reputation, 8);
        assert_eq!(balance.capacity, 2);
    }

    #[test]
    fn wallet_balance_state_persists_fixed_balance_field() {
        let mut state = WalletBalanceState::default();
        let record = state.upsert(
            "agent-a",
            Some("captain-a"),
            &WalletBoundBalance {
                policy_version: 7,
                watt: 42,
                reputation: 3,
                capacity: 2,
            },
            123,
        );

        assert_eq!(record.watt_balance, 42);
        let stored = state.get("agent-a", Some("captain-a")).unwrap();
        assert_eq!(stored.watt_balance, 42);
        assert_eq!(stored.balance().watt, 42);
        assert_eq!(stored.updated_at, 123);
    }
}
