use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SovereigntyMode {
    FrontierClaim,
    Auction,
    Election,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VotingChamber {
    Capital,
    Citizen,
    Reputation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaxMode {
    Flat,
    Progressive,
    Trade,
    Land,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SecurityPolicy {
    PeaceZone,
    LimitedPvp,
    OpenPvp,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AccessPolicy {
    Open,
    InviteOnly,
    Bonded,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanetConstitutionTemplate {
    CorporateCharter,
    MigrantCouncil,
    FrontierCompact,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanetConstitution {
    pub template: PlanetConstitutionTemplate,
    pub sovereignty_mode: SovereigntyMode,
    pub voting_chambers: Vec<VotingChamber>,
    pub tax_modes: Vec<TaxMode>,
    pub security_policy: SecurityPolicy,
    pub access_policy: AccessPolicy,
}

impl PlanetConstitution {
    #[must_use]
    pub fn from_template(template: PlanetConstitutionTemplate) -> Self {
        match template {
            PlanetConstitutionTemplate::CorporateCharter => Self {
                template,
                sovereignty_mode: SovereigntyMode::Auction,
                voting_chambers: vec![VotingChamber::Capital, VotingChamber::Reputation],
                tax_modes: vec![TaxMode::Trade, TaxMode::Flat],
                security_policy: SecurityPolicy::LimitedPvp,
                access_policy: AccessPolicy::Bonded,
            },
            PlanetConstitutionTemplate::MigrantCouncil => Self {
                template,
                sovereignty_mode: SovereigntyMode::Election,
                voting_chambers: vec![VotingChamber::Citizen, VotingChamber::Reputation],
                tax_modes: vec![TaxMode::Progressive, TaxMode::Trade],
                security_policy: SecurityPolicy::PeaceZone,
                access_policy: AccessPolicy::Open,
            },
            PlanetConstitutionTemplate::FrontierCompact => Self {
                template,
                sovereignty_mode: SovereigntyMode::FrontierClaim,
                voting_chambers: vec![VotingChamber::Capital],
                tax_modes: vec![TaxMode::Flat, TaxMode::Land],
                security_policy: SecurityPolicy::OpenPvp,
                access_policy: AccessPolicy::InviteOnly,
            },
        }
    }
}
