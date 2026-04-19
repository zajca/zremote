pub mod agentic;
pub mod agents;
pub mod channel;
pub mod claude;
pub mod events;
pub mod fs;
pub mod knowledge;
pub mod project;
pub mod status;
mod terminal;

pub use agentic::{AgenticAgentMessage, AgenticStatus};
pub use agents::{
    AgentLifecycleMessage, AgentProfileData, AgentServerMessage, KindInfo, SUPPORTED_KINDS,
    supported_kinds,
};
pub use claude::*;
pub use events::{HostInfo, LoopInfo, ServerEvent, SessionInfo};
pub use fs::{FsCompleteEntry, FsCompleteKind, FsCompleteResponse};
pub use knowledge::*;
pub use project::*;
pub use status::*;
pub use terminal::*;

use uuid::Uuid;

pub type HostId = Uuid;
pub type SessionId = Uuid;
pub type AgenticLoopId = Uuid;
