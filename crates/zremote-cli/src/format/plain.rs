//! Plain text output formatter (for piped output / grep).

use std::fmt::Write;

use zremote_client::types::ProjectSettings;
use zremote_client::{
    ActionsResponse, AgenticLoop, ClaudeTask, ConfigValue, DirectoryEntry, Host, HostStatus,
    KnowledgeBase, Memory, ModeInfo, Project, SearchResult, ServerEvent, Session,
};

use super::{Formatter, opt, relative_time};

pub struct PlainFormatter;

impl Formatter for PlainFormatter {
    fn hosts(&self, hosts: &[Host]) -> String {
        hosts
            .iter()
            .map(|h| self.host(h))
            .collect::<Vec<_>>()
            .join("\n---\n")
    }

    fn host(&self, h: &Host) -> String {
        format!(
            "id: {}\nname: {}\nhostname: {}\nstatus: {:?}\nversion: {}\nlast_seen: {}",
            h.id,
            h.name,
            h.hostname,
            h.status,
            opt(&h.agent_version),
            h.last_seen_at
                .as_deref()
                .map_or("-".to_string(), relative_time),
        )
    }

    fn sessions(&self, sessions: &[Session]) -> String {
        sessions
            .iter()
            .map(|s| self.session(s))
            .collect::<Vec<_>>()
            .join("\n---\n")
    }

    fn session(&self, s: &Session) -> String {
        format!(
            "id: {}\nname: {}\nstatus: {:?}\nshell: {}\nworking_dir: {}\ncreated: {}",
            s.id,
            opt(&s.name),
            s.status,
            opt(&s.shell),
            opt(&s.working_dir),
            relative_time(&s.created_at),
        )
    }

    fn projects(&self, projects: &[Project]) -> String {
        projects
            .iter()
            .map(|p| self.project(p))
            .collect::<Vec<_>>()
            .join("\n---\n")
    }

    fn project(&self, p: &Project) -> String {
        format!(
            "id: {}\nname: {}\npath: {}\ntype: {}\nbranch: {}\ndirty: {}",
            p.id,
            p.name,
            p.path,
            p.project_type,
            opt(&p.git_branch),
            p.git_is_dirty,
        )
    }

    fn loops(&self, loops: &[AgenticLoop]) -> String {
        loops
            .iter()
            .map(|l| self.agentic_loop(l))
            .collect::<Vec<_>>()
            .join("\n---\n")
    }

    fn agentic_loop(&self, l: &AgenticLoop) -> String {
        format!(
            "id: {}\nsession: {}\nstatus: {:?}\ntool: {}\ntask: {}\nstarted: {}",
            l.id,
            l.session_id,
            l.status,
            l.tool_name,
            opt(&l.task_name),
            relative_time(&l.started_at),
        )
    }

    fn tasks(&self, tasks: &[ClaudeTask]) -> String {
        tasks
            .iter()
            .map(|t| self.task(t))
            .collect::<Vec<_>>()
            .join("\n---\n")
    }

    fn task(&self, t: &ClaudeTask) -> String {
        let mut result = format!(
            "id: {}\nstatus: {:?}\nmodel: {}\nproject: {}\ncost: {}\ncreated: {}",
            t.id,
            t.status,
            opt(&t.model),
            t.project_path,
            t.total_cost_usd
                .map_or("-".to_string(), |c| format!("${c:.4}")),
            relative_time(&t.created_at),
        );
        if let Some(ref err) = t.error_message {
            let _ = write!(result, "\nerror: {err}");
        }
        result
    }

    fn memories(&self, memories: &[Memory]) -> String {
        memories
            .iter()
            .map(|m| self.memory(m))
            .collect::<Vec<_>>()
            .join("\n---\n")
    }

    fn memory(&self, m: &Memory) -> String {
        format!(
            "id: {}\nkey: {}\ncategory: {:?}\ncontent: {}",
            m.id, m.key, m.category, m.content,
        )
    }

    fn config_value(&self, cv: &ConfigValue) -> String {
        format!("{}: {}", cv.key, cv.value)
    }

    fn settings(&self, settings: &Option<ProjectSettings>) -> String {
        match settings {
            Some(s) => serde_json::to_string_pretty(s).unwrap_or_else(|e| format!("error: {e}")),
            None => "No settings configured.".to_string(),
        }
    }

    fn actions(&self, resp: &ActionsResponse) -> String {
        resp.actions
            .iter()
            .map(|a| format!("{}: {}", a.name, a.command))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn worktrees(&self, worktrees: &[Project]) -> String {
        worktrees
            .iter()
            .map(|w| {
                format!(
                    "id: {}\npath: {}\nbranch: {}\ndirty: {}",
                    w.id,
                    w.path,
                    opt(&w.git_branch),
                    w.git_is_dirty
                )
            })
            .collect::<Vec<_>>()
            .join("\n---\n")
    }

    fn knowledge_status(&self, kb: &KnowledgeBase) -> String {
        format!(
            "status: {:?}\nversion: {}\nerror: {}",
            kb.status,
            opt(&kb.openviking_version),
            opt(&kb.last_error)
        )
    }

    fn search_results(&self, results: &SearchResult) -> String {
        serde_json::to_string_pretty(results).unwrap_or_else(|e| format!("error: {e}"))
    }

    fn status_info(&self, mode: &ModeInfo, hosts: &[Host]) -> String {
        let online = hosts
            .iter()
            .filter(|h| h.status == HostStatus::Online)
            .count();
        format!(
            "mode: {}\nversion: {}\nhosts: {}\nonline: {}",
            mode.mode,
            opt(&mode.version),
            hosts.len(),
            online
        )
    }

    fn event(&self, event: &ServerEvent) -> String {
        serde_json::to_string(event).unwrap_or_else(|_| format!("{event:?}"))
    }

    fn directory_entries(&self, entries: &[DirectoryEntry]) -> String {
        entries
            .iter()
            .map(|e| {
                let kind = if e.is_dir { "dir" } else { "file" };
                format!("{}\t{kind}", e.name)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
