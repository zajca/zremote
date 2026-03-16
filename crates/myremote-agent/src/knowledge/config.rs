/// Generate ov.conf content (JSON) from configuration values.
pub fn generate_ov_conf(
    provider: &str,
    api_key: &str,
    embedding_model: &str,
    vlm_model: &str,
    port: u16,
) -> String {
    let config = serde_json::json!({
        "server": {
            "host": "127.0.0.1",
            "port": port,
        },
        "models": {
            "vlm": {
                "provider": provider,
                "model": vlm_model,
                "api_key": api_key,
            },
            "embedding": {
                "provider": provider,
                "model": embedding_model,
                "api_key": api_key,
            },
        },
    });
    serde_json::to_string_pretty(&config).expect("JSON serialization should not fail")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_conf_default_port() {
        let conf = generate_ov_conf(
            "openrouter",
            "or-key",
            "openai/text-embedding-3-small",
            "google/gemini-2.0-flash-001",
            1933,
        );
        let parsed: serde_json::Value = serde_json::from_str(&conf).unwrap();

        assert_eq!(parsed["server"]["host"], "127.0.0.1");
        assert_eq!(parsed["server"]["port"], 1933);
    }

    #[test]
    fn generate_conf_custom_port() {
        let conf = generate_ov_conf("openai", "sk-test", "text-embedding-3-small", "gpt-4o", 8080);
        let parsed: serde_json::Value = serde_json::from_str(&conf).unwrap();

        assert_eq!(parsed["server"]["port"], 8080);
    }

    #[test]
    fn generate_conf_provider_and_models() {
        let conf = generate_ov_conf(
            "openrouter",
            "or-key",
            "openai/text-embedding-3-small",
            "google/gemini-2.0-flash-001",
            1933,
        );
        let parsed: serde_json::Value = serde_json::from_str(&conf).unwrap();

        assert_eq!(parsed["models"]["vlm"]["provider"], "openrouter");
        assert_eq!(
            parsed["models"]["vlm"]["model"],
            "google/gemini-2.0-flash-001"
        );
        assert_eq!(parsed["models"]["embedding"]["provider"], "openrouter");
        assert_eq!(
            parsed["models"]["embedding"]["model"],
            "openai/text-embedding-3-small"
        );
    }

    #[test]
    fn generate_conf_api_key_in_both_models() {
        let conf = generate_ov_conf(
            "openai",
            "sk-secret-123",
            "text-embedding-3-small",
            "gpt-4o",
            1933,
        );
        let parsed: serde_json::Value = serde_json::from_str(&conf).unwrap();

        assert_eq!(parsed["models"]["vlm"]["api_key"], "sk-secret-123");
        assert_eq!(parsed["models"]["embedding"]["api_key"], "sk-secret-123");
    }

    #[test]
    fn generate_conf_openai_provider() {
        let conf = generate_ov_conf(
            "openai",
            "sk-test",
            "text-embedding-3-small",
            "gpt-4o",
            1933,
        );
        let parsed: serde_json::Value = serde_json::from_str(&conf).unwrap();

        assert_eq!(parsed["models"]["vlm"]["provider"], "openai");
        assert_eq!(parsed["models"]["vlm"]["model"], "gpt-4o");
        assert_eq!(parsed["models"]["embedding"]["provider"], "openai");
        assert_eq!(
            parsed["models"]["embedding"]["model"],
            "text-embedding-3-small"
        );
    }

    #[test]
    fn generate_conf_is_valid_json() {
        let conf = generate_ov_conf(
            "gemini",
            "AIza-key",
            "text-embedding-004",
            "gemini-2.0-flash",
            1933,
        );
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(&conf);
        assert!(parsed.is_ok());
    }
}
