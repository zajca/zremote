pub mod agentic;
pub mod channel;
pub mod claude;
pub mod events;
pub mod knowledge;
pub mod project;
pub mod status;
mod terminal;

pub use agentic::{AgenticAgentMessage, AgenticStatus};
pub use claude::*;
pub use events::{HostInfo, LoopInfo, ServerEvent, SessionInfo};
pub use knowledge::*;
pub use project::*;
pub use status::*;
pub use terminal::*;

use uuid::Uuid;

pub type HostId = Uuid;
pub type SessionId = Uuid;
pub type AgenticLoopId = Uuid;
