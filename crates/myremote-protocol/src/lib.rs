pub mod agentic;
mod terminal;

pub use agentic::*;
pub use terminal::*;

use uuid::Uuid;

pub type HostId = Uuid;
pub type SessionId = Uuid;
pub type AgenticLoopId = Uuid;
