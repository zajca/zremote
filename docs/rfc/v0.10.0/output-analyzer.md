# RFC: Output Analyzer — Real-time PTY Telemetry

**Status:** Draft
**Date:** 2026-03-31
**Author:** zajca
**Parent:** [RFC v0.10.0 Agent Intelligence](README.md) (Phase 1-2)
**Inspiration:** [Hermes IDE](https://github.com/hermes-hq/hermes-ide) `pty/analyzer.rs`

---

## 1. Problem Statement

ZRemote detects AI agents via two channels:

1. **Process tree scan** (`agentic/detector.rs`) — BFS every 1s, matches process names (`claude`, `aider`, `codex`, `gemini`). Returns binary "detected/not detected" with no telemetry.
2. **Claude Code hooks** (`hooks/handler.rs`) — Structured events (PreToolUse, PostToolUse, Stop, Elicitation). Rich data but **Claude-only**.

PTY output streams from reader → main loop → WS clients completely unanalyzed. No token tracking, no cost monitoring, no tool call logging, no phase detection for non-Claude agents.

### Current output flow

```
PTY reader (blocking thread, 4KB chunks, pty.rs:61-105)
  → mpsc::Sender<PtyOutput> (cap: 4096)
  → connection/mod.rs main loop (line 358)
    → bridge::fan_out() → browser WS
    → bridge::record_output() → scrollback
    → outbound_tx → AgentMessage::TerminalOutput → server WS
```

No processing happens between PTY read and WS broadcast.

---

## 2. Goals

- Extract **real-time telemetry** from PTY output of any AI CLI tool
- **Provider-agnostic** adapter pattern (Claude, Aider, Codex, Gemini — extensible)
- **Zero impact** on output latency — analyzer runs inline but doesn't block forwarding
- **Complement** existing hooks (hooks take priority for Claude, analyzer fills gaps)
- **Bounded memory** — all collections capped, ~30KB per session
- Feed analyzer events into existing `AgenticAgentMessage` pipeline

---

## 3. Design

### 3.1 Insertion Point

`crates/zremote-agent/src/connection/mod.rs` line 420-437, in the PTY output handler:

```rust
// CURRENT (line 420-437):
} else {
    bridge::fan_out(bridge_senders, session_id, BrowserMessage::Output { ... }).await;
    bridge::record_output(bridge_scrollback, session_id, data.clone()).await;
    outbound_tx.try_send(AgentMessage::TerminalOutput { session_id, data });
}

// PROPOSED:
} else {
    // NEW: Feed through per-session analyzer
    if let Some(analyzer) = session_analyzers.get_mut(&session_id) {
        for event in analyzer.process_output(&data) {
            handle_analyzer_event(
                session_id, event, &agentic_tx, agentic_manager,
            );
        }
    }

    // Existing: fan out unchanged
    bridge::fan_out(bridge_senders, session_id, BrowserMessage::Output { ... }).await;
    bridge::record_output(bridge_scrollback, session_id, data.clone()).await;
    outbound_tx.try_send(AgentMessage::TerminalOutput { session_id, data });
}

// In input handler (ServerMessage::SessionInput):
if let Some(analyzer) = session_analyzers.get_mut(&session_id) {
    analyzer.mark_input_sent();
}

// Silence check timer (new select! branch):
_ = silence_check_interval.tick() => {
    for (session_id, analyzer) in &mut session_analyzers {
        if let Some(last) = analyzer.last_output_at() {
            if last.elapsed() > Duration::from_secs(3) {
                if let Some(event) = analyzer.check_silence() {
                    handle_analyzer_event(*session_id, event, &agentic_tx, agentic_manager);
                }
            }
        }
    }
}
```

Analyzer state per session:

```rust
// In run_connection(), before main loop:
let mut session_analyzers: HashMap<SessionId, OutputAnalyzer> = HashMap::new();

// On session create (dispatch.rs → handle_session_create):
session_analyzers.insert(session_id, OutputAnalyzer::new());

// On session close/EOF:
session_analyzers.remove(&session_id);
```

### 3.2 OutputAnalyzer

**File:** `crates/zremote-agent/src/agentic/analyzer.rs`

```rust
pub struct OutputAnalyzer {
    registry: ProviderRegistry,
    active_adapter_idx: Option<usize>,

    // Detection
    detected_agent: Option<AgentInfo>,

    // Phase tracking
    current_phase: AnalyzerPhase,
    is_busy: bool,

    // Token ledger — one entry per provider (usually 1)
    token_usage: HashMap<String, ProviderTokens>,
    token_history: VecDeque<(u64, u64)>,     // cap: 30, for sparkline (total_in, total_out)

    // Tool tracking
    tool_calls: VecDeque<ToolCallEvent>,     // cap: 100
    tool_call_counts: HashMap<String, u32>,  // summary

    // File tracking
    files_touched: IndexSet<String>,          // cap: 50, preserves insertion order

    // Buffer for cross-chunk line splitting
    line_buffer: String,                      // partial line from previous chunk
    stripped_buffer: String,                  // last ~16KB of stripped output (for silence check)

    // CWD tracking
    current_cwd: Option<String>,

    // Latency tracking
    last_input_at: Option<Instant>,
    latency_samples: VecDeque<f64>,           // cap: 50, milliseconds

    // Silence detection
    last_output_at: Option<Instant>,

    // Stats
    line_count: u64,
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

/// Events emitted by the analyzer. Mapped to AgenticAgentMessage by the caller.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalyzerPhase {
    Unknown,
    ShellReady,
    Idle,
    Busy,
    NeedsInput,
}
```

#### `process_output()` flow

```
process_output(&mut self, raw: &[u8]) -> Vec<AnalyzerEvent>
  │
  ├─ 0. Latency tracking: if last_input_at set, record sample (50-120,000ms range)
  ├─ 1. Check OSC 7 CWD in RAW data (before stripping): emit CwdChanged if new
  ├─ 2. Strip ANSI escapes (strip-ansi-escapes crate)
  ├─ 3. Busy detection: if visible chars present, set is_busy=true, update last_output_at
  ├─ 4. Prepend line_buffer remainder from previous chunk
  ├─ 5. Split into complete lines (save incomplete last line to line_buffer)
  ├─ 6. For each line:
  │    ├─ Skip empty/whitespace-only
  │    ├─ Agent detection (if not yet detected): registry.detect_agent(line)
  │    │   └─ If detected: set active_adapter_idx, emit AgentDetected
  │    ├─ Model extraction: if agent detected but model unknown, try extract_model_name()
  │    ├─ Line analysis: adapter.analyze_line(line) or generic_analyze(line)
  │    │   Returns LineAnalysis { token_update, tool_call, phase_hint, file_touched }
  │    ├─ Apply token_update → update token_usage HashMap, emit TokenUpdate
  │    │   └─ Token history: push (total_in, total_out) to sparkline buffer (cap: 30)
  │    │   └─ Cost fallback: if no cost in output, estimate from token counts
  │    ├─ Apply tool_call → push to tool_calls deque, emit ToolCall
  │    ├─ Apply phase_hint → if phase changed, emit PhaseChanged
  │    ├─ Apply file_touched → insert to files_touched set
  │    └─ Append to stripped_buffer (cap: 16KB, char-boundary safe)
  │
  └─ 7. Return collected events
```

**Cross-chunk line handling:** PTY chunks arrive at arbitrary byte boundaries. A line like `"Token usage: 1.9K total"` might be split across two 4KB reads. The `line_buffer` field holds the incomplete trailing line from the previous chunk, prepended to the next chunk before splitting.

#### `mark_input_sent()` — Latency Tracking

Called by the connection loop when user input is written to PTY:

```rust
impl OutputAnalyzer {
    /// Mark that input was sent — next output chunk records latency.
    pub fn mark_input_sent(&mut self) {
        self.last_input_at = Some(Instant::now());
    }
}
```

In `process_output()`, if `last_input_at` is set:
- Calculate elapsed ms
- Filter to 50ms..120,000ms range (ignore sub-50ms echo, ignore stale >2min)
- Push to `latency_samples` (cap: 50)
- Exposed via `metrics()` as p50/p95

#### `check_silence()` — Silence Fallback Detection

Called by the connection loop on a timer (e.g. 3s after last output). Resolves ambiguous states when output stops but no prompt was detected:

```rust
impl OutputAnalyzer {
    /// Called when no PTY output for silence_timeout.
    /// Resolves stuck "Busy" state for exotic prompts or TUI menus.
    pub fn check_silence(&mut self) -> Option<AnalyzerEvent> {
        if !self.is_busy {
            return None;
        }

        // Check if any of the last 5 lines in stripped_buffer look like a prompt
        let has_prompt = self.stripped_buffer.lines().rev().take(5).any(|l| {
            let t = l.trim();
            if t.is_empty() { return false; }
            if let Some(idx) = self.active_adapter_idx {
                self.registry.adapters[idx].is_prompt(t)
            } else {
                is_shell_prompt(t)
            }
        });

        self.is_busy = false;

        let new_phase = if has_prompt {
            AnalyzerPhase::Idle
        } else if self.detected_agent.is_some() {
            // Silent + no prompt + agent running = probably waiting for input
            // (TUI menus, interactive prompts that don't match patterns)
            AnalyzerPhase::NeedsInput
        } else {
            AnalyzerPhase::Idle
        };

        if new_phase != self.current_phase {
            self.current_phase = new_phase;
            Some(AnalyzerEvent::PhaseChanged(new_phase))
        } else {
            None
        }
    }
}
```

**Integration in connection/mod.rs:**

```rust
// Add silence check timer alongside agentic_check_interval:
let mut silence_check_interval = tokio::time::interval(Duration::from_secs(1));

// In select! loop:
_ = silence_check_interval.tick() => {
    for (session_id, analyzer) in &mut session_analyzers {
        if let Some(last) = analyzer.last_output_at() {
            if last.elapsed() > Duration::from_secs(3) {
                if let Some(event) = analyzer.check_silence() {
                    handle_analyzer_event(*session_id, event, &agentic_tx, agentic_manager);
                }
            }
        }
    }
}
```

#### OSC 7 CWD Tracking

Terminals report the current working directory via OSC 7 escape sequences. This is parsed from **raw** bytes (before ANSI stripping, since stripping removes the sequence):

```rust
// In process_output(), before stripping:
let raw_text = String::from_utf8_lossy(raw);
if let Some(caps) = patterns::OSC7_RE.captures(&raw_text) {
    let path = percent_decode(&caps[1]);
    if self.current_cwd.as_deref() != Some(&path) {
        self.current_cwd = Some(path.clone());
        events.push(AnalyzerEvent::CwdChanged(path));
    }
}
```

Pattern: `\x1b\]7;file://[^/]*(/.+?)(?:\x07|\x1b\\)`

This enables:
- Accurate working directory for tool call context
- CWD display in GUI without relying on process inspection

#### Token Update Application — Cumulative vs Incremental

Providers report tokens differently:
- **Cumulative** (Claude, Codex): each report is the total so far → **replace** stored values
- **Incremental** (rare): each report is delta → **add** to stored values

```rust
fn apply_token_update(&mut self, tu: TokenUpdate) {
    let entry = self.token_usage.entry(tu.provider.clone()).or_insert_with(|| {
        ProviderTokens {
            input_tokens: 0, output_tokens: 0, cost_usd: None,
            model: "unknown".into(), last_updated: Utc::now(), update_count: 0,
        }
    });

    // Cumulative vs incremental
    if tu.is_cumulative {
        entry.input_tokens = tu.input_tokens;
        entry.output_tokens = tu.output_tokens;
    } else {
        entry.input_tokens += tu.input_tokens;
        entry.output_tokens += tu.output_tokens;
    }

    // Cost: use reported cost, or estimate from token counts
    if let Some(cost) = tu.cost_usd {
        entry.cost_usd = Some(cost);
    } else if entry.cost_usd.is_none() || entry.cost_usd == Some(0.0) {
        entry.cost_usd = Some(estimate_cost(
            &tu.provider, &entry.model, entry.input_tokens, entry.output_tokens,
        ));
    }

    entry.update_count += 1;
    entry.last_updated = Utc::now();

    // Record sparkline sample
    let total_in: u64 = self.token_usage.values().map(|t| t.input_tokens).sum();
    let total_out: u64 = self.token_usage.values().map(|t| t.output_tokens).sum();
    self.token_history.push_back((total_in, total_out));
    if self.token_history.len() > 30 { self.token_history.pop_front(); }
}
```

#### Cost Estimation Fallback

When the CLI output includes token counts but no dollar amount, estimate cost from known pricing:

```rust
/// Estimate cost in USD from token counts and known provider pricing.
/// Used as fallback only — reported costs always take priority.
pub fn estimate_cost(provider: &str, model: &str, input: u64, output: u64) -> f64 {
    let (in_price, out_price) = match (provider, model) {
        // Anthropic (per 1M tokens)
        ("anthropic", m) if m.contains("opus")   => (15.0, 75.0),
        ("anthropic", m) if m.contains("sonnet") => (3.0, 15.0),
        ("anthropic", m) if m.contains("haiku")  => (0.25, 1.25),
        ("anthropic", _)                          => (3.0, 15.0),  // default to sonnet
        // OpenAI
        ("openai", m) if m.contains("gpt-4o")    => (2.50, 10.0),
        ("openai", m) if m.contains("gpt-4")     => (30.0, 60.0),
        ("openai", m) if m.contains("o1")        => (15.0, 60.0),
        ("openai", m) if m.contains("o3")        => (10.0, 40.0),
        ("openai", _)                             => (2.50, 10.0),
        // Google
        ("google", m) if m.contains("pro")       => (1.25, 5.0),
        ("google", m) if m.contains("flash")     => (0.075, 0.30),
        ("google", _)                             => (1.25, 5.0),
        // Unknown provider
        _                                         => (3.0, 15.0),
    };
    (input as f64 / 1_000_000.0) * in_price + (output as f64 / 1_000_000.0) * out_price
}
```

**Note:** Prices are approximate and will drift as providers change pricing. This is a best-effort fallback — when exact costs are available from output, they always take priority. Prices can be updated without protocol changes.

#### Model Change Detection

After initial agent detection, continue scanning for model information:

```rust
// In process_output(), after agent detection:
if let Some(ref mut agent) = self.detected_agent {
    if let Some(model) = extract_model_name(trimmed) {
        let lower = trimmed.to_lowercase();
        let is_model_change = lower.contains("set model to")
            || lower.contains("model:")
            || lower.contains("switching to");
        let is_unknown = agent.model.is_none()
            || agent.model.as_deref() == Some("unknown");

        if is_unknown || is_model_change {
            agent.model = Some(model);
        }
    }
}
```

This catches:
- Model shown on separate line from banner (common in Claude Code)
- `/model` command changing model mid-session (Aider, Codex)
- Model info in Aider's "Main model: ..." line

#### `metrics()` — Snapshot Export

```rust
pub struct AnalyzerMetrics {
    pub line_count: u64,
    pub detected_agent: Option<AgentInfo>,
    pub current_phase: AnalyzerPhase,
    pub token_usage: HashMap<String, ProviderTokens>,
    pub token_history: Vec<(u64, u64)>,           // for sparkline
    pub tool_calls: Vec<ToolCallEvent>,            // last 20
    pub tool_call_counts: HashMap<String, u32>,
    pub files_touched: Vec<String>,
    pub current_cwd: Option<String>,
    pub latency_p50_ms: Option<f64>,
    pub latency_p95_ms: Option<f64>,
}

impl OutputAnalyzer {
    pub fn metrics(&self) -> AnalyzerMetrics {
        AnalyzerMetrics {
            line_count: self.line_count,
            detected_agent: self.detected_agent.clone(),
            current_phase: self.current_phase,
            token_usage: self.token_usage.clone(),
            token_history: self.token_history.iter().copied().collect(),
            tool_calls: self.tool_calls.iter().rev().take(20).cloned().collect(),
            tool_call_counts: self.tool_call_counts.clone(),
            files_touched: self.files_touched.iter().cloned().collect(),
            current_cwd: self.current_cwd.clone(),
            latency_p50_ms: percentile(&self.latency_samples, 50.0),
            latency_p95_ms: percentile(&self.latency_samples, 95.0),
        }
    }
}
```

### 3.3 Provider Adapter Trait

**File:** `crates/zremote-agent/src/agentic/adapters/mod.rs`

```rust
pub trait ProviderAdapter: Send + Sync {
    /// Try to detect this provider's agent from a line of output.
    /// Returns None if not a match, Some(AgentInfo) with confidence score if matched.
    fn detect_agent(&self, line: &str) -> Option<AgentInfo>;

    /// Analyze a single line of ANSI-stripped output.
    /// Returns structured analysis with optional token/tool/phase data.
    fn analyze_line(&self, line: &str) -> LineAnalysis;

    /// Check if this line is a command prompt (agent idle).
    fn is_prompt(&self, line: &str) -> bool;

    /// Provider identifier for logging and metrics.
    fn name(&self) -> &str;
}

pub struct LineAnalysis {
    pub token_update: Option<TokenUpdate>,
    pub tool_call: Option<ToolCallEvent>,
    pub phase_hint: Option<PhaseHint>,
    pub file_touched: Option<String>,
}

pub struct TokenUpdate {
    pub provider: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: Option<f64>,
    pub model: String,
    pub is_cumulative: bool,    // true = replace totals, false = add delta
}

pub enum PhaseHint {
    PromptDetected,  // Agent/shell idle
    WorkStarted,     // Agent began processing (tool call, edit, etc.)
    InputNeeded,     // Agent asking Y/n or permission prompt
}

pub struct AgentInfo {
    pub name: String,       // "Claude Code", "Aider", "Codex", "Gemini CLI"
    pub provider: String,   // "anthropic", "openai", "google"
    pub model: Option<String>,
    pub confidence: f32,    // 0.0-1.0
}
```

#### Provider Registry

```rust
pub struct ProviderRegistry {
    adapters: Vec<Box<dyn ProviderAdapter>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            adapters: vec![
                Box::new(ClaudeAdapter),
                Box::new(AiderAdapter),
                Box::new(CodexAdapter),
                Box::new(GeminiAdapter),
            ],
        }
    }

    /// Try all adapters, return highest confidence match.
    pub fn detect_agent(&self, line: &str) -> Option<(usize, AgentInfo)> {
        self.adapters.iter().enumerate()
            .filter_map(|(i, a)| a.detect_agent(line).map(|info| (i, info)))
            .max_by(|a, b| a.1.confidence.partial_cmp(&b.1.confidence).unwrap())
    }
}
```

### 3.4 Built-in Adapters

#### Claude Code Adapter

**Detection:** `"claude code"`, `"claude-code"`, `"╭"` box with `"claude"` → confidence 0.95
**Token parsing:**
- `CLAUDE_TOKEN_RE`: `input: 12.5K tokens | output: 3.2K tokens`
- `SESSION_COST_RE`: `Session cost: $0.04`
- Skip lines >200 chars (code output false positives)

**Tool detection:** `CLAUDE_TOOL_RE`: lines starting with `●⏺◉•✻*` followed by known tool names (Read, Write, Edit, Bash, Glob, Grep, etc.)
**Prompt:** Short line (<40 chars) ending with `>`, rejecting `->`, `=>`, `>>`
**Input needed:** `(y/n)`, `[y/n]`, `? Allow ...`

#### Aider Adapter

**Detection:** `AIDER_VERSION_RE`: `Aider v0.86.0` → confidence 0.95
**Token parsing:**
- `AIDER_TOKEN_RE`: `Tokens: 22k sent, 21k cache write, 2.4k received`
- `AIDER_COST_RE`: `Cost: $0.12 message, $0.67 session` (use session cost)

**Tool detection:**
- `AIDER_EDIT_RE`: `Applied edit to src/main.py`
- Commit: `Commit 414c394 feat: ...`

**Prompt:** `AIDER_PROMPT_RE`: `> `, `ask> `, `architect> ` (word followed by `> `)

#### Codex Adapter

**Detection:** `CODEX_VERSION_RE`: `OpenAI Codex (v0.98.0)` → confidence 0.95
**Token parsing:** `CODEX_TOKEN_RE`: `Token usage: 1.9K total (1K input + 900 output)`
**Tool detection:**
- `CODEX_TOOL_RE`: `• Running echo hello`
- `CODEX_FILE_OP_RE`: `• Edited file.txt (+1 -1)`

**Prompt:** `>` or `> ` (bare, short line)

#### Gemini CLI Adapter

**Detection:** `"gemini"` + `"cli"` in same line, or ASCII art banner → confidence 0.85
**Token parsing:** `GEMINI_STATS_RE`: `/stats` table rows: `gemini-2.5-pro  10  500  500  2000`
**Tool detection:** `GEMINI_TOOL_RE`: lines with `✓?xo⊷-` prefix + known tools (ReadFile, Shell, Edit, etc.)
**Prompt:** `>`, `!`, `*` (single char, short line)

### 3.5 Regex Patterns

**File:** `crates/zremote-agent/src/agentic/patterns.rs`

All patterns compiled once via `std::sync::LazyLock`:

| Pattern | Regex | Purpose |
|---------|-------|---------|
| `CLAUDE_TOKEN_RE` | `(?i)(?:input\|prompt)[:\s]*([0-9,.]+[kKmM]?)\s*tokens?\s*[\|·/,]\s*(?:output\|completion)[:\s]*([0-9,.]+[kKmM]?)\s*tokens?` | Claude token counts |
| `SESSION_COST_RE` | `(?i)(?:session\|api\|total\|cumulative)\s+cost[:\s]*\$([0-9]+\.?[0-9]*)` | Session cost in USD |
| `CLAUDE_COST_RE` | `(?i)(?:total\s+)?cost[:\s]+\$([0-9]+\.?[0-9]*)` | Generic cost |
| `CLAUDE_TOOL_RE` | `^[●⏺◉•✻\*]\s*(Read\|Write\|Edit\|...)\b` | Claude tool calls |
| `TOOL_CALL_RE` | `^[●⏺◉•]\s*(\w+)\((.+?)\)` | Generic tool(args) |
| `AIDER_VERSION_RE` | `^Aider v(\d+\.\d+\.\d+)` | Aider detection |
| `AIDER_TOKEN_RE` | `(?i)^Tokens:\s*([\d.]+k?)\s*sent...` | Aider token format |
| `AIDER_COST_RE` | `(?i)Cost:\s*\$([\d.]+)\s*message,\s*\$([\d.]+)\s*session` | Aider cost |
| `AIDER_EDIT_RE` | `^Applied edit to (.+)$` | Aider file edit |
| `AIDER_PROMPT_RE` | `^(?:\w+\s?)*>\s*$` | Aider prompt |
| `CODEX_TOKEN_RE` | `(?i)Token usage:\s*([\d.]+[KMBT]?)\s*total\s*\((...)\)` | Codex tokens |
| `CODEX_VERSION_RE` | `(?:>_\s*)?OpenAI Codex\s*(?:\(v\|v)([\d.]+)` | Codex detection |
| `CODEX_TOOL_RE` | `^[•◦]\s*(?:Running\|Ran)\s+(.+)` | Codex tool run |
| `CODEX_FILE_OP_RE` | `^[•◦]\s*(Edited\|Added\|Deleted)\s+(.+?)...` | Codex file ops |
| `GEMINI_STATS_RE` | `(?i)(gemini[\w.-]+)\s+(\d+)\s+([\d,]+)\s+([\d,]+)\s+([\d,]+)` | Gemini /stats |
| `GEMINI_TOOL_RE` | `^[✓?xo⊷\-]\s+(ReadFile\|Shell\|Edit\|...)\b` | Gemini tools |
| `OSC7_RE` | `\x1b\]7;file://[^/]*(/.+?)(?:\x07\|\x1b\\)` | CWD from terminal OSC 7 |
| `FILE_PATH_RE` | `(?:^\|\s)((?:/[\w.@-]+)+\.[\w]+)` | Absolute file paths in output |
| `CD_CMD_RE` | `^\$?\s*cd\s+(.+)` | Fallback CWD from `cd` commands |

**Helper functions** in same file:

```rust
/// Parse token count strings like "12.5K", "1M", "1,234".
pub fn parse_token_count(s: &str) -> u64;

/// Detect shell prompts: "$ ", "% ", "# ", "> ", custom chars (❯, ➜, λ, etc.)
pub fn is_shell_prompt(line: &str) -> bool;

/// Detect Y/n prompts, permission prompts, interactive input requests.
pub fn is_input_needed(line: &str) -> bool;

/// Extract canonical model name from output line.
/// Returns short names: "opus", "sonnet", "haiku", "gpt-4o", "gemini-pro", etc.
pub fn extract_model_name(line: &str) -> Option<String>;

/// Estimate cost from token counts using known provider pricing (fallback).
pub fn estimate_cost(provider: &str, model: &str, input: u64, output: u64) -> f64;

/// Decode percent-encoded strings (for OSC 7 file:// URLs).
pub fn percent_decode(s: &str) -> String;
```

### 3.6 Event Mapping — Analyzer → Agentic Pipeline

```rust
// In connection/mod.rs:
fn handle_analyzer_event(
    session_id: SessionId,
    event: AnalyzerEvent,
    agentic_tx: &mpsc::Sender<AgenticAgentMessage>,
    agentic_manager: &AgenticLoopManager,
) {
    match event {
        AnalyzerEvent::AgentDetected { name, .. } => {
            // Log only — process detection creates the loop.
            // Analyzer detection is faster (first output line vs 1s poll)
            // but loop creation stays with process detector for PID tracking.
            tracing::info!(session = %session_id, agent = %name,
                "agent detected from output (loop created by process detector)");
        }

        AnalyzerEvent::PhaseChanged(phase) => {
            // Map to AgenticStatus and send update IF:
            // 1. A loop exists for this session
            // 2. Hooks haven't sent an update in last 5s (hook priority)
            if !agentic_manager.should_accept_analyzer_update(&session_id) {
                return;
            }
            let status = match phase {
                AnalyzerPhase::Busy => AgenticStatus::Working,
                AnalyzerPhase::Idle | AnalyzerPhase::NeedsInput
                    => AgenticStatus::WaitingForInput,
                _ => return,
            };
            if let Some(loop_id) = agentic_manager.loop_id_for_session(&session_id) {
                let _ = agentic_tx.try_send(AgenticAgentMessage::LoopStateUpdate {
                    loop_id, status, task_name: None,
                });
            }
        }

        AnalyzerEvent::TokenUpdate { input_tokens, output_tokens, cost_usd, .. } => {
            // Send metrics update through agentic channel
            if let Some(loop_id) = agentic_manager.loop_id_for_session(&session_id) {
                let _ = agentic_tx.try_send(AgenticAgentMessage::LoopMetricsUpdate {
                    loop_id, input_tokens, output_tokens, cost_usd,
                });
            }
        }

        AnalyzerEvent::ToolCall { tool, args } => {
            tracing::debug!(session = %session_id, %tool, %args, "tool call detected");
        }

        AnalyzerEvent::CwdChanged(path) => {
            tracing::debug!(session = %session_id, cwd = %path, "working directory changed");
        }
    }
}
```

### 3.7 Hook Priority

When Claude Code hooks are active, both hooks and analyzer emit events. Hooks are authoritative:

```rust
// In AgenticLoopManager:
struct ActiveLoop {
    loop_id: AgenticLoopId,
    tool_name: String,
    detected_pid: u32,
    project_path: String,
    last_hook_update: Option<Instant>,  // NEW
}

impl AgenticLoopManager {
    /// Called by hooks handler when a hook event arrives.
    pub fn mark_hook_update(&mut self, session_id: &SessionId) {
        if let Some(active) = self.loops.get_mut(session_id) {
            active.last_hook_update = Some(Instant::now());
        }
    }

    /// Check if analyzer updates should be accepted (hooks idle >5s).
    pub fn should_accept_analyzer_update(&self, session_id: &SessionId) -> bool {
        match self.loops.get(session_id) {
            Some(active) => match active.last_hook_update {
                Some(t) if t.elapsed() < Duration::from_secs(5) => false,
                _ => true,
            },
            None => true,
        }
    }

    /// Get loop_id for a session (for analyzer event mapping).
    pub fn loop_id_for_session(&self, session_id: &SessionId) -> Option<AgenticLoopId> {
        self.loops.get(session_id).map(|a| a.loop_id)
    }
}
```

### 3.8 Protocol Extension — LoopMetricsUpdate

**File:** `crates/zremote-protocol/src/agentic.rs`

```rust
pub enum AgenticAgentMessage {
    LoopDetected { ... },
    LoopStateUpdate { ... },
    LoopEnded { ... },
    // NEW:
    LoopMetricsUpdate {
        loop_id: AgenticLoopId,
        input_tokens: u64,
        output_tokens: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
    },
}
```

Uses `#[serde(default)]` on new fields for backward compatibility (old agents without analyzer send no metrics, server ignores missing variant via `#[serde(other)]` or graceful match).

**Migration** (new columns on `agentic_loops`):

```sql
ALTER TABLE agentic_loops ADD COLUMN input_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE agentic_loops ADD COLUMN output_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE agentic_loops ADD COLUMN cost_usd REAL;
```

**Server processing** (`crates/zremote-core/src/processing/agentic.rs`):

```rust
AgenticAgentMessage::LoopMetricsUpdate { loop_id, input_tokens, output_tokens, cost_usd } => {
    // UPDATE agentic_loops SET input_tokens=?, output_tokens=?, cost_usd=? WHERE id=?
    // Update in-memory AgenticLoopState
    // Emit ServerEvent::LoopMetricsUpdated { loop_info, host_id, hostname }
}
```

**LoopInfo extension** (`crates/zremote-protocol/src/events.rs`):

```rust
pub struct LoopInfo {
    // existing fields...
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}
```

### 3.9 Memory Bounds

All collections in `OutputAnalyzer` are capped:

| Collection | Cap | Eviction | ~Memory |
|-----------|-----|----------|---------|
| `token_usage` | ~5 providers | HashMap (no eviction) | <1 KB |
| `token_history` | 30 samples | `pop_front()` | 480 B |
| `tool_calls` | 100 | `pop_front()` | ~10 KB |
| `tool_call_counts` | ~20 keys | HashMap (no eviction) | <1 KB |
| `files_touched` | 50 | `pop()` oldest | ~2.5 KB |
| `line_buffer` | ~4 KB | Overwritten each chunk | ~4 KB |
| `stripped_buffer` | 16 KB | Drain from front | 16 KB |
| `latency_samples` | 50 | `pop_front()` | 400 B |
| **Total per session** | | | **~35 KB** |

No unbounded allocations. Safe for hundreds of concurrent sessions.

---

## 4. Files

### CREATE

| File | Description |
|------|-------------|
| `crates/zremote-agent/src/agentic/patterns.rs` | Compiled regex patterns + helpers |
| `crates/zremote-agent/src/agentic/adapters/mod.rs` | Trait, types, registry |
| `crates/zremote-agent/src/agentic/adapters/claude.rs` | Claude Code adapter |
| `crates/zremote-agent/src/agentic/adapters/aider.rs` | Aider adapter |
| `crates/zremote-agent/src/agentic/adapters/codex.rs` | Codex adapter |
| `crates/zremote-agent/src/agentic/adapters/gemini.rs` | Gemini CLI adapter |
| `crates/zremote-agent/src/agentic/analyzer.rs` | OutputAnalyzer core |

### MODIFY

| File | Change |
|------|--------|
| `crates/zremote-agent/src/agentic/mod.rs` | Add `pub mod analyzer; pub mod adapters; pub mod patterns;` |
| `crates/zremote-agent/src/agentic/manager.rs` | Add `last_hook_update`, `loop_id_for_session()`, `should_accept_analyzer_update()`, `mark_hook_update()` |
| `crates/zremote-agent/src/connection/mod.rs` | Add `session_analyzers` HashMap, wire analyzer in PTY output handler, add `handle_analyzer_event()` |
| `crates/zremote-agent/src/connection/dispatch.rs` | Insert analyzer on session create |
| `crates/zremote-agent/src/hooks/handler.rs` | Call `agentic_manager.mark_hook_update()` on hook events |
| `crates/zremote-agent/Cargo.toml` | Add `strip-ansi-escapes` dependency |
| `crates/zremote-protocol/src/agentic.rs` | Add `LoopMetricsUpdate` variant |
| `crates/zremote-protocol/src/events.rs` | Add token/cost fields to `LoopInfo` |
| `crates/zremote-core/src/processing/agentic.rs` | Handle `LoopMetricsUpdate` |
| `crates/zremote-core/migrations/` | New migration for `agentic_loops` columns |

---

## 5. Implementation Phases

### Phase A: Foundation (parallelizable: 2 agents)

**Agent 1:** `patterns.rs` — all regex patterns + `parse_token_count()`, `is_shell_prompt()`, `is_input_needed()` with comprehensive unit tests.

**Agent 2:** `adapters/mod.rs` — `ProviderAdapter` trait, `LineAnalysis`, `TokenUpdate`, `ToolCallEvent`, `PhaseHint`, `AgentInfo`, `ProviderRegistry` with tests.

### Phase B: Adapters (parallelizable: 3 agents)

**Agent 3:** `adapters/claude.rs` — ClaudeAdapter with detection, token parsing, tool calls, prompt detection. Unit tests with real Claude Code output.

**Agent 4:** `adapters/aider.rs` — AiderAdapter with version detection, token/cost parsing, edit detection. Unit tests with real Aider output.

**Agent 5:** `adapters/codex.rs` + `adapters/gemini.rs` — Both simpler adapters. Unit tests.

### Phase C: Core (sequential)

**Agent 6:** `analyzer.rs` — `OutputAnalyzer` with `process_output()`, phase tracking, token ledger, tool tracking, file tracking. Integration tests simulating full PTY sequences.

### Phase D: Wiring (parallelizable: 2 agents)

**Agent 7:** Wire into `connection/mod.rs` + `dispatch.rs`. Add `session_analyzers` HashMap. Add `handle_analyzer_event()`. Modify `manager.rs` for hook priority.

**Agent 8:** Protocol + server: `LoopMetricsUpdate` in protocol, migration, server-side handler, `LoopInfo` extension.

### Dependency graph

```
Phase A: [#1 patterns] ──┐
         [#2 adapters] ──┼── Phase B: [#3 claude] ──┐
                          ├──          [#4 aider]  ──┤
                          └──          [#5 codex+gem]┤
                                                     └── Phase C: [#6 analyzer] ──┬── Phase D: [#7 wiring]
                                                                                   └──          [#8 protocol]
```

---

## 6. Risk Assessment

| Risk | Impact | Mitigation |
|------|--------|------------|
| Regex patterns break on new CLI versions | Medium — false negatives, no crash | Log unmatched lines at TRACE level. Patterns designed to be loose. Easy to add new patterns without code change in future (pattern registry). |
| Cross-chunk line splitting bugs | Medium — missed detections | `line_buffer` preserves partial lines. Unit tests with split chunks. |
| Analyzer adds latency to PTY forwarding | Low — inline sync call | `process_output()` is O(lines × patterns). Typical chunk: 10 lines × 16 patterns = 160 regex checks. Sub-millisecond. If proven slow: move to separate task with channel. |
| Race condition: hooks vs analyzer | Medium — duplicate/conflicting state updates | 5s hook priority window. Hooks always win. Analyzer only fills gaps. |
| Token count parsing inaccuracy | Low — display only | `parse_token_count()` handles K/M/B suffixes and comma separators. Fallback: 0 (safe default). |
| Memory growth with many sessions | Low — bounded | ~20KB per session, all collections capped. 100 sessions = 2MB total. |

---

## 7. Protocol Compatibility

All changes are additive and backward-compatible:

| Change | Compatibility |
|--------|--------------|
| New `LoopMetricsUpdate` variant | Old server ignores unknown variant (serde `#[serde(other)]` on match) |
| New fields on `LoopInfo` | Uses `#[serde(default)]` — old clients see zero/None |
| New DB columns | `DEFAULT 0` / `DEFAULT NULL` — no migration conflict |

---

## 8. Testing

### Unit tests (per module)

- **patterns.rs:** Every regex pattern tested with positive and negative cases. `parse_token_count()` edge cases (commas, K/M suffixes, empty, garbage). `extract_model_name()` for all providers. `estimate_cost()` known pricing. `percent_decode()` edge cases. `OSC7_RE` with real terminal sequences.
- **adapters/*.rs:** Each adapter tested with real CLI output samples. Detection confidence. Token parsing accuracy (cumulative flag correct). Prompt detection with common themes (oh-my-zsh, starship, vanilla, powerlevel10k).
- **analyzer.rs:** Full PTY sequences as integration tests:
  - Claude Code: banner → tool calls → cost line → prompt → idle
  - Aider: version → model → edit → tokens → cost → prompt
  - Unknown agent: shell prompt only → ShellReady phase
  - Cross-chunk splitting: token line split across two process_output() calls
  - Phase transitions: Unknown → ShellReady → Busy → Idle → NeedsInput → Busy → Idle
  - Silence fallback: feed output, wait, call check_silence() → correct phase
  - Latency tracking: mark_input_sent(), feed output, verify p50/p95 in metrics
  - CWD tracking: OSC 7 sequence → CwdChanged event, verify current_cwd in metrics
  - Cost estimation: tokens without cost in output → estimate_cost() used as fallback
  - Token history: multiple TokenUpdate events → sparkline buffer populated
  - Model change: initial "unknown" → detected from later line → updated in AgentInfo

### Integration tests

- Simulate session lifecycle: create analyzer, feed chunks, verify events sequence
- Hook priority: feed hook update, then analyzer phase change within 5s → ignored
- Hook priority: feed hook update, wait 6s, analyzer phase change → accepted
- Silence check: feed busy output, no prompt, call check_silence() → NeedsInput
- Silence check: feed busy output with prompt in last 5 lines, check_silence() → Idle

### Sample test data

```rust
// Claude Code banner
const CLAUDE_BANNER: &[&str] = &[
    "╭────────────────────────────────────────╮",
    "│ ✻ Welcome to Claude Code!              │",
    "│   /help for help                       │",
    "╰────────────────────────────────────────╯",
];

// Claude Code working
const CLAUDE_WORKING: &[&str] = &[
    "● Read(src/main.rs)",
    "● Edit(src/lib.rs)",
    "  ... content ...",
    "● Bash(cargo test)",
];

// Claude Code idle
const CLAUDE_IDLE: &[&str] = &[
    "Total cost: $0.04",
    ">",
];

// Aider session
const AIDER_SESSION: &[&str] = &[
    "Aider v0.86.0",
    "Main model: claude-sonnet-4-20250514",
    "Applied edit to src/main.py",
    "Tokens: 22k sent, 21k cache write, 2.4k received.",
    "Cost: $0.12 message, $0.67 session.",
    "> ",
];
```

---

## 9. Verification

After implementation:

1. `cargo check -p zremote-agent` — compiles
2. `cargo test -p zremote-agent` — all tests pass (new + existing)
3. `cargo clippy --workspace` — no new warnings
4. `cargo test --workspace` — no regressions
5. **Manual test (local mode):**
   - Start `zremote agent local`
   - Open terminal, run `claude` → verify in logs: "agent detected from output", phase changes, token updates
   - Open terminal, run `aider` → verify adapter switches, token parsing
   - Open terminal, plain shell → verify ShellReady phase, no false agent detection
6. **Manual test (server mode):**
   - Verify `LoopMetricsUpdate` arrives at server
   - Verify `LoopInfo` in events WS includes token/cost fields

---

## 10. Differences from Hermes IDE

This RFC is heavily inspired by Hermes IDE's `pty/analyzer.rs`. Transparency on what's shared and what's original:

### Shared design (from Hermes)
- `ProviderAdapter` trait interface and `ProviderRegistry` pattern
- All regex patterns in `patterns.rs` (CLAUDE_TOKEN_RE, AIDER_TOKEN_RE, etc.)
- `parse_token_count()`, `is_shell_prompt()`, `is_input_needed()` helpers
- Phase state machine (Idle/Busy/NeedsInput) with hint-based transitions
- Token ledger with cumulative/incremental handling
- Silence fallback detection (`check_silence()`)
- OSC 7 CWD tracking, cost estimation fallback, model change detection
- Memory bounds (capped collections, ~35KB per session)

### Original to ZRemote
- **Hook priority system** — 5s window where Claude hooks override analyzer (Hermes has no hooks)
- **Push-based event API** — `process_output()` returns `Vec<AnalyzerEvent>` (Hermes uses pull: `take_pending_phase()`)
- **Remote pipeline integration** — Events map to `AgenticAgentMessage` for WS transport to server
- **`LoopMetricsUpdate` protocol message** — Server-side persistence of token/cost data
- **Cross-chunk `line_buffer`** — Explicit partial line handling (Hermes processes per-chunk only)
- **Coexistence with process detector** — Analyzer complements BFS detection, doesn't replace it

### Not included from Hermes (separate RFCs)
- `NodeBuilder` / execution node tracking (v0.10.0 Phase 5)
- Context injection / attunement (v0.10.0 Phase 6)
- Auto-launch flow (`pending_ai_launch`) — ZRemote sessions are user-driven
- Memory fact extraction from output
- Slash command detection / `available_actions`
- Command prediction events

---

## 11. Future Work (NOT in this RFC)

- GUI dashboard for token usage, costs, tool call timeline
- Command tracking (execution nodes) — NodeBuilder pattern from Hermes
- Context delivery system — real-time nudging based on phase detection
- Shell integration — autosuggestion disabling, env injection
- Custom adapter loading from config (user-defined patterns)
- Metrics API endpoint (`GET /api/sessions/:id/analyzer-metrics`)
