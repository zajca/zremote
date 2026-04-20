use clap::Subcommand;
use zremote_client::types::{CreateSessionRequest, UpdateSessionRequest};
use zremote_client::{ApiClient, SessionStatus};

use crate::connection::ConnectionResolver;
use crate::format::Formatter;

#[derive(Debug, Subcommand)]
pub enum SessionCommand {
    /// List sessions (active only by default)
    #[command(alias = "ls")]
    List {
        /// Include closed sessions
        #[arg(long)]
        all: bool,
    },
    /// Create a new session
    Create {
        /// Shell binary
        #[arg(long)]
        shell: Option<String>,
        /// Terminal columns
        #[arg(long)]
        cols: Option<u16>,
        /// Terminal rows
        #[arg(long)]
        rows: Option<u16>,
        /// Starting directory
        #[arg(long)]
        working_dir: Option<String>,
        /// Session display name
        #[arg(long)]
        name: Option<String>,
    },
    /// Show session details
    Get {
        /// Session ID
        session_id: String,
    },
    /// Rename a session
    Rename {
        /// Session ID
        session_id: String,
        /// New display name
        new_name: String,
    },
    /// Close a session
    Close {
        /// Session ID
        session_id: String,
    },
    /// Purge a closed session
    Purge {
        /// Session ID
        session_id: String,
    },
    /// Attach to a session
    #[command(alias = "a")]
    Attach {
        /// Session ID
        session_id: String,
    },
}

#[allow(clippy::too_many_lines)]
pub async fn run(
    client: &ApiClient,
    resolver: &ConnectionResolver,
    fmt: &dyn Formatter,
    command: SessionCommand,
) -> i32 {
    match command {
        SessionCommand::List { all } => {
            let host_id = match resolver.resolve_host_id(client).await {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("Error: {e}");
                    return 1;
                }
            };
            match client.list_sessions(&host_id).await {
                Ok(sessions) => {
                    let sessions = if all {
                        sessions
                    } else {
                        sessions
                            .into_iter()
                            .filter(|s| !matches!(s.status, SessionStatus::Closed))
                            .collect()
                    };
                    println!("{}", fmt.sessions(&sessions));
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        SessionCommand::Create {
            shell,
            cols,
            rows,
            working_dir,
            name,
        } => {
            let host_id = match resolver.resolve_host_id(client).await {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("Error: {e}");
                    return 1;
                }
            };
            let (term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
            let req = CreateSessionRequest {
                name,
                shell,
                cols: cols.unwrap_or(term_cols),
                rows: rows.unwrap_or(term_rows),
                working_dir,
            };
            match client.create_session(&host_id, &req).await {
                Ok(resp) => {
                    println!("{}", resp.id);
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        SessionCommand::Get { session_id } => {
            let full_id = match resolver.resolve_session_id(client, &session_id).await {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("Error: {e}");
                    return 1;
                }
            };
            match client.get_session(&full_id).await {
                Ok(session) => {
                    println!("{}", fmt.session(&session));
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        SessionCommand::Rename {
            session_id,
            new_name,
        } => {
            let full_id = match resolver.resolve_session_id(client, &session_id).await {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("Error: {e}");
                    return 1;
                }
            };
            let req = UpdateSessionRequest {
                name: Some(new_name),
            };
            match client.update_session(&full_id, &req).await {
                Ok(session) => {
                    println!("{}", fmt.session(&session));
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        SessionCommand::Close { session_id } => {
            let full_id = match resolver.resolve_session_id(client, &session_id).await {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("Error: {e}");
                    return 1;
                }
            };
            match client.close_session(&full_id).await {
                Ok(()) => {
                    println!("Session {full_id} closed.");
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        SessionCommand::Purge { session_id } => {
            let full_id = match resolver.resolve_session_id(client, &session_id).await {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("Error: {e}");
                    return 1;
                }
            };
            match client.purge_session(&full_id).await {
                Ok(()) => {
                    println!("Session {full_id} purged.");
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        SessionCommand::Attach { session_id } => {
            let full_id = match resolver.resolve_session_id(client, &session_id).await {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("Error: {e}");
                    return 1;
                }
            };
            crate::terminal::run_attach(client, &full_id).await
        }
    }
}

/// Create a new session and immediately attach to it.
pub async fn run_new(
    client: &ApiClient,
    resolver: &ConnectionResolver,
    shell: Option<String>,
    working_dir: Option<String>,
    name: Option<String>,
) -> i32 {
    let host_id = match resolver.resolve_host_id(client).await {
        Ok(id) => id,
        Err(e) => {
            eprintln!("Error: {e}");
            return 1;
        }
    };
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let req = CreateSessionRequest {
        name,
        shell,
        cols,
        rows,
        working_dir,
    };
    match client.create_session(&host_id, &req).await {
        Ok(resp) => crate::terminal::run_attach(client, &resp.id).await,
        Err(e) => {
            eprintln!("Error: {e}");
            1
        }
    }
}
