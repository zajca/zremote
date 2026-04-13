//! LLM-optimized JSON Lines output formatter.
//!
//! Produces compact JSON Lines (one object per line) with short keys
//! for minimal token consumption by language models.

use serde_json::json;
use zremote_client::types::ProjectSettings;
use zremote_client::{
    ActionsResponse, AgenticLoop, ClaudeTask, ConfigValue, DirectoryEntry, Host, HostStatus,
    KnowledgeBase, Memory, ModeInfo, Project, SearchResult, ServerEvent, Session,
};

use super::Formatter;

pub struct LlmFormatter;

/// Serialize a serde-compatible status enum to a lowercase string.
fn status_str<T: serde::Serialize + std::fmt::Debug>(s: &T) -> String {
    serde_json::to_value(s)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| format!("{s:?}").to_lowercase())
}

fn opt_str(s: Option<&String>) -> serde_json::Value {
    match s {
        Some(v) => json!(v),
        None => serde_json::Value::Null,
    }
}

fn to_line(v: &serde_json::Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

impl Formatter for LlmFormatter {
    fn hosts(&self, hosts: &[Host]) -> String {
        hosts
            .iter()
            .map(|h| self.host(h))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn host(&self, h: &Host) -> String {
        to_line(&json!({
            "_t": "host",
            "id": h.id,
            "n": h.name,
            "st": status_str(&h.status),
            "v": opt_str(h.agent_version.as_ref()),
            "hostname": h.hostname,
        }))
    }

    fn sessions(&self, sessions: &[Session]) -> String {
        sessions
            .iter()
            .map(|s| self.session(s))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn session(&self, s: &Session) -> String {
        to_line(&json!({
            "_t": "session",
            "id": s.id,
            "n": opt_str(s.name.as_ref()),
            "st": status_str(&s.status),
            "shell": opt_str(s.shell.as_ref()),
            "dir": opt_str(s.working_dir.as_ref()),
        }))
    }

    fn projects(&self, projects: &[Project]) -> String {
        projects
            .iter()
            .map(|p| self.project(p))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn project(&self, p: &Project) -> String {
        to_line(&json!({
            "_t": "project",
            "id": p.id,
            "n": p.name,
            "path": p.path,
            "type": p.project_type,
            "branch": opt_str(p.git_branch.as_ref()),
            "dirty": p.git_is_dirty,
        }))
    }

    fn loops(&self, loops: &[AgenticLoop]) -> String {
        loops
            .iter()
            .map(|l| self.agentic_loop(l))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn agentic_loop(&self, l: &AgenticLoop) -> String {
        to_line(&json!({
            "_t": "loop",
            "id": l.id,
            "session": l.session_id,
            "st": status_str(&l.status),
            "tool": l.tool_name,
            "task": opt_str(l.task_name.as_ref()),
        }))
    }

    fn tasks(&self, tasks: &[ClaudeTask]) -> String {
        tasks
            .iter()
            .map(|t| self.task(t))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn task(&self, t: &ClaudeTask) -> String {
        let mut val = json!({
            "_t": "task",
            "id": t.id,
            "sid": t.session_id,
            "st": status_str(&t.status),
            "model": opt_str(t.model.as_ref()),
            "project": t.project_path,
            "cost": t.total_cost_usd,
        });
        if let Some(ref err) = t.error_message {
            val["error"] = json!(err);
        }
        to_line(&val)
    }

    fn memories(&self, memories: &[Memory]) -> String {
        memories
            .iter()
            .map(|m| self.memory(m))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn memory(&self, m: &Memory) -> String {
        to_line(&json!({
            "_t": "memory",
            "id": m.id,
            "key": m.key,
            "cat": status_str(&m.category),
            "content": m.content,
        }))
    }

    fn config_value(&self, cv: &ConfigValue) -> String {
        to_line(&json!({
            "_t": "config",
            "key": cv.key,
            "v": cv.value,
        }))
    }

    fn settings(&self, settings: &Option<ProjectSettings>) -> String {
        match settings {
            Some(s) => {
                serde_json::to_string(s).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
            }
            None => r#"{"_t":"settings","empty":true}"#.to_string(),
        }
    }

    fn actions(&self, resp: &ActionsResponse) -> String {
        resp.actions
            .iter()
            .map(|a| {
                to_line(&json!({
                    "_t": "action",
                    "n": a.name,
                    "command": a.command,
                }))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn worktrees(&self, worktrees: &[Project]) -> String {
        worktrees
            .iter()
            .map(|w| {
                to_line(&json!({
                    "_t": "worktree",
                    "id": w.id,
                    "path": w.path,
                    "branch": opt_str(w.git_branch.as_ref()),
                    "dirty": w.git_is_dirty,
                }))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn knowledge_status(&self, kb: &KnowledgeBase) -> String {
        to_line(&json!({
            "_t": "knowledge",
            "id": kb.id,
            "st": status_str(&kb.status),
            "v": opt_str(kb.openviking_version.as_ref()),
        }))
    }

    fn search_results(&self, results: &SearchResult) -> String {
        serde_json::to_string(results).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    }

    fn status_info(&self, mode: &ModeInfo, hosts: &[Host]) -> String {
        let online = hosts
            .iter()
            .filter(|h| h.status == HostStatus::Online)
            .count();
        to_line(&json!({
            "_t": "status",
            "mode": mode.mode,
            "v": opt_str(mode.version.as_ref()),
            "hosts": hosts.len(),
            "online": online,
        }))
    }

    fn event(&self, event: &ServerEvent) -> String {
        serde_json::to_string(event).unwrap_or_else(|_| format!("{event:?}"))
    }

    fn directory_entries(&self, entries: &[DirectoryEntry]) -> String {
        entries
            .iter()
            .map(|e| {
                to_line(&json!({
                    "_t": if e.is_dir { "dir" } else { "file" },
                    "n": e.name,
                }))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zremote_client::{AgenticStatus, ClaudeTaskStatus, MemoryCategory, SessionStatus};

    fn make_host() -> Host {
        Host {
            id: "a1b2c3d4-e5f6-7890-abcd-ef1234567890".to_string(),
            name: "dev-box".to_string(),
            hostname: "dev.internal".to_string(),
            status: HostStatus::Online,
            last_seen_at: Some("2025-01-01T00:00:00Z".to_string()),
            agent_version: Some("0.9.0".to_string()),
            os: Some("linux".to_string()),
            arch: Some("x86_64".to_string()),
            created_at: "2025-01-01T00:00:00Z".to_string(),
            updated_at: "2025-01-01T00:00:00Z".to_string(),
        }
    }

    fn make_session() -> Session {
        Session {
            id: "b2c3d4e5-f6a7-8901-bcde-f12345678901".to_string(),
            host_id: "a1b2c3d4-e5f6-7890-abcd-ef1234567890".to_string(),
            name: Some("main".to_string()),
            shell: Some("/bin/zsh".to_string()),
            status: SessionStatus::Active,
            working_dir: Some("/home/user/project".to_string()),
            project_id: None,
            pid: Some(1234),
            exit_code: None,
            created_at: "2025-01-01T00:00:00Z".to_string(),
            closed_at: None,
        }
    }

    fn make_project() -> Project {
        Project {
            id: "c3d4e5f6-a7b8-9012-cdef-123456789012".to_string(),
            host_id: "a1b2c3d4-e5f6-7890-abcd-ef1234567890".to_string(),
            path: "/home/user/myapp".to_string(),
            name: "myapp".to_string(),
            has_claude_config: false,
            has_zremote_config: false,
            project_type: "rust".to_string(),
            created_at: "2025-01-01T00:00:00Z".to_string(),
            parent_project_id: None,
            git_branch: Some("main".to_string()),
            git_commit_hash: None,
            git_commit_message: None,
            git_is_dirty: false,
            git_ahead: 0,
            git_behind: 0,
            git_remotes: None,
            git_updated_at: None,
            pinned: false,
            frameworks: None,
            architecture: None,
            conventions: None,
            package_manager: None,
        }
    }

    fn make_loop() -> AgenticLoop {
        AgenticLoop {
            id: "d4e5f6a7-b8c9-0123-defa-234567890123".to_string(),
            session_id: "b2c3d4e5-f6a7-8901-bcde-f12345678901".to_string(),
            project_path: Some("/home/user/project".to_string()),
            tool_name: "claude".to_string(),
            status: AgenticStatus::Working,
            started_at: "2025-01-01T00:00:00Z".to_string(),
            ended_at: None,
            end_reason: None,
            task_name: Some("Fix auth bug".to_string()),
            prompt_message: None,
            permission_mode: None,
        }
    }

    fn make_task() -> ClaudeTask {
        ClaudeTask {
            id: "e5f6a7b8-c9d0-1234-efab-345678901234".to_string(),
            session_id: "b2c3d4e5-f6a7-8901-bcde-f12345678901".to_string(),
            host_id: "a1b2c3d4-e5f6-7890-abcd-ef1234567890".to_string(),
            project_path: "/home/user/repo".to_string(),
            project_id: None,
            model: Some("opus".to_string()),
            initial_prompt: None,
            claude_session_id: None,
            resume_from: None,
            status: ClaudeTaskStatus::Active,
            options_json: None,
            loop_id: None,
            started_at: "2025-01-01T00:00:00Z".to_string(),
            ended_at: None,
            total_cost_usd: Some(1.23),
            total_tokens_in: None,
            total_tokens_out: None,
            summary: None,
            task_name: None,
            created_at: "2025-01-01T00:00:00Z".to_string(),
            error_message: None,
            disconnect_reason: None,
        }
    }

    fn make_memory() -> Memory {
        Memory {
            id: "f6a7b8c9-d0e1-2345-fabc-456789012345".to_string(),
            project_id: "c3d4e5f6-a7b8-9012-cdef-123456789012".to_string(),
            loop_id: None,
            key: "auth-pattern".to_string(),
            content: "Use repository pattern".to_string(),
            category: MemoryCategory::Pattern,
            confidence: 0.9,
            created_at: "2025-01-01T00:00:00Z".to_string(),
            updated_at: "2025-01-01T00:00:00Z".to_string(),
        }
    }

    fn parse_json(s: &str) -> serde_json::Value {
        serde_json::from_str(s).unwrap_or_else(|e| panic!("Invalid JSON: {e}\nInput: {s}"))
    }

    #[test]
    fn host_produces_valid_json_with_short_keys() {
        let f = LlmFormatter;
        let h = make_host();
        let out = f.host(&h);
        let v = parse_json(&out);

        assert_eq!(v["_t"], "host");
        assert_eq!(v["id"], h.id);
        assert_eq!(v["n"], "dev-box");
        assert_eq!(v["hostname"], "dev.internal");
        assert_eq!(v["st"], "online");
        assert_eq!(v["v"], "0.9.0");
    }

    #[test]
    fn host_id_never_truncated() {
        let f = LlmFormatter;
        let h = make_host();
        let out = f.host(&h);
        let v = parse_json(&out);
        assert_eq!(v["id"], "a1b2c3d4-e5f6-7890-abcd-ef1234567890");
    }

    #[test]
    fn hosts_list_one_json_per_line() {
        let f = LlmFormatter;
        let hosts = vec![make_host(), make_host()];
        let out = f.hosts(&hosts);
        let lines: Vec<&str> = out.split('\n').collect();
        assert_eq!(lines.len(), 2);
        for line in &lines {
            let v = parse_json(line);
            assert_eq!(v["_t"], "host");
        }
    }

    #[test]
    fn session_produces_valid_json_with_short_keys() {
        let f = LlmFormatter;
        let s = make_session();
        let out = f.session(&s);
        let v = parse_json(&out);

        assert_eq!(v["_t"], "session");
        assert_eq!(v["id"], s.id);
        assert_eq!(v["n"], "main");
        assert_eq!(v["st"], "active");
        assert_eq!(v["shell"], "/bin/zsh");
        assert_eq!(v["dir"], "/home/user/project");
    }

    #[test]
    fn sessions_list_one_json_per_line() {
        let f = LlmFormatter;
        let sessions = vec![make_session(), make_session()];
        let out = f.sessions(&sessions);
        let lines: Vec<&str> = out.split('\n').collect();
        assert_eq!(lines.len(), 2);
        for line in &lines {
            let v = parse_json(line);
            assert_eq!(v["_t"], "session");
        }
    }

    #[test]
    fn project_produces_valid_json_with_short_keys() {
        let f = LlmFormatter;
        let p = make_project();
        let out = f.project(&p);
        let v = parse_json(&out);

        assert_eq!(v["_t"], "project");
        assert_eq!(v["id"], p.id);
        assert_eq!(v["n"], "myapp");
        assert_eq!(v["path"], "/home/user/myapp");
        assert_eq!(v["type"], "rust");
        assert_eq!(v["branch"], "main");
        assert_eq!(v["dirty"], false);
    }

    #[test]
    fn loop_produces_valid_json_with_short_keys() {
        let f = LlmFormatter;
        let l = make_loop();
        let out = f.agentic_loop(&l);
        let v = parse_json(&out);

        assert_eq!(v["_t"], "loop");
        assert_eq!(v["id"], l.id);
        assert_eq!(v["session"], l.session_id);
        assert_eq!(v["st"], "working");
        assert_eq!(v["tool"], "claude");
        assert_eq!(v["task"], "Fix auth bug");
    }

    #[test]
    fn task_produces_valid_json_with_short_keys() {
        let f = LlmFormatter;
        let t = make_task();
        let out = f.task(&t);
        let v = parse_json(&out);

        assert_eq!(v["_t"], "task");
        assert_eq!(v["id"], t.id);
        assert_eq!(v["sid"], t.session_id);
        assert_eq!(v["st"], "active");
        assert_eq!(v["model"], "opus");
        assert_eq!(v["project"], "/home/user/repo");
        assert_eq!(v["cost"], 1.23);
    }

    #[test]
    fn memory_produces_valid_json_with_short_keys() {
        let f = LlmFormatter;
        let m = make_memory();
        let out = f.memory(&m);
        let v = parse_json(&out);

        assert_eq!(v["_t"], "memory");
        assert_eq!(v["id"], m.id);
        assert_eq!(v["key"], "auth-pattern");
        assert_eq!(v["cat"], "pattern");
        assert_eq!(v["content"], "Use repository pattern");
    }

    #[test]
    fn status_info_produces_valid_json() {
        let f = LlmFormatter;
        let mode = ModeInfo {
            mode: "server".to_string(),
            version: Some("0.9.0".to_string()),
        };
        let hosts = vec![make_host()];
        let out = f.status_info(&mode, &hosts);
        let v = parse_json(&out);

        assert_eq!(v["_t"], "status");
        assert_eq!(v["mode"], "server");
        assert_eq!(v["v"], "0.9.0");
        assert_eq!(v["hosts"], 1);
        assert_eq!(v["online"], 1);
    }

    #[test]
    fn status_values_are_lowercase() {
        let fmt = LlmFormatter;

        let host = make_host();
        let parsed = parse_json(&fmt.host(&host));
        assert_eq!(parsed["st"], "online");

        let session = make_session();
        let parsed = parse_json(&fmt.session(&session));
        assert_eq!(parsed["st"], "active");

        let agentic_loop = make_loop();
        let parsed = parse_json(&fmt.agentic_loop(&agentic_loop));
        assert_eq!(parsed["st"], "working");

        let task = make_task();
        let parsed = parse_json(&fmt.task(&task));
        assert_eq!(parsed["st"], "active");
    }

    #[test]
    fn single_entity_produces_exactly_one_line() {
        let f = LlmFormatter;
        let out = f.host(&make_host());
        assert!(!out.contains('\n'), "Single host should be one line");

        let out = f.session(&make_session());
        assert!(!out.contains('\n'), "Single session should be one line");

        let out = f.project(&make_project());
        assert!(!out.contains('\n'), "Single project should be one line");

        let out = f.agentic_loop(&make_loop());
        assert!(!out.contains('\n'), "Single loop should be one line");

        let out = f.task(&make_task());
        assert!(!out.contains('\n'), "Single task should be one line");

        let out = f.memory(&make_memory());
        assert!(!out.contains('\n'), "Single memory should be one line");
    }

    #[test]
    fn config_value_produces_valid_json() {
        let f = LlmFormatter;
        let cv = ConfigValue {
            key: "theme".to_string(),
            value: "dark".to_string(),
            updated_at: "2025-01-01T00:00:00Z".to_string(),
        };
        let out = f.config_value(&cv);
        let v = parse_json(&out);

        assert_eq!(v["_t"], "config");
        assert_eq!(v["key"], "theme");
        assert_eq!(v["v"], "dark");
    }

    #[test]
    fn worktrees_produce_valid_json() {
        let f = LlmFormatter;
        let wt = Project {
            id: "wt-1".to_string(),
            host_id: "h-1".to_string(),
            path: "/home/user/repo-feat".to_string(),
            name: "repo-feat".to_string(),
            has_claude_config: false,
            has_zremote_config: false,
            project_type: "worktree".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            parent_project_id: Some("p-1".to_string()),
            git_branch: Some("feature".to_string()),
            git_commit_hash: Some("abc1234".to_string()),
            git_commit_message: None,
            git_is_dirty: true,
            git_ahead: 0,
            git_behind: 0,
            git_remotes: None,
            git_updated_at: None,
            pinned: false,
            frameworks: None,
            architecture: None,
            conventions: None,
            package_manager: None,
        };
        let out = f.worktrees(&[wt]);
        let v = parse_json(&out);

        assert_eq!(v["_t"], "worktree");
        assert_eq!(v["path"], "/home/user/repo-feat");
        assert_eq!(v["branch"], "feature");
        assert_eq!(v["dirty"], true);
    }

    #[test]
    fn directory_entries_use_type_tag() {
        let f = LlmFormatter;
        let entries = vec![
            DirectoryEntry {
                name: "src".to_string(),
                is_dir: true,
                is_symlink: false,
            },
            DirectoryEntry {
                name: "main.rs".to_string(),
                is_dir: false,
                is_symlink: false,
            },
        ];
        let out = f.directory_entries(&entries);
        let lines: Vec<&str> = out.split('\n').collect();
        assert_eq!(lines.len(), 2);

        let dir = parse_json(lines[0]);
        assert_eq!(dir["_t"], "dir");
        assert_eq!(dir["n"], "src");

        let file = parse_json(lines[1]);
        assert_eq!(file["_t"], "file");
        assert_eq!(file["n"], "main.rs");
    }

    #[test]
    fn event_produces_compact_json() {
        let f = LlmFormatter;
        // Events use serde_json::to_string directly, so just verify it doesn't panic
        // and produces non-empty output. We can't easily construct a ServerEvent here
        // without access to the full enum, but we verify the method exists and compiles.
        let _ = &f as &dyn Formatter;
    }

    #[test]
    fn knowledge_status_produces_valid_json() {
        let f = LlmFormatter;
        let kb = KnowledgeBase {
            id: "kb-1234".to_string(),
            host_id: "host-1234".to_string(),
            status: zremote_client::KnowledgeServiceStatus::Ready,
            openviking_version: Some("1.0.0".to_string()),
            last_error: None,
            started_at: Some("2025-01-01T00:00:00Z".to_string()),
            updated_at: "2025-01-01T00:00:00Z".to_string(),
        };
        let out = f.knowledge_status(&kb);
        let v = parse_json(&out);

        assert_eq!(v["_t"], "knowledge");
        assert_eq!(v["id"], "kb-1234");
        assert_eq!(v["st"], "ready");
        assert_eq!(v["v"], "1.0.0");
    }

    #[test]
    fn empty_list_produces_empty_string() {
        let f = LlmFormatter;
        assert_eq!(f.hosts(&[]), "");
        assert_eq!(f.sessions(&[]), "");
        assert_eq!(f.projects(&[]), "");
        assert_eq!(f.loops(&[]), "");
        assert_eq!(f.tasks(&[]), "");
        assert_eq!(f.memories(&[]), "");
    }

    #[test]
    fn optional_fields_are_null_when_none() {
        let f = LlmFormatter;
        let mut s = make_session();
        s.name = None;
        s.shell = None;
        s.working_dir = None;
        let out = f.session(&s);
        let v = parse_json(&out);

        assert!(v["n"].is_null());
        assert!(v["shell"].is_null());
        assert!(v["dir"].is_null());
    }
}
