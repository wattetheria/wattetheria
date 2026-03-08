use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use crate::civilization::profiles::Faction;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OrganizationKind {
    Guild,
    Consortium,
    Fleet,
    CivicUnion,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OrganizationRole {
    Founder,
    Officer,
    Member,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OrganizationPermission {
    ManageMembers,
    ManageTreasury,
    PublishMissions,
    ManageGovernance,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OrganizationProposalKind {
    SubnetCharter,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OrganizationProposalStatus {
    Open,
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrganizationProposal {
    pub proposal_id: String,
    pub organization_id: String,
    pub kind: OrganizationProposalKind,
    pub title: String,
    pub summary: String,
    pub proposed_subnet_id: Option<String>,
    pub proposed_subnet_name: Option<String>,
    pub created_by_public_id: String,
    pub created_at: i64,
    pub votes_for: BTreeSet<String>,
    pub votes_against: BTreeSet<String>,
    pub status: OrganizationProposalStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OrganizationSubnetCharterStatus {
    PendingGovernance,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrganizationSubnetCharterApplication {
    pub application_id: String,
    pub organization_id: String,
    pub proposal_id: String,
    pub requested_by_public_id: String,
    pub sponsor_controller_id: String,
    pub proposed_subnet_id: String,
    pub proposed_subnet_name: String,
    pub summary: String,
    pub created_at: i64,
    pub status: OrganizationSubnetCharterStatus,
    pub readiness_snapshot: OrganizationAutonomyTrack,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrganizationReadinessGate {
    pub key: String,
    pub title: String,
    pub complete: bool,
    pub current: i64,
    pub target: i64,
    pub unit: String,
    pub hint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrganizationAutonomyTrack {
    pub current_status: String,
    pub next_action: String,
    pub eligible_for_subnet_charter: bool,
    pub gates: Vec<OrganizationReadinessGate>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrganizationProfile {
    pub organization_id: String,
    pub name: String,
    pub kind: OrganizationKind,
    pub summary: String,
    pub faction_alignment: Option<Faction>,
    pub home_subnet_id: Option<String>,
    pub home_zone_id: Option<String>,
    pub treasury_watt: i64,
    pub created_by_public_id: String,
    pub active: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrganizationCreateSpec {
    pub organization_id: String,
    pub name: String,
    pub kind: OrganizationKind,
    pub summary: String,
    pub faction_alignment: Option<Faction>,
    pub home_subnet_id: Option<String>,
    pub home_zone_id: Option<String>,
    pub founder_public_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrganizationProposalCreateSpec {
    pub organization_id: String,
    pub kind: OrganizationProposalKind,
    pub title: String,
    pub summary: String,
    pub proposed_subnet_id: Option<String>,
    pub proposed_subnet_name: Option<String>,
    pub created_by_public_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrganizationMembership {
    pub organization_id: String,
    pub public_id: String,
    pub role: OrganizationRole,
    pub title: Option<String>,
    pub active: bool,
    pub joined_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct OrganizationRegistry {
    organizations: BTreeMap<String, OrganizationProfile>,
    memberships: BTreeMap<String, Vec<OrganizationMembership>>,
    proposals: BTreeMap<String, OrganizationProposal>,
    charter_applications: BTreeMap<String, OrganizationSubnetCharterApplication>,
}

impl OrganizationRegistry {
    pub fn load_or_new(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create organization registry directory")?;
        }
        if !path.as_ref().exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path.as_ref()).context("read organization registry")?;
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&raw).context("parse organization registry")
    }

    pub fn persist(&self, path: impl AsRef<Path>) -> Result<()> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create organization registry directory")?;
        }
        fs::write(path.as_ref(), serde_json::to_string_pretty(self)?)
            .context("write organization registry")
    }

    pub fn create_organization(
        &mut self,
        spec: OrganizationCreateSpec,
    ) -> Result<OrganizationProfile> {
        if self.organizations.contains_key(&spec.organization_id) {
            bail!("organization already exists");
        }
        let now = Utc::now().timestamp();
        let organization = OrganizationProfile {
            organization_id: spec.organization_id.clone(),
            name: spec.name,
            kind: spec.kind,
            summary: spec.summary,
            faction_alignment: spec.faction_alignment,
            home_subnet_id: spec.home_subnet_id,
            home_zone_id: spec.home_zone_id,
            treasury_watt: 0,
            created_by_public_id: spec.founder_public_id.clone(),
            active: true,
            created_at: now,
            updated_at: now,
        };
        self.organizations
            .insert(spec.organization_id.clone(), organization.clone());
        self.upsert_membership(
            &spec.organization_id,
            &spec.founder_public_id,
            OrganizationRole::Founder,
            Some("Founder".to_string()),
            true,
        )?;
        Ok(organization)
    }

    pub fn upsert_membership(
        &mut self,
        organization_id: &str,
        public_id: &str,
        role: OrganizationRole,
        title: Option<String>,
        active: bool,
    ) -> Result<OrganizationMembership> {
        if !self.organizations.contains_key(organization_id) {
            bail!("organization does not exist");
        }
        let now = Utc::now().timestamp();
        let entries = self
            .memberships
            .entry(organization_id.to_string())
            .or_default();
        let joined_at = entries
            .iter()
            .find(|membership| membership.public_id == public_id)
            .map_or(now, |membership| membership.joined_at);
        let membership = OrganizationMembership {
            organization_id: organization_id.to_string(),
            public_id: public_id.to_string(),
            role,
            title,
            active,
            joined_at,
            updated_at: now,
        };
        if let Some(existing) = entries
            .iter_mut()
            .find(|entry| entry.public_id == public_id)
        {
            *existing = membership.clone();
        } else {
            entries.push(membership.clone());
        }
        if let Some(organization) = self.organizations.get_mut(organization_id) {
            organization.updated_at = now;
        }
        Ok(membership)
    }

    pub fn fund_treasury(
        &mut self,
        organization_id: &str,
        amount_watt: i64,
    ) -> Result<OrganizationProfile> {
        if amount_watt <= 0 {
            bail!("funding amount must be positive");
        }
        let organization = self
            .organizations
            .get_mut(organization_id)
            .ok_or_else(|| anyhow::anyhow!("organization does not exist"))?;
        organization.treasury_watt += amount_watt;
        organization.updated_at = Utc::now().timestamp();
        Ok(organization.clone())
    }

    pub fn spend_treasury(
        &mut self,
        organization_id: &str,
        amount_watt: i64,
    ) -> Result<OrganizationProfile> {
        if amount_watt <= 0 {
            bail!("spend amount must be positive");
        }
        let organization = self
            .organizations
            .get_mut(organization_id)
            .ok_or_else(|| anyhow::anyhow!("organization does not exist"))?;
        if organization.treasury_watt < amount_watt {
            bail!("insufficient organization treasury");
        }
        organization.treasury_watt -= amount_watt;
        organization.updated_at = Utc::now().timestamp();
        Ok(organization.clone())
    }

    #[must_use]
    pub fn organization(&self, organization_id: &str) -> Option<OrganizationProfile> {
        self.organizations.get(organization_id).cloned()
    }

    #[must_use]
    pub fn memberships(&self, organization_id: &str) -> Vec<OrganizationMembership> {
        self.memberships
            .get(organization_id)
            .cloned()
            .unwrap_or_default()
    }

    #[must_use]
    pub fn memberships_for_public(&self, public_id: &str) -> Vec<OrganizationMembership> {
        self.memberships
            .values()
            .flat_map(|entries| entries.iter())
            .filter(|membership| membership.active && membership.public_id == public_id)
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn organizations_for_public(
        &self,
        public_id: &str,
    ) -> Vec<(OrganizationProfile, OrganizationMembership)> {
        self.memberships_for_public(public_id)
            .into_iter()
            .filter_map(|membership| {
                self.organization(&membership.organization_id)
                    .map(|organization| (organization, membership))
            })
            .collect()
    }

    #[must_use]
    pub fn list_organizations(&self) -> Vec<OrganizationProfile> {
        self.organizations.values().cloned().collect()
    }

    #[must_use]
    pub fn membership_for_public(
        &self,
        organization_id: &str,
        public_id: &str,
    ) -> Option<OrganizationMembership> {
        self.memberships(organization_id)
            .into_iter()
            .find(|membership| membership.active && membership.public_id == public_id)
    }

    #[must_use]
    pub fn permissions_for_public(
        &self,
        organization_id: &str,
        public_id: &str,
    ) -> Vec<OrganizationPermission> {
        let Some(membership) = self.membership_for_public(organization_id, public_id) else {
            return Vec::new();
        };
        permissions_for_role(&membership.role)
    }

    #[must_use]
    pub fn has_permission(
        &self,
        organization_id: &str,
        public_id: &str,
        permission: &OrganizationPermission,
    ) -> bool {
        self.permissions_for_public(organization_id, public_id)
            .contains(permission)
    }

    pub fn create_proposal(
        &mut self,
        spec: OrganizationProposalCreateSpec,
    ) -> Result<OrganizationProposal> {
        if self.organization(&spec.organization_id).is_none() {
            bail!("organization does not exist");
        }
        if self
            .membership_for_public(&spec.organization_id, &spec.created_by_public_id)
            .is_none()
        {
            bail!("organization membership required");
        }
        let now = Utc::now().timestamp();
        let proposal = OrganizationProposal {
            proposal_id: format!("{}-proposal-{now}", spec.organization_id),
            organization_id: spec.organization_id.clone(),
            kind: spec.kind,
            title: spec.title,
            summary: spec.summary,
            proposed_subnet_id: spec.proposed_subnet_id,
            proposed_subnet_name: spec.proposed_subnet_name,
            created_by_public_id: spec.created_by_public_id,
            created_at: now,
            votes_for: BTreeSet::new(),
            votes_against: BTreeSet::new(),
            status: OrganizationProposalStatus::Open,
        };
        self.proposals
            .insert(proposal.proposal_id.clone(), proposal.clone());
        if let Some(organization) = self.organizations.get_mut(&spec.organization_id) {
            organization.updated_at = now;
        }
        Ok(proposal)
    }

    pub fn vote_proposal(
        &mut self,
        proposal_id: &str,
        voter_public_id: &str,
        approve: bool,
    ) -> Result<OrganizationProposal> {
        let organization_id = self
            .proposals
            .get(proposal_id)
            .map(|proposal| proposal.organization_id.clone())
            .ok_or_else(|| anyhow::anyhow!("organization proposal not found"))?;
        if self
            .membership_for_public(&organization_id, voter_public_id)
            .is_none()
        {
            bail!("organization membership required");
        }
        let proposal = self
            .proposals
            .get_mut(proposal_id)
            .ok_or_else(|| anyhow::anyhow!("organization proposal not found"))?;
        if proposal.status != OrganizationProposalStatus::Open {
            bail!("organization proposal is not open");
        }
        proposal.votes_for.remove(voter_public_id);
        proposal.votes_against.remove(voter_public_id);
        if approve {
            proposal.votes_for.insert(voter_public_id.to_string());
        } else {
            proposal.votes_against.insert(voter_public_id.to_string());
        }
        Ok(proposal.clone())
    }

    pub fn finalize_proposal(
        &mut self,
        proposal_id: &str,
        min_votes_for: usize,
    ) -> Result<OrganizationProposal> {
        let proposal = self
            .proposals
            .get_mut(proposal_id)
            .ok_or_else(|| anyhow::anyhow!("organization proposal not found"))?;
        if proposal.status != OrganizationProposalStatus::Open {
            bail!("organization proposal is not open");
        }
        proposal.status = if proposal.votes_for.len() >= min_votes_for
            && proposal.votes_for.len() > proposal.votes_against.len()
        {
            OrganizationProposalStatus::Accepted
        } else {
            OrganizationProposalStatus::Rejected
        };
        Ok(proposal.clone())
    }

    #[must_use]
    pub fn list_proposals(&self, organization_id: Option<&str>) -> Vec<OrganizationProposal> {
        self.proposals
            .values()
            .filter(|proposal| {
                organization_id
                    .is_none_or(|organization_id| proposal.organization_id == organization_id)
            })
            .cloned()
            .collect()
    }

    pub fn create_subnet_charter_application(
        &mut self,
        proposal_id: &str,
        requested_by_public_id: &str,
        sponsor_controller_id: &str,
        readiness_snapshot: OrganizationAutonomyTrack,
    ) -> Result<OrganizationSubnetCharterApplication> {
        if !readiness_snapshot.eligible_for_subnet_charter {
            bail!("organization is not ready for a subnet charter");
        }
        let proposal = self
            .proposals
            .get(proposal_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("organization proposal not found"))?;
        if proposal.kind != OrganizationProposalKind::SubnetCharter {
            bail!("organization proposal is not a subnet charter proposal");
        }
        if proposal.status != OrganizationProposalStatus::Accepted {
            bail!("organization proposal must be accepted before charter submission");
        }
        if self
            .membership_for_public(&proposal.organization_id, requested_by_public_id)
            .is_none()
        {
            bail!("organization membership required");
        }
        if self
            .charter_applications
            .values()
            .any(|application| application.proposal_id == proposal_id)
        {
            bail!("charter application already exists for proposal");
        }
        let Some(proposed_subnet_id) = proposal.proposed_subnet_id.clone() else {
            bail!("subnet charter proposal missing proposed_subnet_id");
        };
        let Some(proposed_subnet_name) = proposal.proposed_subnet_name.clone() else {
            bail!("subnet charter proposal missing proposed_subnet_name");
        };
        let now = Utc::now().timestamp();
        let application = OrganizationSubnetCharterApplication {
            application_id: format!("{}-charter-{}", proposal.organization_id, now),
            organization_id: proposal.organization_id,
            proposal_id: proposal.proposal_id,
            requested_by_public_id: requested_by_public_id.to_string(),
            sponsor_controller_id: sponsor_controller_id.to_string(),
            proposed_subnet_id,
            proposed_subnet_name,
            summary: proposal.summary,
            created_at: now,
            status: OrganizationSubnetCharterStatus::PendingGovernance,
            readiness_snapshot,
        };
        self.charter_applications
            .insert(application.application_id.clone(), application.clone());
        Ok(application)
    }

    #[must_use]
    pub fn list_subnet_charter_applications(
        &self,
        organization_id: Option<&str>,
    ) -> Vec<OrganizationSubnetCharterApplication> {
        self.charter_applications
            .values()
            .filter(|application| {
                organization_id
                    .is_none_or(|organization_id| application.organization_id == organization_id)
            })
            .cloned()
            .collect()
    }
}

#[must_use]
pub fn permissions_for_role(role: &OrganizationRole) -> Vec<OrganizationPermission> {
    match role {
        OrganizationRole::Founder => vec![
            OrganizationPermission::ManageMembers,
            OrganizationPermission::ManageTreasury,
            OrganizationPermission::PublishMissions,
            OrganizationPermission::ManageGovernance,
        ],
        OrganizationRole::Officer => vec![
            OrganizationPermission::ManageTreasury,
            OrganizationPermission::PublishMissions,
            OrganizationPermission::ManageGovernance,
        ],
        OrganizationRole::Member => Vec::new(),
    }
}

#[must_use]
pub fn compute_autonomy_track(
    organization: &OrganizationProfile,
    active_member_count: usize,
    open_mission_count: usize,
    settled_mission_count: usize,
    home_subnet_governed: bool,
) -> OrganizationAutonomyTrack {
    let member_gate = readiness_gate(
        "member_core",
        "Build a three-member operating core",
        i64::try_from(active_member_count).unwrap_or(i64::MAX),
        3,
        "members",
        "Recruit enough active members to distribute civic, trade, and security work.",
    );
    let treasury_gate = readiness_gate(
        "treasury_buffer",
        "Maintain a forty-watt treasury buffer",
        organization.treasury_watt,
        40,
        "watt",
        "A stable treasury is required before an organization can underwrite subnet autonomy.",
    );
    let mission_gate = readiness_gate(
        "settled_ops",
        "Settle at least two organization missions",
        i64::try_from(settled_mission_count).unwrap_or(i64::MAX),
        2,
        "missions",
        "Use published missions to prove the organization can coordinate repeatable work.",
    );
    let anchor_gate = readiness_gate(
        "home_anchor",
        "Bind the organization to a home subnet",
        i64::from(organization.home_subnet_id.is_some()),
        1,
        "anchor",
        "A subnet or zone anchor gives the organization a concrete civic base.",
    );
    let governance_gate = readiness_gate(
        "governed_home",
        "Operate around a governed home subnet",
        i64::from(home_subnet_governed),
        1,
        "governed",
        "Subnet creation should emerge from proven home coordination, not from a cold start.",
    );

    let gates = vec![
        member_gate,
        treasury_gate,
        mission_gate,
        anchor_gate,
        governance_gate,
    ];
    let eligible_for_subnet_charter = gates.iter().all(|gate| gate.complete);
    let current_status = if eligible_for_subnet_charter {
        "subnet-ready".to_string()
    } else if active_member_count >= 2 && open_mission_count > 0 && organization.treasury_watt >= 10
    {
        "mission-operational".to_string()
    } else if active_member_count >= 3 {
        "forming-civic-core".to_string()
    } else if active_member_count >= 2 {
        "operational-cell".to_string()
    } else {
        "founder-led".to_string()
    };
    let next_action = if !gates[0].complete {
        "Recruit one more active member into the organization core.".to_string()
    } else if !gates[1].complete {
        "Increase the organization treasury until it can underwrite subnet operations.".to_string()
    } else if !gates[2].complete {
        "Publish and settle organization missions to prove repeatable coordination.".to_string()
    } else if !gates[3].complete {
        "Assign the organization to a concrete home subnet or civic anchor.".to_string()
    } else if !gates[4].complete {
        "Operate against a governed home subnet before petitioning for expansion autonomy."
            .to_string()
    } else {
        "Prepare a subnet charter and governance coalition from the organization's home anchor."
            .to_string()
    };

    OrganizationAutonomyTrack {
        current_status,
        next_action,
        eligible_for_subnet_charter,
        gates,
    }
}

fn readiness_gate(
    key: &str,
    title: &str,
    current: i64,
    target: i64,
    unit: &str,
    hint: &str,
) -> OrganizationReadinessGate {
    OrganizationReadinessGate {
        key: key.to_string(),
        title: title.to_string(),
        complete: current >= target,
        current,
        target,
        unit: unit.to_string(),
        hint: hint.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn organization_registry_roundtrip_and_membership() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("organizations.json");
        let mut registry = OrganizationRegistry::default();

        let organization = registry
            .create_organization(OrganizationCreateSpec {
                organization_id: "guild-aurora".to_string(),
                name: "Aurora Guild".to_string(),
                kind: OrganizationKind::Guild,
                summary: "Frontier logistics collective".to_string(),
                faction_alignment: Some(Faction::Freeport),
                home_subnet_id: Some("planet-test".to_string()),
                home_zone_id: Some("frontier-belt".to_string()),
                founder_public_id: "captain-aurora".to_string(),
            })
            .unwrap();
        assert_eq!(organization.organization_id, "guild-aurora");
        registry
            .upsert_membership(
                "guild-aurora",
                "broker-echo",
                OrganizationRole::Officer,
                Some("Quartermaster".to_string()),
                true,
            )
            .unwrap();
        let funded = registry.fund_treasury("guild-aurora", 25).unwrap();
        assert_eq!(funded.treasury_watt, 25);
        let spent = registry.spend_treasury("guild-aurora", 10).unwrap();
        assert_eq!(spent.treasury_watt, 15);
        registry.persist(&path).unwrap();

        let mut loaded = OrganizationRegistry::load_or_new(&path).unwrap();
        assert_eq!(loaded.list_organizations().len(), 1);
        assert_eq!(loaded.memberships("guild-aurora").len(), 2);
        assert_eq!(loaded.memberships_for_public("broker-echo").len(), 1);
        assert_eq!(loaded.organizations_for_public("captain-aurora").len(), 1);
        assert_eq!(
            loaded.organization("guild-aurora").unwrap().treasury_watt,
            15
        );
        assert_eq!(
            loaded.permissions_for_public("guild-aurora", "captain-aurora"),
            vec![
                OrganizationPermission::ManageMembers,
                OrganizationPermission::ManageTreasury,
                OrganizationPermission::PublishMissions,
                OrganizationPermission::ManageGovernance,
            ]
        );
        assert_eq!(
            loaded.permissions_for_public("guild-aurora", "broker-echo"),
            vec![
                OrganizationPermission::ManageTreasury,
                OrganizationPermission::PublishMissions,
                OrganizationPermission::ManageGovernance,
            ]
        );
        let track = compute_autonomy_track(
            &loaded.organization("guild-aurora").unwrap(),
            2,
            1,
            0,
            false,
        );
        assert_eq!(track.current_status, "mission-operational");
        assert!(!track.eligible_for_subnet_charter);
        assert_eq!(track.gates.len(), 5);

        let proposal = loaded
            .create_proposal(OrganizationProposalCreateSpec {
                organization_id: "guild-aurora".to_string(),
                kind: OrganizationProposalKind::SubnetCharter,
                title: "Charter the Aurora Reach".to_string(),
                summary: "Petition for a dedicated frontier subnet.".to_string(),
                proposed_subnet_id: Some("planet-aurora".to_string()),
                proposed_subnet_name: Some("Aurora Reach".to_string()),
                created_by_public_id: "captain-aurora".to_string(),
            })
            .unwrap();
        assert_eq!(proposal.status, OrganizationProposalStatus::Open);
    }
}
