//! JSON output formatter.

use serde::Serialize;
use zremote_client::types::ProjectSettings;
use zremote_client::{
    ActionsResponse, AgenticLoop, ClaudeTask, ConfigValue, DirectoryEntry, Host, KnowledgeBase,
    Memory, ModeInfo, Project, SearchResult, ServerEvent, Session,
};

use super::Formatter;

pub struct JsonFormatter;

fn to_json<T: Serialize>(v: &T) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"))
}

fn to_json_compact<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"))
}

impl Formatter for JsonFormatter {
    fn hosts(&self, hosts: &[Host]) -> String {
        to_json(&hosts)
    }
    fn host(&self, host: &Host) -> String {
        to_json(host)
    }
    fn sessions(&self, sessions: &[Session]) -> String {
        to_json(&sessions)
    }
    fn session(&self, session: &Session) -> String {
        to_json(session)
    }
    fn projects(&self, projects: &[Project]) -> String {
        to_json(&projects)
    }
    fn project(&self, project: &Project) -> String {
        to_json(project)
    }
    fn loops(&self, loops: &[AgenticLoop]) -> String {
        to_json(&loops)
    }
    fn agentic_loop(&self, l: &AgenticLoop) -> String {
        to_json(l)
    }
    fn tasks(&self, tasks: &[ClaudeTask]) -> String {
        to_json(&tasks)
    }
    fn task(&self, task: &ClaudeTask) -> String {
        to_json(task)
    }
    fn memories(&self, memories: &[Memory]) -> String {
        to_json(&memories)
    }
    fn memory(&self, memory: &Memory) -> String {
        to_json(memory)
    }
    fn config_value(&self, cv: &ConfigValue) -> String {
        to_json(cv)
    }
    fn settings(&self, settings: &Option<ProjectSettings>) -> String {
        to_json(settings)
    }
    fn actions(&self, resp: &ActionsResponse) -> String {
        to_json(resp)
    }
    fn worktrees(&self, worktrees: &[Project]) -> String {
        to_json(&worktrees)
    }
    fn knowledge_status(&self, kb: &KnowledgeBase) -> String {
        to_json(kb)
    }
    fn search_results(&self, results: &SearchResult) -> String {
        to_json(results)
    }
    fn directory_entries(&self, entries: &[DirectoryEntry]) -> String {
        to_json(&entries)
    }

    fn status_info(&self, mode: &ModeInfo, hosts: &[Host]) -> String {
        let status = serde_json::json!({
            "mode": mode.mode,
            "version": mode.version,
            "hosts": hosts.len(),
            "hosts_online": hosts.iter().filter(|h| h.status == zremote_client::HostStatus::Online).count(),
        });
        to_json(&status)
    }

    fn event(&self, event: &ServerEvent) -> String {
        to_json_compact(event)
    }
}
