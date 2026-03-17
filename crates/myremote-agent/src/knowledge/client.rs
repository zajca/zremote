use std::time::Duration;

use myremote_protocol::AgenticLoopId;
use myremote_protocol::knowledge::{
    ExtractedMemory, MemoryCategory, SearchResult, SearchTier, TranscriptFragment,
};

/// HTTP client for the local `OpenViking` API.
pub struct OvClient {
    client: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
}

impl OvClient {
    pub fn new(port: u16, api_key: Option<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
            base_url: format!("http://localhost:{port}"),
            api_key,
        }
    }

    /// Build a request with optional auth header.
    fn request(&self, method: reqwest::Method, url: String) -> reqwest::RequestBuilder {
        let mut builder = self.client.request(method, url);
        if let Some(key) = &self.api_key {
            builder = builder.header("X-API-Key", key);
        }
        builder
    }

    /// Check if `OpenViking` is healthy.
    #[allow(dead_code)]
    pub async fn health(&self) -> Result<bool, OvClientError> {
        let resp = self
            .request(reqwest::Method::GET, format!("{}/health", self.base_url))
            .send()
            .await
            .map_err(OvClientError::Request)?;
        Ok(resp.status().is_success())
    }

    /// Trigger indexing of a project.
    pub async fn index_project(&self, namespace: &str, path: &str) -> Result<(), OvClientError> {
        let body = serde_json::json!({
            "path": path,
            "to": namespace,
            "wait": true,
        });

        let resp = self
            .request(
                reqwest::Method::POST,
                format!("{}/api/v1/resources", self.base_url),
            )
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
        max_results: u32,
    ) -> Result<Vec<SearchResult>, OvClientError> {
        let body = serde_json::json!({
            "query": query,
            "target_uri": namespace,
            "limit": max_results,
        });

        let resp = self
            .request(
                reqwest::Method::POST,
                format!("{}/api/v1/search/find", self.base_url),
            )
            .json(&body)
            .send()
            .await
            .map_err(OvClientError::Request)?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(OvClientError::Api(format!("{status}: {text}")));
        }

        let ov_response: OvSearchResponse = resp.json().await.map_err(OvClientError::Request)?;

        Ok(ov_response
            .results
            .into_iter()
            .map(|r| SearchResult {
                path: r.path,
                score: r.score,
                snippet: r.snippet,
                line_start: r.line_start,
                line_end: r.line_end,
                tier: SearchTier::L1,
            })
            .collect())
    }

    /// Extract memories from a transcript using the session-based flow.
    pub async fn extract_memories(
        &self,
        namespace: &str,
        transcript: &[TranscriptFragment],
        loop_id: AgenticLoopId,
    ) -> Result<Vec<ExtractedMemory>, OvClientError> {
        // Step 1: Create a session
        let resp = self
            .request(
                reqwest::Method::POST,
                format!("{}/api/v1/sessions", self.base_url),
            )
            .send()
            .await
            .map_err(OvClientError::Request)?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(OvClientError::Api(format!(
                "session create {status}: {text}"
            )));
        }

        let session: OvSessionResponse = resp.json().await.map_err(OvClientError::Request)?;
        let session_id = session.id;

        // Step 2: Send each transcript fragment as a message
        for fragment in transcript {
            let body = serde_json::json!({
                "role": fragment.role,
                "content": fragment.content,
            });

            let resp = self
                .request(
                    reqwest::Method::POST,
                    format!("{}/api/v1/sessions/{session_id}/messages", self.base_url),
                )
                .json(&body)
                .send()
                .await
                .map_err(OvClientError::Request)?;

            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(OvClientError::Api(format!(
                    "session message {status}: {text}"
                )));
            }
        }

        // Step 3: Extract memories from the session
        let resp = self
            .request(
                reqwest::Method::POST,
                format!("{}/api/v1/sessions/{session_id}/extract", self.base_url),
            )
            .send()
            .await
            .map_err(OvClientError::Request)?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(OvClientError::Api(format!(
                "session extract {status}: {text}"
            )));
        }

        let ov_memories: Vec<OvMemory> = resp.json().await.map_err(OvClientError::Request)?;

        let _ = namespace; // namespace reserved for future use

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
}

/// OV API response types (internal, not exposed).
#[derive(Debug, serde::Deserialize)]
struct OvSearchResponse {
    results: Vec<OvSearchResult>,
}

#[derive(Debug, serde::Deserialize)]
struct OvSearchResult {
    path: String,
    score: f64,
    snippet: String,
    line_start: Option<u32>,
    line_end: Option<u32>,
}

#[derive(Debug, serde::Deserialize)]
struct OvSessionResponse {
    id: String,
}

#[derive(Debug, serde::Deserialize)]
struct OvMemory {
    key: String,
    content: String,
    category: String,
    confidence: f64,
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
