pub mod config;
pub mod heartbeats;
pub mod manager;
pub mod resources;
pub mod reviewer;
pub mod roles;
pub mod subagent;
pub mod supervisor;
pub mod worker;

pub use config::TeamConfig;
pub use manager::TeamManager;
pub use resources::{ResourceMonitor, ResourceSnapshot};
pub use reviewer::{ReviewAction, TaskReview};
pub use roles::{RoleProfile, RoleProfiles};
pub use subagent::{Subagent, SubagentState};
pub use supervisor::SupervisorControls;
pub use worker::TaskArtifact;
