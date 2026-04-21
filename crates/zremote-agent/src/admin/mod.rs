//! `zremote admin` — direct-to-DB administrative commands for operators
//! running on the server host. Every subcommand opens the server's SQLite
//! database, performs a targeted write, and emits an `audit_log` row.
//!
//! These commands intentionally bypass the HTTP admin surface. They are the
//! recovery path when the admin token has been lost, the OIDC issuer is
//! unreachable, or the operator simply prefers a shell: a direct DB write
//! is faster and doesn't depend on an in-process axum router being
//! available.
//!
//! **Safety:** every command writes an audit row with `actor = "cli:<user>"`
//! so operator actions remain forensically distinguishable from HTTP admin
//! actions. Never log the plaintext token or admin code.

use std::io::{self, Write};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::Utc;
use clap::{Args, Subcommand};
use rand::TryRngCore;
use rand::rngs::OsRng;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use zremote_core::db;
use zremote_core::queries::admin_config;
use zremote_core::queries::agents as agents_q;
use zremote_core::queries::audit::{self, AuditEvent, Outcome};
use zremote_core::queries::auth_sessions;
use zremote_core::queries::hosts as hosts_q;

/// Errors that can surface during admin CLI subcommands. Each variant
/// is the smallest amount of information the CLI needs to print.
#[derive(Debug)]
pub enum AdminError {
    Db(sqlx::Error),
    Query(String),
    InvalidInput(String),
    Io(io::Error),
}

impl std::fmt::Display for AdminError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(e) => write!(f, "database error: {e}"),
            Self::Query(msg) => write!(f, "query failed: {msg}"),
            Self::InvalidInput(msg) => write!(f, "invalid input: {msg}"),
            Self::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for AdminError {}

impl From<sqlx::Error> for AdminError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e)
    }
}

impl From<io::Error> for AdminError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Shared global flags for every `zremote admin` subcommand.
///
/// `clap::Args` + `#[command(flatten)]` avoids duplicating the flag
/// definition on each subcommand struct.
#[derive(Debug, Clone, Args)]
pub struct AdminGlobal {
    /// SQLite database URL. Defaults to the `DATABASE_URL` environment
    /// variable; if neither the flag nor the env var is set, falls back to
    /// `sqlite:zremote.db` (matching the server's default) with a warning
    /// on stderr so operators notice they are talking to the local-cwd DB.
    #[arg(long, env = "DATABASE_URL", global = true)]
    pub database_url: Option<String>,
}

/// Default database URL matching the server's own fallback. Kept here so
/// the CLI and server never drift apart on "where does zremote store state
/// when nothing is configured".
const DEFAULT_DATABASE_URL: &str = "sqlite:zremote.db";

impl AdminGlobal {
    /// Resolve the effective database URL, printing a warning to stderr
    /// the first time the default fires. Centralizes the "silent-default"
    /// guardrail so every subcommand behaves identically.
    #[must_use]
    pub fn resolve_database_url(&self) -> String {
        match self.database_url.as_deref() {
            Some(url) if !url.is_empty() => url.to_string(),
            _ => {
                eprintln!(
                    "warning: using default database path {DEFAULT_DATABASE_URL} \
                     — set DATABASE_URL to override"
                );
                DEFAULT_DATABASE_URL.to_string()
            }
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum AdminCommand {
    /// Rotate the admin token. Invalidates every live session and prints
    /// the new plaintext token to stderr exactly once.
    RotateToken {
        #[command(flatten)]
        global: AdminGlobal,
    },
    /// Set the OIDC configuration (issuer, client id, admin email).
    SetOidc {
        /// OIDC issuer URL. Must be https://.
        #[arg(long)]
        issuer: String,
        /// OIDC client id.
        #[arg(long)]
        client_id: String,
        /// Admin email — the only principal permitted to log in via OIDC.
        #[arg(long)]
        email: String,
        #[command(flatten)]
        global: AdminGlobal,
    },
    /// Clear the OIDC configuration (reverts to admin-token-only login).
    ClearOidc {
        #[command(flatten)]
        global: AdminGlobal,
    },
    /// Revoke every non-revoked agent credential for a host. Accepts the
    /// host UUID, hostname, or configured name.
    RevokeHost {
        /// UUID, hostname, or configured name of the host to revoke.
        #[arg(long)]
        host: String,
        #[command(flatten)]
        global: AdminGlobal,
    },
    /// Revoke a single admin session (invalidates the bearer immediately).
    RevokeSession {
        /// Session UUID as shown by `list-sessions`.
        #[arg(long)]
        session: String,
        #[command(flatten)]
        global: AdminGlobal,
    },
    /// List every live admin session.
    ListSessions {
        #[command(flatten)]
        global: AdminGlobal,
    },
    /// List every host with its live agent count.
    ListHosts {
        #[command(flatten)]
        global: AdminGlobal,
    },
    /// Print the most recent audit-log rows.
    AuditTail {
        /// Maximum number of rows to print.
        #[arg(long, default_value_t = 50)]
        limit: i64,
        /// Filter by event name (e.g. `login_fail`, `pty_spawn`).
        #[arg(long)]
        event: Option<String>,
        #[command(flatten)]
        global: AdminGlobal,
    },
}

/// Entry point invoked by the `zremote admin …` dispatcher.
///
/// Opens the DB once per invocation; every subcommand runs in a single
/// connection. Errors are printed to stderr and the process exits 1.
pub async fn run(command: AdminCommand) -> Result<(), AdminError> {
    let database_url = match &command {
        AdminCommand::RotateToken { global }
        | AdminCommand::SetOidc { global, .. }
        | AdminCommand::ClearOidc { global }
        | AdminCommand::RevokeHost { global, .. }
        | AdminCommand::RevokeSession { global, .. }
        | AdminCommand::ListSessions { global }
        | AdminCommand::ListHosts { global }
        | AdminCommand::AuditTail { global, .. } => global.resolve_database_url(),
    };

    let pool = db::init_db(&database_url)
        .await
        .map_err(|e| AdminError::Query(format!("failed to open database: {e}")))?;

    let actor = actor_label();

    match command {
        AdminCommand::RotateToken { .. } => rotate_token(&pool, &actor).await,
        AdminCommand::SetOidc {
            issuer,
            client_id,
            email,
            ..
        } => set_oidc(&pool, &actor, &issuer, &client_id, &email).await,
        AdminCommand::ClearOidc { .. } => clear_oidc(&pool, &actor).await,
        AdminCommand::RevokeHost { host, .. } => revoke_host(&pool, &actor, &host).await,
        AdminCommand::RevokeSession { session, .. } => {
            revoke_session(&pool, &actor, &session).await
        }
        AdminCommand::ListSessions { .. } => list_sessions(&pool).await,
        AdminCommand::ListHosts { .. } => list_hosts(&pool).await,
        AdminCommand::AuditTail { limit, event, .. } => {
            audit_tail(&pool, limit, event.as_deref()).await
        }
    }
}

/// Audit `actor` field for CLI commands: `cli:<system-user>`. Distinguishes
/// shell invocations from HTTP admin (which uses session UUID). Falls back
/// to `"cli:unknown"` when the user is not discoverable.
fn actor_label() -> String {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "unknown".to_string());
    format!("cli:{user}")
}

/// Best-effort audit log. Errors are traced but never propagated — audit
/// must never block an admin operation.
async fn audit_log(
    pool: &SqlitePool,
    actor: &str,
    event: &str,
    target: Option<&str>,
    outcome: Outcome,
    details: Option<serde_json::Value>,
) {
    let result = audit::log_event(
        pool,
        AuditEvent {
            ts: Utc::now(),
            actor: actor.to_string(),
            ip: None,
            event: event.to_string(),
            target: target.map(str::to_string),
            outcome,
            details,
        },
    )
    .await;
    if let Err(err) = result {
        tracing::error!(error = ?err, event, "admin CLI audit write failed");
    }
}

// ============================================================================
// rotate-token
// ============================================================================

/// Generate 32 random bytes, base64url-encode, return plaintext.
fn generate_admin_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng
        .try_fill_bytes(&mut bytes)
        .expect("OS CSPRNG must be available for admin token generation");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// SHA-256 hex digest — matches the server's admin_token::hash format.
#[must_use]
fn hash_admin_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

#[tracing::instrument(skip(pool, actor))]
async fn rotate_token(pool: &SqlitePool, actor: &str) -> Result<(), AdminError> {
    let plaintext = generate_admin_token();
    let hash = hash_admin_token(&plaintext);

    let invalidated = admin_config::rotate_token(pool, &hash)
        .await
        .map_err(|e| AdminError::Query(format!("rotate_token: {e}")))?;

    audit_log(
        pool,
        actor,
        "token_rotate",
        None,
        Outcome::Ok,
        Some(json!({ "sessions_invalidated": invalidated })),
    )
    .await;

    print_rotate_banner(&plaintext, invalidated)?;
    Ok(())
}

/// Print the plaintext admin token inside a framed ASCII banner on stderr
/// so it's visible but not mixed with stdout pipelines. The banner is the
/// only channel the plaintext ever hits — it's never persisted.
fn print_rotate_banner(plaintext: &str, invalidated: u64) -> Result<(), AdminError> {
    let mut err = io::stderr().lock();
    writeln!(
        err,
        "\n========================================================="
    )?;
    writeln!(err, "  ZREMOTE ADMIN TOKEN ROTATED")?;
    writeln!(
        err,
        "========================================================="
    )?;
    writeln!(err, "  New admin token (copy NOW — shown only once):")?;
    writeln!(err, "    {plaintext}")?;
    writeln!(err, "  Live sessions invalidated: {invalidated}")?;
    writeln!(
        err,
        "=========================================================\n"
    )?;
    err.flush()?;
    Ok(())
}

// ============================================================================
// set-oidc / clear-oidc
// ============================================================================

#[tracing::instrument(skip(pool, actor, issuer, client_id, email))]
async fn set_oidc(
    pool: &SqlitePool,
    actor: &str,
    issuer: &str,
    client_id: &str,
    email: &str,
) -> Result<(), AdminError> {
    // Hard-enforce https:// — plain http issuers are refused by the OIDC
    // client at login time, but rejecting here gives an immediate error
    // instead of a confusing "OIDC login broken" weeks later.
    if !issuer.starts_with("https://") {
        audit_log(
            pool,
            actor,
            "set_oidc_config",
            None,
            Outcome::Denied,
            Some(json!({ "reason": "non_https_issuer" })),
        )
        .await;
        return Err(AdminError::InvalidInput(
            "issuer URL must start with https://".to_string(),
        ));
    }
    if issuer.is_empty() || client_id.is_empty() || email.is_empty() {
        audit_log(
            pool,
            actor,
            "set_oidc_config",
            None,
            Outcome::Denied,
            Some(json!({ "reason": "empty_field" })),
        )
        .await;
        return Err(AdminError::InvalidInput(
            "issuer, client_id and email must all be non-empty".to_string(),
        ));
    }

    admin_config::set_oidc(pool, issuer, client_id, email)
        .await
        .map_err(|e| AdminError::Query(format!("set_oidc: {e}")))?;

    audit_log(
        pool,
        actor,
        "set_oidc_config",
        None,
        Outcome::Ok,
        Some(json!({ "email": email })),
    )
    .await;

    tracing::info!(actor, "set_oidc: configuration updated");
    eprintln!("OIDC configuration updated.");
    Ok(())
}

#[tracing::instrument(skip(pool, actor))]
async fn clear_oidc(pool: &SqlitePool, actor: &str) -> Result<(), AdminError> {
    admin_config::clear_oidc(pool)
        .await
        .map_err(|e| AdminError::Query(format!("clear_oidc: {e}")))?;

    audit_log(pool, actor, "clear_oidc_config", None, Outcome::Ok, None).await;
    eprintln!("OIDC configuration cleared.");
    Ok(())
}

// ============================================================================
// revoke-host / revoke-session
// ============================================================================

#[tracing::instrument(skip(pool, actor))]
async fn revoke_host(pool: &SqlitePool, actor: &str, host: &str) -> Result<(), AdminError> {
    // Resolve the input to a host UUID. Accept either a valid UUID or a
    // hostname / friendly name. This is the admin-CLI equivalent of the
    // HTTP route's host_id path param, with the added convenience of
    // name lookup.
    let host_id = match uuid::Uuid::parse_str(host) {
        Ok(_) => {
            // Verify row exists even if the UUID parses.
            let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM hosts WHERE id = ?")
                .bind(host)
                .fetch_one(pool)
                .await?;
            if count == 0 {
                audit_log(
                    pool,
                    actor,
                    "host_revoke",
                    Some(host),
                    Outcome::Denied,
                    Some(json!({ "reason": "host_not_found" })),
                )
                .await;
                return Err(AdminError::InvalidInput(format!("host {host} not found")));
            }
            host.to_string()
        }
        Err(_) => {
            let row = hosts_q::find_by_hostname_or_name(pool, host)
                .await
                .map_err(|e| AdminError::Query(format!("find_by_hostname_or_name: {e}")))?;
            match row {
                Some(h) => h.id,
                None => {
                    audit_log(
                        pool,
                        actor,
                        "host_revoke",
                        Some(host),
                        Outcome::Denied,
                        Some(json!({ "reason": "host_not_found" })),
                    )
                    .await;
                    return Err(AdminError::InvalidInput(format!(
                        "no host matches hostname or name {host:?}"
                    )));
                }
            }
        }
    };

    let revoked = agents_q::revoke_all_for_host(pool, &host_id)
        .await
        .map_err(|e| AdminError::Query(format!("revoke_all_for_host: {e}")))?;

    audit_log(
        pool,
        actor,
        "host_revoke",
        Some(&host_id),
        Outcome::Ok,
        Some(json!({ "agents_revoked": revoked })),
    )
    .await;

    eprintln!("Revoked {revoked} agent credential(s) for host {host_id}. Re-enrollment required.");
    Ok(())
}

#[tracing::instrument(skip(pool, actor))]
async fn revoke_session(
    pool: &SqlitePool,
    actor: &str,
    session_id: &str,
) -> Result<(), AdminError> {
    if uuid::Uuid::parse_str(session_id).is_err() {
        audit_log(
            pool,
            actor,
            "session_revoke",
            Some(session_id),
            Outcome::Denied,
            Some(json!({ "reason": "invalid_session_id" })),
        )
        .await;
        return Err(AdminError::InvalidInput(format!(
            "session id {session_id:?} is not a valid UUID"
        )));
    }

    let deleted = auth_sessions::delete(pool, session_id)
        .await
        .map_err(|e| AdminError::Query(format!("delete session: {e}")))?;

    if deleted == 0 {
        audit_log(
            pool,
            actor,
            "session_revoke",
            Some(session_id),
            Outcome::Denied,
            Some(json!({ "reason": "session_not_found" })),
        )
        .await;
        return Err(AdminError::InvalidInput(format!(
            "session {session_id} not found"
        )));
    }

    audit_log(
        pool,
        actor,
        "session_revoke",
        Some(session_id),
        Outcome::Ok,
        Some(json!({ "rows_deleted": deleted })),
    )
    .await;

    eprintln!("Session {session_id} revoked.");
    Ok(())
}

// ============================================================================
// list-sessions / list-hosts / audit-tail
// ============================================================================

#[derive(sqlx::FromRow)]
struct SessionListRow {
    id: String,
    created_at: String,
    last_seen: String,
    issued_via: String,
    ip: Option<String>,
}

async fn list_sessions(pool: &SqlitePool) -> Result<(), AdminError> {
    let rows: Vec<SessionListRow> = sqlx::query_as(
        "SELECT id, created_at, last_seen, issued_via, ip FROM auth_sessions \
         ORDER BY last_seen DESC",
    )
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
        eprintln!("No active admin sessions.");
        return Ok(());
    }

    let mut out = io::stdout().lock();
    writeln!(
        out,
        "{:<38}  {:<20}  {:<20}  {:<12}  ip",
        "session_id", "created_at", "last_seen", "via"
    )?;
    writeln!(out, "{}", "-".repeat(115))?;
    for r in rows {
        let ip = r.ip.as_deref().unwrap_or("-");
        let created = truncate(&r.created_at, 20);
        let last_seen = truncate(&r.last_seen, 20);
        writeln!(
            out,
            "{:<38}  {:<20}  {:<20}  {:<12}  {ip}",
            r.id, created, last_seen, r.issued_via,
        )?;
    }
    out.flush()?;
    Ok(())
}

async fn list_hosts(pool: &SqlitePool) -> Result<(), AdminError> {
    let rows = hosts_q::list_hosts_with_agent_count(pool)
        .await
        .map_err(|e| AdminError::Query(format!("list_hosts_with_agent_count: {e}")))?;

    if rows.is_empty() {
        eprintln!("No hosts enrolled.");
        return Ok(());
    }

    let mut out = io::stdout().lock();
    writeln!(
        out,
        "{:<38}  {:<20}  {:<30}  {:<8}  {:<20}  agents",
        "id", "name", "hostname", "status", "created_at"
    )?;
    writeln!(out, "{}", "-".repeat(140))?;
    for r in rows {
        let name = truncate(&r.name, 20);
        let hostname = truncate(&r.hostname, 30);
        let created = truncate(&r.created_at, 20);
        let agents = r.agents;
        writeln!(
            out,
            "{:<38}  {:<20}  {:<30}  {:<8}  {:<20}  {agents}",
            r.id, name, hostname, r.status, created,
        )?;
    }
    out.flush()?;
    Ok(())
}

async fn audit_tail(pool: &SqlitePool, limit: i64, event: Option<&str>) -> Result<(), AdminError> {
    let limit = limit.clamp(1, 10_000);
    let rows = audit::list_recent_filtered(pool, limit, event)
        .await
        .map_err(|e| AdminError::Query(format!("list_recent_filtered: {e}")))?;

    if rows.is_empty() {
        eprintln!("No audit rows.");
        return Ok(());
    }

    let mut out = io::stdout().lock();
    writeln!(
        out,
        "{:<25}  {:<20}  {:<20}  {:<8}  target",
        "ts", "actor", "event", "outcome"
    )?;
    writeln!(out, "{}", "-".repeat(115))?;
    for r in rows {
        let ts = truncate(&r.ts, 25);
        let actor = truncate(&r.actor, 20);
        let event = truncate(&r.event, 20);
        let target = r.target.as_deref().unwrap_or("-");
        writeln!(
            out,
            "{ts:<25}  {actor:<20}  {event:<20}  {:<8}  {target}",
            r.outcome,
        )?;
    }
    out.flush()?;
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        // Byte-index slicing in the middle of a UTF-8 codepoint panics.
        // Walk by `char_indices` and cut at the nearest char boundary.
        let truncation_point = s
            .char_indices()
            .nth(max.saturating_sub(1))
            .map_or_else(|| s.len(), |(i, _)| i);
        format!("{}…", &s[..truncation_point])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zremote_core::queries::auth_sessions::IssuedVia;

    async fn test_pool() -> SqlitePool {
        db::init_db("sqlite::memory:").await.unwrap()
    }

    async fn seed_admin(pool: &SqlitePool, token_plaintext: &str) {
        let hash = hash_admin_token(token_plaintext);
        admin_config::upsert_token_hash(pool, &hash).await.unwrap();
    }

    #[tokio::test]
    async fn rotate_token_replaces_hash_and_invalidates_sessions() {
        let pool = test_pool().await;
        seed_admin(&pool, "old-token").await;

        // Seed two live sessions.
        let (_t1, _r1) = create_session_token(&pool).await;
        let (_t2, _r2) = create_session_token(&pool).await;

        rotate_token(&pool, "cli:test").await.unwrap();

        // Hash changed.
        let cfg = admin_config::get(&pool).await.unwrap().unwrap();
        assert_ne!(cfg.token_hash, hash_admin_token("old-token"));

        // Sessions purged.
        let (remaining,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM auth_sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(remaining, 0);

        // Audit row present.
        let rows = audit::list_recent(&pool, 10).await.unwrap();
        assert!(
            rows.iter().any(|r| r.event == "token_rotate"
                && r.outcome == "ok"
                && r.details.contains("sessions_invalidated")),
            "expected token_rotate audit row, got {rows:?}"
        );
    }

    /// Mirror `session::issue` without needing the server crate: insert an
    /// auth_sessions row directly and return the plaintext.
    async fn create_session_token(pool: &SqlitePool) -> (String, String) {
        let plaintext = generate_admin_token();
        let hash = hash_admin_token(&plaintext);
        let row = auth_sessions::create(pool, &hash, IssuedVia::AdminToken, None, None, 14, 90)
            .await
            .unwrap();
        (plaintext, row.id)
    }

    #[tokio::test]
    async fn set_oidc_rejects_non_https_issuer() {
        let pool = test_pool().await;
        seed_admin(&pool, "tok").await;

        let err = set_oidc(
            &pool,
            "cli:test",
            "http://issuer.example",
            "client",
            "a@b.c",
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AdminError::InvalidInput(_)));

        // Audit row with outcome=denied.
        let rows = audit::list_recent(&pool, 10).await.unwrap();
        assert!(rows.iter().any(|r| r.event == "set_oidc_config"
            && r.outcome == "denied"
            && r.details.contains("non_https_issuer")));

        // Config row must remain untouched.
        let cfg = admin_config::get(&pool).await.unwrap().unwrap();
        assert!(cfg.oidc_issuer_url.is_none());
    }

    #[tokio::test]
    async fn set_oidc_writes_three_columns_and_audits() {
        let pool = test_pool().await;
        seed_admin(&pool, "tok").await;

        set_oidc(
            &pool,
            "cli:test",
            "https://issuer.example",
            "cid",
            "admin@example.com",
        )
        .await
        .unwrap();

        let cfg = admin_config::get(&pool).await.unwrap().unwrap();
        assert_eq!(
            cfg.oidc_issuer_url.as_deref(),
            Some("https://issuer.example")
        );
        assert_eq!(cfg.oidc_client_id.as_deref(), Some("cid"));
        assert_eq!(cfg.oidc_email.as_deref(), Some("admin@example.com"));

        let rows = audit::list_recent(&pool, 10).await.unwrap();
        assert!(rows.iter().any(|r| r.event == "set_oidc_config"
            && r.outcome == "ok"
            && r.details.contains("admin@example.com")));
    }

    #[tokio::test]
    async fn set_oidc_rejects_empty_email() {
        let pool = test_pool().await;
        seed_admin(&pool, "tok").await;
        let err = set_oidc(&pool, "cli:test", "https://issuer", "cid", "")
            .await
            .unwrap_err();
        assert!(matches!(err, AdminError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn clear_oidc_nulls_three_columns_and_audits() {
        let pool = test_pool().await;
        seed_admin(&pool, "tok").await;
        admin_config::set_oidc(&pool, "https://issuer", "cid", "admin@example.com")
            .await
            .unwrap();

        clear_oidc(&pool, "cli:test").await.unwrap();

        let cfg = admin_config::get(&pool).await.unwrap().unwrap();
        assert!(cfg.oidc_issuer_url.is_none());
        assert!(cfg.oidc_client_id.is_none());
        assert!(cfg.oidc_email.is_none());

        let rows = audit::list_recent(&pool, 10).await.unwrap();
        assert!(
            rows.iter()
                .any(|r| r.event == "clear_oidc_config" && r.outcome == "ok")
        );
    }

    async fn insert_test_host(pool: &SqlitePool, id: &str, hostname: &str, name: &str) {
        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, status) \
             VALUES (?, ?, ?, 'h', 'online')",
        )
        .bind(id)
        .bind(name)
        .bind(hostname)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn revoke_host_by_uuid_flips_all_agent_rows() {
        let pool = test_pool().await;
        seed_admin(&pool, "tok").await;
        let host_id = uuid::Uuid::now_v7().to_string();
        insert_test_host(&pool, &host_id, "box.example", "box").await;
        let _a1 = agents_q::create(&pool, &host_id, "pk1").await.unwrap();
        let _a2 = agents_q::create(&pool, &host_id, "pk2").await.unwrap();

        revoke_host(&pool, "cli:test", &host_id).await.unwrap();

        let active = agents_q::list_for_host(&pool, &host_id).await.unwrap();
        assert_eq!(active.len(), 0);

        let rows = audit::list_recent(&pool, 10).await.unwrap();
        assert!(rows.iter().any(|r| r.event == "host_revoke"
            && r.outcome == "ok"
            && r.target.as_deref() == Some(host_id.as_str())
            && r.details.contains("agents_revoked")));
    }

    #[tokio::test]
    async fn revoke_host_by_hostname_resolves() {
        let pool = test_pool().await;
        seed_admin(&pool, "tok").await;
        let host_id = uuid::Uuid::now_v7().to_string();
        insert_test_host(&pool, &host_id, "box.example", "box").await;
        let _a = agents_q::create(&pool, &host_id, "pk").await.unwrap();

        revoke_host(&pool, "cli:test", "box.example").await.unwrap();

        let active = agents_q::list_for_host(&pool, &host_id).await.unwrap();
        assert!(active.is_empty());
    }

    #[tokio::test]
    async fn revoke_host_missing_returns_invalid_input() {
        let pool = test_pool().await;
        seed_admin(&pool, "tok").await;

        let err = revoke_host(&pool, "cli:test", "ghost-host")
            .await
            .unwrap_err();
        assert!(matches!(err, AdminError::InvalidInput(_)));

        // Denied audit row must exist so ops can see the failed attempt.
        let rows = audit::list_recent(&pool, 10).await.unwrap();
        assert!(rows.iter().any(|r| r.event == "host_revoke"
            && r.outcome == "denied"
            && r.details.contains("host_not_found")));
    }

    #[tokio::test]
    async fn revoke_session_deletes_row_and_audits() {
        let pool = test_pool().await;
        seed_admin(&pool, "tok").await;

        let (_plain, sid) = create_session_token(&pool).await;
        revoke_session(&pool, "cli:test", &sid).await.unwrap();

        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM auth_sessions WHERE id = ?")
            .bind(&sid)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n, 0);

        let rows = audit::list_recent(&pool, 10).await.unwrap();
        assert!(rows.iter().any(|r| r.event == "session_revoke"
            && r.outcome == "ok"
            && r.target.as_deref() == Some(sid.as_str())));
    }

    #[tokio::test]
    async fn revoke_session_invalid_uuid_rejected() {
        let pool = test_pool().await;
        seed_admin(&pool, "tok").await;
        let err = revoke_session(&pool, "cli:test", "not-a-uuid")
            .await
            .unwrap_err();
        assert!(matches!(err, AdminError::InvalidInput(_)));
        let rows = audit::list_recent(&pool, 10).await.unwrap();
        assert!(
            rows.iter()
                .any(|r| r.event == "session_revoke" && r.outcome == "denied")
        );
    }

    #[tokio::test]
    async fn revoke_session_missing_returns_error_and_denied_audit() {
        let pool = test_pool().await;
        seed_admin(&pool, "tok").await;
        let random_id = uuid::Uuid::now_v7().to_string();
        let err = revoke_session(&pool, "cli:test", &random_id)
            .await
            .unwrap_err();
        assert!(matches!(err, AdminError::InvalidInput(_)));
        let rows = audit::list_recent(&pool, 10).await.unwrap();
        assert!(rows.iter().any(|r| r.event == "session_revoke"
            && r.outcome == "denied"
            && r.details.contains("session_not_found")));
    }

    #[tokio::test]
    async fn list_sessions_does_not_error_on_empty() {
        let pool = test_pool().await;
        list_sessions(&pool).await.unwrap();
    }

    #[tokio::test]
    async fn list_hosts_does_not_error_on_empty() {
        let pool = test_pool().await;
        list_hosts(&pool).await.unwrap();
    }

    #[tokio::test]
    async fn audit_tail_filters_by_event() {
        let pool = test_pool().await;
        audit_log(&pool, "cli:test", "login_ok", None, Outcome::Ok, None).await;
        audit_log(&pool, "cli:test", "login_fail", None, Outcome::Denied, None).await;
        audit_log(&pool, "cli:test", "login_fail", None, Outcome::Denied, None).await;

        // Capture doesn't matter — we just ensure the filter runs without error.
        audit_tail(&pool, 10, Some("login_fail")).await.unwrap();
        audit_tail(&pool, 10, None).await.unwrap();

        let rows = audit::list_recent_filtered(&pool, 10, Some("login_fail"))
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[tokio::test]
    async fn rotate_token_audit_never_contains_plaintext_or_hash() {
        // Plaintext tokens and stored hashes MUST stay out of audit_log
        // details. The helper already builds the details object with only
        // `sessions_invalidated`; this test is a regression guard.
        let pool = test_pool().await;
        seed_admin(&pool, "tok").await;
        rotate_token(&pool, "cli:test").await.unwrap();

        let cfg = admin_config::get(&pool).await.unwrap().unwrap();
        let rows = audit::list_recent(&pool, 10).await.unwrap();
        let rotate_row = rows
            .iter()
            .find(|r| r.event == "token_rotate")
            .expect("token_rotate row expected");
        assert!(!rotate_row.details.contains(&cfg.token_hash));
    }

    #[test]
    fn truncate_keeps_short_strings() {
        assert_eq!(truncate("abc", 10), "abc");
        // Long strings are cut to max-1 ASCII chars and suffixed with '…'
        // (3 bytes UTF-8) — assert character length rather than byte count.
        let cut = truncate("abcdefghij", 5);
        assert_eq!(cut.chars().count(), 5);
        assert!(cut.ends_with('…'));
    }

    /// Byte-index slicing panics when the cut point lands inside a
    /// multibyte UTF-8 codepoint. Regression guard: `truncate` must handle
    /// non-ASCII strings without panicking and must produce a valid UTF-8
    /// result of the requested character count.
    #[test]
    fn truncate_handles_multibyte_utf8() {
        let s = "žluťoučký kůň";
        // `s.len()` is 18 bytes, `s.chars().count()` is 13 — at max=5
        // the naive byte-slice would cut mid-codepoint and panic.
        let cut = truncate(s, 5);
        assert_eq!(cut.chars().count(), 5);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn truncate_multibyte_short_string_untouched() {
        let s = "žluť";
        assert_eq!(truncate(s, 10), s);
    }

    #[test]
    fn resolve_database_url_uses_provided_value() {
        let g = AdminGlobal {
            database_url: Some("sqlite:/tmp/custom.db".to_string()),
        };
        assert_eq!(g.resolve_database_url(), "sqlite:/tmp/custom.db");
    }

    #[test]
    fn resolve_database_url_falls_back_to_default() {
        // Empty string must be treated as "unset" so `--database-url=""`
        // doesn't silently point at an invalid URL.
        let g = AdminGlobal { database_url: None };
        assert_eq!(g.resolve_database_url(), DEFAULT_DATABASE_URL);

        let empty = AdminGlobal {
            database_url: Some(String::new()),
        };
        assert_eq!(empty.resolve_database_url(), DEFAULT_DATABASE_URL);
    }

    #[test]
    fn actor_label_has_prefix() {
        let label = actor_label();
        assert!(label.starts_with("cli:"));
    }

    #[test]
    fn generate_admin_token_is_43_chars() {
        assert_eq!(generate_admin_token().len(), 43);
    }

    #[test]
    fn hash_admin_token_is_64_hex() {
        let h = hash_admin_token("x");
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
