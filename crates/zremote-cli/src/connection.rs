//! Connection resolution: server URL and host ID resolution.

use zremote_client::{ApiClient, ApiError, Host, Session};

use crate::GlobalOpts;

/// Resolves the API client and host ID from global options.
pub struct ConnectionResolver {
    base_url: String,
    host_override: Option<String>,
    is_local: bool,
    no_interactive: bool,
}

impl ConnectionResolver {
    pub fn new(opts: &GlobalOpts) -> Self {
        let base_url = if opts.local {
            "http://127.0.0.1:3000".to_string()
        } else {
            opts.server.clone()
        };

        Self {
            base_url,
            host_override: opts.host.clone(),
            is_local: opts.local,
            no_interactive: opts.no_interactive,
        }
    }

    /// Create the API client.
    pub fn client(&self) -> Result<ApiClient, ApiError> {
        ApiClient::new(&self.base_url)
    }

    /// Resolve the target host ID.
    ///
    /// In local mode, auto-detects the single host.
    /// With `--host`, accepts a UUID or unique name/hostname prefix.
    /// In server mode without `--host`, errors (or could prompt interactively).
    pub async fn resolve_host_id(&self, client: &ApiClient) -> Result<String, CliConnectionError> {
        // Resolve by name, hostname, or ID prefix (including full UUIDs)
        if let Some(ref host) = self.host_override {
            return self.resolve_by_prefix(client, host).await;
        }

        // Local mode: auto-detect single host
        if self.is_local {
            let hosts = client.list_hosts().await.map_err(CliConnectionError::Api)?;
            return match hosts.len() {
                0 => Err(CliConnectionError::NoHostsFound),
                1 => Ok(hosts[0].id.clone()),
                _ => Err(CliConnectionError::AmbiguousHost {
                    matches: hosts
                        .iter()
                        .map(|h| format!("{} ({})", h.name, h.id))
                        .collect(),
                }),
            };
        }

        // Server mode without --host
        if self.no_interactive {
            return Err(CliConnectionError::NoHostSpecified);
        }

        // Try to auto-select if there's only one host
        let hosts = client.list_hosts().await.map_err(CliConnectionError::Api)?;
        match hosts.len() {
            0 => Err(CliConnectionError::NoHostsFound),
            1 => Ok(hosts[0].id.clone()),
            _ => Err(CliConnectionError::NoHostSpecified),
        }
    }

    /// Resolve a session ID or UUID prefix to a full UUID.
    ///
    /// Accepts a full UUID or a prefix that uniquely matches one session for the
    /// resolved host. Mirrors the prefix-matching UX of `session list`, which
    /// truncates IDs to 8 chars.
    pub async fn resolve_session_id(
        &self,
        client: &ApiClient,
        input: &str,
    ) -> Result<String, CliConnectionError> {
        let host_id = self.resolve_host_id(client).await?;
        let sessions = client
            .list_sessions(&host_id)
            .await
            .map_err(CliConnectionError::Api)?;

        match_session_prefix(&sessions, input)
    }

    async fn resolve_by_prefix(
        &self,
        client: &ApiClient,
        prefix: &str,
    ) -> Result<String, CliConnectionError> {
        let hosts = client.list_hosts().await.map_err(CliConnectionError::Api)?;

        let prefix_lower = prefix.to_lowercase();

        // Exact match takes priority over prefix matching
        if let Some(exact) = hosts.iter().find(|h| {
            h.name.to_lowercase() == prefix_lower
                || h.hostname.to_lowercase() == prefix_lower
                || h.id.to_lowercase() == prefix_lower
        }) {
            return Ok(exact.id.clone());
        }

        let matches: Vec<&Host> = hosts
            .iter()
            .filter(|h| {
                h.name.to_lowercase().starts_with(&prefix_lower)
                    || h.hostname.to_lowercase().starts_with(&prefix_lower)
                    || h.id.to_lowercase().starts_with(&prefix_lower)
            })
            .collect();

        match matches.len() {
            0 => Err(CliConnectionError::HostNotFound(prefix.to_string())),
            1 => Ok(matches[0].id.clone()),
            _ => Err(CliConnectionError::AmbiguousHost {
                matches: matches
                    .iter()
                    .map(|h| format!("{} ({})", h.name, h.id))
                    .collect(),
            }),
        }
    }
}

/// Filter `sessions` by ID prefix (case-insensitive).
///
/// Returns `Ok(full_id)` when exactly one session matches, else a typed error.
fn match_session_prefix(sessions: &[Session], input: &str) -> Result<String, CliConnectionError> {
    let input_lower = input.to_lowercase();
    let matches: Vec<&Session> = sessions
        .iter()
        .filter(|s| s.id.to_lowercase().starts_with(&input_lower))
        .collect();

    match matches.len() {
        0 => Err(CliConnectionError::SessionNotFound(input.to_string())),
        1 => Ok(matches[0].id.clone()),
        _ => Err(CliConnectionError::AmbiguousSession {
            matches: matches.iter().map(|s| s.id.clone()).collect(),
        }),
    }
}

/// Errors specific to connection/host resolution.
#[derive(Debug)]
pub enum CliConnectionError {
    Api(ApiError),
    NoHostSpecified,
    NoHostsFound,
    HostNotFound(String),
    AmbiguousHost { matches: Vec<String> },
    SessionNotFound(String),
    AmbiguousSession { matches: Vec<String> },
}

impl std::fmt::Display for CliConnectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Api(e) => write!(f, "{e}"),
            Self::NoHostSpecified => {
                write!(f, "multiple hosts available — specify --host <ID_OR_NAME>")
            }
            Self::NoHostsFound => write!(f, "no hosts found (is the agent running?)"),
            Self::HostNotFound(prefix) => {
                write!(f, "no host matching '{prefix}' found")
            }
            Self::AmbiguousHost { matches } => {
                write!(f, "ambiguous host — multiple matches:")?;
                for m in matches {
                    write!(f, "\n  {m}")?;
                }
                Ok(())
            }
            Self::SessionNotFound(prefix) => {
                write!(f, "no session matching '{prefix}' found")
            }
            Self::AmbiguousSession { matches } => {
                write!(f, "ambiguous session — multiple matches:")?;
                for m in matches {
                    write!(f, "\n  {m}")?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zremote_client::SessionStatus;

    fn mk_session(id: &str) -> Session {
        Session {
            id: id.to_string(),
            host_id: "host".to_string(),
            name: None,
            shell: None,
            status: SessionStatus::Active,
            working_dir: None,
            project_id: None,
            pid: None,
            exit_code: None,
            created_at: String::new(),
            closed_at: None,
        }
    }

    #[test]
    fn prefix_unique_match() {
        let sessions = vec![
            mk_session("de683bd4-1111-2222-3333-444455556666"),
            mk_session("99999999-aaaa-bbbb-cccc-dddddddddddd"),
        ];
        let resolved = match_session_prefix(&sessions, "de683bd4").unwrap();
        assert_eq!(resolved, "de683bd4-1111-2222-3333-444455556666");
    }

    #[test]
    fn prefix_case_insensitive() {
        let sessions = vec![mk_session("DE683BD4-1111-2222-3333-444455556666")];
        let resolved = match_session_prefix(&sessions, "de683bd4").unwrap();
        assert_eq!(resolved, "DE683BD4-1111-2222-3333-444455556666");
    }

    #[test]
    fn prefix_no_match() {
        let sessions = vec![mk_session("de683bd4-1111-2222-3333-444455556666")];
        let err = match_session_prefix(&sessions, "ffffffff").unwrap_err();
        assert!(matches!(err, CliConnectionError::SessionNotFound(_)));
    }

    #[test]
    fn prefix_ambiguous_match() {
        let sessions = vec![
            mk_session("de683bd4-1111-2222-3333-444455556666"),
            mk_session("de683bd4-ffff-0000-0000-000000000000"),
        ];
        let err = match_session_prefix(&sessions, "de683bd4").unwrap_err();
        match err {
            CliConnectionError::AmbiguousSession { matches } => assert_eq!(matches.len(), 2),
            other => panic!("expected AmbiguousSession, got {other:?}"),
        }
    }

    #[test]
    fn prefix_empty_sessions() {
        let err = match_session_prefix(&[], "de683bd4").unwrap_err();
        assert!(matches!(err, CliConnectionError::SessionNotFound(_)));
    }
}
