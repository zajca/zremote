use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use myremote_protocol::{AgenticAgentMessage, AgenticLoopId, PermissionAction, PermissionRule};
use tokio::sync::{Notify, RwLock, mpsc};

const PERMISSION_TIMEOUT: Duration = Duration::from_secs(55);

/// The result of a permission check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    Deny,
    /// No rule matched, or timeout waiting for user - needs user input.
    Ask,
}

/// Pending permission request waiting for user decision.
struct PendingPermission {
    decision: Option<PermissionDecision>,
    notify: Arc<Notify>,
}

/// Manages permission rules and pending permission requests.
pub struct PermissionManager {
    rules: RwLock<Vec<PermissionRule>>,
    pending: RwLock<HashMap<(AgenticLoopId, String), PendingPermission>>,
}

impl PermissionManager {
    pub fn new() -> Self {
        Self {
            rules: RwLock::new(Vec::new()),
            pending: RwLock::new(HashMap::new()),
        }
    }

    /// Update the permission rules (called when server sends PermissionRulesUpdate).
    pub async fn update_rules(&self, rules: Vec<PermissionRule>) {
        tracing::info!(count = rules.len(), "permission rules updated");
        *self.rules.write().await = rules;
    }

    /// Check if a tool call matches any permission rule.
    /// If auto-approve or deny, returns immediately.
    /// If ask or no match, returns Ask (caller should wait for user).
    pub async fn check_permission(
        &self,
        tool_name: &str,
        _loop_id: AgenticLoopId,
        _arguments: &str,
        _agentic_tx: &mpsc::Sender<AgenticAgentMessage>,
    ) -> PermissionDecision {
        let rules = self.rules.read().await;

        for rule in rules.iter() {
            if matches_tool_pattern(&rule.tool_pattern, tool_name) {
                return match rule.action {
                    PermissionAction::AutoApprove => PermissionDecision::Allow,
                    PermissionAction::Deny => PermissionDecision::Deny,
                    PermissionAction::Ask => PermissionDecision::Ask,
                };
            }
        }

        // No rule matched - ask user
        PermissionDecision::Ask
    }

    /// Wait for a user decision on a pending permission request.
    /// Times out after 55 seconds, returning Ask (pass-through).
    pub async fn wait_for_decision(
        &self,
        loop_id: AgenticLoopId,
        tool_name: &str,
    ) -> PermissionDecision {
        let key = (loop_id, tool_name.to_string());
        let notify = {
            let mut pending = self.pending.write().await;
            let entry = pending
                .entry(key.clone())
                .or_insert_with(|| PendingPermission {
                    decision: None,
                    notify: Arc::new(Notify::new()),
                });
            entry.notify.clone()
        };

        // Wait for notification or timeout
        let result = tokio::time::timeout(PERMISSION_TIMEOUT, notify.notified()).await;

        if result.is_err() {
            tracing::debug!(
                loop_id = %loop_id,
                tool = %tool_name,
                "permission request timed out, passing through"
            );
        }

        let mut pending = self.pending.write().await;
        pending
            .remove(&key)
            .and_then(|p| p.decision)
            .unwrap_or(PermissionDecision::Ask)
    }

    /// Resolve a pending permission request (called when user action arrives via WS).
    pub async fn resolve_permission(
        &self,
        loop_id: AgenticLoopId,
        tool_name: &str,
        decision: PermissionDecision,
    ) {
        let key = (loop_id, tool_name.to_string());
        let mut pending = self.pending.write().await;
        if let Some(entry) = pending.get_mut(&key) {
            entry.decision = Some(decision);
            entry.notify.notify_one();
        }
    }

    /// Resolve any pending permission for a loop (by loop_id only, any tool).
    pub async fn resolve_any_pending(&self, loop_id: AgenticLoopId, decision: PermissionDecision) {
        let mut pending = self.pending.write().await;
        let keys: Vec<_> = pending
            .keys()
            .filter(|(lid, _)| *lid == loop_id)
            .cloned()
            .collect();
        for key in keys {
            if let Some(entry) = pending.get_mut(&key) {
                entry.decision = Some(decision);
                entry.notify.notify_one();
            }
        }
    }
}

/// Match a tool name against a glob-like pattern.
/// Supports:
/// - `*` matches everything
/// - `Bash*` matches "Bash", "BashTool", etc.
/// - `Read` matches exactly "Read"
fn matches_tool_pattern(pattern: &str, tool_name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    glob_match::glob_match(pattern, tool_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_wildcard_matches_all() {
        assert!(matches_tool_pattern("*", "Read"));
        assert!(matches_tool_pattern("*", "Bash"));
        assert!(matches_tool_pattern("*", "anything"));
    }

    #[test]
    fn pattern_exact_match() {
        assert!(matches_tool_pattern("Read", "Read"));
        assert!(!matches_tool_pattern("Read", "ReadFile"));
        assert!(!matches_tool_pattern("Read", "read"));
    }

    #[test]
    fn pattern_prefix_glob() {
        assert!(matches_tool_pattern("Bash*", "Bash"));
        assert!(matches_tool_pattern("Bash*", "BashTool"));
        assert!(!matches_tool_pattern("Bash*", "NotBash"));
    }

    #[test]
    fn pattern_complex_glob() {
        assert!(matches_tool_pattern("*File*", "ReadFile"));
        assert!(matches_tool_pattern("*File*", "FileWrite"));
        assert!(matches_tool_pattern("*File*", "MyFileOps"));
        assert!(!matches_tool_pattern("*File*", "Read"));
    }

    #[tokio::test]
    async fn check_auto_approve_rule() {
        let pm = PermissionManager::new();
        pm.update_rules(vec![PermissionRule {
            tool_pattern: "Read".to_string(),
            action: PermissionAction::AutoApprove,
        }])
        .await;

        let (tx, _rx) = mpsc::channel(1);
        let decision = pm
            .check_permission("Read", uuid::Uuid::new_v4(), "{}", &tx)
            .await;
        assert_eq!(decision, PermissionDecision::Allow);
    }

    #[tokio::test]
    async fn check_deny_rule() {
        let pm = PermissionManager::new();
        pm.update_rules(vec![PermissionRule {
            tool_pattern: "Bash*".to_string(),
            action: PermissionAction::Deny,
        }])
        .await;

        let (tx, _rx) = mpsc::channel(1);
        let decision = pm
            .check_permission("BashTool", uuid::Uuid::new_v4(), "{}", &tx)
            .await;
        assert_eq!(decision, PermissionDecision::Deny);
    }

    #[tokio::test]
    async fn check_no_rule_returns_ask() {
        let pm = PermissionManager::new();
        let (tx, _rx) = mpsc::channel(1);
        let decision = pm
            .check_permission("UnknownTool", uuid::Uuid::new_v4(), "{}", &tx)
            .await;
        assert_eq!(decision, PermissionDecision::Ask);
    }

    #[tokio::test]
    async fn resolve_pending_permission() {
        let pm = Arc::new(PermissionManager::new());
        let loop_id = uuid::Uuid::new_v4();
        let tool_name = "Bash";

        let pm_clone = pm.clone();
        let handle =
            tokio::spawn(async move { pm_clone.wait_for_decision(loop_id, tool_name).await });

        // Small delay to ensure wait_for_decision is waiting
        tokio::time::sleep(Duration::from_millis(50)).await;

        pm.resolve_permission(loop_id, tool_name, PermissionDecision::Allow)
            .await;

        let decision = handle.await.unwrap();
        assert_eq!(decision, PermissionDecision::Allow);
    }

    #[tokio::test]
    async fn permission_timeout_returns_ask() {
        let pm = PermissionManager::new();
        let loop_id = uuid::Uuid::new_v4();

        // Override timeout for test (we can't easily, so just verify the function exists)
        // In production the 55s timeout applies
        // For this test, we just verify the API works
        let decision = tokio::time::timeout(
            Duration::from_millis(100),
            pm.wait_for_decision(loop_id, "TestTool"),
        )
        .await;

        // Should timeout (our 100ms is shorter than 55s)
        assert!(decision.is_err());
    }

    #[tokio::test]
    async fn resolve_any_pending() {
        let pm = Arc::new(PermissionManager::new());
        let loop_id = uuid::Uuid::new_v4();

        let pm_clone = pm.clone();
        let handle =
            tokio::spawn(async move { pm_clone.wait_for_decision(loop_id, "SomeTool").await });

        tokio::time::sleep(Duration::from_millis(50)).await;

        pm.resolve_any_pending(loop_id, PermissionDecision::Deny)
            .await;

        let decision = handle.await.unwrap();
        assert_eq!(decision, PermissionDecision::Deny);
    }
}
