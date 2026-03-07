//! Core kernel modules for the wattetheria Rust MVP.

pub mod brain;
pub mod civilization;
pub mod governance;
pub mod security;
pub mod storage;
pub mod tasks;
pub mod types;

pub use brain::mcp;
pub use brain::night_shift;
pub use brain::plugin_registry;
pub use brain::policy_engine;
pub use brain::skill_package;
pub use brain::skill_runtime;
pub use civilization::emergency;
pub use civilization::metrics;
pub use civilization::missions;
pub use civilization::profiles;
pub use civilization::world;
pub use governance::mailbox;
pub use governance::oracle;
pub use security::admission;
pub use security::capabilities;
pub use security::hashcash;
pub use security::identity;
pub use security::signing;
pub use security::trust;
pub use storage::audit;
pub use storage::data_ops;
pub use storage::event_log;
pub use storage::online_proof;
pub use storage::summary;
pub use tasks::galaxy_task;
pub use tasks::swarm_bridge;
pub use tasks::task_engine;
