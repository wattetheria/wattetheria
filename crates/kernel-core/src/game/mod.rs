pub mod anchor;
pub mod catalog;
pub mod mission_pack;
pub mod onboarding;
pub mod progression;
pub mod qualification;
pub mod starter;

pub use anchor::{MissionAnchor, locate_anchor};
pub use catalog::{FactionPlaybook, GameCatalog, GameStageDefinition, RolePlaybook, catalog};
pub use mission_pack::{
    GameMissionPack, GameMissionPackSummary, GameMissionPayloadSchema, GameMissionTemplate,
    MissionPackPhase, MissionPayloadField, bootstrap_mission_pack, mission_pack_set, stage_label,
};
pub use onboarding::{
    OnboardingActionCard, OnboardingActionKind, OnboardingFlow, OnboardingState, OnboardingStep,
    compute_onboarding_flow, compute_onboarding_state,
};
pub use progression::{
    GameComputation, GameObjective, GameStage, GameStatus, GovernanceGate, GovernanceJourney,
    HomeAnchor, ProgressionTier, compute_governance_journey, compute_status,
};
pub use qualification::{
    QualificationState, QualificationTrack, QualificationUnlock, compute_qualifications,
};
pub use starter::{
    StarterMissionSet, StarterMissionTemplate, StarterObjectiveChain, StarterObjectiveState,
    StarterObjectiveStep, bootstrap_starter_missions, starter_mission_set,
};
