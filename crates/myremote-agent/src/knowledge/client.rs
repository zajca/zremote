use std::time::Duration;

use myremote_protocol::knowledge::{
    ExtractedMemory, MemoryCategory, SearchResult, SearchTier, TranscriptFragment,
};
use myremote_protocol::AgenticLoopId;

/// HTTP client for the local `OpenViking` API.
pub struct OvClient {
    client: reqwest::Client,
    base_url: String,
}

impl OvClient {
    pub fn new(port: u16) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
            base_url: format!("http://localhost:{port}"),
        }
    }

    /// Check if `OpenViking` is healthy.
    #[allow(dead_code)]
    pub async fn health(&self) -> Result<bool, OvClientError> {
        let resp = self
            .client
            .get(format!("{}/health", self.base_url))
            .send()
            .await
            .map_err(OvClientError::Request)?;
        Ok(resp.status().is_success())
    }

    /// Trigger indexing of a project.
    pub async fn index_project(
        &self,
        namespace: &str,
        path: &str,
        force: bool,
    ) -> Result<(), OvClientError> {
        let body = serde_json::json!({
            "namespace": namespace,
            "path": path,
            "force": force,
        });

        let resp = self
            .client
            .post(format!("{}/api/v1/resources/index", self.base_url))
            .json(&body)
            .send()
            .await
            .map_err(OvClientError::Request)?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(OvClientError::Api(format!("{status}: {text}")));
        }

        Ok(())
    }

    /// Search the knowledge base.
    pub async fn search(
        &self,
        namespace: &str,
        query: &str,
        tier: &str,
        max_results: u32,
    ) -> Result<Vec<SearchResult>, OvClientError> {
        let body = serde_json::json!({
            "namespace": namespace,
            "query": query,
            "tier": tier,
            "max_results": max_results,
        });

        let resp = self
            .client
            .post(format!("{}/api/v1/search/find", self.base_url))
            .json(&body)
            .send()
            .await
            .map_err(OvClientError::Request)?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(OvClientError::Api(format!("{status}: {text}")));
        }

        // Parse the OV response format and convert to our SearchResult
        let ov_results: Vec<OvSearchResult> =
            resp.json().await.map_err(OvClientError::Request)?;

        let tier_enum = match tier {
            "l0" => SearchTier::L0,
            "l2" => SearchTier::L2,
            _ => SearchTier::L1,
        };

        Ok(ov_results
            .into_iter()
            .map(|r| SearchResult {
                path: r.path,
                score: r.score,
                snippet: r.snippet,
                line_start: r.line_start,
                line_end: r.line_end,
                tier: tier_enum,
            })
            .collect())
    }

    /// Extract memories from a transcript.
    pub async fn extract_memories(
        &self,
        namespace: &str,
        transcript: &[TranscriptFragment],
        loop_id: AgenticLoopId,
    ) -> Result<Vec<ExtractedMemory>, OvClientError> {
        let body = serde_json::json!({
            "namespace": namespace,
            "session_transcript": transcript,
        });

        let resp = self
            .client
            .post(format!("{}/api/v1/memories/extract", self.base_url))
            .json(&body)
            .send()
            .await
            .map_err(OvClientError::Request)?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(OvClientError::Api(format!("{status}: {text}")));
        }

        let ov_memories: Vec<OvMemory> = resp.json().await.map_err(OvClientError::Request)?;

        Ok(ov_memories
            .into_iter()
            .map(|m| ExtractedMemory {
                key: m.key,
                content: m.content,
                category: parse_memory_category(&m.category),
                confidence: m.confidence,
                source_loop_id: loop_id,
            })
            .collect())
    }

    /// Synthesize knowledge into instructions.
    pub async fn synthesize_knowledge(
        &self,
        namespace: &str,
    ) -> Result<(String, u32), OvClientError> {
        let body = serde_json::json!({
            "namespace": namespace,
        });

        let resp = self
            .client
            .post(format!("{}/api/v1/knowledge/synthesize", self.base_url))
            .json(&body)
            .send()
            .await
            .map_err(OvClientError::Request)?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(OvClientError::Api(format!("{status}: {text}")));
        }

        let result: OvSynthesisResult = resp.json().await.map_err(OvClientError::Request)?;

        if result.content.is_empty() {
            return Ok((
                "# Project Knowledge\n\nNo memories have been extracted yet.\n".to_string(),
                0,
            ));
        }

        Ok((result.content, result.memories_used))
    }
}

/// OV API response types (internal, not exposed).
#[derive(Debug, serde::Deserialize)]
struct OvSearchResult {
    path: String,
    score: f64,
    snippet: String,
    line_start: Option<u32>,
    line_end: Option<u32>,
}

#[derive(Debug, serde::Deserialize)]
struct OvMemory {
    key: String,
    content: String,
    category: String,
    confidence: f64,
}

#[derive(Debug, serde::Deserialize)]
struct OvSynthesisResult {
    content: String,
    memories_used: u32,
}

fn parse_memory_category(s: &str) -> MemoryCategory {
    match s {
        "decision" => MemoryCategory::Decision,
        "pitfall" => MemoryCategory::Pitfall,
        "preference" => MemoryCategory::Preference,
        "architecture" => MemoryCategory::Architecture,
        "convention" => MemoryCategory::Convention,
        // "pattern" and anything unknown defaults to Pattern
        _ => MemoryCategory::Pattern,
    }
}

/// Errors from the OV HTTP client.
#[derive(Debug)]
pub enum OvClientError {
    Request(reqwest::Error),
    Api(String),
}

impl std::fmt::Display for OvClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request(e) => write!(f, "HTTP request failed: {e}"),
            Self::Api(msg) => write!(f, "OV API error: {msg}"),
        }
    }
}

impl std::error::Error for OvClientError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_memory_category_known() {
        assert_eq!(parse_memory_category("pattern"), MemoryCategory::Pattern);
        assert_eq!(parse_memory_category("decision"), MemoryCategory::Decision);
        assert_eq!(parse_memory_category("pitfall"), MemoryCategory::Pitfall);
        assert_eq!(
            parse_memory_category("preference"),
            MemoryCategory::Preference
        );
        assert_eq!(
            parse_memory_category("architecture"),
            MemoryCategory::Architecture
        );
        assert_eq!(
            parse_memory_category("convention"),
            MemoryCategory::Convention
        );
    }

    #[test]
    fn parse_memory_category_unknown_defaults_to_pattern() {
        assert_eq!(parse_memory_category("unknown"), MemoryCategory::Pattern);
        assert_eq!(parse_memory_category(""), MemoryCategory::Pattern);
    }

    #[test]
    fn client_error_display() {
        let err = OvClientError::Api("404: not found".to_string());
        assert!(err.to_string().contains("404"));
    }
}
