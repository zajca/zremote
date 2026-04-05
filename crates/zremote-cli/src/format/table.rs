//! Table output formatter using comfy-table.

use std::fmt::Write;

use comfy_table::{ContentArrangement, Table};
use zremote_client::types::ProjectSettings;
use zremote_client::{
    ActionsResponse, AgenticLoop, ClaudeTask, ConfigValue, DirectoryEntry, Host, HostStatus,
    KnowledgeBase, Memory, ModeInfo, Project, SearchResult, ServerEvent, Session,
};

use super::{Formatter, opt, relative_time, short_id, truncate};

pub struct TableFormatter;

fn new_table(headers: &[&str]) -> Table {
    let mut table = Table::new();
    table.set_content_arrangement(ContentArrangement::Dynamic);
    table.load_preset(comfy_table::presets::NOTHING);
    table.set_header(headers);
    table
}

impl Formatter for TableFormatter {
    fn hosts(&self, hosts: &[Host]) -> String {
        if hosts.is_empty() {
            return "No hosts found.".to_string();
        }
        let mut t = new_table(&["ID", "NAME", "HOSTNAME", "STATUS", "VERSION", "LAST SEEN"]);
        for h in hosts {
            t.add_row([
                short_id(&h.id),
                &h.name,
                &h.hostname,
                &format!("{:?}", h.status).to_lowercase(),
                opt(&h.agent_version),
                &h.last_seen_at
                    .as_deref()
                    .map_or("-".to_string(), relative_time),
            ]);
        }
        t.to_string()
    }

    fn host(&self, h: &Host) -> String {
        format!(
            "ID:        {}\nName:      {}\nHostname:  {}\nStatus:    {:?}\nVersion:   {}\nOS:        {}\nArch:      {}\nLast Seen: {}\nCreated:   {}",
            h.id,
            h.name,
            h.hostname,
            h.status,
            opt(&h.agent_version),
            opt(&h.os),
            opt(&h.arch),
            h.last_seen_at
                .as_deref()
                .map_or("-".to_string(), relative_time),
            relative_time(&h.created_at),
        )
    }

    fn sessions(&self, sessions: &[Session]) -> String {
        if sessions.is_empty() {
            return "No sessions found.".to_string();
        }
        let mut t = new_table(&["ID", "NAME", "STATUS", "SHELL", "WORKING DIR", "CREATED"]);
        for s in sessions {
            t.add_row([
                short_id(&s.id),
                opt(&s.name),
                &format!("{:?}", s.status).to_lowercase(),
                opt(&s.shell),
                &s.working_dir
                    .as_deref()
                    .map_or("-".to_string(), |d| truncate(d, 40)),
                &relative_time(&s.created_at),
            ]);
        }
        t.to_string()
    }

    fn session(&self, s: &Session) -> String {
        format!(
            "ID:          {}\nName:        {}\nHost:        {}\nStatus:      {:?}\nShell:       {}\nWorking Dir: {}\nPID:         {}\nExit Code:   {}\nCreated:     {}\nClosed:      {}",
            s.id,
            opt(&s.name),
            short_id(&s.host_id),
            s.status,
            opt(&s.shell),
            opt(&s.working_dir),
            s.pid.map_or("-".to_string(), |p| p.to_string()),
            s.exit_code.map_or("-".to_string(), |c| c.to_string()),
            relative_time(&s.created_at),
            s.closed_at
                .as_deref()
                .map_or("-".to_string(), relative_time),
        )
    }

    fn projects(&self, projects: &[Project]) -> String {
        if projects.is_empty() {
            return "No projects found.".to_string();
        }
        let mut t = new_table(&["ID", "NAME", "PATH", "BRANCH", "DIRTY", "TYPE"]);
        for p in projects {
            t.add_row([
                short_id(&p.id),
                &truncate(&p.name, 25),
                &truncate(&p.path, 40),
                opt(&p.git_branch),
                if p.git_is_dirty { "yes" } else { "-" },
                &p.project_type,
            ]);
        }
        t.to_string()
    }

    fn project(&self, p: &Project) -> String {
        format!(
            "ID:       {}\nName:     {}\nPath:     {}\nType:     {}\nBranch:   {}\nCommit:   {}\nDirty:    {}\nAhead:    {}\nBehind:   {}\nPinned:   {}",
            p.id,
            p.name,
            p.path,
            p.project_type,
            opt(&p.git_branch),
            opt(&p.git_commit_hash),
            p.git_is_dirty,
            p.git_ahead,
            p.git_behind,
            p.pinned,
        )
    }

    fn loops(&self, loops: &[AgenticLoop]) -> String {
        if loops.is_empty() {
            return "No agentic loops found.".to_string();
        }
        let mut t = new_table(&["ID", "SESSION", "STATUS", "TOOL", "TASK", "STARTED"]);
        for l in loops {
            t.add_row([
                short_id(&l.id),
                short_id(&l.session_id),
                &format!("{:?}", l.status).to_lowercase(),
                &l.tool_name,
                opt(&l.task_name),
                &relative_time(&l.started_at),
            ]);
        }
        t.to_string()
    }

    fn agentic_loop(&self, l: &AgenticLoop) -> String {
        format!(
            "ID:         {}\nSession:    {}\nProject:    {}\nTool:       {}\nStatus:     {:?}\nTask:       {}\nStarted:    {}\nEnded:      {}\nEnd Reason: {}",
            l.id,
            l.session_id,
            opt(&l.project_path),
            l.tool_name,
            l.status,
            opt(&l.task_name),
            relative_time(&l.started_at),
            l.ended_at.as_deref().map_or("-".to_string(), relative_time),
            opt(&l.end_reason),
        )
    }

    fn tasks(&self, tasks: &[ClaudeTask]) -> String {
        if tasks.is_empty() {
            return "No Claude tasks found.".to_string();
        }
        let mut t = new_table(&["ID", "STATUS", "MODEL", "PROJECT", "COST", "STARTED"]);
        for task in tasks {
            t.add_row([
                short_id(&task.id),
                &format!("{:?}", task.status).to_lowercase(),
                opt(&task.model),
                &truncate(&task.project_path, 35),
                &task
                    .total_cost_usd
                    .map_or("-".to_string(), |c| format!("${c:.4}")),
                &relative_time(&task.created_at),
            ]);
        }
        t.to_string()
    }

    fn task(&self, t: &ClaudeTask) -> String {
        let mut result = format!(
            "ID:          {}\nSession:     {}\nHost:        {}\nProject:     {}\nModel:       {}\nStatus:      {:?}\nPrompt:      {}\nCost:        {}\nTokens In:   {}\nTokens Out:  {}\nStarted:     {}\nEnded:       {}\nSummary:     {}",
            t.id,
            short_id(&t.session_id),
            short_id(&t.host_id),
            t.project_path,
            opt(&t.model),
            t.status,
            t.initial_prompt
                .as_deref()
                .map_or("-".to_string(), |p| truncate(p, 60)),
            t.total_cost_usd
                .map_or("-".to_string(), |c| format!("${c:.4}")),
            t.total_tokens_in.map_or("-".to_string(), |n| n.to_string()),
            t.total_tokens_out
                .map_or("-".to_string(), |n| n.to_string()),
            relative_time(&t.created_at),
            t.ended_at.as_deref().map_or("-".to_string(), relative_time),
            t.summary
                .as_deref()
                .map_or("-".to_string(), |s| truncate(s, 80)),
        );
        if let Some(ref err) = t.error_message {
            let _ = write!(result, "\nError:       {err}");
        }
        result
    }

    fn memories(&self, memories: &[Memory]) -> String {
        if memories.is_empty() {
            return "No memories found.".to_string();
        }
        let mut t = new_table(&["ID", "KEY", "CATEGORY", "CONFIDENCE", "CREATED"]);
        for m in memories {
            t.add_row([
                short_id(&m.id),
                &truncate(&m.key, 30),
                &format!("{:?}", m.category).to_lowercase(),
                &format!("{:.2}", m.confidence),
                &relative_time(&m.created_at),
            ]);
        }
        t.to_string()
    }

    fn memory(&self, m: &Memory) -> String {
        format!(
            "ID:         {}\nKey:        {}\nCategory:   {:?}\nConfidence: {:.2}\nContent:    {}\nCreated:    {}\nUpdated:    {}",
            m.id,
            m.key,
            m.category,
            m.confidence,
            m.content,
            relative_time(&m.created_at),
            relative_time(&m.updated_at),
        )
    }

    fn config_value(&self, cv: &ConfigValue) -> String {
        format!("{} = {}", cv.key, cv.value)
    }

    fn settings(&self, settings: &Option<ProjectSettings>) -> String {
        match settings {
            Some(s) => serde_json::to_string_pretty(s).unwrap_or_else(|e| format!("Error: {e}")),
            None => "No settings configured.".to_string(),
        }
    }

    fn actions(&self, resp: &ActionsResponse) -> String {
        if resp.actions.is_empty() {
            return "No actions configured.".to_string();
        }
        let mut t = new_table(&["NAME", "COMMAND", "DESCRIPTION"]);
        for a in &resp.actions {
            t.add_row([
                &a.name,
                &truncate(&a.command, 40),
                &a.description.as_deref().unwrap_or("-").to_string(),
            ]);
        }
        t.to_string()
    }

    fn worktrees(&self, worktrees: &[Project]) -> String {
        if worktrees.is_empty() {
            return "No worktrees found.".to_string();
        }
        let mut t = new_table(&["ID", "PATH", "BRANCH", "DIRTY"]);
        for w in worktrees {
            let dirty = if w.git_is_dirty { "yes" } else { "-" };
            t.add_row([
                short_id(&w.id),
                &truncate(&w.path, 45),
                opt(&w.git_branch),
                dirty,
            ]);
        }
        t.to_string()
    }

    fn knowledge_status(&self, kb: &KnowledgeBase) -> String {
        format!(
            "ID:      {}\nHost:    {}\nStatus:  {:?}\nVersion: {}\nError:   {}\nUpdated: {}",
            short_id(&kb.id),
            short_id(&kb.host_id),
            kb.status,
            opt(&kb.openviking_version),
            opt(&kb.last_error),
            relative_time(&kb.updated_at),
        )
    }

    fn search_results(&self, results: &SearchResult) -> String {
        serde_json::to_string_pretty(results).unwrap_or_else(|e| format!("Error: {e}"))
    }

    fn status_info(&self, mode: &ModeInfo, hosts: &[Host]) -> String {
        let online = hosts
            .iter()
            .filter(|h| h.status == HostStatus::Online)
            .count();
        format!(
            "Mode:         {}\nVersion:      {}\nHosts:        {} ({} online)",
            mode.mode,
            opt(&mode.version),
            hosts.len(),
            online,
        )
    }

    #[allow(clippy::too_many_lines)]
    fn event(&self, event: &ServerEvent) -> String {
        match event {
            ServerEvent::HostConnected { host } => {
                format!(
                    "[host_connected] {} ({})",
                    host.hostname,
                    short_id(&host.id)
                )
            }
            ServerEvent::HostDisconnected { host_id } => {
                format!("[host_disconnected] {}", short_id(host_id))
            }
            ServerEvent::HostStatusChanged { host_id, status } => {
                format!("[host_status] {} -> {:?}", short_id(host_id), status)
            }
            ServerEvent::SessionCreated { session } => {
                format!(
                    "[session_created] {} on {}",
                    short_id(&session.id),
                    short_id(&session.host_id)
                )
            }
            ServerEvent::SessionClosed {
                session_id,
                exit_code,
            } => {
                format!(
                    "[session_closed] {} (exit: {})",
                    short_id(session_id),
                    exit_code.map_or("-".to_string(), |c| c.to_string())
                )
            }
            ServerEvent::SessionSuspended { session_id } => {
                format!("[session_suspended] {}", short_id(session_id))
            }
            ServerEvent::SessionResumed { session_id } => {
                format!("[session_resumed] {}", short_id(session_id))
            }
            ServerEvent::SessionUpdated { session_id } => {
                format!("[session_updated] {}", short_id(session_id))
            }
            ServerEvent::LoopDetected {
                loop_info,
                hostname,
                ..
            } => {
                format!(
                    "[loop_detected] {} on {} ({:?})",
                    short_id(&loop_info.id),
                    hostname,
                    loop_info.status
                )
            }
            ServerEvent::LoopStatusChanged {
                loop_info,
                hostname,
                ..
            } => {
                format!(
                    "[loop_status] {} on {} -> {:?}",
                    short_id(&loop_info.id),
                    hostname,
                    loop_info.status
                )
            }
            ServerEvent::LoopEnded {
                loop_info,
                hostname,
                ..
            } => {
                format!("[loop_ended] {} on {}", short_id(&loop_info.id), hostname)
            }
            ServerEvent::ProjectsUpdated { host_id } => {
                format!("[projects_updated] host {}", short_id(host_id))
            }
            ServerEvent::ClaudeTaskStarted {
                task_id,
                project_path,
                ..
            } => {
                format!(
                    "[claude_task_started] {} in {}",
                    short_id(task_id),
                    truncate(project_path, 35)
                )
            }
            ServerEvent::ClaudeTaskUpdated {
                task_id, status, ..
            } => {
                format!(
                    "[claude_task_updated] {} -> {:?}",
                    short_id(task_id),
                    status
                )
            }
            ServerEvent::ClaudeTaskEnded {
                task_id, status, ..
            } => {
                format!("[claude_task_ended] {} ({:?})", short_id(task_id), status)
            }
            _ => {
                // Fallback for events without a specific format
                serde_json::to_string(event).unwrap_or_else(|_| format!("{event:?}"))
            }
        }
    }

    fn directory_entries(&self, entries: &[DirectoryEntry]) -> String {
        if entries.is_empty() {
            return "Empty directory.".to_string();
        }
        let mut t = new_table(&["NAME", "TYPE"]);
        for e in entries {
            let kind = if e.is_dir { "dir" } else { "file" };
            t.add_row([&e.name, kind]);
        }
        t.to_string()
    }
}
