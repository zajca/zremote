use super::handler::HookPayload;
use super::mapper::SessionMapper;
use crate::knowledge::context_delivery::DeliveryCoordinator;

/// Builds context strings for CC hook responses.
///
/// Integrates with Phase 6 `DeliveryCoordinator` to deliver pending
/// context nudges via `additionalContext` instead of PTY injection.
#[derive(Clone)]
pub struct HookContextProvider {
    mapper: SessionMapper,
}

impl HookContextProvider {
    pub fn new(mapper: SessionMapper) -> Self {
        Self { mapper }
    }

    /// Build context for a `PreToolUse` hook response.
    ///
    /// Uses [`SessionMapper::try_resolve`] (single attempt, no retry) to avoid
    /// blocking tool calls. If a pending context nudge exists in the
    /// `DeliveryCoordinator`, delivers it here (preferred over PTY injection
    /// for Claude Code sessions).
    pub async fn build_pre_tool_context(
        &self,
        payload: &HookPayload,
        delivery_coordinator: &mut DeliveryCoordinator,
    ) -> Option<String> {
        let mapped = self.mapper.try_resolve(&payload.session_id).await?;

        // Check for pending context nudge from Phase 6 delivery system.
        // Delivering via hook additionalContext is preferred over PTY injection
        // because it is CC-native, zero-latency, and confirmed.
        if let Some(content) = delivery_coordinator.on_phase_idle(&mapped.session_id) {
            tracing::info!(
                cc_session = %payload.session_id,
                content_len = content.len(),
                "delivering pending context nudge via hook additionalContext"
            );
            return Some(content);
        }

        // Basic loop info when no pending nudge.
        Some(format!(
            "[ZRemote] Loop: {} | Session: {}",
            mapped.loop_id, mapped.session_id
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn test_payload(cc_session: &str) -> HookPayload {
        HookPayload {
            session_id: cc_session.to_string(),
            hook_event_name: "PreToolUse".to_string(),
            transcript_path: None,
            cwd: None,
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_response: None,
            message: None,
            stop_hook_active: None,
            prompt: None,
            source: None,
            mcp_server_name: None,
            mode: None,
            elicitation_id: None,
            requested_schema: None,
            permission_mode: None,
        }
    }

    #[tokio::test]
    async fn returns_none_for_unknown_session() {
        let mapper = SessionMapper::new();
        let provider = HookContextProvider::new(mapper);
        let mut coordinator = DeliveryCoordinator::new();
        let payload = test_payload("unknown-session");

        let result = provider
            .build_pre_tool_context(&payload, &mut coordinator)
            .await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn returns_basic_info_for_mapped_session() {
        let mapper = SessionMapper::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        mapper.register_loop(session_id, loop_id).await;

        // Resolve to auto-register CC session mapping
        let _ = mapper.resolve_loop_id("cc-test", None).await;

        let provider = HookContextProvider::new(mapper);
        let mut coordinator = DeliveryCoordinator::new();
        let payload = test_payload("cc-test");

        let result = provider
            .build_pre_tool_context(&payload, &mut coordinator)
            .await;
        assert!(result.is_some());
        let ctx = result.unwrap();
        assert!(ctx.contains(&loop_id.to_string()));
        assert!(ctx.contains(&session_id.to_string()));
    }

    #[tokio::test]
    async fn delivers_pending_nudge_from_coordinator() {
        use crate::knowledge::context_delivery::{
            ContextAssembler, ContextTrigger, SessionContext,
        };

        let mapper = SessionMapper::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        mapper.register_loop(session_id, loop_id).await;
        let _ = mapper.resolve_loop_id("cc-nudge", None).await;

        let provider = HookContextProvider::new(mapper);
        let mut coordinator = DeliveryCoordinator::new();

        // Queue a pending nudge
        let context = ContextAssembler::assemble(
            "test-project",
            "/tmp/test",
            "rust",
            None,
            &[],
            &[],
            &["use snake_case".to_string()],
            ContextTrigger::ManualPush,
        );
        coordinator.on_context_changed(session_id, context);

        let payload = test_payload("cc-nudge");
        let result = provider
            .build_pre_tool_context(&payload, &mut coordinator)
            .await;

        assert!(result.is_some());
        let ctx = result.unwrap();
        // Should contain the rendered context from the nudge, not basic info
        assert!(ctx.contains("ZRemote Context Update"));
        assert!(ctx.contains("test-project"));

        // Nudge should be consumed (second call returns basic info)
        assert!(!coordinator.has_pending(&session_id));
    }
}
