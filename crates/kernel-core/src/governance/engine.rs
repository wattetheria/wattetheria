//! Subnet-as-planet governance rules: license, bond, and genesis multisig.

use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use crate::identity::Identity;
use crate::signing::{canonical_bytes, sign_payload, verify_payload};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CivicLicense {
    pub agent_id: String,
    pub issued_at: i64,
    pub expires_at: i64,
    pub issued_by: String,
    pub proof_event_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SovereigntyBond {
    pub agent_id: String,
    pub amount_watt: i64,
    pub locked_until: i64,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenesisApproval {
    pub signer_agent_id: String,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubnetPlanet {
    pub subnet_id: String,
    pub name: String,
    pub creator: String,
    pub tax_rate: f64,
    pub created_at: i64,
    pub validators: BTreeSet<String>,
    pub relays: BTreeSet<String>,
    pub archivists: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GenesisPayload<'a> {
    subnet_id: &'a str,
    name: &'a str,
    creator: &'a str,
    created_at: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct GovernanceEngine {
    licenses: BTreeMap<String, CivicLicense>,
    bonds: BTreeMap<String, SovereigntyBond>,
    planets: BTreeMap<String, SubnetPlanet>,
    proposals: BTreeMap<String, GovernanceProposal>,
    validator_heartbeats: BTreeMap<String, BTreeMap<String, i64>>,
}

#[derive(Debug, Clone)]
pub struct PlanetCreationRequest {
    pub subnet_id: String,
    pub name: String,
    pub creator: String,
    pub created_at: i64,
    pub tax_rate: f64,
    pub min_bond: i64,
    pub min_approvals: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    Open,
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernanceProposal {
    pub proposal_id: String,
    pub subnet_id: String,
    pub kind: String,
    pub payload: serde_json::Value,
    pub created_by: String,
    pub created_at: i64,
    pub votes_for: BTreeSet<String>,
    pub votes_against: BTreeSet<String>,
    pub status: ProposalStatus,
}

impl GovernanceEngine {
    pub fn load_or_new(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create governance state directory")?;
        }
        if !path.as_ref().exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path.as_ref()).context("read governance state")?;
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&raw).context("parse governance state")
    }

    pub fn persist(&self, path: impl AsRef<Path>) -> Result<()> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create governance state directory")?;
        }
        fs::write(path.as_ref(), serde_json::to_string_pretty(self)?)
            .context("write governance state")
    }

    pub fn issue_license(
        &mut self,
        agent_id: &str,
        issuer: &str,
        proof_event_hash: &str,
        ttl_days: i64,
    ) -> CivicLicense {
        let now = Utc::now().timestamp();
        let license = CivicLicense {
            agent_id: agent_id.to_string(),
            issued_at: now,
            expires_at: now + ttl_days * 24 * 3600,
            issued_by: issuer.to_string(),
            proof_event_hash: proof_event_hash.to_string(),
        };
        self.licenses.insert(agent_id.to_string(), license.clone());
        license
    }

    pub fn lock_bond(
        &mut self,
        agent_id: &str,
        amount_watt: i64,
        lock_days: i64,
    ) -> SovereigntyBond {
        let now = Utc::now().timestamp();
        let bond = SovereigntyBond {
            agent_id: agent_id.to_string(),
            amount_watt,
            locked_until: now + lock_days * 24 * 3600,
            active: true,
        };
        self.bonds.insert(agent_id.to_string(), bond.clone());
        bond
    }

    #[must_use]
    pub fn has_valid_license(&self, agent_id: &str) -> bool {
        self.licenses
            .get(agent_id)
            .is_some_and(|license| license.expires_at >= Utc::now().timestamp())
    }

    #[must_use]
    pub fn has_active_bond(&self, agent_id: &str, min_amount: i64) -> bool {
        self.bonds.get(agent_id).is_some_and(|bond| {
            bond.active
                && bond.amount_watt >= min_amount
                && bond.locked_until >= Utc::now().timestamp()
        })
    }

    pub fn sign_genesis(
        subnet_id: &str,
        name: &str,
        creator: &str,
        created_at: i64,
        identity: &Identity,
    ) -> Result<GenesisApproval> {
        let payload = GenesisPayload {
            subnet_id,
            name,
            creator,
            created_at,
        };
        Ok(GenesisApproval {
            signer_agent_id: identity.agent_id.clone(),
            signature: sign_payload(&payload, identity)?,
        })
    }

    pub fn create_planet(
        &mut self,
        request: &PlanetCreationRequest,
        approvals: &[GenesisApproval],
    ) -> Result<SubnetPlanet> {
        // Gate 1: prevent duplicate subnet IDs.
        if self.planets.contains_key(&request.subnet_id) {
            bail!("subnet already exists");
        }
        // Gate 2: creator must hold a valid civic license.
        if !self.has_valid_license(&request.creator) {
            bail!("creator missing valid civic license");
        }
        // Gate 3: creator must lock enough sovereignty bond.
        if !self.has_active_bond(&request.creator, request.min_bond) {
            bail!("creator missing active sovereignty bond");
        }

        let payload = GenesisPayload {
            subnet_id: &request.subnet_id,
            name: &request.name,
            creator: &request.creator,
            created_at: request.created_at,
        };

        let unique_signers: BTreeSet<_> = approvals
            .iter()
            .map(|a| a.signer_agent_id.clone())
            .collect();
        if unique_signers.len() < request.min_approvals {
            bail!("not enough unique genesis approvals");
        }

        // Every approval must sign the same genesis payload.
        for approval in approvals {
            if !verify_payload(&payload, &approval.signature, &approval.signer_agent_id)
                .context("verify genesis approval")?
            {
                bail!("invalid genesis signature");
            }
        }

        let planet = SubnetPlanet {
            subnet_id: request.subnet_id.clone(),
            name: request.name.clone(),
            creator: request.creator.clone(),
            tax_rate: request.tax_rate,
            created_at: request.created_at,
            validators: unique_signers.clone(),
            relays: BTreeSet::from([request.creator.clone()]),
            archivists: unique_signers,
        };
        self.planets
            .insert(request.subnet_id.clone(), planet.clone());
        Ok(planet)
    }

    #[must_use]
    pub fn planet(&self, subnet_id: &str) -> Option<&SubnetPlanet> {
        self.planets.get(subnet_id)
    }

    #[must_use]
    pub fn list_planets(&self) -> Vec<SubnetPlanet> {
        self.planets.values().cloned().collect()
    }

    #[must_use]
    pub fn list_proposals(&self, subnet_filter: Option<&str>) -> Vec<GovernanceProposal> {
        let mut proposals: Vec<_> = self
            .proposals
            .values()
            .filter(|proposal| subnet_filter.is_none_or(|subnet| proposal.subnet_id == subnet))
            .cloned()
            .collect();
        proposals.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.proposal_id.cmp(&b.proposal_id))
        });
        proposals
    }

    pub fn genesis_payload_digest(
        subnet_id: &str,
        name: &str,
        creator: &str,
        created_at: i64,
    ) -> Result<String> {
        let payload = GenesisPayload {
            subnet_id,
            name,
            creator,
            created_at,
        };
        Ok(hex::encode(canonical_bytes(&payload)?))
    }

    pub fn create_proposal(
        &mut self,
        subnet_id: &str,
        kind: &str,
        payload: serde_json::Value,
        created_by: &str,
    ) -> Result<GovernanceProposal> {
        let planet = self
            .planets
            .get(subnet_id)
            .context("subnet not found for proposal")?;
        if !planet.validators.contains(created_by) && planet.creator != created_by {
            bail!("creator is not allowed to open proposal");
        }

        let proposal = GovernanceProposal {
            proposal_id: uuid::Uuid::new_v4().to_string(),
            subnet_id: subnet_id.to_string(),
            kind: kind.to_string(),
            payload,
            created_by: created_by.to_string(),
            created_at: Utc::now().timestamp(),
            votes_for: BTreeSet::new(),
            votes_against: BTreeSet::new(),
            status: ProposalStatus::Open,
        };
        self.proposals
            .insert(proposal.proposal_id.clone(), proposal.clone());
        Ok(proposal)
    }

    pub fn vote_proposal(&mut self, proposal_id: &str, voter: &str, approve: bool) -> Result<()> {
        let proposal = self
            .proposals
            .get_mut(proposal_id)
            .context("proposal not found")?;
        if proposal.status != ProposalStatus::Open {
            bail!("proposal is already finalized");
        }

        let planet = self
            .planets
            .get(&proposal.subnet_id)
            .context("proposal subnet missing")?;
        if !planet.validators.contains(voter) {
            bail!("voter is not a validator");
        }

        proposal.votes_for.remove(voter);
        proposal.votes_against.remove(voter);
        if approve {
            proposal.votes_for.insert(voter.to_string());
        } else {
            proposal.votes_against.insert(voter.to_string());
        }
        Ok(())
    }

    pub fn finalize_proposal(
        &mut self,
        proposal_id: &str,
        min_votes_for: usize,
    ) -> Result<GovernanceProposal> {
        let (apply_effects, snapshot) = {
            let proposal = self
                .proposals
                .get_mut(proposal_id)
                .context("proposal not found")?;
            if proposal.status != ProposalStatus::Open {
                return Ok(proposal.clone());
            }

            if proposal.votes_for.len() >= min_votes_for
                && proposal.votes_for.len() > proposal.votes_against.len()
            {
                proposal.status = ProposalStatus::Accepted;
                let snapshot = (
                    proposal.subnet_id.clone(),
                    proposal.kind.clone(),
                    proposal.payload.clone(),
                );
                (Some(snapshot), proposal.clone())
            } else {
                proposal.status = ProposalStatus::Rejected;
                (None, proposal.clone())
            }
        };

        if let Some((subnet_id, kind, payload)) = apply_effects {
            self.apply_proposal_effects(&subnet_id, &kind, &payload)?;
        }

        Ok(snapshot)
    }

    fn apply_proposal_effects(
        &mut self,
        subnet_id: &str,
        kind: &str,
        payload: &serde_json::Value,
    ) -> Result<()> {
        if kind == "update_tax_rate" {
            let tax_rate = payload["tax_rate"]
                .as_f64()
                .context("proposal tax_rate must be f64")?;
            let planet = self
                .planets
                .get_mut(subnet_id)
                .context("proposal target subnet missing")?;
            planet.tax_rate = tax_rate;
        }
        Ok(())
    }

    pub fn record_validator_heartbeat(&mut self, subnet_id: &str, validator: &str, ts: i64) {
        self.validator_heartbeats
            .entry(subnet_id.to_string())
            .or_default()
            .insert(validator.to_string(), ts);
    }

    pub fn rotate_validators(
        &mut self,
        subnet_id: &str,
        stale_after_sec: i64,
        candidate_pool: &[String],
    ) -> Result<Vec<String>> {
        let now = Utc::now().timestamp();
        let heartbeat_map = self
            .validator_heartbeats
            .entry(subnet_id.to_string())
            .or_default();
        let planet = self
            .planets
            .get_mut(subnet_id)
            .context("subnet not found for rotation")?;

        planet.validators.retain(|validator| {
            heartbeat_map
                .get(validator)
                .is_some_and(|ts| now - *ts <= stale_after_sec)
        });

        for candidate in candidate_pool {
            if planet.validators.is_empty() || !planet.validators.contains(candidate) {
                planet.validators.insert(candidate.clone());
            }
            if planet.validators.len() >= 3 {
                break;
            }
        }

        Ok(planet.validators.iter().cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn persistence_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("governance.json");

        let mut gov = GovernanceEngine::default();
        let creator = Identity::new_random();
        let s1 = Identity::new_random();
        let s2 = Identity::new_random();

        gov.issue_license(&creator.agent_id, &creator.agent_id, "proof", 7);
        gov.lock_bond(&creator.agent_id, 100, 30);

        let ts = Utc::now().timestamp();
        let approvals = vec![
            GovernanceEngine::sign_genesis("planet-p", "Planet P", &creator.agent_id, ts, &s1)
                .unwrap(),
            GovernanceEngine::sign_genesis("planet-p", "Planet P", &creator.agent_id, ts, &s2)
                .unwrap(),
        ];
        let request = PlanetCreationRequest {
            subnet_id: "planet-p".to_string(),
            name: "Planet P".to_string(),
            creator: creator.agent_id.clone(),
            created_at: ts,
            tax_rate: 0.05,
            min_bond: 50,
            min_approvals: 2,
        };
        gov.create_planet(&request, &approvals).unwrap();
        gov.persist(&path).unwrap();

        let loaded = GovernanceEngine::load_or_new(&path).unwrap();
        assert!(loaded.planet("planet-p").is_some());
        assert!(loaded.has_valid_license(&creator.agent_id));
        assert!(loaded.has_active_bond(&creator.agent_id, 50));
    }

    #[test]
    fn planet_creation_requires_license_bond_multisig() {
        let mut gov = GovernanceEngine::default();
        let creator = Identity::new_random();
        let s1 = Identity::new_random();
        let s2 = Identity::new_random();

        gov.issue_license(&creator.agent_id, &creator.agent_id, "proof", 7);
        gov.lock_bond(&creator.agent_id, 100, 30);

        let ts = Utc::now().timestamp();
        let approvals = vec![
            GovernanceEngine::sign_genesis("planet-a", "Planet A", &creator.agent_id, ts, &s1)
                .unwrap(),
            GovernanceEngine::sign_genesis("planet-a", "Planet A", &creator.agent_id, ts, &s2)
                .unwrap(),
        ];

        let request = PlanetCreationRequest {
            subnet_id: "planet-a".to_string(),
            name: "Planet A".to_string(),
            creator: creator.agent_id.clone(),
            created_at: ts,
            tax_rate: 0.05,
            min_bond: 50,
            min_approvals: 2,
        };
        let planet = gov.create_planet(&request, &approvals).unwrap();

        assert_eq!(planet.subnet_id, "planet-a");
        assert!(gov.planet("planet-a").is_some());
    }

    #[test]
    fn rejects_insufficient_approvals() {
        let mut gov = GovernanceEngine::default();
        let creator = Identity::new_random();
        let signer = Identity::new_random();
        gov.issue_license(&creator.agent_id, &creator.agent_id, "proof", 7);
        gov.lock_bond(&creator.agent_id, 100, 30);
        let ts = Utc::now().timestamp();
        let approvals = vec![
            GovernanceEngine::sign_genesis("planet-b", "Planet B", &creator.agent_id, ts, &signer)
                .unwrap(),
        ];

        let request = PlanetCreationRequest {
            subnet_id: "planet-b".to_string(),
            name: "Planet B".to_string(),
            creator: creator.agent_id.clone(),
            created_at: ts,
            tax_rate: 0.05,
            min_bond: 50,
            min_approvals: 2,
        };
        let err = gov.create_planet(&request, &approvals).unwrap_err();

        assert!(
            err.to_string()
                .contains("not enough unique genesis approvals")
        );
    }

    #[test]
    fn proposal_vote_finalize_and_rotation_flow() {
        let mut gov = GovernanceEngine::default();
        let creator = Identity::new_random();
        let s1 = Identity::new_random();
        let s2 = Identity::new_random();

        gov.issue_license(&creator.agent_id, &creator.agent_id, "proof", 7);
        gov.lock_bond(&creator.agent_id, 100, 30);
        let ts = Utc::now().timestamp();

        let approvals = vec![
            GovernanceEngine::sign_genesis("planet-x", "Planet X", &creator.agent_id, ts, &s1)
                .unwrap(),
            GovernanceEngine::sign_genesis("planet-x", "Planet X", &creator.agent_id, ts, &s2)
                .unwrap(),
        ];
        let request = PlanetCreationRequest {
            subnet_id: "planet-x".to_string(),
            name: "Planet X".to_string(),
            creator: creator.agent_id.clone(),
            created_at: ts,
            tax_rate: 0.05,
            min_bond: 50,
            min_approvals: 2,
        };
        let planet = gov.create_planet(&request, &approvals).unwrap();

        let proposal = gov
            .create_proposal(
                &planet.subnet_id,
                "update_tax_rate",
                serde_json::json!({"tax_rate": 0.08}),
                &creator.agent_id,
            )
            .unwrap();

        gov.vote_proposal(&proposal.proposal_id, &s1.agent_id, true)
            .unwrap();
        gov.vote_proposal(&proposal.proposal_id, &s2.agent_id, true)
            .unwrap();
        let all_proposals = gov.list_proposals(None);
        assert_eq!(all_proposals.len(), 1);
        let filtered = gov.list_proposals(Some("planet-x"));
        assert_eq!(filtered.len(), 1);

        let finalized = gov.finalize_proposal(&proposal.proposal_id, 2).unwrap();
        assert_eq!(finalized.status, ProposalStatus::Accepted);
        assert!((gov.planet("planet-x").unwrap().tax_rate - 0.08).abs() < f64::EPSILON);

        gov.record_validator_heartbeat("planet-x", &s1.agent_id, Utc::now().timestamp());
        gov.record_validator_heartbeat("planet-x", &s2.agent_id, Utc::now().timestamp() - 10_000);
        let rotated = gov
            .rotate_validators(
                "planet-x",
                3600,
                &[creator.agent_id.clone(), s2.agent_id.clone()],
            )
            .unwrap();
        assert!(rotated.contains(&s1.agent_id));
    }
}
