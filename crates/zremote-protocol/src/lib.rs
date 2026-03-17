pub mod agentic;
pub mod claude;
pub mod knowledge;
pub mod project;
mod terminal;

pub use agentic::*;
pub use claude::*;
pub use knowledge::*;
pub use project::*;
pub use terminal::*;

use uuid::Uuid;

pub type HostId = Uuid;
pub type SessionId = Uuid;
pub type AgenticLoopId = Uuid;
