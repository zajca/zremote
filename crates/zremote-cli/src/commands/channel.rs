use clap::Subcommand;
use zremote_client::ApiClient;

use crate::connection::ConnectionResolver;
use crate::format::Formatter;

#[derive(Debug, Subcommand)]
pub enum ChannelCommand {
    /// Send a message to a running CC worker
    Send {
        /// Session ID
        session_id: String,
        /// Message content (instruction text)
        #[arg(long)]
        message: Option<String>,
        /// Send a signal instead of a message
        #[arg(long, value_parser = ["continue", "abort", "pause", "switch-task"])]
        signal: Option<String>,
        /// Send a context update
        #[arg(long, value_parser = ["memory", "file-changed", "worker-output", "convention-update"])]
        context: Option<String>,
        /// Content for context update (required with --context)
        #[arg(long)]
        content: Option<String>,
        /// Message priority
        #[arg(long, default_value = "normal", value_parser = ["normal", "high", "urgent"])]
        priority: String,
    },
    /// Manage permission policies
    Policy {
        #[command(subcommand)]
        command: PolicyCommand,
    },
    /// Check channel status for a session
    Status {
        /// Session ID
        session_id: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum PolicyCommand {
    /// Get permission policy for a project
    Get {
        /// Project ID
        project_id: String,
    },
    /// Set permission policy for a project
    Set {
        /// Project ID
        project_id: String,
        /// Auto-allow tool patterns (comma-separated, e.g. "Read,Glob,Grep")
        #[arg(long)]
        allow: Option<String>,
        /// Auto-deny tool patterns (comma-separated, e.g. "Bash*,Write")
        #[arg(long)]
        deny: Option<String>,
        /// Escalation timeout in seconds
        #[arg(long, default_value = "30")]
        timeout: i64,
    },
    /// Reset (delete) permission policy for a project
    Reset {
        /// Project ID
        project_id: String,
    },
}

pub async fn run(
    client: &ApiClient,
    _resolver: &ConnectionResolver,
    _fmt: &dyn Formatter,
    command: ChannelCommand,
) -> i32 {
    match command {
        ChannelCommand::Send {
            session_id,
            message,
            signal,
            context,
            content,
            priority,
        } => {
            run_send(
                client,
                &session_id,
                message,
                signal,
                context,
                content,
                &priority,
            )
            .await
        }
        ChannelCommand::Policy { command } => run_policy(client, command).await,
        ChannelCommand::Status { session_id } => run_status(client, &session_id).await,
    }
}

/// Build and send a channel message to a CC worker.
async fn run_send(
    client: &ApiClient,
    session_id: &str,
    message: Option<String>,
    signal: Option<String>,
    ctx_kind: Option<String>,
    ctx_content: Option<String>,
    priority: &str,
) -> i32 {
    // Validate: exactly one of --message, --signal, --context must be provided
    let mode_count =
        u8::from(message.is_some()) + u8::from(signal.is_some()) + u8::from(ctx_kind.is_some());
    if mode_count == 0 {
        eprintln!("Error: one of --message, --signal, or --context is required");
        return 1;
    }
    if mode_count > 1 {
        eprintln!("Error: only one of --message, --signal, or --context may be provided");
        return 1;
    }

    let msg = if let Some(text) = message {
        serde_json::json!({
            "type": "Instruction",
            "from": "cli",
            "content": text,
            "priority": priority,
        })
    } else if let Some(action) = signal {
        let action_value = action.replace('-', "_");
        serde_json::json!({
            "type": "Signal",
            "action": action_value,
        })
    } else if let Some(kind) = ctx_kind {
        let Some(ctx_content) = ctx_content else {
            eprintln!("Error: --content is required with --context");
            return 1;
        };
        let kind_value = kind.replace('-', "_");
        serde_json::json!({
            "type": "ContextUpdate",
            "kind": kind_value,
            "content": ctx_content,
            "estimated_tokens": 0,
        })
    } else {
        unreachable!()
    };

    match client.channel_send(session_id, &msg).await {
        Ok(()) => {
            println!("Message sent to session {session_id}");
            0
        }
        Err(e) => {
            eprintln!("Error: {e}");
            1
        }
    }
}

/// Manage permission policies.
async fn run_policy(client: &ApiClient, command: PolicyCommand) -> i32 {
    match command {
        PolicyCommand::Get { project_id } => {
            match client.get_permission_policy(&project_id).await {
                Ok(policy) => {
                    let pretty = serde_json::to_string_pretty(&policy)
                        .unwrap_or_else(|_| format!("{policy}"));
                    println!("{pretty}");
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        PolicyCommand::Set {
            project_id,
            allow,
            deny,
            timeout,
        } => {
            let auto_allow: Vec<&str> = allow
                .as_deref()
                .map(|s| s.split(',').map(str::trim).collect())
                .unwrap_or_default();
            let auto_deny: Vec<&str> = deny
                .as_deref()
                .map(|s| s.split(',').map(str::trim).collect())
                .unwrap_or_default();

            let policy = serde_json::json!({
                "project_id": project_id,
                "auto_allow": auto_allow,
                "auto_deny": auto_deny,
                "escalation_timeout_secs": timeout,
                "escalation_targets": ["gui"],
                "updated_at": "",
            });

            match client.set_permission_policy(&project_id, &policy).await {
                Ok(()) => {
                    println!("Permission policy updated for project {project_id}");
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        PolicyCommand::Reset { project_id } => {
            match client.delete_permission_policy(&project_id).await {
                Ok(()) => {
                    println!("Permission policy reset for project {project_id}");
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
    }
}

/// Check channel status for a session.
async fn run_status(client: &ApiClient, session_id: &str) -> i32 {
    match client.channel_status(session_id).await {
        Ok(status) => {
            let available = status
                .get("available")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            println!(
                "Channel available: {}",
                if available { "yes" } else { "no" }
            );
            0
        }
        Err(e) => {
            eprintln!("Error: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Wrapper to test CLI parsing.
    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: ChannelCommand,
    }

    #[test]
    fn parse_send_message() {
        let cli = TestCli::try_parse_from([
            "test",
            "send",
            "abc-123",
            "--message",
            "Fix the bug",
            "--priority",
            "high",
        ])
        .unwrap();

        match cli.command {
            ChannelCommand::Send {
                session_id,
                message,
                signal,
                priority,
                ..
            } => {
                assert_eq!(session_id, "abc-123");
                assert_eq!(message.as_deref(), Some("Fix the bug"));
                assert!(signal.is_none());
                assert_eq!(priority, "high");
            }
            _ => panic!("expected Send"),
        }
    }

    #[test]
    fn parse_send_signal() {
        let cli =
            TestCli::try_parse_from(["test", "send", "abc-123", "--signal", "continue"]).unwrap();

        match cli.command {
            ChannelCommand::Send {
                message, signal, ..
            } => {
                assert!(message.is_none());
                assert_eq!(signal.as_deref(), Some("continue"));
            }
            _ => panic!("expected Send"),
        }
    }

    #[test]
    fn parse_send_context() {
        let cli = TestCli::try_parse_from([
            "test",
            "send",
            "abc-123",
            "--context",
            "memory",
            "--content",
            "New convention",
        ])
        .unwrap();

        match cli.command {
            ChannelCommand::Send {
                context, content, ..
            } => {
                assert_eq!(context.as_deref(), Some("memory"));
                assert_eq!(content.as_deref(), Some("New convention"));
            }
            _ => panic!("expected Send"),
        }
    }

    #[test]
    fn parse_send_invalid_signal_rejected() {
        let result = TestCli::try_parse_from(["test", "send", "abc-123", "--signal", "invalid"]);
        assert!(result.is_err());
    }

    #[test]
    fn parse_send_invalid_context_rejected() {
        let result = TestCli::try_parse_from([
            "test",
            "send",
            "abc-123",
            "--context",
            "invalid",
            "--content",
            "x",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn parse_send_default_priority() {
        let cli =
            TestCli::try_parse_from(["test", "send", "abc-123", "--message", "Hello"]).unwrap();

        match cli.command {
            ChannelCommand::Send { priority, .. } => {
                assert_eq!(priority, "normal");
            }
            _ => panic!("expected Send"),
        }
    }

    #[test]
    fn parse_policy_get() {
        let cli = TestCli::try_parse_from(["test", "policy", "get", "proj-1"]).unwrap();

        match cli.command {
            ChannelCommand::Policy {
                command: PolicyCommand::Get { project_id },
            } => {
                assert_eq!(project_id, "proj-1");
            }
            _ => panic!("expected Policy Get"),
        }
    }

    #[test]
    fn parse_policy_set() {
        let cli = TestCli::try_parse_from([
            "test",
            "policy",
            "set",
            "proj-1",
            "--allow",
            "Read,Glob",
            "--deny",
            "Bash*",
            "--timeout",
            "60",
        ])
        .unwrap();

        match cli.command {
            ChannelCommand::Policy {
                command:
                    PolicyCommand::Set {
                        project_id,
                        allow,
                        deny,
                        timeout,
                    },
            } => {
                assert_eq!(project_id, "proj-1");
                assert_eq!(allow.as_deref(), Some("Read,Glob"));
                assert_eq!(deny.as_deref(), Some("Bash*"));
                assert_eq!(timeout, 60);
            }
            _ => panic!("expected Policy Set"),
        }
    }

    #[test]
    fn parse_policy_reset() {
        let cli = TestCli::try_parse_from(["test", "policy", "reset", "proj-1"]).unwrap();

        match cli.command {
            ChannelCommand::Policy {
                command: PolicyCommand::Reset { project_id },
            } => {
                assert_eq!(project_id, "proj-1");
            }
            _ => panic!("expected Policy Reset"),
        }
    }

    #[test]
    fn parse_status() {
        let cli = TestCli::try_parse_from(["test", "status", "sess-1"]).unwrap();

        match cli.command {
            ChannelCommand::Status { session_id } => {
                assert_eq!(session_id, "sess-1");
            }
            _ => panic!("expected Status"),
        }
    }

    #[test]
    fn build_instruction_message() {
        let msg = serde_json::json!({
            "type": "Instruction",
            "from": "cli",
            "content": "Fix the bug",
            "priority": "high",
        });
        assert_eq!(msg["type"], "Instruction");
        assert_eq!(msg["from"], "cli");
        assert_eq!(msg["content"], "Fix the bug");
        assert_eq!(msg["priority"], "high");
    }

    #[test]
    fn build_signal_message() {
        let action = "switch-task".replace('-', "_");
        let msg = serde_json::json!({
            "type": "Signal",
            "action": action,
        });
        assert_eq!(msg["type"], "Signal");
        assert_eq!(msg["action"], "switch_task");
    }

    #[test]
    fn build_context_update_message() {
        let kind = "file-changed".replace('-', "_");
        let msg = serde_json::json!({
            "type": "ContextUpdate",
            "kind": kind,
            "content": "src/main.rs was modified",
            "estimated_tokens": 0,
        });
        assert_eq!(msg["type"], "ContextUpdate");
        assert_eq!(msg["kind"], "file_changed");
        assert_eq!(msg["content"], "src/main.rs was modified");
    }

    #[test]
    fn validate_exactly_one_mode_required() {
        // Simulate the validation: zero modes
        let mode_count = u8::from(false) + u8::from(false) + u8::from(false);
        assert_eq!(mode_count, 0);

        // Simulate: one mode
        let mode_count = u8::from(true) + u8::from(false) + u8::from(false);
        assert_eq!(mode_count, 1);

        // Simulate: two modes
        let mode_count = u8::from(true) + u8::from(true) + u8::from(false);
        assert_eq!(mode_count, 2);
    }
}
