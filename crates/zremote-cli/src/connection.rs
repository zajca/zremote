//! Connection resolution: server URL and host ID resolution.

use zremote_client::{ApiClient, ApiError, Host};

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
        // If explicit host is a UUID-like string, use directly
        if let Some(ref host) = self.host_override {
            if looks_like_uuid(host) {
                return Ok(host.clone());
            }
            // Otherwise treat as name/hostname prefix
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

    async fn resolve_by_prefix(
        &self,
        client: &ApiClient,
        prefix: &str,
    ) -> Result<String, CliConnectionError> {
        let hosts = client.list_hosts().await.map_err(CliConnectionError::Api)?;

        let prefix_lower = prefix.to_lowercase();
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

fn looks_like_uuid(s: &str) -> bool {
    // Simple heuristic: 36 chars with dashes in the right places
    s.len() == 36
        && s.chars().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// Errors specific to connection/host resolution.
#[derive(Debug)]
pub enum CliConnectionError {
    Api(ApiError),
    NoHostSpecified,
    NoHostsFound,
    HostNotFound(String),
    AmbiguousHost { matches: Vec<String> },
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
        }
    }
}
