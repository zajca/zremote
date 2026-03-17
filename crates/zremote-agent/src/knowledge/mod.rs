pub mod client;
pub mod config;
pub mod process;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use tokio::sync::mpsc;
use zremote_protocol::AgentMessage;
use zremote_protocol::knowledge::{
    CachedMemory, ExtractedMemory, KnowledgeAgentMessage, KnowledgeServerMessage, MemoryCategory,
    ServiceAction, WriteMdMode,
};

use self::client::OvClient;
use self::process::OvProcess;

/// Marker line separating user content from auto-generated content in CLAUDE.md.
const CLAUDE_MD_MARKER: &str = "<!-- ZRemote Knowledge (auto-generated, do not edit below) -->";

/// Minimum confidence threshold for including memories in CLAUDE.md and skills.
const MIN_CONFIDENCE: f64 = 0.6;

/// Minimum number of memories in a category to generate a skill file.
const MIN_MEMORIES_FOR_SKILL: usize = 3;

/// Orchestrates `OpenViking` lifecycle and message handling.
pub struct KnowledgeManager {
    process: OvProcess,
    client: OvClient,
    outbound_tx: mpsc::Sender<AgentMessage>,
    config_dir: PathBuf,
    api_key: Option<String>,
    port: u16,
    enabled: bool,
}

impl KnowledgeManager {
    pub fn new(
        binary: String,
        port: u16,
        config_dir: std::path::PathBuf,
        api_key: Option<String>,
        outbound_tx: mpsc::Sender<AgentMessage>,
    ) -> Self {
        let config_path = config_dir.join("ov.conf");
        let env_vars: Vec<(String, String)> = api_key
            .as_ref()
            .map(|k| vec![("OPENROUTER_API_KEY".to_string(), k.clone())])
            .unwrap_or_default();
        Self {
            process: OvProcess::new(binary, port, config_path, env_vars),
            client: OvClient::new(port, api_key.clone()),
            config_dir,
            api_key,
            port,
            outbound_tx,
            enabled: false,
        }
    }

    /// Handle a `KnowledgeServerMessage` from the server.
    pub async fn handle_message(&mut self, msg: KnowledgeServerMessage) {
        match msg {
            KnowledgeServerMessage::ServiceControl { action } => match action {
                ServiceAction::Start => self.start_service().await,
                ServiceAction::Stop => self.stop_service().await,
                ServiceAction::Restart => {
                    self.stop_service().await;
                    self.start_service().await;
                }
            },
            KnowledgeServerMessage::IndexProject {
                project_path,
                force_reindex,
            } => {
                self.index_project(&project_path, force_reindex).await;
            }
            KnowledgeServerMessage::Search {
                project_path,
                request_id,
                query,
                tier,
                max_results,
            } => {
                self.search(&project_path, request_id, &query, tier, max_results)
                    .await;
            }
            KnowledgeServerMessage::ExtractMemory {
                loop_id,
                project_path,
                transcript,
            } => {
                self.extract_memory(loop_id, &project_path, &transcript)
                    .await;
            }
            KnowledgeServerMessage::GenerateInstructions { project_path } => {
                self.generate_instructions(&project_path).await;
            }
            KnowledgeServerMessage::WriteClaudeMd {
                project_path,
                content,
                mode,
            } => {
                self.write_claude_md(&project_path, &content, mode).await;
            }
            KnowledgeServerMessage::BootstrapProject {
                project_path,
                existing_claude_md,
            } => {
                self.bootstrap_project(&project_path, existing_claude_md.as_deref())
                    .await;
            }
            KnowledgeServerMessage::GenerateSkills { project_path } => {
                self.generate_skills(&project_path).await;
            }
        }
    }

    fn send_knowledge_msg(&self, msg: KnowledgeAgentMessage) {
        if self
            .outbound_tx
            .try_send(AgentMessage::KnowledgeAction(msg))
            .is_err()
        {
            tracing::warn!("outbound channel full, knowledge message dropped");
        }
    }

    async fn start_service(&mut self) {
        self.send_knowledge_msg(KnowledgeAgentMessage::ServiceStatus {
            status: zremote_protocol::knowledge::KnowledgeServiceStatus::Starting,
            version: None,
            error: None,
        });

        // Write config file before starting OV process
        if let Some(ref key) = self.api_key {
            let conf = config::generate_ov_conf(
                "openrouter",
                key,
                "openai/text-embedding-3-small",
                "google/gemini-2.0-flash-001",
                self.port,
            );
            if let Err(e) = tokio::fs::create_dir_all(&self.config_dir).await {
                self.send_knowledge_msg(KnowledgeAgentMessage::ServiceStatus {
                    status: zremote_protocol::knowledge::KnowledgeServiceStatus::Error,
                    version: None,
                    error: Some(format!("failed to create config dir: {e}")),
                });
                return;
            }
            let conf_path = self.config_dir.join("ov.conf");
            if let Err(e) = tokio::fs::write(&conf_path, &conf).await {
                self.send_knowledge_msg(KnowledgeAgentMessage::ServiceStatus {
                    status: zremote_protocol::knowledge::KnowledgeServiceStatus::Error,
                    version: None,
                    error: Some(format!("failed to write ov.conf: {e}")),
                });
                return;
            }
            tracing::info!(path = %conf_path.display(), "wrote OpenViking config");
        }

        match self.process.start().await {
            Ok(()) => {
                self.enabled = true;
                self.send_knowledge_msg(KnowledgeAgentMessage::ServiceStatus {
                    status: zremote_protocol::knowledge::KnowledgeServiceStatus::Ready,
                    version: None,
                    error: None,
                });
                tracing::info!("OpenViking service started");
            }
            Err(e) => {
                self.send_knowledge_msg(KnowledgeAgentMessage::ServiceStatus {
                    status: zremote_protocol::knowledge::KnowledgeServiceStatus::Error,
                    version: None,
                    error: Some(e.to_string()),
                });
                tracing::error!(error = %e, "failed to start OpenViking");
            }
        }
    }

    async fn stop_service(&mut self) {
        if let Err(e) = self.process.stop().await {
            tracing::error!(error = %e, "failed to stop OpenViking");
        }
        self.enabled = false;
        self.send_knowledge_msg(KnowledgeAgentMessage::ServiceStatus {
            status: zremote_protocol::knowledge::KnowledgeServiceStatus::Stopped,
            version: None,
            error: None,
        });
    }

    async fn index_project(&self, project_path: &str, _force_reindex: bool) {
        if !self.enabled {
            tracing::warn!("OpenViking not running, cannot index");
            return;
        }
        let namespace = format!(
            "viking://resources/{}/",
            project_name_from_path(project_path)
        );
        match self.client.index_project(&namespace, project_path).await {
            Ok(()) => {
                tracing::info!(project_path, "indexing started");
            }
            Err(e) => {
                tracing::error!(project_path, error = %e, "failed to start indexing");
                self.send_knowledge_msg(KnowledgeAgentMessage::IndexingProgress {
                    project_path: project_path.to_string(),
                    status: zremote_protocol::knowledge::IndexingStatus::Failed,
                    files_processed: 0,
                    files_total: 0,
                    error: Some(e.to_string()),
                });
            }
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    async fn search(
        &self,
        project_path: &str,
        request_id: uuid::Uuid,
        query: &str,
        _tier: zremote_protocol::knowledge::SearchTier,
        max_results: Option<u32>,
    ) {
        if !self.enabled {
            self.send_knowledge_msg(KnowledgeAgentMessage::SearchResults {
                project_path: project_path.to_string(),
                request_id,
                results: vec![],
                duration_ms: 0,
            });
            return;
        }
        let namespace = format!(
            "viking://resources/{}/",
            project_name_from_path(project_path)
        );
        let start = std::time::Instant::now();
        match self
            .client
            .search(&namespace, query, max_results.unwrap_or(20))
            .await
        {
            Ok(results) => {
                let duration_ms = start.elapsed().as_millis() as u64;
                self.send_knowledge_msg(KnowledgeAgentMessage::SearchResults {
                    project_path: project_path.to_string(),
                    request_id,
                    results,
                    duration_ms,
                });
            }
            Err(e) => {
                tracing::error!(error = %e, "search failed");
                self.send_knowledge_msg(KnowledgeAgentMessage::SearchResults {
                    project_path: project_path.to_string(),
                    request_id,
                    results: vec![],
                    duration_ms: start.elapsed().as_millis() as u64,
                });
            }
        }
    }

    async fn extract_memory(
        &self,
        loop_id: zremote_protocol::AgenticLoopId,
        project_path: &str,
        transcript: &[zremote_protocol::knowledge::TranscriptFragment],
    ) {
        if !self.enabled {
            tracing::warn!("OpenViking not running, cannot extract memories");
            return;
        }
        let namespace = format!(
            "viking://resources/{}/",
            project_name_from_path(project_path)
        );
        match self
            .client
            .extract_memories(&namespace, transcript, loop_id)
            .await
        {
            Ok(memories) => {
                // Sync to local memory cache for MCP server
                sync_memories_to_cache(project_path, &memories).await;

                self.send_knowledge_msg(KnowledgeAgentMessage::MemoryExtracted {
                    loop_id,
                    memories,
                });
            }
            Err(e) => {
                tracing::error!(error = %e, "memory extraction failed");
            }
        }
    }

    async fn generate_instructions(&self, project_path: &str) {
        if !self.enabled {
            tracing::warn!("OpenViking not running, cannot generate instructions");
            self.send_knowledge_msg(KnowledgeAgentMessage::InstructionsGenerated {
                project_path: project_path.to_string(),
                content:
                    "# Project Knowledge\n\nOpenViking service is not running. Start it first.\n"
                        .to_string(),
                memories_used: 0,
            });
            return;
        }
        let (content, memories_used) = synthesize_from_cache(project_path).await;
        self.send_knowledge_msg(KnowledgeAgentMessage::InstructionsGenerated {
            project_path: project_path.to_string(),
            content,
            memories_used,
        });
    }

    // --- Phase 1: Write-to-Disk Pipeline ---

    /// Write generated knowledge content to `{project_path}/.claude/CLAUDE.md`.
    ///
    /// If `content` is empty, generates instructions first (used by auto-regeneration flow).
    #[allow(clippy::cast_possible_truncation)]
    async fn write_claude_md(&self, project_path: &str, content: &str, mode: WriteMdMode) {
        let generated;
        let actual_content = if content.is_empty() && self.enabled {
            let (c, _) = synthesize_from_cache(project_path).await;
            generated = format_claude_md_section(&c);
            &generated
        } else {
            content
        };
        let result = write_claude_md_to_disk(project_path, actual_content, mode).await;
        match result {
            Ok(bytes) => {
                tracing::info!(project_path, bytes, "CLAUDE.md written");
                self.send_knowledge_msg(KnowledgeAgentMessage::ClaudeMdWritten {
                    project_path: project_path.to_string(),
                    bytes_written: bytes,
                    error: None,
                });
            }
            Err(e) => {
                tracing::error!(project_path, error = %e, "failed to write CLAUDE.md");
                self.send_knowledge_msg(KnowledgeAgentMessage::ClaudeMdWritten {
                    project_path: project_path.to_string(),
                    bytes_written: 0,
                    error: Some(e.to_string()),
                });
            }
        }
    }

    // --- Phase 4: Bootstrapping Existing Projects ---

    /// Bootstrap knowledge for a project: index files, extract seed memories, write initial CLAUDE.md.
    #[allow(clippy::cast_possible_truncation)]
    async fn bootstrap_project(&self, project_path: &str, existing_claude_md: Option<&str>) {
        if !self.enabled {
            self.send_knowledge_msg(KnowledgeAgentMessage::BootstrapComplete {
                project_path: project_path.to_string(),
                files_indexed: 0,
                memories_seeded: 0,
                error: Some("OpenViking service is not running".to_string()),
            });
            return;
        }

        let namespace = format!(
            "viking://resources/{}/",
            project_name_from_path(project_path)
        );

        // Step 1: Index project files
        if let Err(e) = self.client.index_project(&namespace, project_path).await {
            tracing::error!(project_path, error = %e, "bootstrap: failed to index");
            self.send_knowledge_msg(KnowledgeAgentMessage::BootstrapComplete {
                project_path: project_path.to_string(),
                files_indexed: 0,
                memories_seeded: 0,
                error: Some(format!("indexing failed: {e}")),
            });
            return;
        }

        let mut memories_seeded: u32 = 0;

        // Step 2: If existing CLAUDE.md, extract memories from it
        let claude_md = match existing_claude_md {
            Some(content) => Some(content.to_string()),
            None => {
                let path = Path::new(project_path).join(".claude").join("CLAUDE.md");
                tokio::fs::read_to_string(&path).await.ok()
            }
        };

        if let Some(ref content) = claude_md {
            let synthetic_transcript = vec![zremote_protocol::knowledge::TranscriptFragment {
                role: "system".to_string(),
                content: format!("Project instructions:\n{content}"),
                timestamp: chrono::Utc::now(),
            }];

            let dummy_loop_id = uuid::Uuid::nil();
            match self
                .client
                .extract_memories(&namespace, &synthetic_transcript, dummy_loop_id)
                .await
            {
                Ok(ref memories) => {
                    memories_seeded += u32::try_from(memories.len()).unwrap_or(u32::MAX);
                    sync_memories_to_cache(project_path, memories).await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "bootstrap: failed to extract memories from CLAUDE.md");
                }
            }
        }

        // Step 3: Read README.md for additional seed data
        let readme_path = Path::new(project_path).join("README.md");
        if let Ok(readme_content) = tokio::fs::read_to_string(&readme_path).await {
            // Truncate to first 500 lines
            let truncated: String = readme_content
                .lines()
                .take(500)
                .collect::<Vec<_>>()
                .join("\n");

            let transcript = vec![zremote_protocol::knowledge::TranscriptFragment {
                role: "system".to_string(),
                content: format!("Project README:\n{truncated}"),
                timestamp: chrono::Utc::now(),
            }];

            let dummy_loop_id = uuid::Uuid::nil();
            match self
                .client
                .extract_memories(&namespace, &transcript, dummy_loop_id)
                .await
            {
                Ok(ref memories) => {
                    memories_seeded += u32::try_from(memories.len()).unwrap_or(u32::MAX);
                    sync_memories_to_cache(project_path, memories).await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "bootstrap: failed to extract memories from README");
                }
            }
        }

        // Step 4: Generate initial CLAUDE.md section
        let (content, _) = synthesize_from_cache(project_path).await;
        let formatted = format_claude_md_section(&content);
        if let Err(e) =
            write_claude_md_to_disk(project_path, &formatted, WriteMdMode::Section).await
        {
            tracing::warn!(error = %e, "bootstrap: failed to write initial CLAUDE.md");
        }

        // Step 5: Write .mcp.json
        write_mcp_json(project_path).await;

        self.send_knowledge_msg(KnowledgeAgentMessage::BootstrapComplete {
            project_path: project_path.to_string(),
            files_indexed: 0, // OV doesn't return exact count from index_project
            memories_seeded,
            error: None,
        });
    }

    // --- Phase 5: Skills Generation ---

    /// Generate skill files from cached memories.
    async fn generate_skills(&self, project_path: &str) {
        let cache = read_memory_cache(project_path).await;
        let skills_written = write_skill_files(project_path, &cache).await;

        self.send_knowledge_msg(KnowledgeAgentMessage::SkillsGenerated {
            project_path: project_path.to_string(),
            skills_written,
        });
    }
}

/// Synthesize knowledge locally from the memory cache.
///
/// Returns `(content, memories_used)`. This replaces the old OV-side synthesis.
#[allow(clippy::cast_possible_truncation)]
async fn synthesize_from_cache(project_path: &str) -> (String, u32) {
    let cache = read_memory_cache(project_path).await;
    let high_confidence: Vec<_> = cache
        .iter()
        .filter(|m| m.confidence >= MIN_CONFIDENCE)
        .collect();

    if high_confidence.is_empty() {
        return (
            "# Project Knowledge\n\nNo memories have been extracted yet.\n".to_string(),
            0,
        );
    }

    let mut output = String::from("# Project Knowledge\n\n");

    // Group by category
    let categories: &[(MemoryCategory, &str)] = &[
        (MemoryCategory::Architecture, "Architecture"),
        (MemoryCategory::Convention, "Conventions"),
        (MemoryCategory::Pattern, "Patterns"),
        (MemoryCategory::Decision, "Decisions"),
        (MemoryCategory::Pitfall, "Pitfalls"),
        (MemoryCategory::Preference, "Preferences"),
    ];

    for (cat, label) in categories {
        let cat_memories: Vec<_> = high_confidence
            .iter()
            .filter(|m| m.category == *cat)
            .collect();
        if !cat_memories.is_empty() {
            let _ = write!(output, "## {label}\n\n");
            for mem in &cat_memories {
                let _ = writeln!(output, "- **{}**: {}", mem.key, mem.content);
            }
            output.push('\n');
        }
    }

    let count = u32::try_from(high_confidence.len()).unwrap_or(u32::MAX);
    (output, count)
}

/// Extract project name from path (last component).
pub fn project_name_from_path(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

// --- Phase 1: File I/O ---

/// Write CLAUDE.md content to disk using the specified mode.
async fn write_claude_md_to_disk(
    project_path: &str,
    content: &str,
    mode: WriteMdMode,
) -> Result<u64, std::io::Error> {
    let claude_dir = Path::new(project_path).join(".claude");
    let claude_md_path = claude_dir.join("CLAUDE.md");

    match mode {
        WriteMdMode::Replace => {
            tokio::fs::create_dir_all(&claude_dir).await?;
            tokio::fs::write(&claude_md_path, content).await?;
            Ok(u64::try_from(content.len()).unwrap_or(u64::MAX))
        }
        WriteMdMode::Section => {
            tokio::fs::create_dir_all(&claude_dir).await?;
            let new_content = if let Ok(existing) = tokio::fs::read_to_string(&claude_md_path).await
            {
                if let Some(marker_pos) = existing.find(CLAUDE_MD_MARKER) {
                    // Keep everything above the marker, replace below
                    let above = &existing[..marker_pos];
                    format!("{above}{CLAUDE_MD_MARKER}\n\n{content}")
                } else {
                    // Append marker + content to existing file
                    format!("{existing}\n\n{CLAUDE_MD_MARKER}\n\n{content}")
                }
            } else {
                // No file exists: create with marker + content
                format!("{CLAUDE_MD_MARKER}\n\n{content}")
            };
            tokio::fs::write(&claude_md_path, &new_content).await?;
            Ok(u64::try_from(new_content.len()).unwrap_or(u64::MAX))
        }
    }
}

/// Format synthesized content into a structured CLAUDE.md section template.
fn format_claude_md_section(raw_content: &str) -> String {
    // The raw_content is already synthesized by OpenViking. Wrap it in
    // the structured template as a pass-through. The OV synthesis already
    // organizes content by category, so we use it directly rather than
    // re-parsing. Add the knowledge tools section at the end.
    let mut output = String::new();
    output.push_str(raw_content.trim());
    output.push_str("\n\n## Knowledge Tools\nThis project has a ZRemote knowledge base. Use MCP tools for detailed queries:\n");
    output.push_str("- `knowledge_search`: semantic code search\n");
    output.push_str("- `knowledge_memories`: query project learnings\n");
    output
}

// --- Phase 2/3: Memory Cache ---

/// Get the memory cache directory path.
fn memory_cache_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".zremote")
        .join("memories")
}

/// Sync extracted memories to a local JSON cache file for the MCP server.
async fn sync_memories_to_cache(project_path: &str, memories: &[ExtractedMemory]) {
    let project_name = project_name_from_path(project_path);
    let cache_dir = memory_cache_dir();

    if let Err(e) = tokio::fs::create_dir_all(&cache_dir).await {
        tracing::warn!(error = %e, "failed to create memory cache directory");
        return;
    }

    let cache_path = cache_dir.join(format!("{project_name}.json"));
    let now = chrono::Utc::now();

    // Load existing cache and merge
    let mut cached: Vec<CachedMemory> =
        if let Ok(data) = tokio::fs::read_to_string(&cache_path).await {
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            Vec::new()
        };

    for mem in memories {
        if let Some(existing) = cached.iter_mut().find(|c| c.key == mem.key) {
            if mem.confidence >= existing.confidence {
                existing.content.clone_from(&mem.content);
                existing.category = mem.category;
                existing.confidence = mem.confidence;
                existing.updated_at = now;
            }
        } else {
            cached.push(CachedMemory {
                key: mem.key.clone(),
                content: mem.content.clone(),
                category: mem.category,
                confidence: mem.confidence,
                updated_at: now,
            });
        }
    }

    match serde_json::to_string_pretty(&cached) {
        Ok(json) => {
            if let Err(e) = tokio::fs::write(&cache_path, json).await {
                tracing::warn!(error = %e, "failed to write memory cache");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to serialize memory cache");
        }
    }
}

/// Read memory cache from disk (public for MCP server).
pub async fn read_memory_cache_for_project(project_path: &str) -> Vec<CachedMemory> {
    read_memory_cache(project_path).await
}

/// Read memory cache from disk.
async fn read_memory_cache(project_path: &str) -> Vec<CachedMemory> {
    let project_name = project_name_from_path(project_path);
    let cache_path = memory_cache_dir().join(format!("{project_name}.json"));

    if let Ok(data) = tokio::fs::read_to_string(&cache_path).await {
        serde_json::from_str(&data).unwrap_or_default()
    } else {
        Vec::new()
    }
}

/// Write `.mcp.json` to register the MCP server for a project.
async fn write_mcp_json(project_path: &str) {
    let mcp_path = Path::new(project_path).join(".mcp.json");

    // Don't overwrite existing .mcp.json
    if tokio::fs::metadata(&mcp_path).await.is_ok() {
        tracing::debug!(project_path, ".mcp.json already exists, skipping");
        return;
    }

    let content = serde_json::json!({
        "mcpServers": {
            "zremote-knowledge": {
                "command": "zremote-agent",
                "args": ["mcp-serve", "--project", project_path],
                "env": {}
            }
        }
    });

    match serde_json::to_string_pretty(&content) {
        Ok(json) => {
            if let Err(e) = tokio::fs::write(&mcp_path, json).await {
                tracing::warn!(error = %e, "failed to write .mcp.json");
            } else {
                tracing::info!(project_path, "wrote .mcp.json for MCP server registration");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to serialize .mcp.json");
        }
    }
}

// --- Phase 5: Skills ---

/// Generate skill files from cached memories.
/// Returns the number of skill files written.
#[allow(clippy::cast_possible_truncation)]
async fn write_skill_files(project_path: &str, memories: &[CachedMemory]) -> u32 {
    let skills_dir = Path::new(project_path).join(".claude").join("skills");
    let mut skills_written: u32 = 0;

    // Category -> label mapping
    let categories: &[(MemoryCategory, &str, &str)] = &[
        (
            MemoryCategory::Architecture,
            "architecture",
            "Project architecture decisions and component relationships",
        ),
        (
            MemoryCategory::Pattern,
            "patterns",
            "Common patterns and implementation approaches",
        ),
        (
            MemoryCategory::Pitfall,
            "pitfalls",
            "Known pitfalls and common mistakes to avoid",
        ),
        (
            MemoryCategory::Convention,
            "conventions",
            "Code conventions and style guidelines",
        ),
        (
            MemoryCategory::Decision,
            "decisions",
            "Key technical decisions and their rationale",
        ),
        (
            MemoryCategory::Preference,
            "preferences",
            "Team and project preferences",
        ),
    ];

    let mut active_categories = std::collections::HashSet::new();

    for (cat, name, description) in categories {
        let cat_memories: Vec<&CachedMemory> = memories
            .iter()
            .filter(|m| m.category == *cat && m.confidence >= MIN_CONFIDENCE)
            .collect();

        if cat_memories.len() >= MIN_MEMORIES_FOR_SKILL {
            active_categories.insert(*name);
            let skill_dir = skills_dir.join(format!("zremote-{name}"));
            if let Err(e) = tokio::fs::create_dir_all(&skill_dir).await {
                tracing::warn!(error = %e, name, "failed to create skill directory");
                continue;
            }

            let mut content =
                format!("---\nname: zremote-{name}\ndescription: {description}\n---\n\n");
            for mem in &cat_memories {
                let _ = write!(content, "## {}\n{}\n\n", mem.key, mem.content);
            }

            let skill_path = skill_dir.join("SKILL.md");
            if let Err(e) = tokio::fs::write(&skill_path, &content).await {
                tracing::warn!(error = %e, name, "failed to write skill file");
            } else {
                skills_written += 1;
            }
        }
    }

    // Clean up skills for categories that no longer qualify
    if let Ok(mut entries) = tokio::fs::read_dir(&skills_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if let Some(cat) = name_str.strip_prefix("zremote-")
                && !active_categories.contains(cat)
                && let Err(e) = tokio::fs::remove_dir_all(entry.path()).await
            {
                tracing::warn!(error = %e, cat, "failed to clean up old skill");
            }
        }
    }

    skills_written
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_name_from_path_extracts_last_component() {
        assert_eq!(project_name_from_path("/home/user/project"), "project");
        assert_eq!(project_name_from_path("/home/user/my-app"), "my-app");
        assert_eq!(project_name_from_path("simple"), "simple");
    }

    #[test]
    fn format_section_appends_tools() {
        let result = format_claude_md_section("Some knowledge content");
        assert!(result.contains("Some knowledge content"));
        assert!(result.contains("## Knowledge Tools"));
        assert!(result.contains("knowledge_search"));
        assert!(result.contains("knowledge_memories"));
    }

    #[tokio::test]
    async fn write_claude_md_section_creates_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap();

        let content = "Test generated content";
        let bytes = write_claude_md_to_disk(project_path, content, WriteMdMode::Section)
            .await
            .unwrap();
        assert!(bytes > 0);

        let written = tokio::fs::read_to_string(dir.path().join(".claude/CLAUDE.md"))
            .await
            .unwrap();
        assert!(written.starts_with(CLAUDE_MD_MARKER));
        assert!(written.contains("Test generated content"));
    }

    #[tokio::test]
    async fn write_claude_md_section_preserves_user_content() {
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap();
        let claude_dir = dir.path().join(".claude");
        tokio::fs::create_dir_all(&claude_dir).await.unwrap();

        // Write existing file with marker
        let existing =
            format!("# User Content\nDo not delete\n\n{CLAUDE_MD_MARKER}\n\nOld generated stuff");
        tokio::fs::write(claude_dir.join("CLAUDE.md"), &existing)
            .await
            .unwrap();

        // Write new content in section mode
        write_claude_md_to_disk(project_path, "New generated content", WriteMdMode::Section)
            .await
            .unwrap();

        let written = tokio::fs::read_to_string(claude_dir.join("CLAUDE.md"))
            .await
            .unwrap();

        // User content preserved
        assert!(written.contains("# User Content"));
        assert!(written.contains("Do not delete"));
        // Old generated content replaced
        assert!(!written.contains("Old generated stuff"));
        // New content present
        assert!(written.contains("New generated content"));
    }

    #[tokio::test]
    async fn write_claude_md_section_appends_to_existing_without_marker() {
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap();
        let claude_dir = dir.path().join(".claude");
        tokio::fs::create_dir_all(&claude_dir).await.unwrap();

        let existing = "# Existing CLAUDE.md\nSome user content\n";
        tokio::fs::write(claude_dir.join("CLAUDE.md"), existing)
            .await
            .unwrap();

        write_claude_md_to_disk(project_path, "Generated content", WriteMdMode::Section)
            .await
            .unwrap();

        let written = tokio::fs::read_to_string(claude_dir.join("CLAUDE.md"))
            .await
            .unwrap();

        assert!(written.contains("# Existing CLAUDE.md"));
        assert!(written.contains("Some user content"));
        assert!(written.contains(CLAUDE_MD_MARKER));
        assert!(written.contains("Generated content"));
    }

    #[tokio::test]
    async fn write_claude_md_replace_mode() {
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap();
        let claude_dir = dir.path().join(".claude");
        tokio::fs::create_dir_all(&claude_dir).await.unwrap();

        let existing = "# Old content\n";
        tokio::fs::write(claude_dir.join("CLAUDE.md"), existing)
            .await
            .unwrap();

        write_claude_md_to_disk(project_path, "Replaced content", WriteMdMode::Replace)
            .await
            .unwrap();

        let written = tokio::fs::read_to_string(claude_dir.join("CLAUDE.md"))
            .await
            .unwrap();
        assert_eq!(written, "Replaced content");
        assert!(!written.contains("Old content"));
    }

    #[tokio::test]
    async fn memory_cache_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let _project_path = format!("{}/test-project", dir.path().display());

        let memories = vec![ExtractedMemory {
            key: "test-key".to_string(),
            content: "test content".to_string(),
            category: MemoryCategory::Pattern,
            confidence: 0.9,
            source_loop_id: uuid::Uuid::nil(),
        }];

        // Override cache dir for test
        let cache_dir = dir.path().join(".zremote").join("memories");
        tokio::fs::create_dir_all(&cache_dir).await.unwrap();

        // We can't easily override the home dir in tests, but we can test
        // the read/write format at least
        let cached = vec![CachedMemory {
            key: "test-key".to_string(),
            content: "test content".to_string(),
            category: MemoryCategory::Pattern,
            confidence: 0.9,
            updated_at: chrono::Utc::now(),
        }];

        let json = serde_json::to_string_pretty(&cached).unwrap();
        let parsed: Vec<CachedMemory> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].key, "test-key");

        drop(memories);
    }

    #[tokio::test]
    async fn write_skill_files_creates_qualifying_skills() {
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap();

        let now = chrono::Utc::now();
        let memories: Vec<CachedMemory> = (0..4)
            .map(|i| CachedMemory {
                key: format!("pattern-{i}"),
                content: format!("Pattern content {i}"),
                category: MemoryCategory::Pattern,
                confidence: 0.8,
                updated_at: now,
            })
            .collect();

        let written = write_skill_files(project_path, &memories).await;
        assert_eq!(written, 1);

        let skill_path = dir.path().join(".claude/skills/zremote-patterns/SKILL.md");
        let content = tokio::fs::read_to_string(skill_path).await.unwrap();
        assert!(content.contains("name: zremote-patterns"));
        assert!(content.contains("pattern-0"));
    }

    #[tokio::test]
    async fn write_skill_files_skips_low_count_categories() {
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap();

        let now = chrono::Utc::now();
        // Only 2 memories -- below MIN_MEMORIES_FOR_SKILL threshold
        let memories = vec![
            CachedMemory {
                key: "pitfall-1".to_string(),
                content: "Don't do this".to_string(),
                category: MemoryCategory::Pitfall,
                confidence: 0.9,
                updated_at: now,
            },
            CachedMemory {
                key: "pitfall-2".to_string(),
                content: "Also bad".to_string(),
                category: MemoryCategory::Pitfall,
                confidence: 0.8,
                updated_at: now,
            },
        ];

        let written = write_skill_files(project_path, &memories).await;
        assert_eq!(written, 0);
    }

    #[tokio::test]
    async fn write_skill_files_cleans_up_old_skills() {
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap();

        // Create an old skill directory that should be cleaned up
        let old_skill = dir.path().join(".claude/skills/zremote-obsolete");
        tokio::fs::create_dir_all(&old_skill).await.unwrap();
        tokio::fs::write(old_skill.join("SKILL.md"), "old")
            .await
            .unwrap();

        let written = write_skill_files(project_path, &[]).await;
        assert_eq!(written, 0);

        // Old skill should be cleaned up
        assert!(!old_skill.exists());
    }

    #[tokio::test]
    async fn write_mcp_json_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap();

        write_mcp_json(project_path).await;

        let content = tokio::fs::read_to_string(dir.path().join(".mcp.json"))
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(parsed["mcpServers"]["zremote-knowledge"].is_object());
    }

    #[tokio::test]
    async fn write_mcp_json_does_not_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap();

        let existing = r#"{"existing": true}"#;
        tokio::fs::write(dir.path().join(".mcp.json"), existing)
            .await
            .unwrap();

        write_mcp_json(project_path).await;

        let content = tokio::fs::read_to_string(dir.path().join(".mcp.json"))
            .await
            .unwrap();
        assert_eq!(content, existing);
    }
}
