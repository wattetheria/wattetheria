use crate::civilization::missions::CivilMission;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const SYSTEM_PUZZLE_CHALLENGE_TASK_KIND: &str = "system_puzzle_challenge";
pub const SYSTEM_PUZZLE_VERIFICATION_TASK_KIND: &str = "system_puzzle_verification";
pub const PROOF_SCHEME_ZK_HASHCASH_V1: &str = "zk_hashcash_v1";
pub const VERIFICATION_VERDICT_VALID: &str = "valid";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SystemPuzzleRewardPolicy {
    #[serde(default = "default_proposer_watt")]
    pub proposer_watt: i64,
    #[serde(default = "default_solver_watt")]
    pub solver_watt: i64,
    #[serde(default = "default_verifier_watt")]
    pub verifier_watt: i64,
}

impl Default for SystemPuzzleRewardPolicy {
    fn default() -> Self {
        Self {
            proposer_watt: default_proposer_watt(),
            solver_watt: default_solver_watt(),
            verifier_watt: default_verifier_watt(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemPuzzleChallenge {
    pub task_kind: String,
    pub challenge_id: String,
    pub slot_id: String,
    pub template_id: String,
    pub challenge_seed: String,
    pub difficulty_bits: u32,
    pub proof_scheme: String,
    pub proposer_public_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposer_agent_identity: Option<String>,
    pub issued_at: i64,
    #[serde(default)]
    pub reward_policy: SystemPuzzleRewardPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemPuzzleProofEnvelope {
    pub proof_scheme: String,
    pub public_inputs: Value,
    pub public_output: Value,
    pub proof: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemPuzzleVerificationMissionPayload {
    pub task_kind: String,
    pub challenge: SystemPuzzleChallenge,
    pub solver_public_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solver_agent_identity: Option<String>,
    pub solution_id: String,
    pub proof: SystemPuzzleProofEnvelope,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemPuzzleVerificationReceipt {
    pub task_kind: String,
    pub challenge_id: String,
    pub solution_id: String,
    pub solver_public_id: String,
    pub verifier_public_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifier_agent_identity: Option<String>,
    pub verdict: String,
    pub proof_hash: String,
    pub verified_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifier_notes: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SystemPuzzleSettlement {
    pub proposer_public_id: String,
    pub proposer_agent_identity: Option<String>,
    pub solver_public_id: String,
    pub solver_agent_identity: Option<String>,
    pub verifier_public_id: String,
    pub verifier_agent_identity: Option<String>,
    pub solution_id: String,
    pub challenge_id: String,
    pub reward_policy: SystemPuzzleRewardPolicy,
    pub receipt: SystemPuzzleVerificationReceipt,
}

#[must_use]
pub fn is_system_puzzle_verification_payload(payload: &Value) -> bool {
    payload
        .get("task_kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == SYSTEM_PUZZLE_VERIFICATION_TASK_KIND)
}

pub fn proof_hash(proof: &SystemPuzzleProofEnvelope) -> Result<String> {
    let canonical = serde_jcs::to_vec(proof).context("canonicalize system puzzle proof")?;
    Ok(hex::encode(Sha256::digest(canonical)))
}

pub fn system_puzzle_settlement_from_mission(
    mission: &CivilMission,
) -> Result<Option<SystemPuzzleSettlement>> {
    if !is_system_puzzle_verification_payload(&mission.payload) {
        return Ok(None);
    }
    let payload: SystemPuzzleVerificationMissionPayload =
        serde_json::from_value(mission.payload.clone())
            .context("parse system puzzle verification mission payload")?;
    if payload.task_kind != SYSTEM_PUZZLE_VERIFICATION_TASK_KIND {
        bail!("system puzzle verification payload has invalid task_kind");
    }
    if payload.challenge.task_kind != SYSTEM_PUZZLE_CHALLENGE_TASK_KIND {
        bail!("system puzzle challenge has invalid task_kind");
    }
    if payload.challenge.proof_scheme != payload.proof.proof_scheme {
        bail!("system puzzle proof_scheme does not match challenge");
    }
    let completed_by = mission
        .completed_by
        .as_deref()
        .context("system puzzle verification mission is missing completed_by")?;
    if mission.claimed_by.as_deref() != Some(completed_by) {
        bail!("system puzzle verification mission completer must be the claimer");
    }
    let result = mission
        .completion_result
        .clone()
        .context("system puzzle verification mission is missing completion_result")?;
    let receipt: SystemPuzzleVerificationReceipt =
        serde_json::from_value(result).context("parse system puzzle verification receipt")?;
    validate_receipt(&payload, &receipt)?;
    verify_proof(&payload)?;

    Ok(Some(SystemPuzzleSettlement {
        proposer_public_id: payload.challenge.proposer_public_id.clone(),
        proposer_agent_identity: payload.challenge.proposer_agent_identity.clone(),
        solver_public_id: payload.solver_public_id.clone(),
        solver_agent_identity: payload.solver_agent_identity.clone(),
        verifier_public_id: receipt.verifier_public_id.clone(),
        verifier_agent_identity: receipt.verifier_agent_identity.clone(),
        solution_id: payload.solution_id.clone(),
        challenge_id: payload.challenge.challenge_id.clone(),
        reward_policy: payload.challenge.reward_policy.clone(),
        receipt,
    }))
}

fn validate_receipt(
    payload: &SystemPuzzleVerificationMissionPayload,
    receipt: &SystemPuzzleVerificationReceipt,
) -> Result<()> {
    if receipt.task_kind != SYSTEM_PUZZLE_VERIFICATION_TASK_KIND {
        bail!("system puzzle verification receipt has invalid task_kind");
    }
    if receipt.verdict != VERIFICATION_VERDICT_VALID {
        bail!("system puzzle verification receipt is not valid");
    }
    if receipt.challenge_id != payload.challenge.challenge_id {
        bail!("system puzzle receipt challenge_id does not match payload");
    }
    if receipt.solution_id != payload.solution_id {
        bail!("system puzzle receipt solution_id does not match payload");
    }
    if receipt.solver_public_id != payload.solver_public_id {
        bail!("system puzzle receipt solver_public_id does not match payload");
    }
    if receipt.verifier_public_id == payload.solver_public_id {
        bail!("system puzzle solver cannot verify its own solution");
    }
    if receipt.proof_hash != proof_hash(&payload.proof)? {
        bail!("system puzzle receipt proof_hash does not match proof");
    }
    Ok(())
}

fn verify_proof(payload: &SystemPuzzleVerificationMissionPayload) -> Result<()> {
    match payload.proof.proof_scheme.as_str() {
        PROOF_SCHEME_ZK_HASHCASH_V1 => verify_zk_hashcash_v1(payload),
        scheme => bail!("unsupported system puzzle proof_scheme `{scheme}`"),
    }
}

fn verify_zk_hashcash_v1(payload: &SystemPuzzleVerificationMissionPayload) -> Result<()> {
    let nonce = payload
        .proof
        .proof
        .get("nonce")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("zk_hashcash_v1 proof is missing nonce")?;
    let expected_digest = payload
        .proof
        .proof
        .get("digest")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("zk_hashcash_v1 proof is missing digest")?;
    let digest_input = format!(
        "{}:{}:{}",
        payload.challenge.challenge_seed, payload.solver_public_id, nonce
    );
    let digest = Sha256::digest(digest_input.as_bytes());
    let digest_hex = hex::encode(digest);
    if digest_hex != expected_digest {
        bail!("zk_hashcash_v1 proof digest does not match challenge input");
    }
    if leading_zero_bits(&digest) < payload.challenge.difficulty_bits {
        bail!("zk_hashcash_v1 proof does not satisfy difficulty");
    }
    Ok(())
}

fn leading_zero_bits(bytes: &[u8]) -> u32 {
    let mut total = 0;
    for byte in bytes {
        if *byte == 0 {
            total += 8;
        } else {
            total += byte.leading_zeros();
            break;
        }
    }
    total
}

const fn default_proposer_watt() -> i64 {
    1
}

const fn default_solver_watt() -> i64 {
    8
}

const fn default_verifier_watt() -> i64 {
    2
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::civilization::missions::{
        MissionDomain, MissionPublisherKind, MissionReward, MissionScope, MissionStatus,
    };
    use serde_json::json;

    #[test]
    fn system_puzzle_verification_mission_settles_valid_proof() {
        let challenge = sample_challenge();
        let proof = sample_proof(&challenge, "solver-a");
        let receipt = sample_receipt(&challenge, "solver-a", "verifier-b", &proof);
        let mut mission = sample_mission(challenge, proof, "solver-a");
        mission.claimed_by = Some("verifier-b".to_string());
        mission.completed_by = Some("verifier-b".to_string());
        mission.completion_result = Some(serde_json::to_value(receipt).unwrap());
        mission.status = MissionStatus::Settled;

        let settlement = system_puzzle_settlement_from_mission(&mission)
            .unwrap()
            .expect("system puzzle settlement");

        assert_eq!(settlement.proposer_public_id, "proposer-a");
        assert_eq!(settlement.solver_public_id, "solver-a");
        assert_eq!(settlement.verifier_public_id, "verifier-b");
        assert_eq!(settlement.reward_policy.solver_watt, 8);
    }

    #[test]
    fn system_puzzle_rejects_self_verification() {
        let challenge = sample_challenge();
        let proof = sample_proof(&challenge, "solver-a");
        let receipt = sample_receipt(&challenge, "solver-a", "solver-a", &proof);
        let mut mission = sample_mission(challenge, proof, "solver-a");
        mission.claimed_by = Some("solver-a".to_string());
        mission.completed_by = Some("solver-a".to_string());
        mission.completion_result = Some(serde_json::to_value(receipt).unwrap());

        let error = system_puzzle_settlement_from_mission(&mission)
            .unwrap_err()
            .to_string();

        assert!(error.contains("solver cannot verify"));
    }

    fn sample_challenge() -> SystemPuzzleChallenge {
        SystemPuzzleChallenge {
            task_kind: SYSTEM_PUZZLE_CHALLENGE_TASK_KIND.to_string(),
            challenge_id: "challenge-1".to_string(),
            slot_id: "slot-2026-06-01T00:00Z".to_string(),
            template_id: "zk-hashcash-demo".to_string(),
            challenge_seed: "seed-1".to_string(),
            difficulty_bits: 0,
            proof_scheme: PROOF_SCHEME_ZK_HASHCASH_V1.to_string(),
            proposer_public_id: "proposer-a".to_string(),
            proposer_agent_identity: Some("Agent-Proposer".to_string()),
            issued_at: 1,
            reward_policy: SystemPuzzleRewardPolicy::default(),
        }
    }

    fn sample_proof(
        challenge: &SystemPuzzleChallenge,
        solver_public_id: &str,
    ) -> SystemPuzzleProofEnvelope {
        let nonce = "nonce-1";
        let digest = hex::encode(Sha256::digest(
            format!(
                "{}:{}:{}",
                challenge.challenge_seed, solver_public_id, nonce
            )
            .as_bytes(),
        ));
        SystemPuzzleProofEnvelope {
            proof_scheme: PROOF_SCHEME_ZK_HASHCASH_V1.to_string(),
            public_inputs: json!({
                "challenge_id": challenge.challenge_id,
                "challenge_seed": challenge.challenge_seed,
                "difficulty_bits": challenge.difficulty_bits,
                "solver_public_id": solver_public_id,
            }),
            public_output: json!({"digest": digest}),
            proof: json!({"nonce": nonce, "digest": digest}),
        }
    }

    fn sample_receipt(
        challenge: &SystemPuzzleChallenge,
        solver_public_id: &str,
        verifier_public_id: &str,
        proof: &SystemPuzzleProofEnvelope,
    ) -> SystemPuzzleVerificationReceipt {
        SystemPuzzleVerificationReceipt {
            task_kind: SYSTEM_PUZZLE_VERIFICATION_TASK_KIND.to_string(),
            challenge_id: challenge.challenge_id.clone(),
            solution_id: "solution-1".to_string(),
            solver_public_id: solver_public_id.to_string(),
            verifier_public_id: verifier_public_id.to_string(),
            verifier_agent_identity: Some("Agent-Verifier".to_string()),
            verdict: VERIFICATION_VERDICT_VALID.to_string(),
            proof_hash: proof_hash(proof).unwrap(),
            verified_at: 2,
            verifier_notes: None,
        }
    }

    fn sample_mission(
        challenge: SystemPuzzleChallenge,
        proof: SystemPuzzleProofEnvelope,
        solver_public_id: &str,
    ) -> CivilMission {
        CivilMission {
            mission_id: "mission-1".to_string(),
            title: "Verify system puzzle".to_string(),
            description: "Verify a submitted puzzle proof.".to_string(),
            publisher: solver_public_id.to_string(),
            publisher_kind: MissionPublisherKind::System,
            domain: MissionDomain::Power,
            scope: MissionScope::RealWorld,
            subnet_id: None,
            zone_id: None,
            required_role: None,
            required_faction: None,
            reward: Some(MissionReward {
                agent_watt: 0,
                reputation: 0,
                capacity: 0,
                treasury_share_watt: 0,
            }),
            payload: serde_json::to_value(SystemPuzzleVerificationMissionPayload {
                task_kind: SYSTEM_PUZZLE_VERIFICATION_TASK_KIND.to_string(),
                challenge,
                solver_public_id: solver_public_id.to_string(),
                solver_agent_identity: Some("Agent-Solver".to_string()),
                solution_id: "solution-1".to_string(),
                proof,
            })
            .unwrap(),
            lat: None,
            lng: None,
            coordinate_source: None,
            created_at: 1,
            updated_at: 1,
            claimed_by: None,
            completed_by: None,
            completion_result: None,
            settled_at: None,
            status: MissionStatus::Completed,
        }
    }
}
