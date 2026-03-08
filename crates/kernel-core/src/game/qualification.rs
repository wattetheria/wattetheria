use serde::{Deserialize, Serialize};

use crate::civilization::metrics::CivilizationScores;
use crate::civilization::missions::{MissionBoard, MissionDomain, MissionStatus};
use crate::civilization::profiles::{CitizenProfile, RolePath};
use crate::governance::GovernanceEngine;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QualificationState {
    Locked,
    Starter,
    Qualified,
    Advanced,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QualificationTrack {
    pub key: String,
    pub title: String,
    pub state: QualificationState,
    pub progress_pct: u8,
    pub next_requirement: String,
    pub summary: String,
    pub unlocks: Vec<QualificationUnlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QualificationUnlock {
    pub key: String,
    pub title: String,
    pub description: String,
    pub unlocked: bool,
}

#[must_use]
pub fn compute_qualifications(
    controller_id: &str,
    profile: Option<&CitizenProfile>,
    scores: &CivilizationScores,
    missions: &MissionBoard,
    governance: &GovernanceEngine,
) -> Vec<QualificationTrack> {
    let settled = missions.list(Some(&MissionStatus::Settled));
    let settled_for_agent: Vec<_> = settled
        .into_iter()
        .filter(|mission| mission.completed_by.as_deref() == Some(controller_id))
        .collect();
    let role_track = role_track(profile, scores, &settled_for_agent);
    let civic = civic_track(controller_id, scores, governance);
    let expansion = qualification(
        "expansion_planning",
        "Expansion Planning",
        governance
            .list_planets()
            .into_iter()
            .filter(|planet| {
                planet.creator == controller_id || planet.validators.contains(controller_id)
            })
            .count(),
        scores.total_influence,
        "Tracks readiness to participate in future map growth and frontier expansion.",
    );

    vec![role_track, civic, expansion]
}

fn role_track(
    profile: Option<&CitizenProfile>,
    scores: &CivilizationScores,
    settled_for_agent: &[crate::civilization::missions::CivilMission],
) -> QualificationTrack {
    let domain_count = |domain: MissionDomain| -> usize {
        settled_for_agent
            .iter()
            .filter(|mission| mission.domain == domain)
            .count()
    };
    match profile.map(|profile| &profile.role) {
        Some(RolePath::Operator) => qualification(
            "operator_logistics",
            "Operator Logistics",
            domain_count(MissionDomain::Wealth),
            scores.wealth,
            "Proves infrastructure and throughput reliability.",
        ),
        Some(RolePath::Broker) => qualification(
            "broker_corridor",
            "Broker Corridor",
            domain_count(MissionDomain::Trade),
            scores.trade,
            "Proves market coordination and corridor liquidity.",
        ),
        Some(RolePath::Enforcer) => qualification(
            "enforcer_patrol",
            "Enforcer Patrol",
            domain_count(MissionDomain::Security),
            scores.security,
            "Proves route protection and coercive authority.",
        ),
        Some(RolePath::Artificer) => qualification(
            "artificer_signal",
            "Artificer Signal",
            domain_count(MissionDomain::Culture),
            scores.culture,
            "Proves cultural gravity and place-making impact.",
        ),
        None => QualificationTrack {
            key: "role_unset".to_string(),
            title: "Role Alignment".to_string(),
            state: QualificationState::Locked,
            progress_pct: 0,
            next_requirement: "Choose a role profile.".to_string(),
            summary: "Choose a role profile before qualification tracks can advance.".to_string(),
            unlocks: Vec::new(),
        },
    }
}

fn civic_track(
    controller_id: &str,
    scores: &CivilizationScores,
    governance: &GovernanceEngine,
) -> QualificationTrack {
    let has_license = governance.has_valid_license(controller_id);
    let has_bond = governance.has_active_bond(controller_id, 1);
    let influence_ready = scores.total_influence >= 250;
    let civic_state = if has_license && has_bond {
        QualificationState::Qualified
    } else if influence_ready {
        QualificationState::Starter
    } else {
        QualificationState::Locked
    };
    QualificationTrack {
        key: "civic_governance".to_string(),
        title: "Civic Governance".to_string(),
        state: civic_state.clone(),
        progress_pct: civic_progress(has_license, has_bond, influence_ready),
        next_requirement: civic_requirement(has_license, has_bond, influence_ready),
        summary:
            "Tracks readiness to move from mission work into formal sovereignty participation."
                .to_string(),
        unlocks: vec![
            unlock(
                "proposal_participation",
                "Proposal participation",
                "Review and vote on local governance proposals with real civic stake.",
                civic_state != QualificationState::Locked,
            ),
            unlock(
                "treasury_stewardship",
                "Treasury stewardship",
                "Fund and supervise treasury-backed public work for your home anchor.",
                matches!(
                    civic_state,
                    QualificationState::Qualified | QualificationState::Advanced
                ),
            ),
            unlock(
                "sovereignty_readiness",
                "Sovereignty readiness",
                "Take the final step from civic participant toward active sovereignty control.",
                matches!(
                    civic_state,
                    QualificationState::Qualified | QualificationState::Advanced
                ),
            ),
        ],
    }
}

fn qualification(
    key: &str,
    title: &str,
    mission_count: usize,
    score: i64,
    summary: &str,
) -> QualificationTrack {
    let state = state_from_mission_and_score(mission_count, score);
    let progress_pct = progress_from_mission_and_score(mission_count, score);
    let next_requirement = requirement_from_mission_and_score(mission_count, score);

    QualificationTrack {
        key: key.to_string(),
        title: title.to_string(),
        state: state.clone(),
        unlocks: role_unlocks(key, &state),
        progress_pct,
        next_requirement,
        summary: summary.to_string(),
    }
}

fn state_from_mission_and_score(mission_count: usize, score: i64) -> QualificationState {
    if mission_count >= 4 || score >= 250 {
        QualificationState::Advanced
    } else if mission_count >= 2 || score >= 120 {
        QualificationState::Qualified
    } else if mission_count >= 1 || score >= 40 {
        QualificationState::Starter
    } else {
        QualificationState::Locked
    }
}

fn progress_from_mission_and_score(mission_count: usize, score: i64) -> u8 {
    let mission_progress = u8::try_from((mission_count.min(4) * 100) / 4).unwrap_or(100);
    let score_progress = u8::try_from((score.clamp(0, 250) * 100) / 250).unwrap_or(100);
    mission_progress.max(score_progress)
}

fn requirement_from_mission_and_score(mission_count: usize, score: i64) -> String {
    match state_from_mission_and_score(mission_count, score) {
        QualificationState::Locked => "Finish 1 aligned mission or reach 40 score.".to_string(),
        QualificationState::Starter => "Finish 2 aligned missions or reach 120 score.".to_string(),
        QualificationState::Qualified => {
            "Finish 4 aligned missions or reach 250 score.".to_string()
        }
        QualificationState::Advanced => "Track fully unlocked for this phase.".to_string(),
    }
}

fn civic_progress(has_license: bool, has_bond: bool, influence_ready: bool) -> u8 {
    let completed = usize::from(influence_ready) + usize::from(has_license) + usize::from(has_bond);
    u8::try_from((completed * 100) / 3).unwrap_or(100)
}

fn civic_requirement(has_license: bool, has_bond: bool, influence_ready: bool) -> String {
    if !influence_ready {
        "Reach 250 total influence to enter civic readiness.".to_string()
    } else if !has_license {
        "Secure a valid civic license.".to_string()
    } else if !has_bond {
        "Lock an active sovereignty bond.".to_string()
    } else {
        "Track fully unlocked for this phase.".to_string()
    }
}

fn role_unlocks(key: &str, state: &QualificationState) -> Vec<QualificationUnlock> {
    let domain_title = match key {
        "operator_logistics" => "Infrastructure authority",
        "broker_corridor" => "Corridor leverage",
        "enforcer_patrol" => "Route authority",
        "artificer_signal" => "Cultural gravity",
        "expansion_planning" => "Expansion sponsorship",
        _ => "Role progression",
    };
    vec![
        unlock(
            "starter_contracts",
            "Starter contracts",
            "Take the first role-aligned missions for this profession.",
            !matches!(state, QualificationState::Locked),
        ),
        unlock(
            "phase_mission_packs",
            "Stage mission packs",
            "Receive stronger role and civic mission packs tied to your current phase.",
            matches!(
                state,
                QualificationState::Qualified | QualificationState::Advanced
            ),
        ),
        unlock(
            "advanced_role_authority",
            domain_title,
            "Signal to the client and future systems that this role can carry higher-stakes work.",
            matches!(state, QualificationState::Advanced),
        ),
    ]
}

fn unlock(key: &str, title: &str, description: &str, unlocked: bool) -> QualificationUnlock {
    QualificationUnlock {
        key: key.to_string(),
        title: title.to_string(),
        description: description.to_string(),
        unlocked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::civilization::missions::{MissionPublisherKind, MissionReward};
    use crate::civilization::profiles::{Faction, StrategyProfile};

    #[test]
    fn qualifications_reflect_role_and_governance_readiness() {
        let mut board = MissionBoard::default();
        let mission = board.publish(
            "Route support",
            "Keep the corridor liquid",
            "planet-test",
            MissionPublisherKind::Organization,
            MissionDomain::Trade,
            Some("planet-test".to_string()),
            Some("genesis-core".to_string()),
            Some(RolePath::Broker),
            Some(Faction::Freeport),
            MissionReward {
                agent_watt: 10,
                reputation: 2,
                capacity: 1,
                treasury_share_watt: 0,
            },
            serde_json::json!({}),
        );
        board.claim(&mission.mission_id, "agent-a").unwrap();
        board.complete(&mission.mission_id, "agent-a").unwrap();
        board.settle(&mission.mission_id).unwrap();

        let profile = CitizenProfile {
            agent_id: "agent-a".to_string(),
            faction: Faction::Freeport,
            role: RolePath::Broker,
            strategy: StrategyProfile::Balanced,
            home_subnet_id: Some("planet-test".to_string()),
            home_zone_id: Some("genesis-core".to_string()),
            updated_at: 0,
        };
        let tracks = compute_qualifications(
            "agent-a",
            Some(&profile),
            &CivilizationScores {
                wealth: 20,
                power: 15,
                security: 10,
                trade: 80,
                culture: 5,
                total_influence: 140,
            },
            &board,
            &GovernanceEngine::default(),
        );
        assert_eq!(tracks.len(), 3);
        assert!(tracks.iter().any(
            |track| track.key == "broker_corridor" && track.state != QualificationState::Locked
        ));
        assert!(
            tracks
                .iter()
                .all(|track| !track.next_requirement.is_empty())
        );
        assert!(
            tracks
                .iter()
                .find(|track| track.key == "broker_corridor")
                .is_some_and(|track| !track.unlocks.is_empty())
        );
    }
}
