pub mod consequence;
pub mod model;
pub mod registry;
pub mod state;
pub mod travel;
pub mod validator;

pub use consequence::{
    TravelConsequence, TravelGovernanceImpact, TravelMissionHighlight, TravelMissionImpact,
    evaluate_arrival_consequence,
};
pub use model::{
    GalaxyMap, GalaxyMapSummary, MapCoordinate, PlanetKind, PlanetNode, RouteEdge, StarSystem,
};
pub use registry::GalaxyMapRegistry;
pub use state::{
    TravelPosition, TravelSession, TravelSessionStatus, TravelStateRecord, TravelStateRegistry,
    resolve_system_position,
};
pub use travel::{TravelLeg, TravelOption, TravelPlan, TravelRiskLevel, TravelWarning};
