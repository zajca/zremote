use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use chrono::Utc;
use indexmap::IndexSet;

use super::adapters::aider::AiderAdapter;
use super::adapters::claude::ClaudeAdapter;
use super::adapters::codex::CodexAdapter;
use super::adapters::gemini::GeminiAdapter;
use super::adapters::{
    AgentInfo, LineAnalysis, PhaseHint, ProviderAdapter, ProviderRegistry, TokenUpdate,
    ToolCallEvent,
};
use super::patterns;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalyzerPhase {
    Unknown,
    ShellReady,
    Idle,
    Busy,
    NeedsInput,
}

#[derive(Debug, Clone)]
pub enum AnalyzerEvent {
    AgentDetected {
        name: String,
        provider: String,
        model: Option<String>,
        confidence: f32,
    },
    PhaseChanged(AnalyzerPhase),
    TokenUpdate {
        provider: String,
        input_tokens: u64,
        output_tokens: u64,
        cost_usd: Option<f64>,
    },
    ToolCall {
        tool: String,
        args: String,
    },
    CwdChanged(String),
}

#[derive(Debug, Clone)]
pub struct ProviderTokens {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: Option<f64>,
    pub model: String,
    pub last_updated: chrono::DateTime<chrono::Utc>,
    pub update_count: u32,
}

pub struct AnalyzerMetrics {
    pub line_count: u64,
    pub detected_agent: Option<AgentInfo>,
    pub current_phase: AnalyzerPhase,
    pub token_usage: HashMap<String, ProviderTokens>,
    pub token_history: Vec<(u64, u64)>,
    pub tool_calls: Vec<ToolCallEvent>,
    pub tool_call_counts: HashMap<String, u32>,
    pub files_touched: Vec<String>,
    pub current_cwd: Option<String>,
    pub latency_p50_ms: Option<f64>,
    pub latency_p95_ms: Option<f64>,
}

// ---------------------------------------------------------------------------
// Caps
// ---------------------------------------------------------------------------

const TOKEN_HISTORY_CAP: usize = 30;
const TOOL_CALLS_CAP: usize = 100;
const FILES_TOUCHED_CAP: usize = 50;
const LATENCY_SAMPLES_CAP: usize = 50;
const STRIPPED_BUFFER_CAP: usize = 16 * 1024;
const METRICS_TOOL_CALLS_LIMIT: usize = 20;

const LATENCY_MIN_MS: f64 = 50.0;
const LATENCY_MAX_MS: f64 = 120_000.0;

// ---------------------------------------------------------------------------
// OutputAnalyzer
// ---------------------------------------------------------------------------

pub struct OutputAnalyzer {
    registry: ProviderRegistry,
    active_adapter_idx: Option<usize>,

    detected_agent: Option<AgentInfo>,

    current_phase: AnalyzerPhase,
    is_busy: bool,

    token_usage: HashMap<String, ProviderTokens>,
    token_history: VecDeque<(u64, u64)>,

    tool_calls: VecDeque<ToolCallEvent>,
    tool_call_counts: HashMap<String, u32>,

    files_touched: IndexSet<String>,

    line_buffer: String,
    stripped_buffer: String,

    current_cwd: Option<String>,

    last_input_at: Option<Instant>,
    latency_samples: VecDeque<f64>,

    last_output_at: Option<Instant>,

    line_count: u64,
}

impl OutputAnalyzer {
    #[must_use]
    pub fn new() -> Self {
        let mut registry = ProviderRegistry::new();
        registry.adapters = vec![
            Box::new(ClaudeAdapter),
            Box::new(AiderAdapter),
            Box::new(CodexAdapter),
            Box::new(GeminiAdapter),
        ];

        Self {
            registry,
            active_adapter_idx: None,
            detected_agent: None,
            current_phase: AnalyzerPhase::Unknown,
            is_busy: false,
            token_usage: HashMap::new(),
            token_history: VecDeque::with_capacity(TOKEN_HISTORY_CAP),
            tool_calls: VecDeque::with_capacity(TOOL_CALLS_CAP),
            tool_call_counts: HashMap::new(),
            files_touched: IndexSet::new(),
            line_buffer: String::new(),
            stripped_buffer: String::new(),
            current_cwd: None,
            last_input_at: None,
            latency_samples: VecDeque::with_capacity(LATENCY_SAMPLES_CAP),
            last_output_at: None,
            line_count: 0,
        }
    }

    /// Main entry point: process raw PTY output bytes.
    pub fn process_output(&mut self, raw: &[u8]) -> Vec<AnalyzerEvent> {
        let mut events = Vec::new();

        // 1. Latency tracking
        if let Some(input_at) = self.last_input_at.take() {
            let elapsed_ms = input_at.elapsed().as_secs_f64() * 1000.0;
            if (LATENCY_MIN_MS..=LATENCY_MAX_MS).contains(&elapsed_ms) {
                if self.latency_samples.len() >= LATENCY_SAMPLES_CAP {
                    self.latency_samples.pop_front();
                }
                self.latency_samples.push_back(elapsed_ms);
            }
        }

        // 2. OSC 7 CWD — check raw bytes before stripping ANSI
        let raw_str = String::from_utf8_lossy(raw);
        if let Some(caps) = patterns::OSC7_RE.captures(&raw_str) {
            let path = patterns::percent_decode(&caps[1]);
            if self.current_cwd.as_deref() != Some(&path) {
                self.current_cwd = Some(path.clone());
                events.push(AnalyzerEvent::CwdChanged(path));
            }
        }

        // 3. Strip ANSI escapes
        let stripped_bytes = strip_ansi_escapes::strip(raw_str.as_bytes());
        let stripped = String::from_utf8_lossy(&stripped_bytes);

        // 4. Busy detection — any visible chars means output is happening
        let has_visible = stripped.chars().any(|c| !c.is_whitespace());
        if has_visible {
            self.is_busy = true;
            self.last_output_at = Some(Instant::now());
        }

        // 5. Cross-chunk line handling
        let mut text = String::with_capacity(self.line_buffer.len() + stripped.len());
        text.push_str(&self.line_buffer);
        text.push_str(&stripped);
        self.line_buffer.clear();

        let has_trailing_newline = text.ends_with('\n');
        let mut lines: Vec<&str> = text.split('\n').collect();

        // Save incomplete last line back to buffer
        if !has_trailing_newline && let Some(last) = lines.pop() {
            self.line_buffer.push_str(last);
        }

        // 6. Process each complete line
        for line in &lines {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            self.line_count += 1;

            // Agent detection
            if self.detected_agent.is_none()
                && let Some((idx, info)) = self.registry.detect_agent(trimmed)
            {
                self.active_adapter_idx = Some(idx);
                events.push(AnalyzerEvent::AgentDetected {
                    name: info.name.clone(),
                    provider: info.provider.clone(),
                    model: info.model.clone(),
                    confidence: info.confidence,
                });
                self.detected_agent = Some(info);
            }

            // Model extraction — try to fill in unknown model
            if let Some(ref mut agent) = self.detected_agent
                && (agent.model.is_none() || agent.model.as_deref() == Some("unknown"))
                && let Some(model) = patterns::extract_model_name(trimmed)
            {
                agent.model = Some(model);
            }

            // Line analysis
            let analysis = if let Some(idx) = self.active_adapter_idx {
                let a = self.registry.adapters[idx].analyze_line(trimmed);
                // If adapter didn't set phase_hint, check its prompt detection
                if a.phase_hint.is_none() && self.registry.adapters[idx].is_prompt(trimmed) {
                    let mut patched = a;
                    patched.phase_hint = Some(PhaseHint::PromptDetected);
                    patched
                } else {
                    a
                }
            } else {
                self.generic_analysis(trimmed, line)
            };

            self.apply_analysis(analysis, &mut events);

            // Append to stripped_buffer (cap at 16KB)
            self.stripped_buffer.push_str(trimmed);
            self.stripped_buffer.push('\n');
            if self.stripped_buffer.len() > STRIPPED_BUFFER_CAP {
                let drain = self.stripped_buffer.len() - STRIPPED_BUFFER_CAP;
                // Find a newline boundary to drain cleanly
                let cut = self.stripped_buffer[drain..]
                    .find('\n')
                    .map_or(drain, |pos| drain + pos + 1);
                self.stripped_buffer.drain(..cut);
            }
        }

        events
    }

    /// Mark that user input was sent (for latency tracking).
    pub fn mark_input_sent(&mut self) {
        self.last_input_at = Some(Instant::now());
    }

    /// Called when no PTY output for ~3s. Checks if we should transition phase.
    pub fn check_silence(&mut self) -> Option<AnalyzerEvent> {
        if !self.is_busy {
            return None;
        }

        self.is_busy = false;

        // Check last 5 lines of stripped_buffer for prompts
        let last_lines: Vec<&str> = self.stripped_buffer.lines().rev().take(5).collect();

        let prompt_found = last_lines.iter().any(|line| {
            if let Some(idx) = self.active_adapter_idx {
                self.registry.adapters[idx].is_prompt(line)
            } else {
                patterns::is_shell_prompt(line)
            }
        });

        let new_phase = if prompt_found {
            AnalyzerPhase::Idle
        } else if self.detected_agent.is_some() {
            AnalyzerPhase::NeedsInput
        } else {
            AnalyzerPhase::Idle
        };

        if new_phase == self.current_phase {
            None
        } else {
            self.current_phase = new_phase;
            Some(AnalyzerEvent::PhaseChanged(new_phase))
        }
    }

    /// Returns the last time output was received.
    #[must_use]
    pub fn last_output_at(&self) -> Option<Instant> {
        self.last_output_at
    }

    /// Snapshot export of current metrics.
    #[must_use]
    pub fn metrics(&self) -> AnalyzerMetrics {
        AnalyzerMetrics {
            line_count: self.line_count,
            detected_agent: self.detected_agent.clone(),
            current_phase: self.current_phase,
            token_usage: self.token_usage.clone(),
            token_history: self.token_history.iter().copied().collect(),
            tool_calls: self
                .tool_calls
                .iter()
                .rev()
                .take(METRICS_TOOL_CALLS_LIMIT)
                .rev()
                .cloned()
                .collect(),
            tool_call_counts: self.tool_call_counts.clone(),
            files_touched: self.files_touched.iter().cloned().collect(),
            current_cwd: self.current_cwd.clone(),
            latency_p50_ms: percentile(&self.latency_samples, 50.0),
            latency_p95_ms: percentile(&self.latency_samples, 95.0),
        }
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    fn generic_analysis(&self, line: &str, untrimmed: &str) -> LineAnalysis {
        let mut analysis = LineAnalysis::default();

        // Check both trimmed and untrimmed for shell prompts (e.g. "$ " needs trailing space)
        if patterns::is_shell_prompt(line) || patterns::is_shell_prompt(untrimmed) {
            analysis.phase_hint = Some(PhaseHint::PromptDetected);
        } else if patterns::is_input_needed(line) {
            analysis.phase_hint = Some(PhaseHint::InputNeeded);
        }

        analysis
    }

    fn apply_analysis(&mut self, analysis: LineAnalysis, events: &mut Vec<AnalyzerEvent>) {
        // Token update
        if let Some(tu) = analysis.token_update {
            let provider = tu.provider.clone();
            let input = tu.input_tokens;
            let output = tu.output_tokens;
            let cost = self.apply_token_update(tu);
            events.push(AnalyzerEvent::TokenUpdate {
                provider,
                input_tokens: input,
                output_tokens: output,
                cost_usd: cost,
            });
        }

        // Tool call
        if let Some(tc) = analysis.tool_call {
            let tool = tc.tool.clone();
            let args = tc.args.clone();

            if self.tool_calls.len() >= TOOL_CALLS_CAP {
                self.tool_calls.pop_front();
            }
            self.tool_calls.push_back(tc);

            *self.tool_call_counts.entry(tool.clone()).or_insert(0) += 1;

            events.push(AnalyzerEvent::ToolCall { tool, args });
        }

        // Phase hint
        if let Some(hint) = analysis.phase_hint {
            let new_phase = match hint {
                PhaseHint::PromptDetected => {
                    if self.detected_agent.is_some() {
                        AnalyzerPhase::Idle
                    } else {
                        AnalyzerPhase::ShellReady
                    }
                }
                PhaseHint::WorkStarted => AnalyzerPhase::Busy,
                PhaseHint::InputNeeded => AnalyzerPhase::NeedsInput,
            };

            if new_phase != self.current_phase {
                self.current_phase = new_phase;
                events.push(AnalyzerEvent::PhaseChanged(new_phase));
            }
        }

        // File touched
        if let Some(file) = analysis.file_touched {
            if self.files_touched.len() >= FILES_TOUCHED_CAP {
                self.files_touched.shift_remove_index(0);
            }
            self.files_touched.insert(file);
        }
    }

    fn apply_token_update(&mut self, tu: TokenUpdate) -> Option<f64> {
        let entry = self
            .token_usage
            .entry(tu.provider.clone())
            .or_insert_with(|| ProviderTokens {
                input_tokens: 0,
                output_tokens: 0,
                cost_usd: None,
                model: tu.model.clone(),
                last_updated: Utc::now(),
                update_count: 0,
            });

        if tu.is_cumulative {
            entry.input_tokens = tu.input_tokens;
            entry.output_tokens = tu.output_tokens;
        } else {
            entry.input_tokens += tu.input_tokens;
            entry.output_tokens += tu.output_tokens;
        }

        // Update model if provided and non-empty
        if !tu.model.is_empty() {
            entry.model = tu.model.clone();
        }

        // Cost: use reported cost, or estimate
        let cost = tu.cost_usd.or_else(|| {
            let model = if entry.model.is_empty() {
                "unknown"
            } else {
                &entry.model
            };
            Some(patterns::estimate_cost(
                &tu.provider,
                model,
                entry.input_tokens,
                entry.output_tokens,
            ))
        });
        entry.cost_usd = cost;

        entry.update_count += 1;
        entry.last_updated = Utc::now();

        // Record sparkline sample: sum all providers
        let (total_in, total_out) = self.token_usage.values().fold((0u64, 0u64), |(i, o), pt| {
            (i + pt.input_tokens, o + pt.output_tokens)
        });
        if self.token_history.len() >= TOKEN_HISTORY_CAP {
            self.token_history.pop_front();
        }
        self.token_history.push_back((total_in, total_out));

        cost
    }
}

impl Default for OutputAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn percentile(samples: &VecDeque<f64>, p: f64) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }

    let mut sorted: Vec<f64> = samples.iter().copied().collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    #[allow(clippy::cast_precision_loss, clippy::cast_sign_loss)]
    let idx = (p / 100.0 * (sorted.len() as f64 - 1.0)).round() as usize;
    let idx = idx.min(sorted.len() - 1);
    Some(sorted[idx])
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_analyzer() -> OutputAnalyzer {
        OutputAnalyzer::new()
    }

    // -- Test 1: Claude Code full sequence --

    #[test]
    fn claude_full_sequence() {
        let mut analyzer = make_analyzer();

        // Banner → AgentDetected
        let events = analyzer.process_output(b"Welcome to Claude Code!\n");
        assert!(events.iter().any(|e| matches!(e,
            AnalyzerEvent::AgentDetected { name, provider, .. }
            if name == "Claude Code" && provider == "anthropic"
        )));

        // Tool calls → ToolCall + PhaseChanged(Busy)
        let events = analyzer.process_output("● Read src/main.rs\n".as_bytes());
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AnalyzerEvent::ToolCall { tool, .. } if tool == "Read"))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AnalyzerEvent::PhaseChanged(AnalyzerPhase::Busy)))
        );

        // Cost line → TokenUpdate
        let events = analyzer.process_output(b"input: 12.5K tokens | output: 1,234 tokens\n");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AnalyzerEvent::TokenUpdate { .. }))
        );

        // Prompt → PhaseChanged(Idle)
        let events = analyzer.process_output(b">\n");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AnalyzerEvent::PhaseChanged(AnalyzerPhase::Idle)))
        );
    }

    // -- Test 2: Aider session --

    #[test]
    fn aider_session() {
        let mut analyzer = make_analyzer();

        // Version → AgentDetected
        let events = analyzer.process_output(b"Aider v0.86.0\n");
        assert!(events.iter().any(|e| matches!(e,
            AnalyzerEvent::AgentDetected { name, .. } if name.contains("Aider")
        )));

        // Edit → ToolCall
        let events = analyzer.process_output(b"Applied edit to src/main.py\n");
        assert!(events.iter().any(|e| matches!(e,
            AnalyzerEvent::ToolCall { tool, args } if tool == "edit" && args == "src/main.py"
        )));

        // Token line → TokenUpdate
        let events =
            analyzer.process_output(b"Tokens: 12.5k sent, 1.2k cache_write, 500 received\n");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AnalyzerEvent::TokenUpdate { .. }))
        );

        // Prompt → PhaseChanged(Idle)
        let events = analyzer.process_output(b"> \n");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AnalyzerEvent::PhaseChanged(AnalyzerPhase::Idle)))
        );
    }

    // -- Test 3: Unknown agent → ShellReady --

    #[test]
    fn unknown_agent_shell_ready() {
        let mut analyzer = make_analyzer();

        let events = analyzer.process_output(b"$ \n");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AnalyzerEvent::PhaseChanged(AnalyzerPhase::ShellReady)))
        );
        assert!(analyzer.detected_agent.is_none());
    }

    // -- Test 4: Cross-chunk splitting --

    #[test]
    fn cross_chunk_splitting() {
        let mut analyzer = make_analyzer();

        // First detection so we have an adapter active
        analyzer.process_output(b"Welcome to Claude Code!\n");

        // Split a token line across two calls
        let events1 = analyzer.process_output(b"input: 5K tokens");
        assert!(
            events1
                .iter()
                .all(|e| !matches!(e, AnalyzerEvent::TokenUpdate { .. }))
        );

        let events2 = analyzer.process_output(b" | output: 1K tokens\n");
        assert!(
            events2
                .iter()
                .any(|e| matches!(e, AnalyzerEvent::TokenUpdate { .. }))
        );
    }

    // -- Test 5: Phase transitions --

    #[test]
    fn phase_transitions() {
        let mut analyzer = make_analyzer();
        assert_eq!(analyzer.current_phase, AnalyzerPhase::Unknown);

        // Unknown → ShellReady (no agent, shell prompt)
        analyzer.process_output(b"$ \n");
        assert_eq!(analyzer.current_phase, AnalyzerPhase::ShellReady);

        // Detect agent
        analyzer.process_output(b"Welcome to Claude Code!\n");

        // → Busy (tool call)
        analyzer.process_output("● Edit src/lib.rs\n".as_bytes());
        assert_eq!(analyzer.current_phase, AnalyzerPhase::Busy);

        // → Idle (prompt)
        analyzer.process_output(b">\n");
        assert_eq!(analyzer.current_phase, AnalyzerPhase::Idle);

        // → NeedsInput (input prompt)
        analyzer.process_output(b"Allow this action? (y/n)\n");
        assert_eq!(analyzer.current_phase, AnalyzerPhase::NeedsInput);

        // → Busy (tool call again)
        analyzer.process_output("● Bash cargo test\n".as_bytes());
        assert_eq!(analyzer.current_phase, AnalyzerPhase::Busy);

        // → Idle (prompt)
        analyzer.process_output(b">\n");
        assert_eq!(analyzer.current_phase, AnalyzerPhase::Idle);
    }

    // -- Test 6: Silence fallback --

    #[test]
    fn silence_fallback() {
        let mut analyzer = make_analyzer();

        // Set up as busy with a prompt in buffer
        analyzer.process_output(b"Welcome to Claude Code!\n");
        analyzer.process_output("● Read file.rs\n".as_bytes());
        assert!(analyzer.is_busy);

        // Add a prompt to the buffer
        analyzer.process_output(b">\n");

        // Make it busy again
        analyzer.process_output("● Edit file.rs\n".as_bytes());
        assert!(analyzer.is_busy);

        // Silence check should find the prompt in buffer and go Idle
        // (already Busy from Edit, but let's set it up so the phase is Busy)
        let event = analyzer.check_silence();
        // Phase was already Idle from the ">" line, then changed to Busy from Edit,
        // silence should find prompt → Idle
        assert!(event.is_some());
        if let Some(AnalyzerEvent::PhaseChanged(phase)) = event {
            assert_eq!(phase, AnalyzerPhase::Idle);
        }
    }

    #[test]
    fn silence_when_not_busy() {
        let mut analyzer = make_analyzer();
        assert!(analyzer.check_silence().is_none());
    }

    #[test]
    fn silence_needs_input_when_no_prompt() {
        let mut analyzer = make_analyzer();

        // Detect agent, make busy with non-prompt output
        analyzer.process_output(b"Welcome to Claude Code!\n");
        analyzer.process_output(b"Processing something important...\n");
        analyzer.is_busy = true;

        // Clear the stripped buffer of any prompt-like content
        analyzer.stripped_buffer.clear();
        analyzer
            .stripped_buffer
            .push_str("Processing something important...\n");

        let event = analyzer.check_silence();
        assert!(event.is_some());
        if let Some(AnalyzerEvent::PhaseChanged(phase)) = event {
            assert_eq!(phase, AnalyzerPhase::NeedsInput);
        }
    }

    // -- Test 7: Latency tracking --

    #[test]
    fn latency_tracking() {
        let mut analyzer = make_analyzer();

        // Simulate several input→output cycles
        for _ in 0..10 {
            analyzer.mark_input_sent();
            // Small sleep to get a measurable latency — use a manual approach
            // by setting last_input_at to a past instant
            analyzer.last_input_at = Some(Instant::now() - std::time::Duration::from_millis(100));
            analyzer.process_output(b"some output\n");
        }

        let metrics = analyzer.metrics();
        assert!(metrics.latency_p50_ms.is_some());
        assert!(metrics.latency_p95_ms.is_some());

        let p50 = metrics.latency_p50_ms.unwrap();
        let p95 = metrics.latency_p95_ms.unwrap();
        assert!(p50 >= 50.0); // Should be at least 100ms but allow for timing
        assert!(p95 >= p50);
    }

    // -- Test 8: CWD tracking --

    #[test]
    fn cwd_tracking() {
        let mut analyzer = make_analyzer();

        let osc7 = b"\x1b]7;file://myhost/home/user/project\x07some output\n";
        let events = analyzer.process_output(osc7);

        assert!(events.iter().any(|e| matches!(e,
            AnalyzerEvent::CwdChanged(path) if path == "/home/user/project"
        )));
        assert_eq!(analyzer.current_cwd.as_deref(), Some("/home/user/project"));

        // Same path again should not emit event
        let events2 = analyzer.process_output(osc7);
        assert!(
            !events2
                .iter()
                .any(|e| matches!(e, AnalyzerEvent::CwdChanged(_)))
        );

        // Different path should emit
        let osc7_2 = b"\x1b]7;file://myhost/tmp/other\x07output\n";
        let events3 = analyzer.process_output(osc7_2);
        assert!(events3.iter().any(|e| matches!(e,
            AnalyzerEvent::CwdChanged(path) if path == "/tmp/other"
        )));
    }

    #[test]
    fn cwd_tracking_percent_encoded() {
        let mut analyzer = make_analyzer();

        let osc7 = b"\x1b]7;file://host/home/user/my%20project\x07\n";
        let events = analyzer.process_output(osc7);
        assert!(events.iter().any(|e| matches!(e,
            AnalyzerEvent::CwdChanged(path) if path == "/home/user/my project"
        )));
    }

    // -- Test 9: Token history sparkline --

    #[test]
    fn token_history_populated() {
        let mut analyzer = make_analyzer();

        analyzer.process_output(b"Welcome to Claude Code!\n");

        // Feed multiple token lines
        for i in 1..=5 {
            let line = format!("input: {}K tokens | output: {}K tokens\n", i, i);
            analyzer.process_output(line.as_bytes());
        }

        let metrics = analyzer.metrics();
        assert_eq!(metrics.token_history.len(), 5);

        // Last entry should have cumulative values (since Claude is cumulative)
        let last = metrics.token_history.last().unwrap();
        assert_eq!(last.0, 5000); // 5K input
        assert_eq!(last.1, 5000); // 5K output
    }

    // -- Test 10: Memory bounds --

    #[test]
    fn memory_bounds_token_history() {
        let mut analyzer = make_analyzer();
        analyzer.process_output(b"Welcome to Claude Code!\n");

        for i in 1..=50 {
            let line = format!("input: {}K tokens | output: {}K tokens\n", i, i);
            analyzer.process_output(line.as_bytes());
        }

        assert!(analyzer.token_history.len() <= TOKEN_HISTORY_CAP);
    }

    #[test]
    fn memory_bounds_tool_calls() {
        let mut analyzer = make_analyzer();
        analyzer.process_output(b"Welcome to Claude Code!\n");

        for i in 0..150 {
            let line = format!("● Read file{i}.rs\n");
            analyzer.process_output(line.as_bytes());
        }

        assert!(analyzer.tool_calls.len() <= TOOL_CALLS_CAP);
    }

    #[test]
    fn memory_bounds_files_touched() {
        let mut analyzer = make_analyzer();
        analyzer.process_output(b"Welcome to Claude Code!\n");

        for i in 0..80 {
            let line = format!("● Read /home/user/src/file{i}.rs\n");
            analyzer.process_output(line.as_bytes());
        }

        assert!(analyzer.files_touched.len() <= FILES_TOUCHED_CAP);
    }

    #[test]
    fn memory_bounds_latency_samples() {
        let mut analyzer = make_analyzer();

        for _ in 0..80 {
            analyzer.last_input_at = Some(Instant::now() - std::time::Duration::from_millis(100));
            analyzer.process_output(b"output\n");
        }

        assert!(analyzer.latency_samples.len() <= LATENCY_SAMPLES_CAP);
    }

    #[test]
    fn memory_bounds_stripped_buffer() {
        let mut analyzer = make_analyzer();

        // Feed a lot of data
        let big_line = format!("{}\n", "x".repeat(2000));
        for _ in 0..20 {
            analyzer.process_output(big_line.as_bytes());
        }

        assert!(analyzer.stripped_buffer.len() <= STRIPPED_BUFFER_CAP + 2048);
    }

    // -- Additional edge case tests --

    #[test]
    fn empty_input_produces_no_events() {
        let mut analyzer = make_analyzer();
        let events = analyzer.process_output(b"");
        assert!(events.is_empty());
    }

    #[test]
    fn whitespace_only_lines_skipped() {
        let mut analyzer = make_analyzer();
        let events = analyzer.process_output(b"   \n  \n\n");
        assert!(events.is_empty());
        assert_eq!(analyzer.line_count, 0);
    }

    #[test]
    fn metrics_snapshot() {
        let mut analyzer = make_analyzer();
        analyzer.process_output(b"Welcome to Claude Code!\n");
        analyzer.process_output("● Read src/main.rs\n".as_bytes());
        analyzer.process_output(b"input: 1K tokens | output: 500 tokens\n");

        let metrics = analyzer.metrics();
        assert!(metrics.detected_agent.is_some());
        assert_eq!(metrics.detected_agent.unwrap().name, "Claude Code");
        assert!(metrics.line_count >= 3);
        assert!(!metrics.tool_calls.is_empty());
        assert!(!metrics.token_usage.is_empty());
    }

    #[test]
    fn metrics_tool_calls_capped_at_20() {
        let mut analyzer = make_analyzer();
        analyzer.process_output(b"Welcome to Claude Code!\n");

        for i in 0..50 {
            let line = format!("● Read file{i}.rs\n");
            analyzer.process_output(line.as_bytes());
        }

        let metrics = analyzer.metrics();
        assert!(metrics.tool_calls.len() <= METRICS_TOOL_CALLS_LIMIT);
    }

    #[test]
    fn percentile_helper_empty() {
        let samples = VecDeque::new();
        assert!(percentile(&samples, 50.0).is_none());
    }

    #[test]
    fn percentile_helper_single() {
        let mut samples = VecDeque::new();
        samples.push_back(42.0);
        assert_eq!(percentile(&samples, 50.0), Some(42.0));
        assert_eq!(percentile(&samples, 95.0), Some(42.0));
    }

    #[test]
    fn percentile_helper_multiple() {
        let mut samples = VecDeque::new();
        for i in 1..=100 {
            #[allow(clippy::cast_precision_loss)]
            samples.push_back(i as f64);
        }
        let p50 = percentile(&samples, 50.0).unwrap();
        let p95 = percentile(&samples, 95.0).unwrap();
        assert!((p50 - 50.0).abs() < 1.5);
        assert!((p95 - 95.0).abs() < 1.5);
    }

    #[test]
    fn model_extraction_on_detection() {
        let mut analyzer = make_analyzer();
        // Detect Claude first
        analyzer.process_output(b"Welcome to Claude Code!\n");
        assert!(analyzer.detected_agent.as_ref().unwrap().model.is_none());

        // Feed a line with model info
        analyzer.process_output(b"Using claude-opus-4 model\n");
        assert_eq!(
            analyzer.detected_agent.as_ref().unwrap().model.as_deref(),
            Some("opus")
        );
    }

    #[test]
    fn token_update_cumulative_replaces() {
        let mut analyzer = make_analyzer();
        analyzer.process_output(b"Welcome to Claude Code!\n");

        // First update: 1K in, 500 out
        analyzer.process_output(b"input: 1K tokens | output: 500 tokens\n");
        let tokens = &analyzer.token_usage["anthropic"];
        assert_eq!(tokens.input_tokens, 1000);
        assert_eq!(tokens.output_tokens, 500);

        // Second update (cumulative): replaces with 2K, 1K
        analyzer.process_output(b"input: 2K tokens | output: 1K tokens\n");
        let tokens = &analyzer.token_usage["anthropic"];
        assert_eq!(tokens.input_tokens, 2000);
        assert_eq!(tokens.output_tokens, 1000);
    }

    #[test]
    fn token_update_delta_adds() {
        let mut analyzer = make_analyzer();
        analyzer.process_output(b"Aider v0.86.0\n");

        // First update: 500 sent, 200 received (delta)
        analyzer.process_output(b"Tokens: 500 sent, 200 received\n");
        let tokens = &analyzer.token_usage["openai"];
        assert_eq!(tokens.input_tokens, 500);
        assert_eq!(tokens.output_tokens, 200);

        // Second update: adds more
        analyzer.process_output(b"Tokens: 300 sent, 100 received\n");
        let tokens = &analyzer.token_usage["openai"];
        assert_eq!(tokens.input_tokens, 800);
        assert_eq!(tokens.output_tokens, 300);
    }

    #[test]
    fn default_trait() {
        let analyzer = OutputAnalyzer::default();
        assert_eq!(analyzer.current_phase, AnalyzerPhase::Unknown);
        assert_eq!(analyzer.line_count, 0);
    }
}
