//! Local standing derived from the mission board.
//!
//! Watt is not computed here. The gateway ledger is the only writer of Watt balances; nodes
//! read them over HTTP (`control-plane::ledger_client`). What a node still derives locally
//! from its own mission history is reputation and capacity, which feed the ranking score
//! together with the ledger balance.

use crate::civilization::missions::{MissionBoard, MissionStatus};
use crate::types::AgentStats;
use serde::{Deserialize, Serialize};

const DEFAULT_AGENT_COMPUTE: i64 = 1;
const DEFAULT_AGENT_PRESTIGE: i64 = 0;

/// Reputation and capacity an Agent earned by settling missions on this node.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MissionStanding {
    pub reputation: i64,
    pub capacity: i64,
}

impl MissionStanding {
    /// Combines local standing with the Watt balance the ledger reports.
    #[must_use]
    pub fn stats_with_watt(&self, watt: i64) -> AgentStats {
        AgentStats {
            power: (DEFAULT_AGENT_COMPUTE + (self.capacity / 10)).max(DEFAULT_AGENT_COMPUTE),
            watt,
            reputation: self.reputation,
            capacity: self.capacity,
        }
    }
}

#[must_use]
pub fn ranking_compute(stats: &AgentStats) -> i64 {
    stats.power.max(DEFAULT_AGENT_COMPUTE)
}

#[must_use]
pub fn ranking_prestige(_stats: &AgentStats) -> i64 {
    DEFAULT_AGENT_PRESTIGE
}

#[must_use]
pub fn ranking_score_tenths(stats: &AgentStats) -> i64 {
    stats
        .watt
        .saturating_add(ranking_compute(stats).saturating_mul(100))
        .saturating_add(ranking_prestige(stats).saturating_mul(1_000))
}

#[must_use]
pub fn ranking_score(stats: &AgentStats) -> f64 {
    score_from_tenths(ranking_score_tenths(stats))
}

fn score_from_tenths(score_tenths: i64) -> f64 {
    let whole = score_tenths.div_euclid(10);
    let tenths = score_tenths.rem_euclid(10);
    format!("{whole}.{tenths}")
        .parse::<f64>()
        .expect("ranking score with one decimal is finite")
}

/// Sums the reputation and capacity of every settled mission `controller_id` executed.
#[must_use]
pub fn mission_standing(missions: &MissionBoard, controller_id: &str) -> MissionStanding {
    let mut standing = MissionStanding::default();
    for mission in missions.list(Some(&MissionStatus::Settled)) {
        if mission.completed_by.as_deref() != Some(controller_id) {
            continue;
        }
        if let Some(reward) = mission.reward.as_ref() {
            standing.reputation += reward.reputation;
            standing.capacity += reward.capacity;
        }
    }
    standing
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::civilization::missions::{MissionDomain, MissionPublisherKind, MissionReward};
    use serde_json::json;

    #[test]
    fn mission_standing_counts_only_settled_missions_of_the_executor() {
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
            },
            json!({}),
        );
        assert_eq!(
            mission_standing(&board, "agent-a"),
            MissionStanding::default()
        );

        board.claim(&mission.mission_id, "agent-a").unwrap();
        board
            .complete(&mission.mission_id, "agent-a", None)
            .unwrap();
        assert_eq!(
            mission_standing(&board, "agent-a"),
            MissionStanding::default()
        );

        board.settle(&mission.mission_id).unwrap();
        let standing = mission_standing(&board, "agent-a");
        assert_eq!(
            standing,
            MissionStanding {
                reputation: 4,
                capacity: 1
            }
        );
        assert_eq!(
            mission_standing(&board, "agent-b"),
            MissionStanding::default()
        );
    }

    #[test]
    fn stats_take_watt_from_the_ledger_not_from_missions() {
        let standing = MissionStanding {
            reputation: 3,
            capacity: 25,
        };
        let stats = standing.stats_with_watt(120);
        assert_eq!(stats.watt, 120);
        assert_eq!(stats.reputation, 3);
        assert_eq!(stats.capacity, 25);
        assert_eq!(stats.power, 3);
        assert_eq!(ranking_score_tenths(&stats), 120 + 300);
        assert!((ranking_score(&stats) - 42.0).abs() < f64::EPSILON);
    }
}
