pub mod aider;
pub mod claude;
pub mod codex;
pub mod gemini;

/// Trait for provider-specific output analysis.
/// Each AI coding agent (Claude Code, Aider, Codex, Gemini CLI) implements this
/// to parse its terminal output format.
pub trait ProviderAdapter: Send + Sync {
    /// Try to detect this provider's agent from a line of output.
    fn detect_agent(&self, line: &str) -> Option<AgentInfo>;

    /// Analyze a single line of ANSI-stripped output.
    fn analyze_line(&self, line: &str) -> LineAnalysis;

    /// Check if this line is a command prompt (agent idle).
    fn is_prompt(&self, line: &str) -> bool;

    /// Provider identifier for logging and metrics.
    fn name(&self) -> &str;
}

#[derive(Debug, Clone, Default)]
pub struct LineAnalysis {
    pub token_update: Option<TokenUpdate>,
    pub tool_call: Option<ToolCallEvent>,
    pub phase_hint: Option<PhaseHint>,
    pub file_touched: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TokenUpdate {
    pub provider: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: Option<f64>,
    pub model: String,
    /// true = replace totals, false = add delta
    pub is_cumulative: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseHint {
    /// Agent/shell idle
    PromptDetected,
    /// Agent began processing
    WorkStarted,
    /// Agent asking Y/n or permission prompt
    InputNeeded,
}

#[derive(Debug, Clone)]
pub struct ToolCallEvent {
    pub tool: String,
    pub args: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone)]
pub struct AgentInfo {
    /// e.g. "Claude Code", "Aider", "Codex", "Gemini CLI"
    pub name: String,
    /// e.g. "anthropic", "openai", "google"
    pub provider: String,
    pub model: Option<String>,
    /// 0.0-1.0
    pub confidence: f32,
}

pub struct ProviderRegistry {
    pub(crate) adapters: Vec<Box<dyn ProviderAdapter>>,
}

impl ProviderRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            adapters: Vec::new(),
        }
    }

    /// Try all adapters, return highest confidence match.
    #[must_use]
    pub fn detect_agent(&self, line: &str) -> Option<(usize, AgentInfo)> {
        self.adapters
            .iter()
            .enumerate()
            .filter_map(|(i, a)| a.detect_agent(line).map(|info| (i, info)))
            .max_by(|a, b| {
                a.1.confidence
                    .partial_cmp(&b.1.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockAdapter {
        adapter_name: String,
        agent_info: Option<AgentInfo>,
    }

    impl MockAdapter {
        fn new(name: &str, info: Option<AgentInfo>) -> Self {
            Self {
                adapter_name: name.to_string(),
                agent_info: info,
            }
        }
    }

    impl ProviderAdapter for MockAdapter {
        fn detect_agent(&self, _line: &str) -> Option<AgentInfo> {
            self.agent_info.clone()
        }

        fn analyze_line(&self, _line: &str) -> LineAnalysis {
            LineAnalysis::default()
        }

        fn is_prompt(&self, _line: &str) -> bool {
            false
        }

        fn name(&self) -> &str {
            &self.adapter_name
        }
    }

    #[test]
    fn registry_new_creates_empty() {
        let registry = ProviderRegistry::new();
        assert!(registry.adapters.is_empty());
    }

    #[test]
    fn registry_detect_agent_returns_none_when_empty() {
        let registry = ProviderRegistry::new();
        assert!(registry.detect_agent("some output line").is_none());
    }

    #[test]
    fn line_analysis_default_has_all_none() {
        let analysis = LineAnalysis::default();
        assert!(analysis.token_update.is_none());
        assert!(analysis.tool_call.is_none());
        assert!(analysis.phase_hint.is_none());
        assert!(analysis.file_touched.is_none());
    }

    #[test]
    fn registry_returns_highest_confidence_match() {
        let mut registry = ProviderRegistry::new();

        registry.adapters.push(Box::new(MockAdapter::new(
            "low",
            Some(AgentInfo {
                name: "Low Confidence".to_string(),
                provider: "test".to_string(),
                model: None,
                confidence: 0.3,
            }),
        )));

        registry.adapters.push(Box::new(MockAdapter::new(
            "high",
            Some(AgentInfo {
                name: "High Confidence".to_string(),
                provider: "test".to_string(),
                model: None,
                confidence: 0.9,
            }),
        )));

        registry.adapters.push(Box::new(MockAdapter::new(
            "medium",
            Some(AgentInfo {
                name: "Medium Confidence".to_string(),
                provider: "test".to_string(),
                model: None,
                confidence: 0.6,
            }),
        )));

        let result = registry.detect_agent("any line");
        assert!(result.is_some());
        let (idx, info) = result.unwrap();
        assert_eq!(idx, 1);
        assert_eq!(info.name, "High Confidence");
        assert!((info.confidence - 0.9).abs() < f32::EPSILON);
    }

    #[test]
    fn registry_returns_none_when_no_adapter_matches() {
        let mut registry = ProviderRegistry::new();
        registry
            .adapters
            .push(Box::new(MockAdapter::new("none", None)));
        assert!(registry.detect_agent("any line").is_none());
    }
}
