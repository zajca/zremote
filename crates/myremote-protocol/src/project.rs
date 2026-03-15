use serde::{Deserialize, Serialize};

/// Information about a discovered project on a remote host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectInfo {
    pub path: String,
    pub name: String,
    pub has_claude_config: bool,
    pub project_type: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_info_roundtrip() {
        let info = ProjectInfo {
            path: "/home/user/myproject".to_string(),
            name: "myproject".to_string(),
            has_claude_config: true,
            project_type: "rust".to_string(),
        };
        let json = serde_json::to_string(&info).expect("serialize");
        let parsed: ProjectInfo = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(info, parsed);
    }

    #[test]
    fn project_info_without_claude_config() {
        let info = ProjectInfo {
            path: "/home/user/webapp".to_string(),
            name: "webapp".to_string(),
            has_claude_config: false,
            project_type: "node".to_string(),
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["has_claude_config"], false);
        assert_eq!(json["project_type"], "node");
    }
}
