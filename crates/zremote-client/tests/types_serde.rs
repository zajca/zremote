use zremote_client::types::{
    AgenticLoop, ClaudeTask, ConfigValue, CreateSessionRequest, Host, KnowledgeBase,
    ListClaudeTasksFilter, ListLoopsFilter, Memory, Project, ServerEvent, Session, TerminalEvent,
};
use zremote_client::{
    AgenticStatus, ClaudeTaskStatus, KnowledgeServiceStatus, MemoryCategory, SessionStatus,
};

// ---------------------------------------------------------------------------
// Response type roundtrip tests
// ---------------------------------------------------------------------------

#[test]
fn host_deserialize() {
    let json = r#"{
        "id": "h-1234",
        "name": "my-server",
        "hostname": "server.example.com",
        "status": "online",
        "last_seen_at": "2026-03-24T10:00:00Z",
        "agent_version": "0.3.9",
        "os": "linux",
        "arch": "x86_64",
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-03-24T10:00:00Z"
    }"#;
    let host: Host = serde_json::from_str(json).unwrap();
    assert_eq!(host.id, "h-1234");
    assert_eq!(host.name, "my-server");
    assert_eq!(host.hostname, "server.example.com");
    assert_eq!(host.status, zremote_client::HostStatus::Online);
    assert_eq!(host.agent_version.as_deref(), Some("0.3.9"));
    assert_eq!(host.os.as_deref(), Some("linux"));
    assert_eq!(host.arch.as_deref(), Some("x86_64"));
}

#[test]
fn host_deserialize_minimal() {
    let json = r#"{
        "id": "h-1234",
        "name": "server",
        "hostname": "host",
        "status": "offline",
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z"
    }"#;
    let host: Host = serde_json::from_str(json).unwrap();
    assert!(host.last_seen_at.is_none());
    assert!(host.agent_version.is_none());
    assert!(host.os.is_none());
    assert!(host.arch.is_none());
}

#[test]
fn session_deserialize() {
    let json = r#"{
        "id": "s-abcd",
        "host_id": "h-1234",
        "name": "dev-session",
        "shell": "/bin/zsh",
        "status": "active",
        "working_dir": "/home/user/project",
        "project_id": "p-001",
        "pid": 12345,
        "exit_code": null,
        "created_at": "2026-03-24T09:00:00Z",
        "closed_at": null
    }"#;
    let session: Session = serde_json::from_str(json).unwrap();
    assert_eq!(session.id, "s-abcd");
    assert_eq!(session.host_id, "h-1234");
    assert_eq!(session.name.as_deref(), Some("dev-session"));
    assert_eq!(session.shell.as_deref(), Some("/bin/zsh"));
    assert_eq!(session.status, SessionStatus::Active);
    assert_eq!(session.pid, Some(12345));
    assert!(session.exit_code.is_none());
}

#[test]
fn project_deserialize() {
    let json = r#"{
        "id": "p-001",
        "host_id": "h-1234",
        "path": "/home/user/myapp",
        "name": "myapp",
        "has_claude_config": true,
        "has_zremote_config": false,
        "project_type": "rust",
        "created_at": "2026-01-15T00:00:00Z",
        "parent_project_id": null,
        "git_branch": "main",
        "git_commit_hash": "abc123",
        "git_commit_message": "Initial commit",
        "git_is_dirty": true,
        "git_ahead": 2,
        "git_behind": 0,
        "git_remotes": "origin",
        "git_updated_at": "2026-03-24T08:00:00Z",
        "pinned": true
    }"#;
    let project: Project = serde_json::from_str(json).unwrap();
    assert_eq!(project.id, "p-001");
    assert_eq!(project.name, "myapp");
    assert!(project.has_claude_config);
    assert!(!project.has_zremote_config);
    assert_eq!(project.project_type, "rust");
    assert!(project.git_is_dirty);
    assert_eq!(project.git_ahead, 2);
    assert_eq!(project.git_behind, 0);
    assert!(project.pinned);
}

#[test]
fn project_deserialize_defaults() {
    let json = r#"{
        "id": "p-002",
        "host_id": "h-1234",
        "path": "/tmp/test",
        "name": "test",
        "project_type": "node",
        "created_at": "2026-01-01T00:00:00Z"
    }"#;
    let project: Project = serde_json::from_str(json).unwrap();
    assert!(!project.has_claude_config);
    assert!(!project.has_zremote_config);
    assert!(!project.git_is_dirty);
    assert_eq!(project.git_ahead, 0);
    assert_eq!(project.git_behind, 0);
    assert!(!project.pinned);
}

#[test]
fn agentic_loop_deserialize() {
    let json = r#"{
        "id": "l-1111",
        "session_id": "s-abcd",
        "project_path": "/home/user/myapp",
        "tool_name": "claude_code",
        "status": "working",
        "started_at": "2026-03-24T10:00:00Z",
        "ended_at": null,
        "end_reason": null,
        "task_name": "Fix bug #42"
    }"#;
    let al: AgenticLoop = serde_json::from_str(json).unwrap();
    assert_eq!(al.id, "l-1111");
    assert_eq!(al.session_id, "s-abcd");
    assert_eq!(al.tool_name, "claude_code");
    assert_eq!(al.status, AgenticStatus::Working);
    assert_eq!(al.task_name.as_deref(), Some("Fix bug #42"));
}

#[test]
fn agentic_loop_all_statuses() {
    for (json_val, expected) in [
        ("working", AgenticStatus::Working),
        ("waiting_for_input", AgenticStatus::WaitingForInput),
        ("error", AgenticStatus::Error),
        ("completed", AgenticStatus::Completed),
        ("some_future_status", AgenticStatus::Unknown),
    ] {
        let json = format!(
            r#"{{"id":"l","session_id":"s","tool_name":"t","status":"{json_val}","started_at":"2026-01-01T00:00:00Z"}}"#
        );
        let al: AgenticLoop = serde_json::from_str(&json).unwrap();
        assert_eq!(al.status, expected, "status mismatch for {json_val}");
    }
}

#[test]
fn claude_task_deserialize() {
    let json = r#"{
        "id": "ct-001",
        "session_id": "s-abcd",
        "host_id": "h-1234",
        "project_path": "/home/user/myapp",
        "project_id": "p-001",
        "model": "opus",
        "initial_prompt": "Fix the tests",
        "claude_session_id": "cs-999",
        "resume_from": null,
        "status": "active",
        "options_json": null,
        "loop_id": "l-1111",
        "started_at": "2026-03-24T10:00:00Z",
        "ended_at": null,
        "total_cost_usd": 0.15,
        "total_tokens_in": 5000,
        "total_tokens_out": 1200,
        "summary": "Fixed 3 failing tests",
        "task_name": "Fix tests",
        "created_at": "2026-03-24T09:59:00Z"
    }"#;
    let task: ClaudeTask = serde_json::from_str(json).unwrap();
    assert_eq!(task.id, "ct-001");
    assert_eq!(task.status, ClaudeTaskStatus::Active);
    assert_eq!(task.model.as_deref(), Some("opus"));
    assert_eq!(task.total_cost_usd, Some(0.15));
    assert_eq!(task.total_tokens_in, Some(5000));
    assert_eq!(task.summary.as_deref(), Some("Fixed 3 failing tests"));
}

#[test]
fn claude_task_all_statuses() {
    for (json_val, expected) in [
        ("starting", ClaudeTaskStatus::Starting),
        ("active", ClaudeTaskStatus::Active),
        ("completed", ClaudeTaskStatus::Completed),
        ("error", ClaudeTaskStatus::Error),
    ] {
        let json = format!(
            r#"{{"id":"ct","session_id":"s","host_id":"h","project_path":"/p","status":"{json_val}","started_at":"t","created_at":"t"}}"#
        );
        let task: ClaudeTask = serde_json::from_str(&json).unwrap();
        assert_eq!(task.status, expected, "status mismatch for {json_val}");
    }
}

#[test]
fn config_value_deserialize() {
    let json = r#"{
        "key": "theme",
        "value": "dark",
        "updated_at": "2026-03-24T10:00:00Z"
    }"#;
    let cv: ConfigValue = serde_json::from_str(json).unwrap();
    assert_eq!(cv.key, "theme");
    assert_eq!(cv.value, "dark");
}

#[test]
fn knowledge_base_deserialize() {
    let json = r#"{
        "id": "kb-001",
        "host_id": "h-1234",
        "status": "ready",
        "openviking_version": "1.2.0",
        "last_error": null,
        "started_at": "2026-03-24T08:00:00Z",
        "updated_at": "2026-03-24T10:00:00Z"
    }"#;
    let kb: KnowledgeBase = serde_json::from_str(json).unwrap();
    assert_eq!(kb.id, "kb-001");
    assert_eq!(kb.status, KnowledgeServiceStatus::Ready);
    assert_eq!(kb.openviking_version.as_deref(), Some("1.2.0"));
}

#[test]
fn knowledge_base_all_statuses() {
    for (json_val, expected) in [
        ("starting", KnowledgeServiceStatus::Starting),
        ("ready", KnowledgeServiceStatus::Ready),
        ("indexing", KnowledgeServiceStatus::Indexing),
        ("error", KnowledgeServiceStatus::Error),
        ("stopped", KnowledgeServiceStatus::Stopped),
    ] {
        let json = format!(r#"{{"id":"kb","host_id":"h","status":"{json_val}","updated_at":"t"}}"#);
        let kb: KnowledgeBase = serde_json::from_str(&json).unwrap();
        assert_eq!(kb.status, expected, "status mismatch for {json_val}");
    }
}

#[test]
fn memory_deserialize() {
    let json = r#"{
        "id": "m-001",
        "project_id": "p-001",
        "loop_id": "l-1111",
        "key": "prefer-async",
        "content": "Use async for all I/O operations",
        "category": "preference",
        "confidence": 0.95,
        "created_at": "2026-03-24T10:00:00Z",
        "updated_at": "2026-03-24T10:00:00Z"
    }"#;
    let mem: Memory = serde_json::from_str(json).unwrap();
    assert_eq!(mem.id, "m-001");
    assert_eq!(mem.key, "prefer-async");
    assert_eq!(mem.category, MemoryCategory::Preference);
    assert!((mem.confidence - 0.95).abs() < f64::EPSILON);
}

#[test]
fn memory_all_categories() {
    for (json_val, expected) in [
        ("pattern", MemoryCategory::Pattern),
        ("decision", MemoryCategory::Decision),
        ("pitfall", MemoryCategory::Pitfall),
        ("preference", MemoryCategory::Preference),
        ("architecture", MemoryCategory::Architecture),
        ("convention", MemoryCategory::Convention),
    ] {
        let json = format!(
            r#"{{"id":"m","project_id":"p","key":"k","content":"c","category":"{json_val}","confidence":0.5,"created_at":"t","updated_at":"t"}}"#
        );
        let mem: Memory = serde_json::from_str(&json).unwrap();
        assert_eq!(mem.category, expected, "category mismatch for {json_val}");
    }
}

// ---------------------------------------------------------------------------
// ServerEvent parsing tests (all variants)
// ---------------------------------------------------------------------------

#[test]
fn server_event_session_created() {
    let json = r#"{
        "type": "session_created",
        "session": {
            "id": "s-abcd",
            "host_id": "h-1234",
            "status": "active"
        }
    }"#;
    let event: ServerEvent = serde_json::from_str(json).unwrap();
    match event {
        ServerEvent::SessionCreated { session } => {
            assert_eq!(session.id, "s-abcd");
            assert_eq!(session.host_id, "h-1234");
            assert_eq!(session.status, SessionStatus::Active);
        }
        other => panic!("expected SessionCreated, got {other:?}"),
    }
}

#[test]
fn server_event_session_closed() {
    let json = r#"{"type": "session_closed", "session_id": "s-abcd", "exit_code": 0}"#;
    let event: ServerEvent = serde_json::from_str(json).unwrap();
    match event {
        ServerEvent::SessionClosed {
            session_id,
            exit_code,
        } => {
            assert_eq!(session_id, "s-abcd");
            assert_eq!(exit_code, Some(0));
        }
        other => panic!("expected SessionClosed, got {other:?}"),
    }
}

#[test]
fn server_event_session_closed_no_exit_code() {
    let json = r#"{"type": "session_closed", "session_id": "s-abcd", "exit_code": null}"#;
    let event: ServerEvent = serde_json::from_str(json).unwrap();
    match event {
        ServerEvent::SessionClosed { exit_code, .. } => assert!(exit_code.is_none()),
        other => panic!("expected SessionClosed, got {other:?}"),
    }
}

#[test]
fn server_event_session_updated() {
    let json = r#"{"type": "session_updated", "session_id": "s-abcd"}"#;
    let event: ServerEvent = serde_json::from_str(json).unwrap();
    assert!(matches!(
        event,
        ServerEvent::SessionUpdated { session_id } if session_id == "s-abcd"
    ));
}

#[test]
fn server_event_session_suspended() {
    let json = r#"{"type": "session_suspended", "session_id": "s-abcd"}"#;
    let event: ServerEvent = serde_json::from_str(json).unwrap();
    assert!(matches!(
        event,
        ServerEvent::SessionSuspended { session_id } if session_id == "s-abcd"
    ));
}

#[test]
fn server_event_session_resumed() {
    let json = r#"{"type": "session_resumed", "session_id": "s-abcd"}"#;
    let event: ServerEvent = serde_json::from_str(json).unwrap();
    assert!(matches!(
        event,
        ServerEvent::SessionResumed { session_id } if session_id == "s-abcd"
    ));
}

#[test]
fn server_event_host_connected() {
    let json = r#"{
        "type": "host_connected",
        "host": {
            "id": "h-1234",
            "hostname": "server.example.com",
            "status": "online",
            "agent_version": "0.3.9",
            "os": "linux",
            "arch": "x86_64"
        }
    }"#;
    let event: ServerEvent = serde_json::from_str(json).unwrap();
    match event {
        ServerEvent::HostConnected { host } => {
            assert_eq!(host.id, "h-1234");
            assert_eq!(host.hostname, "server.example.com");
            assert_eq!(host.agent_version.as_deref(), Some("0.3.9"));
        }
        other => panic!("expected HostConnected, got {other:?}"),
    }
}

#[test]
fn server_event_host_disconnected() {
    let json = r#"{"type": "host_disconnected", "host_id": "h-1234"}"#;
    let event: ServerEvent = serde_json::from_str(json).unwrap();
    assert!(matches!(
        event,
        ServerEvent::HostDisconnected { host_id } if host_id == "h-1234"
    ));
}

#[test]
fn server_event_host_status_changed() {
    let json = r#"{"type": "host_status_changed", "host_id": "h-1234", "status": "offline"}"#;
    let event: ServerEvent = serde_json::from_str(json).unwrap();
    match event {
        ServerEvent::HostStatusChanged { host_id, status } => {
            assert_eq!(host_id, "h-1234");
            assert_eq!(status, zremote_client::HostStatus::Offline);
        }
        other => panic!("expected HostStatusChanged, got {other:?}"),
    }
}

#[test]
fn server_event_projects_updated() {
    let json = r#"{"type": "projects_updated", "host_id": "h-1234"}"#;
    let event: ServerEvent = serde_json::from_str(json).unwrap();
    assert!(matches!(
        event,
        ServerEvent::ProjectsUpdated { host_id } if host_id == "h-1234"
    ));
}

#[test]
fn server_event_loop_detected() {
    let json = r#"{
        "type": "agentic_loop_detected",
        "loop": {
            "id": "l-1111",
            "session_id": "s-abcd",
            "project_path": "/home/user/myapp",
            "tool_name": "claude_code",
            "status": "working",
            "started_at": "2026-03-24T10:00:00Z"
        },
        "host_id": "h-1234",
        "hostname": "server.example.com"
    }"#;
    let event: ServerEvent = serde_json::from_str(json).unwrap();
    match event {
        ServerEvent::LoopDetected {
            loop_info,
            host_id,
            hostname,
        } => {
            assert_eq!(loop_info.id, "l-1111");
            assert_eq!(loop_info.tool_name, "claude_code");
            assert_eq!(loop_info.status, AgenticStatus::Working);
            assert_eq!(host_id, "h-1234");
            assert_eq!(hostname, "server.example.com");
        }
        other => panic!("expected LoopDetected, got {other:?}"),
    }
}

#[test]
fn server_event_loop_state_changed() {
    let json = r#"{
        "type": "agentic_loop_state_update",
        "loop": {
            "id": "l-1111",
            "session_id": "s-abcd",
            "tool_name": "claude_code",
            "status": "waiting_for_input",
            "started_at": "2026-03-24T10:00:00Z"
        },
        "host_id": "h-1234",
        "hostname": "server.example.com"
    }"#;
    let event: ServerEvent = serde_json::from_str(json).unwrap();
    match event {
        ServerEvent::LoopStatusChanged {
            loop_info, host_id, ..
        } => {
            assert_eq!(loop_info.status, AgenticStatus::WaitingForInput);
            assert_eq!(host_id, "h-1234");
        }
        other => panic!("expected LoopStatusChanged, got {other:?}"),
    }
}

#[test]
fn server_event_loop_ended() {
    let json = r#"{
        "type": "agentic_loop_ended",
        "loop": {
            "id": "l-1111",
            "session_id": "s-abcd",
            "tool_name": "claude_code",
            "status": "completed",
            "started_at": "2026-03-24T10:00:00Z",
            "ended_at": "2026-03-24T10:05:00Z",
            "end_reason": "natural"
        },
        "host_id": "h-1234",
        "hostname": "server.example.com"
    }"#;
    let event: ServerEvent = serde_json::from_str(json).unwrap();
    match event {
        ServerEvent::LoopEnded { loop_info, .. } => {
            assert_eq!(loop_info.status, AgenticStatus::Completed);
            assert_eq!(loop_info.end_reason.as_deref(), Some("natural"));
        }
        other => panic!("expected LoopEnded, got {other:?}"),
    }
}

#[test]
fn server_event_knowledge_status_changed() {
    let json = r#"{
        "type": "knowledge_status_changed",
        "host_id": "h-1234",
        "status": "ready",
        "error": null
    }"#;
    let event: ServerEvent = serde_json::from_str(json).unwrap();
    match event {
        ServerEvent::KnowledgeStatusChanged {
            host_id,
            status,
            error,
        } => {
            assert_eq!(host_id, "h-1234");
            assert_eq!(status, "ready");
            assert!(error.is_none());
        }
        other => panic!("expected KnowledgeStatusChanged, got {other:?}"),
    }
}

#[test]
fn server_event_indexing_progress() {
    let json = r#"{
        "type": "indexing_progress",
        "project_id": "p-001",
        "project_path": "/home/user/myapp",
        "status": "in_progress",
        "files_processed": 42,
        "files_total": 100
    }"#;
    let event: ServerEvent = serde_json::from_str(json).unwrap();
    match event {
        ServerEvent::IndexingProgress {
            project_id,
            files_processed,
            files_total,
            ..
        } => {
            assert_eq!(project_id, "p-001");
            assert_eq!(files_processed, 42);
            assert_eq!(files_total, 100);
        }
        other => panic!("expected IndexingProgress, got {other:?}"),
    }
}

#[test]
fn server_event_memory_extracted() {
    let json = r#"{
        "type": "memory_extracted",
        "project_id": "p-001",
        "loop_id": "l-1111",
        "memory_count": 5
    }"#;
    let event: ServerEvent = serde_json::from_str(json).unwrap();
    match event {
        ServerEvent::MemoryExtracted {
            project_id,
            loop_id,
            memory_count,
        } => {
            assert_eq!(project_id, "p-001");
            assert_eq!(loop_id, "l-1111");
            assert_eq!(memory_count, 5);
        }
        other => panic!("expected MemoryExtracted, got {other:?}"),
    }
}

#[test]
fn server_event_worktree_error() {
    let json = r#"{
        "type": "worktree_error",
        "host_id": "h-1234",
        "project_path": "/home/user/myapp",
        "message": "branch already exists"
    }"#;
    let event: ServerEvent = serde_json::from_str(json).unwrap();
    match event {
        ServerEvent::WorktreeError {
            host_id, message, ..
        } => {
            assert_eq!(host_id, "h-1234");
            assert_eq!(message, "branch already exists");
        }
        other => panic!("expected WorktreeError, got {other:?}"),
    }
}

#[test]
fn server_event_claude_task_started() {
    let json = r#"{
        "type": "claude_task_started",
        "task_id": "ct-001",
        "session_id": "s-abcd",
        "host_id": "h-1234",
        "project_path": "/home/user/myapp"
    }"#;
    let event: ServerEvent = serde_json::from_str(json).unwrap();
    match event {
        ServerEvent::ClaudeTaskStarted {
            task_id,
            session_id,
            host_id,
            project_path,
        } => {
            assert_eq!(task_id, "ct-001");
            assert_eq!(session_id, "s-abcd");
            assert_eq!(host_id, "h-1234");
            assert_eq!(project_path, "/home/user/myapp");
        }
        other => panic!("expected ClaudeTaskStarted, got {other:?}"),
    }
}

#[test]
fn server_event_claude_task_updated() {
    let json = r#"{
        "type": "claude_task_updated",
        "task_id": "ct-001",
        "status": "active",
        "loop_id": "l-1111"
    }"#;
    let event: ServerEvent = serde_json::from_str(json).unwrap();
    match event {
        ServerEvent::ClaudeTaskUpdated {
            task_id,
            status,
            loop_id,
        } => {
            assert_eq!(task_id, "ct-001");
            assert_eq!(status, ClaudeTaskStatus::Active);
            assert_eq!(loop_id.as_deref(), Some("l-1111"));
        }
        other => panic!("expected ClaudeTaskUpdated, got {other:?}"),
    }
}

#[test]
fn server_event_claude_task_ended() {
    let json = r#"{
        "type": "claude_task_ended",
        "task_id": "ct-001",
        "status": "completed",
        "summary": "Done fixing tests"
    }"#;
    let event: ServerEvent = serde_json::from_str(json).unwrap();
    match event {
        ServerEvent::ClaudeTaskEnded {
            task_id,
            status,
            summary,
        } => {
            assert_eq!(task_id, "ct-001");
            assert_eq!(status, ClaudeTaskStatus::Completed);
            assert_eq!(summary.as_deref(), Some("Done fixing tests"));
        }
        other => panic!("expected ClaudeTaskEnded, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Request type serialization tests
// ---------------------------------------------------------------------------

#[test]
fn create_session_request_new_defaults() {
    let req = CreateSessionRequest::new(120, 40);
    assert_eq!(req.cols, 120);
    assert_eq!(req.rows, 40);
    assert!(req.name.is_none());
    assert!(req.shell.is_none());
    assert!(req.working_dir.is_none());
}

#[test]
fn create_session_request_skips_none_fields() {
    let req = CreateSessionRequest::new(80, 24);
    let json = serde_json::to_value(&req).unwrap();
    assert!(!json.as_object().unwrap().contains_key("name"));
    assert!(!json.as_object().unwrap().contains_key("shell"));
    assert!(!json.as_object().unwrap().contains_key("working_dir"));
    assert_eq!(json["cols"], 80);
    assert_eq!(json["rows"], 24);
}

#[test]
fn create_session_request_includes_set_fields() {
    let req = CreateSessionRequest {
        name: Some("my-session".to_string()),
        shell: Some("/bin/bash".to_string()),
        cols: 100,
        rows: 30,
        working_dir: Some("/home/user".to_string()),
    };
    let json = serde_json::to_value(&req).unwrap();
    assert_eq!(json["name"], "my-session");
    assert_eq!(json["shell"], "/bin/bash");
    assert_eq!(json["working_dir"], "/home/user");
}

// ---------------------------------------------------------------------------
// TerminalEvent tests (public enum - not serde, just verify construction)
// ---------------------------------------------------------------------------

#[test]
fn terminal_event_variants_constructible() {
    let events: Vec<TerminalEvent> = vec![
        TerminalEvent::Output(vec![0x41, 0x42]),
        TerminalEvent::PaneOutput {
            pane_id: "p1".to_string(),
            data: vec![0x43],
        },
        TerminalEvent::PaneAdded {
            pane_id: "p1".to_string(),
            index: 0,
        },
        TerminalEvent::PaneRemoved {
            pane_id: "p1".to_string(),
        },
        TerminalEvent::SessionClosed { exit_code: Some(0) },
        TerminalEvent::ScrollbackStart { cols: 80, rows: 24 },
        TerminalEvent::ScrollbackEnd { truncated: false },
        TerminalEvent::SessionSuspended,
        TerminalEvent::SessionResumed,
    ];
    assert_eq!(events.len(), 9);
}

// ---------------------------------------------------------------------------
// Filter serialization tests
// ---------------------------------------------------------------------------

#[test]
fn list_loops_filter_empty_serializes_clean() {
    let filter = ListLoopsFilter::default();
    let json = serde_json::to_value(&filter).unwrap();
    let obj = json.as_object().unwrap();
    assert!(obj.is_empty(), "empty filter should have no keys: {obj:?}");
}

#[test]
fn list_loops_filter_with_values() {
    let filter = ListLoopsFilter {
        status: Some("working".to_string()),
        host_id: Some("h-1234".to_string()),
        session_id: None,
        project_id: None,
    };
    let json = serde_json::to_value(&filter).unwrap();
    let obj = json.as_object().unwrap();
    assert_eq!(obj.len(), 2);
    assert_eq!(json["status"], "working");
    assert_eq!(json["host_id"], "h-1234");
}

#[test]
fn list_claude_tasks_filter_empty_serializes_clean() {
    let filter = ListClaudeTasksFilter::default();
    let json = serde_json::to_value(&filter).unwrap();
    let obj = json.as_object().unwrap();
    assert!(obj.is_empty(), "empty filter should have no keys: {obj:?}");
}

#[test]
fn list_claude_tasks_filter_with_values() {
    let filter = ListClaudeTasksFilter {
        host_id: Some("h-1234".to_string()),
        status: Some("active".to_string()),
        project_id: Some("p-001".to_string()),
    };
    let json = serde_json::to_value(&filter).unwrap();
    let obj = json.as_object().unwrap();
    assert_eq!(obj.len(), 3);
    assert_eq!(json["host_id"], "h-1234");
    assert_eq!(json["status"], "active");
    assert_eq!(json["project_id"], "p-001");
}

// ---------------------------------------------------------------------------
// ServerEvent roundtrip (serialize then deserialize)
// ---------------------------------------------------------------------------

#[test]
fn server_event_roundtrip_session_created() {
    use zremote_client::SessionInfo;
    let event = ServerEvent::SessionCreated {
        session: SessionInfo {
            id: "s-1".to_string(),
            host_id: "h-1".to_string(),
            shell: Some("/bin/bash".to_string()),
            status: SessionStatus::Active,
        },
    };
    let json = serde_json::to_string(&event).unwrap();
    let parsed: ServerEvent = serde_json::from_str(&json).unwrap();
    match parsed {
        ServerEvent::SessionCreated { session } => {
            assert_eq!(session.id, "s-1");
        }
        other => panic!("roundtrip failed: {other:?}"),
    }
}

#[test]
fn server_event_roundtrip_loop_detected() {
    use zremote_client::LoopInfo;
    let event = ServerEvent::LoopDetected {
        loop_info: LoopInfo {
            id: "l-1".to_string(),
            session_id: "s-1".to_string(),
            project_path: Some("/p".to_string()),
            tool_name: "claude_code".to_string(),
            status: AgenticStatus::Working,
            started_at: "2026-03-24T10:00:00Z".to_string(),
            ended_at: None,
            end_reason: None,
            task_name: None,
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: None,
        },
        host_id: "h-1".to_string(),
        hostname: "host".to_string(),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains(r#""type":"agentic_loop_detected"#));
    assert!(json.contains(r#""loop":"#));
    let parsed: ServerEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(parsed, ServerEvent::LoopDetected { .. }));
}
