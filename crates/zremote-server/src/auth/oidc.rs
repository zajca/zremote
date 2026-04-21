//! OIDC login primitives (RFC auth-overhaul §Phase 2, Method B).
//!
//! Implements the Authorization-Code-with-PKCE flow against a single
//! admin-configured OIDC issuer. The server never accepts an access token or
//! an unverified ID token as proof of identity — every successful login goes
//! through:
//!
//! 1. Discovery of the provider metadata (`.well-known/openid-configuration`).
//! 2. Client-side PKCE S256 challenge + random `state` + random `nonce` are
//!    generated, saved in an in-memory [`OidcFlowStore`] keyed by `state`,
//!    and returned to the GUI alongside the authorization URL.
//! 3. GUI shells out to the system browser, user authenticates, provider
//!    redirects back to the GUI's loopback listener
//!    (`http://127.0.0.1:<gui-port>/oidc/callback?code=…&state=…`).
//! 4. GUI forwards `{ code, state }` to `POST /api/auth/oidc/callback`. The
//!    server redeems the state (single-use), exchanges `code` +
//!    `pkce_verifier` for a token response, and verifies the ID token's
//!    signature (via JWKS fetched during discovery), `iss` (matches the
//!    issuer URL in `admin_config`), `aud` (matches the configured
//!    `client_id`), `exp` / `nbf` (with a small clock-skew allowance
//!    provided by the verifier), and `nonce` (matches the one we stashed
//!    in step 2).
//! 5. Finally, the server constant-time-compares the `email` claim (falling
//!    back to `preferred_username` only if `email` is absent and the
//!    `preferred_username` value syntactically looks like an email) against
//!    `admin_config.oidc_email`. Only on *both* signature-valid ID token
//!    *and* allowlisted email does the caller receive a session token.
//!
//! **Oracle-collapse (RFC T-5):** every failure branch here — unknown
//! state, PKCE mismatch, expired flow, token-exchange error, any
//! `ClaimsVerificationError`, email mismatch — collapses at the HTTP
//! boundary into the same opaque `401 { "error": "unauthorized" }` with the
//! [`crate::auth_mw::AUTH_FAIL_MIN_LATENCY`] floor. The internal
//! [`OidcError`] variants exist solely for server-side audit precision.
//!
//! **SSRF guard (openidconnect 4.x security note):** the reqwest client
//! this module ships with is built with
//! `redirect(reqwest::redirect::Policy::none())`. Following redirects on
//! the discovery/JWKS/token endpoint would let a malicious provider
//! coerce the server into reaching arbitrary internal URLs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use openidconnect::core::{
    CoreAuthenticationFlow, CoreClient, CoreGenderClaim, CoreIdTokenClaims, CoreIdTokenVerifier,
    CoreProviderMetadata,
};
use openidconnect::reqwest::Client as OidcReqwestClient;
use openidconnect::{
    AuthorizationCode, ClientId, CsrfToken, EmptyAdditionalClaims, IdTokenClaims, IssuerUrl, Nonce,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, url,
};
use subtle::ConstantTimeEq;

/// Maximum time between `/api/auth/oidc/init` and `/api/auth/oidc/callback`.
/// The OIDC spec does not mandate a particular value; browsers typically
/// complete a login within a minute. Ten minutes leaves headroom for a
/// user with a password manager interstitial or MFA prompt while keeping
/// the in-memory store bounded.
pub const OIDC_FLOW_TTL: Duration = Duration::from_secs(600);

/// Hard cap on the number of concurrent in-flight OIDC login flows. The
/// governor on `/api/auth/*` keeps this small in practice, but we want a
/// defence-in-depth ceiling: even if the rate limiter fails open, a
/// runaway client cannot force the map to grow without bound. Same
/// philosophy as [`crate::auth::ws_ticket::MAX_TICKETS`]; sized smaller
/// because OIDC login is a much lower-volume path.
pub const MAX_OIDC_FLOWS: usize = 256;

/// One in-flight OIDC login flow. Stored in [`OidcFlowStore`] until the
/// matching callback arrives or the entry expires.
#[derive(Debug)]
pub struct OidcFlowEntry {
    /// PKCE verifier bound to the challenge in the authorization URL.
    pub pkce_verifier: PkceCodeVerifier,
    /// Nonce echoed by the IdP in the ID token's `nonce` claim.
    pub nonce: Nonce,
    /// Redirect URI the GUI listens on. Sent during authorize *and*
    /// required exactly for the token-exchange call.
    pub redirect_uri: RedirectUrl,
    /// Expiration instant; entries past this are swept on lookup.
    pub expires_at: Instant,
}

/// In-memory store of in-flight OIDC flows, keyed by `state`. Cloneable:
/// the inner `Mutex<HashMap>` is wrapped in `Arc` so clones share state
/// the way `TicketStore` does.
#[derive(Clone)]
pub struct OidcFlowStore {
    inner: Arc<Mutex<HashMap<String, OidcFlowEntry>>>,
    ttl: Duration,
    max_entries: usize,
}

impl Default for OidcFlowStore {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for OidcFlowStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OidcFlowStore")
            .field("ttl", &self.ttl)
            .field("max_entries", &self.max_entries)
            .finish_non_exhaustive()
    }
}

impl OidcFlowStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            ttl: OIDC_FLOW_TTL,
            max_entries: MAX_OIDC_FLOWS,
        }
    }

    /// Test-only constructor to exercise TTL + saturation without waiting
    /// ten minutes or churning a 256-entry map.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_caps(ttl: Duration, max_entries: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            ttl,
            max_entries,
        }
    }

    /// Insert an entry for a freshly-minted login flow. Returns
    /// [`OidcError::Full`] if the store is saturated with live entries
    /// after a sweep of the expired ones — that's a DoS guard, not an
    /// attacker-triggered failure on a single request.
    pub fn insert(
        &self,
        state: &CsrfToken,
        pkce_verifier: PkceCodeVerifier,
        nonce: Nonce,
        redirect_uri: RedirectUrl,
    ) -> Result<(), OidcError> {
        let mut guard = self
            .inner
            .lock()
            .expect("oidc flow store lock poisoned — fail closed");
        if guard.len() >= self.max_entries {
            let now = Instant::now();
            guard.retain(|_, e| e.expires_at > now);
            if guard.len() >= self.max_entries {
                return Err(OidcError::Full);
            }
        }
        guard.insert(
            state.secret().clone(),
            OidcFlowEntry {
                pkce_verifier,
                nonce,
                redirect_uri,
                expires_at: Instant::now() + self.ttl,
            },
        );
        Ok(())
    }

    /// Redeem the flow associated with `state`. Removes the row on lookup
    /// (single-use semantics). Returns [`OidcError::UnknownState`] for
    /// missing and [`OidcError::Expired`] for past-TTL — both collapse to
    /// the same 401 at the HTTP boundary, but they are kept distinct here
    /// for auditing.
    pub fn redeem(&self, state: &str) -> Result<OidcFlowEntry, OidcError> {
        let entry = {
            let mut guard = self
                .inner
                .lock()
                .expect("oidc flow store lock poisoned — fail closed");
            guard.remove(state)
        };
        let entry = entry.ok_or(OidcError::UnknownState)?;
        if Instant::now() >= entry.expires_at {
            return Err(OidcError::Expired);
        }
        Ok(entry)
    }

    /// Drop all entries that have expired. Intended for periodic sweeps
    /// by a future maintenance task; not required for correctness because
    /// `insert` sweeps on saturation and `redeem` checks on lookup.
    pub fn purge_expired(&self) -> usize {
        let now = Instant::now();
        let mut guard = self
            .inner
            .lock()
            .expect("oidc flow store lock poisoned — fail closed");
        let before = guard.len();
        guard.retain(|_, entry| entry.expires_at > now);
        before - guard.len()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .expect("oidc flow store lock poisoned — fail closed")
            .len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Static OIDC configuration required to initialize a login. Populated from
/// `admin_config` on each request path — we never cache issuer/client_id
/// into a long-lived struct because the admin may rotate them.
#[derive(Debug, Clone)]
pub struct OidcConfig {
    pub issuer_url: String,
    pub client_id: String,
    /// The one permitted email. Constant-time-compared against the `email`
    /// claim post-verification.
    pub allowed_email: String,
}

/// Result of a successful `/api/auth/oidc/init` call. `auth_url` is the
/// URL the browser should open; `state` is the CSRF token the GUI must
/// forward back verbatim in the callback.
#[derive(Debug)]
pub struct InitiatedFlow {
    pub auth_url: url::Url,
    pub state: String,
}

/// Internal error taxonomy. See module-level doc for the oracle-collapse
/// policy: the HTTP layer maps every variant except [`OidcError::Full`] to
/// the same 401.
#[derive(Debug)]
pub enum OidcError {
    /// Provider metadata (`.well-known/openid-configuration`) could not be
    /// fetched or parsed. Most often this means the admin mistyped the
    /// issuer URL or the IdP is down.
    Discovery(String),
    /// `issuer_url` / `client_id` / `redirect_uri` / `email` failed a
    /// local parse before we ever reached the network.
    Configuration(String),
    /// Token-exchange request failed. Includes network errors and OIDC
    /// error responses.
    TokenExchange(String),
    /// Token response did not carry an `id_token` at all. Most real IdPs
    /// always return one, so this usually means a misconfigured client
    /// (e.g. missing `openid` scope, which we add automatically; or the
    /// IdP returning a non-OIDC response).
    MissingIdToken,
    /// Signature / iss / aud / exp / nbf / nonce verification failed.
    /// Returned by `openidconnect::IdToken::claims`.
    ClaimsVerification(String),
    /// ID token carries no `email` (or the claim is blank) and
    /// `preferred_username` also lacks an email-shaped value.
    MissingEmail,
    /// `email` present + verified, but does not match
    /// `admin_config.oidc_email`. Constant-time compared.
    EmailMismatch,
    /// Callback arrived with a `state` we do not recognise — either never
    /// issued, forged by an attacker, or previously redeemed.
    UnknownState,
    /// Callback arrived after [`OIDC_FLOW_TTL`]. Single-use redemption
    /// already removed the row.
    Expired,
    /// Too many concurrent flows. Server-health condition; never
    /// attacker-triggerable from a single request (rate limiter caps
    /// burst; see [`crate::rate_limit`]).
    Full,
}

impl std::fmt::Display for OidcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Discovery(m) => write!(f, "oidc discovery failed: {m}"),
            Self::Configuration(m) => write!(f, "oidc configuration error: {m}"),
            Self::TokenExchange(m) => write!(f, "oidc token exchange failed: {m}"),
            Self::MissingIdToken => write!(f, "oidc token response missing id_token"),
            Self::ClaimsVerification(m) => write!(f, "oidc claims verification failed: {m}"),
            Self::MissingEmail => write!(f, "oidc id_token carries no email-shaped claim"),
            Self::EmailMismatch => write!(f, "oidc email does not match admin_config.oidc_email"),
            Self::UnknownState => write!(f, "oidc callback state unknown"),
            Self::Expired => write!(f, "oidc flow expired"),
            Self::Full => write!(f, "oidc flow store is full"),
        }
    }
}

impl std::error::Error for OidcError {}

/// Build the reqwest client used for every OIDC HTTP call. Redirects are
/// disabled (SSRF guard — see module-level doc). A 10-second per-request
/// timeout bounds discovery / JWKS / token calls so a wedged IdP cannot
/// pin an OIDC flow indefinitely; the flow store TTL would sweep the
/// entry anyway, but without a request timeout the task holding the
/// connection stays alive.
fn build_reqwest_client() -> Result<OidcReqwestClient, OidcError> {
    OidcReqwestClient::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| OidcError::Discovery(format!("reqwest client build failed: {e}")))
}

/// Parse the configured URLs / IDs before we hit the network. Keeps the
/// init path synchronous-failure on bad admin input.
fn parse_config(
    config: &OidcConfig,
    redirect_uri: &str,
) -> Result<(IssuerUrl, ClientId, RedirectUrl), OidcError> {
    let issuer = IssuerUrl::new(config.issuer_url.clone())
        .map_err(|e| OidcError::Configuration(format!("issuer_url: {e}")))?;
    // Production issuers must be HTTPS. Discovery, JWKS, and the token
    // endpoint all flow over this origin, so an `http://` issuer would
    // let a network-positioned attacker swap JWKS or tokens. The `cfg(test)`
    // escape lets unit tests point at `httpmock`'s `http://127.0.0.1:N`
    // without weakening production.
    #[cfg(not(test))]
    if issuer.url().scheme() != "https" {
        return Err(OidcError::Configuration(
            "issuer_url must use https".to_string(),
        ));
    }
    let client_id = ClientId::new(config.client_id.clone());
    let redirect = RedirectUrl::new(redirect_uri.to_string())
        .map_err(|e| OidcError::Configuration(format!("redirect_uri: {e}")))?;
    Ok((issuer, client_id, redirect))
}

/// Fetch provider metadata via discovery. Shared by [`init`] and
/// [`complete`]; kept as a standalone fn so the reqwest client /
/// redirect-disabling / error-mapping live in one place.
async fn fetch_metadata(
    issuer: IssuerUrl,
    http: &OidcReqwestClient,
) -> Result<CoreProviderMetadata, OidcError> {
    CoreProviderMetadata::discover_async(issuer, http)
        .await
        .map_err(|e| OidcError::Discovery(format!("{e}")))
}

/// Start a new OIDC login. Stores the PKCE verifier + nonce + redirect URI
/// in `store`, keyed by the returned `state`, and returns the auth URL the
/// caller should open in the browser.
pub async fn init(
    config: &OidcConfig,
    redirect_uri: &str,
    store: &OidcFlowStore,
) -> Result<InitiatedFlow, OidcError> {
    let http = build_reqwest_client()?;
    let (issuer, client_id, redirect) = parse_config(config, redirect_uri)?;
    let metadata = fetch_metadata(issuer, &http).await?;
    let client = CoreClient::from_provider_metadata(metadata, client_id, None)
        .set_redirect_uri(redirect.clone());

    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
    let (auth_url, csrf_state, nonce) = client
        .authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        )
        .add_scope(Scope::new("email".to_string()))
        .add_scope(Scope::new("profile".to_string()))
        .set_pkce_challenge(pkce_challenge)
        .url();

    let state_token = csrf_state.secret().clone();
    store.insert(&csrf_state, pkce_verifier, nonce, redirect)?;

    Ok(InitiatedFlow {
        auth_url,
        state: state_token,
    })
}

/// Verified identity emitted on a successful OIDC login. The `email` field
/// is what the caller will use to mint the session; everything else
/// remains internal to this module.
#[derive(Debug, Clone)]
pub struct VerifiedIdentity {
    pub email: String,
}

/// Complete a login: redeem the flow, exchange `code`, verify the ID
/// token, enforce the email allowlist.
pub async fn complete(
    config: &OidcConfig,
    code: &str,
    state: &str,
    store: &OidcFlowStore,
) -> Result<VerifiedIdentity, OidcError> {
    let flow = store.redeem(state)?;
    let redirect_uri_str = flow.redirect_uri.to_string();

    let http = build_reqwest_client()?;
    let (issuer, client_id, redirect) = parse_config(config, &redirect_uri_str)?;
    let metadata = fetch_metadata(issuer, &http).await?;
    let client =
        CoreClient::from_provider_metadata(metadata, client_id, None).set_redirect_uri(redirect);

    let token_response = client
        .exchange_code(AuthorizationCode::new(code.to_string()))
        .map_err(|e| OidcError::Configuration(format!("exchange_code: {e}")))?
        .set_pkce_verifier(flow.pkce_verifier)
        .request_async(&http)
        .await
        .map_err(|e| OidcError::TokenExchange(format!("{e}")))?;

    let id_token = token_response
        .extra_fields()
        .id_token()
        .ok_or(OidcError::MissingIdToken)?;

    let verifier: CoreIdTokenVerifier = client.id_token_verifier();
    let claims: &CoreIdTokenClaims = id_token
        .claims(&verifier, &flow.nonce)
        .map_err(|e| OidcError::ClaimsVerification(format!("{e}")))?;

    let extracted = extract_email(claims).ok_or(OidcError::MissingEmail)?;
    if ct_eq_email(&extracted, &config.allowed_email) {
        Ok(VerifiedIdentity { email: extracted })
    } else {
        Err(OidcError::EmailMismatch)
    }
}

/// Pull a usable email out of the verified claims. Prefers the standard
/// `email` claim; falls back to `preferred_username` only if it parses as
/// a syntactic email. The fallback exists because some enterprise IdPs
/// (notably Azure AD B2C defaults, some Ping tenants) do not return an
/// `email` claim unless a separate API permission is granted, but do
/// return the login-email in `preferred_username`. We still apply the
/// same constant-time allowlist check, so the fallback only widens the
/// source of the comparison — never what it compares against.
fn extract_email(claims: &IdTokenClaims<EmptyAdditionalClaims, CoreGenderClaim>) -> Option<String> {
    // If the IdP chooses to include `email_verified`, honour it: an IdP
    // saying "we accepted this email but did not verify ownership" is not
    // good enough to bind a ZRemote session. If the claim is absent we
    // fall back to accepting the email (many enterprise IdPs omit it when
    // the directory is authoritative), but when present it must be true.
    if matches!(claims.email_verified(), Some(false)) {
        return None;
    }
    if let Some(email) = claims.email() {
        let s: &str = email.as_str();
        if !s.is_empty() {
            return Some(s.to_string());
        }
    }
    if let Some(username) = claims.preferred_username() {
        let s: &str = username.as_str();
        if !s.is_empty() && is_email_shaped(s) {
            return Some(s.to_string());
        }
    }
    None
}

/// Cheap "is this an email" check. Not a full RFC 5321 parse — it only
/// has to be strict enough that `preferred_username` values like
/// `john.doe` (no `@`) are rejected, so that we never trust a
/// non-email `preferred_username` against an email-shaped allowlist.
fn is_email_shaped(s: &str) -> bool {
    let mut parts = s.splitn(2, '@');
    let local = parts.next().unwrap_or("");
    let domain = parts.next().unwrap_or("");
    !local.is_empty() && !domain.is_empty() && domain.contains('.')
}

/// Constant-time comparison of two emails after ASCII-lowercasing.
/// Email-address comparison is case-insensitive in the local-part (per
/// RFC 5321 §2.4, sort of — spec-wise the local-part *could* be case
/// sensitive, but no real IdP relies on that and doing a case-sensitive
/// match here would lock out users whose IdP capitalises their email
/// differently from what the admin typed). We ASCII-lowercase both
/// sides before the `ct_eq` so the comparison is still constant-time
/// *and* canonicalised.
fn ct_eq_email(a: &str, b: &str) -> bool {
    let a_lower = a.to_ascii_lowercase();
    let b_lower = b.to_ascii_lowercase();
    if a_lower.len() != b_lower.len() {
        return false;
    }
    a_lower.as_bytes().ct_eq(b_lower.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_email_shaped_accepts_typical_emails() {
        assert!(is_email_shaped("admin@example.com"));
        assert!(is_email_shaped("a.b+tag@sub.example.co.uk"));
    }

    #[test]
    fn is_email_shaped_rejects_non_emails() {
        assert!(!is_email_shaped("johndoe"));
        assert!(!is_email_shaped("johndoe@localhost"));
        assert!(!is_email_shaped("@example.com"));
        assert!(!is_email_shaped("a@"));
        assert!(!is_email_shaped(""));
    }

    #[test]
    fn ct_eq_email_is_case_insensitive_ascii() {
        assert!(ct_eq_email("Admin@Example.Com", "admin@example.com"));
        assert!(ct_eq_email("admin@example.com", "ADMIN@EXAMPLE.COM"));
        assert!(!ct_eq_email("admin@example.com", "someone@example.com"));
        assert!(!ct_eq_email("admin@example.com", "admin@example.co"));
    }

    #[test]
    fn store_insert_redeem_round_trip() {
        let store = OidcFlowStore::new();
        let state = CsrfToken::new("s1".to_string());
        let verifier = PkceCodeVerifier::new("v1".to_string());
        let nonce = Nonce::new("n1".to_string());
        let redirect =
            RedirectUrl::new("http://127.0.0.1:12345/oidc/callback".to_string()).unwrap();

        store.insert(&state, verifier, nonce, redirect).unwrap();
        let entry = store.redeem(state.secret()).unwrap();
        assert_eq!(entry.nonce.secret(), "n1");
        // Single-use: second redeem fails.
        let err = store.redeem(state.secret()).unwrap_err();
        assert!(matches!(err, OidcError::UnknownState));
    }

    #[test]
    fn store_rejects_expired_entries() {
        let store = OidcFlowStore::with_caps(Duration::from_nanos(1), 4);
        let state = CsrfToken::new("expired".to_string());
        store
            .insert(
                &state,
                PkceCodeVerifier::new("v".to_string()),
                Nonce::new("n".to_string()),
                RedirectUrl::new("http://127.0.0.1:1/x".to_string()).unwrap(),
            )
            .unwrap();
        // Sleep so the entry is definitely past expiry.
        std::thread::sleep(Duration::from_millis(2));
        let err = store.redeem(state.secret()).unwrap_err();
        assert!(matches!(err, OidcError::Expired));
    }

    #[test]
    fn store_saturation_is_bounded() {
        let store = OidcFlowStore::with_caps(Duration::from_secs(60), 2);
        for i in 0..2 {
            store
                .insert(
                    &CsrfToken::new(format!("s{i}")),
                    PkceCodeVerifier::new("v".into()),
                    Nonce::new("n".into()),
                    RedirectUrl::new("http://127.0.0.1:1/x".into()).unwrap(),
                )
                .unwrap();
        }
        let err = store
            .insert(
                &CsrfToken::new("overflow".into()),
                PkceCodeVerifier::new("v".into()),
                Nonce::new("n".into()),
                RedirectUrl::new("http://127.0.0.1:1/x".into()).unwrap(),
            )
            .unwrap_err();
        assert!(matches!(err, OidcError::Full));
    }

    #[test]
    fn parse_config_rejects_invalid_urls() {
        let cfg = OidcConfig {
            issuer_url: "not-a-url".to_string(),
            client_id: "c".into(),
            allowed_email: "a@b.c".into(),
        };
        let err = parse_config(&cfg, "http://127.0.0.1:1/x").unwrap_err();
        assert!(matches!(err, OidcError::Configuration(_)));

        let cfg_ok = OidcConfig {
            issuer_url: "https://issuer.example".into(),
            client_id: "c".into(),
            allowed_email: "a@b.c".into(),
        };
        let err = parse_config(&cfg_ok, "not-a-redirect").unwrap_err();
        assert!(matches!(err, OidcError::Configuration(_)));

        parse_config(&cfg_ok, "http://127.0.0.1:1/x").unwrap();
    }
}

/// End-to-end OIDC tests spun up against an in-process `httpmock` IdP.
/// Exercises discovery → PKCE authorize → token exchange → JWKS verify →
/// email allowlist, plus every failure branch listed in the RFC test
/// matrix.
#[cfg(test)]
mod integration_tests {
    use super::*;
    use chrono::{Duration as ChronoDuration, Utc};
    use httpmock::MockServer;
    use openidconnect::core::{
        CoreIdToken, CoreIdTokenClaims, CoreJsonWebKey, CoreJsonWebKeySet, CoreJwsSigningAlgorithm,
        CoreProviderMetadata, CoreResponseType, CoreRsaPrivateSigningKey,
        CoreSubjectIdentifierType, CoreTokenResponse, CoreTokenType,
    };
    use openidconnect::{
        AccessToken, Audience, AuthUrl, EmptyAdditionalClaims, EmptyAdditionalProviderMetadata,
        EmptyExtraTokenFields, EndUserEmail, EndUserUsername, IdTokenFields, JsonWebKeyId,
        JsonWebKeySetUrl, PrivateSigningKey, ResponseTypes, StandardClaims, SubjectIdentifier,
        TokenUrl,
    };
    use std::time::Duration;

    /// Fixed RSA-2048 private key (PKCS#1 PEM) used to sign test ID tokens
    /// and derive the JWKS. Generated once via `openssl genrsa`; committed
    /// only because it's test-only, never loaded in production code paths,
    /// and regenerating per-test adds ~300 ms per case on cold runs.
    const TEST_RSA_PEM: &str = "-----BEGIN RSA PRIVATE KEY-----\n\
MIIEogIBAAKCAQEAoWTHvbh+uXGFw9N+BA8AekYGVbt8DCQeh9UHMter5hPlaHtA\n\
fAqKG7PQrNDb5UFzTRsT6qZUjkvM2oJWmKehHpUcvmCtltiLigaaySsJ/27+yNnv\n\
A4v8Y9xzEZPlCOq4RgPa3ALVryu5oiBfPbMX+BmsM70IZYCdlqXmG1jbN76tZg0M\n\
IvLWu4K5nQPex7tfp7Qzgzb389f0q/x/rvO9sK3MWbVe2ux/5XuajKDMWR3bNuBr\n\
9sXAbyJe/ugTXy6wT8UtHg8+SQ+9cMipvCmKOnTRRGegjhJ7aUVGQsoleQ6XKNZo\n\
qIHSRpzUJe30K1fzq+0vkiI6Hp04mpFjn3HQNwIDAQABAoIBADogdW3nikCY2c/v\n\
FmY4zve6y6JJ/YHT6mkKeOa/VWpuhQOtzEpAc4BJsWDkciYt/exp0bEDydVcCII0\n\
SiL90KIWmz0Xzb1T7WG/QjUsupOUMtA86X/yBWsj5Q+SH/2np8mTrtnpbXODAH8b\n\
QKIUpA/XkzUpImKIQXmV83uq830uLNCiCWeO1s1tlpHOzO8V/lVfaNeja7p+BQyZ\n\
hOO/THEAeOmHg8sDQW97IpCRJtbAHvy7kJT3y2k1zmETG1N8M4ZIhTZ/hPd9fJoZ\n\
yWBHX/jybXqIwTZ9HcgCkxa6SlD4RZiB7Z3emxcGKztL7UIgfH0Yg7Q5rfdbvNQf\n\
8U1YHAUCgYEA4Rey3uxpiP3BTOH7v5SI6c2h9nC/iN8EPvpiZeWeHE4of1Pou0Xl\n\
LZGMANX5jpf6NsGmSGckahQ9mvae+/aiNayhmUXNOSi2D+bOmuGHABRY6nqJSO7i\n\
tZIfwcSLGdiOEQqIWFMyhXtZvJaVZ7bgsljUy/HwJmdN0ohuVEGuch0CgYEAt437\n\
JRalm4DJbG73PNktZtIpd9Fyv/ymHmdZvHR9pOVI3QG3eamim4uuzo1h6mOOZqTC\n\
K2hdOglXYNfLO1sgcCIa/NZ6Bd/nnzdQ/N2Q5WnG/t9GlDyT+CGZdWc+WGsNgMHr\n\
c7XlIsU1EMl+g5JVKPVgUi14MBwuBMIJlBg7O2MCgYAIfYRZtEEm0auA4uVEDK49\n\
Y2xAh3AyEXdviLI9dbPJDYmpg9i7d591YJAPWALZxhHCDvver0VIWwsX1UWZ62ui\n\
6qgNx/w9s7NqViJk5Szaa+oOriCPh7M1dhWMkYVNrEVvjx4ldr3pGwX/fw6ToupG\n\
z+L27mFIkYz16/99XhzeYQKBgCsAzof/6EioQYhv7uiIkQR31FNH9LRaAqk42WM3\n\
f4A0X3+3uT59qaT7crbdlMUPEfumOf9lcgH40knUBL8hOFZNBzmZHflmXaOFmCnF\n\
1v6Ia6CmuqhcEOafKI7C425flkhGJl1zjf05apdGPaehjuYLpsdZ88CBuZ5Pv2K8\n\
0pO1AoGANyc6SNEhcN0pHtstR2GhM9DtndJnbN6XxEIyq39/7PXd2ju2c4nKLM2f\n\
4CRAWMZHzOSd3EjXAaw3gxYrMG3YkhTYfa4Oqym6hjsYi2aHCRtv6UMO7jg9c0ol\n\
OhPlUgxLFgTyTP0+jKuF+bOzOk8v4i4C60PGl7aed3UT2NHUUf4=\n\
-----END RSA PRIVATE KEY-----\n";

    const TEST_KID: &str = "test-kid-1";
    const TEST_CLIENT_ID: &str = "zremote-test";
    const TEST_EMAIL: &str = "admin@example.com";
    const TEST_REDIRECT: &str = "http://127.0.0.1:0/oidc/callback";

    /// Clone-friendly holder for the signing key + its JWK representation.
    /// Parsed once per test run; `CoreRsaPrivateSigningKey` is not `Clone`
    /// because it owns an RNG trait object, so we create it on demand.
    struct TestSigner;

    impl TestSigner {
        fn signing_key() -> CoreRsaPrivateSigningKey {
            CoreRsaPrivateSigningKey::from_pem(
                TEST_RSA_PEM,
                Some(JsonWebKeyId::new(TEST_KID.to_string())),
            )
            .expect("fixed test RSA key must parse")
        }

        fn jwks_json() -> serde_json::Value {
            let key = Self::signing_key();
            let public: CoreJsonWebKey = key.as_verification_key();
            serde_json::to_value(CoreJsonWebKeySet::new(vec![public])).expect("jwks serialization")
        }
    }

    /// OIDC provider mock. Owns the `MockServer` so mocks live as long as
    /// the test. The mock server listens on a loopback port the builder
    /// picks for us.
    struct MockProvider {
        server: MockServer,
        issuer: String,
    }

    impl MockProvider {
        fn start() -> Self {
            let server = MockServer::start();
            let issuer = server.base_url();
            Self { server, issuer }
        }

        /// Serve a valid `/.well-known/openid-configuration` pointing at
        /// this server's token + JWKS endpoints.
        fn mount_discovery(&self) {
            let issuer = self.issuer.clone();
            let token_url = format!("{issuer}/oauth2/token");
            let jwks_url = format!("{issuer}/oauth2/jwks");
            let auth_url = format!("{issuer}/oauth2/authorize");

            let metadata = CoreProviderMetadata::new(
                IssuerUrl::new(issuer.clone()).unwrap(),
                AuthUrl::new(auth_url).unwrap(),
                JsonWebKeySetUrl::new(jwks_url).unwrap(),
                vec![ResponseTypes::new(vec![CoreResponseType::Code])],
                vec![CoreSubjectIdentifierType::Public],
                vec![CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256],
                EmptyAdditionalProviderMetadata {},
            )
            .set_token_endpoint(Some(TokenUrl::new(token_url).unwrap()))
            .set_scopes_supported(Some(vec![
                Scope::new("openid".to_string()),
                Scope::new("email".to_string()),
                Scope::new("profile".to_string()),
            ]));

            self.server.mock(|when, then| {
                when.method("GET").path("/.well-known/openid-configuration");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(serde_json::to_string(&metadata).unwrap());
            });
            self.server.mock(|when, then| {
                when.method("GET").path("/oauth2/jwks");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(TestSigner::jwks_json().to_string());
            });
        }

        /// Mount a token endpoint mock that returns a pre-built response.
        /// Tests first generate a flow (which fixes the nonce), then build
        /// the id_token + token response against that nonce, then call
        /// this to register the mock — mimicking the "provider knows the
        /// nonce because the authorize request carried it" flow without
        /// going through a real browser.
        fn mount_token_response(&self, response: CoreTokenResponse) {
            self.server.mock(move |when, then| {
                when.method("POST").path("/oauth2/token");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(serde_json::to_string(&response).unwrap());
            });
        }
    }

    fn build_token_response(id_token: CoreIdToken) -> CoreTokenResponse {
        let extra = IdTokenFields::new(Some(id_token), EmptyExtraTokenFields {});
        CoreTokenResponse::new(
            AccessToken::new("test-access-token".to_string()),
            CoreTokenType::Bearer,
            extra,
        )
    }

    /// Build a signed ID token with the given claims. Returns the JWT.
    fn sign_id_token(claims: CoreIdTokenClaims) -> CoreIdToken {
        let signer = TestSigner::signing_key();
        CoreIdToken::new(
            claims,
            &signer,
            CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
            None,
            None,
        )
        .expect("id token signing must succeed")
    }

    /// Common "happy path" claim set. Caller may mutate before signing.
    fn baseline_claims(issuer: &str, nonce: &Nonce, audience: &str) -> CoreIdTokenClaims {
        let now = Utc::now();
        CoreIdTokenClaims::new(
            IssuerUrl::new(issuer.to_string()).unwrap(),
            vec![Audience::new(audience.to_string())],
            now + ChronoDuration::minutes(5),
            now,
            StandardClaims::new(SubjectIdentifier::new("user-123".to_string()))
                .set_email(Some(EndUserEmail::new(TEST_EMAIL.to_string())))
                .set_email_verified(Some(true)),
            EmptyAdditionalClaims {},
        )
        .set_nonce(Some(nonce.clone()))
    }

    /// Drive `init` + `complete` through a mock, given an id_token builder
    /// that receives the issuer URL and the flow's nonce. Returns the
    /// result of `complete`.
    ///
    /// Ordering:
    /// 1. Spin up the mock provider and mount discovery.
    /// 2. Call `init` (seeds `state.oidc_flows`, generates PKCE+nonce).
    /// 3. Read the nonce from the flow store.
    /// 4. Ask the caller's builder for an id_token claiming that nonce.
    /// 5. Mount the token endpoint mock that returns the signed id_token.
    /// 6. Call `complete`.
    async fn run_flow_with(
        mock_builder: impl Fn(&str, &Nonce) -> CoreIdToken,
    ) -> (Result<VerifiedIdentity, OidcError>, OidcFlowStore) {
        let provider = MockProvider::start();
        provider.mount_discovery();

        let config = OidcConfig {
            issuer_url: provider.issuer.clone(),
            client_id: TEST_CLIENT_ID.into(),
            allowed_email: TEST_EMAIL.into(),
        };

        let store = OidcFlowStore::new();
        let flow = init(&config, TEST_REDIRECT, &store).await.unwrap();

        let nonce_str = {
            let guard = store.inner.lock().unwrap();
            guard
                .get(&flow.state)
                .expect("flow entry present after init")
                .nonce
                .secret()
                .clone()
        };
        let nonce = Nonce::new(nonce_str);
        let id_token = mock_builder(&provider.issuer, &nonce);
        provider.mount_token_response(build_token_response(id_token));

        let result = complete(&config, "code-xyz", &flow.state, &store).await;
        (result, store)
    }

    #[tokio::test]
    async fn happy_path_returns_verified_identity() {
        let (result, _store) = run_flow_with(|issuer, nonce| {
            sign_id_token(baseline_claims(issuer, nonce, TEST_CLIENT_ID))
        })
        .await;
        let identity = result.expect("happy path must succeed");
        assert_eq!(identity.email, TEST_EMAIL);
    }

    #[tokio::test]
    async fn callback_with_unknown_state_is_rejected() {
        let provider = MockProvider::start();
        provider.mount_discovery();
        let config = OidcConfig {
            issuer_url: provider.issuer.clone(),
            client_id: TEST_CLIENT_ID.into(),
            allowed_email: TEST_EMAIL.into(),
        };
        let store = OidcFlowStore::new();
        let err = complete(&config, "code-xyz", "never-issued", &store)
            .await
            .unwrap_err();
        assert!(matches!(err, OidcError::UnknownState));
    }

    #[tokio::test]
    async fn callback_with_wrong_email_is_rejected() {
        let (result, _) = run_flow_with(|issuer, nonce| {
            let now = Utc::now();
            let claims = CoreIdTokenClaims::new(
                IssuerUrl::new(issuer.to_string()).unwrap(),
                vec![Audience::new(TEST_CLIENT_ID.into())],
                now + ChronoDuration::minutes(5),
                now,
                StandardClaims::new(SubjectIdentifier::new("intruder".into()))
                    .set_email(Some(EndUserEmail::new("someone-else@example.com".into()))),
                EmptyAdditionalClaims {},
            )
            .set_nonce(Some(nonce.clone()));
            sign_id_token(claims)
        })
        .await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, OidcError::EmailMismatch),
            "wrong email must surface as EmailMismatch, got {err:?}"
        );
    }

    #[tokio::test]
    async fn callback_with_wrong_audience_is_rejected() {
        let (result, _) = run_flow_with(|issuer, nonce| {
            sign_id_token(baseline_claims(issuer, nonce, "some-other-client"))
        })
        .await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, OidcError::ClaimsVerification(_)),
            "aud mismatch must fail in ClaimsVerification, got {err:?}"
        );
    }

    #[tokio::test]
    async fn callback_with_expired_id_token_is_rejected() {
        let (result, _) = run_flow_with(|issuer, nonce| {
            let now = Utc::now();
            let claims = CoreIdTokenClaims::new(
                IssuerUrl::new(issuer.to_string()).unwrap(),
                vec![Audience::new(TEST_CLIENT_ID.into())],
                now - ChronoDuration::minutes(1), // already expired
                now - ChronoDuration::minutes(10),
                StandardClaims::new(SubjectIdentifier::new("u".into()))
                    .set_email(Some(EndUserEmail::new(TEST_EMAIL.into()))),
                EmptyAdditionalClaims {},
            )
            .set_nonce(Some(nonce.clone()));
            sign_id_token(claims)
        })
        .await;
        let err = result.unwrap_err();
        assert!(matches!(err, OidcError::ClaimsVerification(_)));
    }

    #[tokio::test]
    async fn callback_with_nonce_mismatch_is_rejected() {
        let (result, _) = run_flow_with(|issuer, _real_nonce| {
            // Sign with a nonce the flow store doesn't know about.
            let forged = Nonce::new("forged-nonce".into());
            sign_id_token(baseline_claims(issuer, &forged, TEST_CLIENT_ID))
        })
        .await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, OidcError::ClaimsVerification(_)),
            "nonce mismatch must surface as ClaimsVerification, got {err:?}"
        );
    }

    #[tokio::test]
    async fn callback_with_tampered_signature_is_rejected() {
        let (result, _) = run_flow_with(|issuer, nonce| {
            let good = sign_id_token(baseline_claims(issuer, nonce, TEST_CLIENT_ID));
            // Flip a single character in the signature portion of the JWT.
            let raw: String = serde_json::to_value(&good)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string();
            let mut parts: Vec<&str> = raw.split('.').collect();
            // Flip a character in the *middle* of the signature segment.
            // Earlier we flipped the last char, but base64url-encoded
            // RSA-2048 signatures (342 chars = 2052 bits) have 4 padding
            // bits in the final char; flipping those is a no-op, so the
            // test was flaky based on which char the signature ended on.
            // A mid-string byte is always meaningful.
            let sig = parts[2];
            let mut chars: Vec<char> = sig.chars().collect();
            assert!(
                chars.len() > 20,
                "signature too short for mid-string tampering"
            );
            let mid = chars.len() / 2;
            chars[mid] = if chars[mid] == 'A' { 'B' } else { 'A' };
            let new_sig: String = chars.into_iter().collect();
            parts[2] = &new_sig;
            let tampered = parts.join(".");
            serde_json::from_value::<CoreIdToken>(serde_json::Value::String(tampered)).unwrap()
        })
        .await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, OidcError::ClaimsVerification(_)),
            "tampered signature must fail in ClaimsVerification, got {err:?}"
        );
    }

    #[tokio::test]
    async fn init_publishes_state_and_stores_flow() {
        let provider = MockProvider::start();
        provider.mount_discovery();
        let config = OidcConfig {
            issuer_url: provider.issuer.clone(),
            client_id: TEST_CLIENT_ID.into(),
            allowed_email: TEST_EMAIL.into(),
        };
        let store = OidcFlowStore::new();
        let flow = init(&config, TEST_REDIRECT, &store).await.unwrap();

        assert_eq!(store.len(), 1);
        assert!(flow.auth_url.as_str().contains("state="));
        assert!(flow.auth_url.as_str().contains("code_challenge="));
        assert!(
            flow.auth_url
                .as_str()
                .contains("code_challenge_method=S256")
        );
        assert!(!flow.state.is_empty());
    }

    /// Smoke test for the `preferred_username` fallback: when the `email`
    /// claim is absent but `preferred_username` looks like an email, it's
    /// accepted.
    #[tokio::test]
    async fn callback_falls_back_to_preferred_username_when_email_missing() {
        let (result, _) = run_flow_with(|issuer, nonce| {
            let now = Utc::now();
            let claims = CoreIdTokenClaims::new(
                IssuerUrl::new(issuer.to_string()).unwrap(),
                vec![Audience::new(TEST_CLIENT_ID.into())],
                now + ChronoDuration::minutes(5),
                now,
                StandardClaims::new(SubjectIdentifier::new("u".into()))
                    .set_preferred_username(Some(EndUserUsername::new(TEST_EMAIL.into()))),
                EmptyAdditionalClaims {},
            )
            .set_nonce(Some(nonce.clone()));
            sign_id_token(claims)
        })
        .await;
        let identity = result.expect("preferred_username fallback must succeed");
        assert_eq!(identity.email, TEST_EMAIL);
    }

    /// `preferred_username` must NOT be trusted when it is a non-email
    /// string, even if its text happens to match the configured email
    /// prefix. Guards against an IdP that emits `preferred_username =
    /// "admin"` while the configured allowlist is `admin@example.com`.
    #[tokio::test]
    async fn callback_rejects_non_email_preferred_username() {
        let (result, _) = run_flow_with(|issuer, nonce| {
            let now = Utc::now();
            let claims = CoreIdTokenClaims::new(
                IssuerUrl::new(issuer.to_string()).unwrap(),
                vec![Audience::new(TEST_CLIENT_ID.into())],
                now + ChronoDuration::minutes(5),
                now,
                StandardClaims::new(SubjectIdentifier::new("u".into()))
                    .set_preferred_username(Some(EndUserUsername::new("admin".into()))),
                EmptyAdditionalClaims {},
            )
            .set_nonce(Some(nonce.clone()));
            sign_id_token(claims)
        })
        .await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, OidcError::MissingEmail),
            "non-email preferred_username must be rejected, got {err:?}"
        );
    }

    /// The in-flight flow store is TTL-bounded: a callback arriving after
    /// [`OIDC_FLOW_TTL`] is rejected as Expired.
    #[tokio::test]
    async fn callback_rejects_expired_flow_entry() {
        let provider = MockProvider::start();
        provider.mount_discovery();
        let config = OidcConfig {
            issuer_url: provider.issuer.clone(),
            client_id: TEST_CLIENT_ID.into(),
            allowed_email: TEST_EMAIL.into(),
        };
        let store = OidcFlowStore::with_caps(Duration::from_millis(1), 8);
        let flow = init(&config, TEST_REDIRECT, &store).await.unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
        let err = complete(&config, "code-xyz", &flow.state, &store)
            .await
            .unwrap_err();
        assert!(matches!(err, OidcError::Expired));
    }
}
