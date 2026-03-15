use std::path::Path;

/// Generate ov.conf content from configuration values.
pub fn generate_ov_conf(
    provider: &str,
    api_key: &str,
    embedding_model: &str,
    vlm_model: &str,
    port: u16,
    data_dir: &Path,
) -> String {
    let prefix = model_prefix(provider);
    format!(
        r#"[server]
port = {port}
data_dir = "{data_dir}"

[provider]
type = "litellm"
api_key = "{api_key}"

[models]
embedding = "{prefix}{embedding_model}"
vlm = "{prefix}{vlm_model}"

[indexing]
chunk_size = 1024
chunk_overlap = 128
exclude_patterns = ["*.lock", "node_modules/**", "target/**", ".git/**", "*.pyc"]

[search]
default_tier = "L1"
max_results = 20
"#,
        data_dir = data_dir.display()
    )
}

/// Map provider name to `LiteLLM` model prefix.
pub fn model_prefix(provider: &str) -> &str {
    match provider {
        "gemini" => "gemini/",
        "openrouter" => "openrouter/",
        // "openai" and other providers use no prefix
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn model_prefix_openai() {
        assert_eq!(model_prefix("openai"), "");
    }

    #[test]
    fn model_prefix_gemini() {
        assert_eq!(model_prefix("gemini"), "gemini/");
    }

    #[test]
    fn model_prefix_openrouter() {
        assert_eq!(model_prefix("openrouter"), "openrouter/");
    }

    #[test]
    fn model_prefix_unknown_defaults_empty() {
        assert_eq!(model_prefix("unknown"), "");
        assert_eq!(model_prefix(""), "");
    }

    #[test]
    fn generate_conf_openai() {
        let conf = generate_ov_conf(
            "openai",
            "sk-test",
            "text-embedding-3-small",
            "gpt-4o",
            1933,
            &PathBuf::from("/data/ov"),
        );
        assert!(conf.contains("port = 1933"));
        assert!(conf.contains(r#"api_key = "sk-test""#));
        assert!(conf.contains(r#"embedding = "text-embedding-3-small""#));
        assert!(conf.contains(r#"vlm = "gpt-4o""#));
        assert!(conf.contains(r#"data_dir = "/data/ov""#));
    }

    #[test]
    fn generate_conf_gemini() {
        let conf = generate_ov_conf(
            "gemini",
            "AIza...",
            "text-embedding-004",
            "gemini-2.0-flash",
            1933,
            &PathBuf::from("/data/ov"),
        );
        assert!(conf.contains(r#"embedding = "gemini/text-embedding-004""#));
        assert!(conf.contains(r#"vlm = "gemini/gemini-2.0-flash""#));
    }

    #[test]
    fn generate_conf_openrouter() {
        let conf = generate_ov_conf(
            "openrouter",
            "or-key",
            "openai/text-embedding-3-small",
            "anthropic/claude-sonnet-4",
            1933,
            &PathBuf::from("/data/ov"),
        );
        assert!(conf.contains(r#"embedding = "openrouter/openai/text-embedding-3-small""#));
        assert!(conf.contains(r#"vlm = "openrouter/anthropic/claude-sonnet-4""#));
    }
}
