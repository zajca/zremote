use sysinfo::{Pid, System};

/// A detected agentic tool process.
#[derive(Debug, Clone)]
pub struct DetectedTool {
    pub tool_name: String,
    pub pid: u32,
}

/// Known agentic tool signatures matched against process names.
const KNOWN_TOOLS: &[(&str, &str)] = &[
    ("claude", "claude-code"),
    ("codex", "codex"),
    ("gemini", "gemini-cli"),
    ("aider", "aider"),
];

/// Inspect child processes of the given shell PID and detect known agentic tools.
///
/// Uses `sysinfo::System` to enumerate processes and check if any child
/// (or descendant) of `parent_pid` matches a known tool signature.
pub fn detect_agentic_tool(parent_pid: u32, system: &System) -> Option<DetectedTool> {
    let parent = Pid::from_u32(parent_pid);

    // Find direct and indirect children matching known tool names.
    // We do a breadth-first search through the process tree.
    let mut queue = vec![parent];
    let mut visited = std::collections::HashSet::new();

    while let Some(current_pid) = queue.pop() {
        if !visited.insert(current_pid) {
            continue;
        }

        for (pid, process) in system.processes() {
            if process.parent() == Some(current_pid) {
                let name = process.name().to_string_lossy().to_lowercase();
                let cmd_line = process
                    .cmd()
                    .iter()
                    .map(|s| s.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase();
                for &(signature, tool_name) in KNOWN_TOOLS {
                    if name.contains(signature) || cmd_line.contains(signature) {
                        return Some(DetectedTool {
                            tool_name: tool_name.to_string(),
                            pid: pid.as_u32(),
                        });
                    }
                }
                queue.push(*pid);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_detection_for_nonexistent_pid() {
        let system = System::new();
        let result = detect_agentic_tool(999_999_999, &system);
        assert!(result.is_none());
    }

    #[test]
    fn known_tools_list_is_not_empty() {
        assert!(!KNOWN_TOOLS.is_empty());
    }

    #[test]
    fn detected_tool_clone_and_debug() {
        let tool = DetectedTool {
            tool_name: "claude-code".to_string(),
            pid: 1234,
        };
        let cloned = tool.clone();
        assert_eq!(cloned.tool_name, "claude-code");
        assert_eq!(cloned.pid, 1234);
        let debug = format!("{tool:?}");
        assert!(debug.contains("claude-code"));
    }
}
