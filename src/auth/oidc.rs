//! OpenID Connect against Google (or, in tests, any discovery-compliant issuer).
//!
//! The verification here is the security boundary of the whole feature: everything
//! downstream trusts the email address this module returns. Every check the spec calls
//! for (SEC-1, SEC-2) is enforced, and an ID token that fails any of them yields an
//! error rather than a partially-trusted identity.

use std::sync::RwLock;

use anyhow::{Context, Result, anyhow, bail};
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;

/// Google's OIDC discovery document.
pub const GOOGLE_DISCOVERY_URL: &str =
    "https://accounts.google.com/.well-known/openid-configuration";

/// Tolerance for clock skew when checking `exp` on an ID token.
const ID_TOKEN_LEEWAY_SECONDS: u64 = 5;

/// Scopes requested at sign-in.
///
/// `openid email` identifies the user; `offline` access via `access_type=offline` is what
/// yields the refresh token that D18 requires for re-validation. No `profile` scope is
/// requested — the ACL model is email-only, so a name or avatar would be data collected
/// for no purpose.
pub const SCOPES: &str = "openid email";

/// Endpoints resolved from a provider's discovery document.
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderMetadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
}

/// A discovered provider plus its cached signing keys.
pub struct Provider {
    pub metadata: ProviderMetadata,
    jwks: RwLock<JwkSet>,
    http: reqwest::Client,
}

impl Provider {
    /// Fetch the discovery document and the initial JWKS.
    pub async fn discover(http: reqwest::Client, discovery_url: &str) -> Result<Self> {
        let metadata: ProviderMetadata = http
            .get(discovery_url)
            .send()
            .await
            .with_context(|| format!("fetching OIDC discovery from {discovery_url}"))?
            .error_for_status()
            .with_context(|| format!("OIDC discovery at {discovery_url} returned an error"))?
            .json()
            .await
            .with_context(|| format!("parsing OIDC discovery from {discovery_url}"))?;

        let jwks = fetch_jwks(&http, &metadata.jwks_uri).await?;
        Ok(Self {
            metadata,
            jwks: RwLock::new(jwks),
            http,
        })
    }

    /// Build the URL to send the browser to (US-3).
    pub fn authorization_url(
        &self,
        client_id: &str,
        redirect_uri: &str,
        state: &str,
        nonce: &str,
        code_challenge: &str,
    ) -> String {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("client_id", client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", SCOPES)
            .append_pair("state", state)
            .append_pair("nonce", nonce)
            .append_pair("code_challenge", code_challenge)
            .append_pair("code_challenge_method", "S256")
            // Without these two Google returns a refresh token only on the user's very
            // first consent, which would leave most sessions unable to re-validate.
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent")
            .finish();

        let separator = if self.metadata.authorization_endpoint.contains('?') {
            '&'
        } else {
            '?'
        };
        format!(
            "{}{}{}",
            self.metadata.authorization_endpoint, separator, query
        )
    }

    /// Exchange an authorization code for tokens (US-4).
    pub async fn exchange_code(
        &self,
        client_id: &str,
        client_secret: &str,
        redirect_uri: &str,
        code: &str,
        code_verifier: &str,
    ) -> Result<TokenResponse> {
        let response = self
            .http
            .post(&self.metadata.token_endpoint)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("client_id", client_id),
                ("client_secret", client_secret),
                ("redirect_uri", redirect_uri),
                ("code_verifier", code_verifier),
            ])
            .send()
            .await
            .context("contacting the token endpoint")?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            // The body can echo request parameters, so it is summarised rather than
            // logged verbatim (NFR-4).
            bail!(
                "token endpoint returned {}: {}",
                status,
                summarize_error(&body)
            );
        }
        serde_json::from_str(&body).context("parsing the token endpoint response")
    }

    /// Re-validate a session by redeeming its refresh token (US-8).
    ///
    /// Distinguishes an explicit rejection from an unreachable provider. Both fail
    /// closed per D20, but the log line must say which happened — during a Google
    /// incident that distinction is the whole diagnosis.
    pub async fn refresh(
        &self,
        client_id: &str,
        client_secret: &str,
        refresh_token: &str,
    ) -> std::result::Result<TokenResponse, RefreshFailure> {
        let response = self
            .http
            .post(&self.metadata.token_endpoint)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", client_id),
                ("client_secret", client_secret),
            ])
            .send()
            .await
            .map_err(|err| RefreshFailure::Unreachable(err.to_string()))?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if status.is_success() {
            return serde_json::from_str(&body).map_err(|err| {
                RefreshFailure::Unreachable(format!("unparseable response: {err}"))
            });
        }
        if status.is_server_error() {
            return Err(RefreshFailure::Unreachable(format!(
                "provider returned {status}"
            )));
        }
        Err(RefreshFailure::Rejected(summarize_error(&body)))
    }

    /// Verify an ID token and return the identity it asserts.
    pub async fn verify_id_token(
        &self,
        id_token: &str,
        client_id: &str,
        expected_nonce: Option<&str>,
    ) -> Result<Identity> {
        let header = decode_header(id_token).context("reading the ID token header")?;
        if header.alg != Algorithm::RS256 {
            bail!("ID token algorithm {:?} is not RS256", header.alg);
        }
        let kid = header
            .kid
            .clone()
            .ok_or_else(|| anyhow!("ID token header has no `kid`"))?;

        let key = match self.decoding_key(&kid) {
            Some(key) => key,
            None => {
                // Google rotates signing keys; an unknown kid warrants exactly one refetch.
                self.refresh_jwks().await?;
                self.decoding_key(&kid)
                    .ok_or_else(|| anyhow!("no signing key matches ID token `kid` {kid}"))?
            }
        };

        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[client_id]);
        validation.set_issuer(&[self.metadata.issuer.as_str()]);
        validation.validate_exp = true;
        validation.set_required_spec_claims(&["exp", "iss", "aud"]);
        // The library defaults to 60s of leeway, which would accept a token that expired
        // a minute ago. An ID token is verified once, seconds after it is minted, so the
        // only tolerance needed is for genuine clock skew between this host and Google.
        validation.leeway = ID_TOKEN_LEEWAY_SECONDS;

        let data = decode::<IdTokenClaims>(id_token, &key, &validation)
            .context("ID token failed verification")?;
        let claims = data.claims;

        // A nonce is issued on every authorization request, so a token that omits or
        // mismatches it is either replayed or belongs to a different flow.
        if let Some(expected) = expected_nonce {
            match claims.nonce.as_deref() {
                Some(actual) if actual == expected => {}
                Some(_) => bail!("ID token nonce does not match the authorization request"),
                None => bail!("ID token has no nonce"),
            }
        }

        let email = claims
            .email
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow!("ID token has no email claim"))?;

        // SEC-1. An unverified address is one the account holder never proved they own.
        if !claims.email_verified {
            bail!("ID token email {} is not verified by the provider", email);
        }

        Ok(Identity {
            email: super::email::normalize_email(email),
            subject: claims.sub,
        })
    }

    fn decoding_key(&self, kid: &str) -> Option<DecodingKey> {
        let jwks = self.jwks.read().ok()?;
        let jwk = jwks.find(kid)?;
        DecodingKey::from_jwk(jwk).ok()
    }

    async fn refresh_jwks(&self) -> Result<()> {
        let fetched = fetch_jwks(&self.http, &self.metadata.jwks_uri).await?;
        if let Ok(mut guard) = self.jwks.write() {
            *guard = fetched;
        }
        Ok(())
    }
}

async fn fetch_jwks(http: &reqwest::Client, jwks_uri: &str) -> Result<JwkSet> {
    http.get(jwks_uri)
        .send()
        .await
        .with_context(|| format!("fetching JWKS from {jwks_uri}"))?
        .error_for_status()
        .with_context(|| format!("JWKS endpoint {jwks_uri} returned an error"))?
        .json::<JwkSet>()
        .await
        .with_context(|| format!("parsing JWKS from {jwks_uri}"))
}

/// The verified identity behind a session. Only the address is retained downstream.
#[derive(Debug, Clone)]
pub struct Identity {
    pub email: String,
    pub subject: String,
}

/// Why a refresh attempt failed (US-8).
#[derive(Debug, Clone)]
pub enum RefreshFailure {
    /// The provider explicitly refused: the grant is dead, the account is gone or
    /// suspended, or consent was withdrawn.
    Rejected(String),
    /// The provider could not be reached, or answered with a server error.
    Unreachable(String),
}

impl RefreshFailure {
    pub fn reason(&self) -> &str {
        match self {
            RefreshFailure::Rejected(reason) => reason,
            RefreshFailure::Unreachable(reason) => reason,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            RefreshFailure::Rejected(_) => "rejected",
            RefreshFailure::Unreachable(_) => "unreachable",
        }
    }
}

/// Token endpoint response. `refresh_token` is absent on a refresh that reuses the
/// existing grant, which is normal and not an error.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    #[serde(default)]
    pub id_token: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct IdTokenClaims {
    sub: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default, deserialize_with = "flexible_bool")]
    email_verified: bool,
    #[serde(default)]
    nonce: Option<String>,
}

/// Google has historically sent `email_verified` as both a JSON boolean and the strings
/// `"true"`/`"false"`. Accept both, and treat anything else as unverified.
fn flexible_bool<'de, D>(deserializer: D) -> std::result::Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Bool(b) => Ok(b),
        serde_json::Value::String(s) => match s.as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            other => Err(D::Error::custom(format!(
                "email_verified has unexpected value {other:?}"
            ))),
        },
        serde_json::Value::Null => Ok(false),
        other => Err(D::Error::custom(format!(
            "email_verified has unexpected type {other:?}"
        ))),
    }
}

/// Extract just the OAuth `error` code from a token endpoint error body.
///
/// The full body can contain request parameters, so only the short machine code is
/// surfaced into errors and logs.
fn summarize_error(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|error| error.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unspecified error".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_error_extracts_oauth_code() {
        assert_eq!(
            summarize_error(
                r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#
            ),
            "invalid_grant"
        );
    }

    #[test]
    fn summarize_error_handles_non_json() {
        assert_eq!(summarize_error("<html>502</html>"), "unspecified error");
    }

    #[test]
    fn flexible_bool_accepts_boolean_and_string() {
        #[derive(Deserialize)]
        struct Probe {
            #[serde(deserialize_with = "flexible_bool")]
            value: bool,
        }
        let from_bool: Probe = serde_json::from_str(r#"{"value": true}"#).unwrap();
        assert!(from_bool.value);
        let from_string: Probe = serde_json::from_str(r#"{"value": "true"}"#).unwrap();
        assert!(from_string.value);
        let false_string: Probe = serde_json::from_str(r#"{"value": "false"}"#).unwrap();
        assert!(!false_string.value);
    }

    #[test]
    fn refresh_failure_reports_its_kind() {
        assert_eq!(
            RefreshFailure::Rejected("invalid_grant".into()).kind(),
            "rejected"
        );
        assert_eq!(
            RefreshFailure::Unreachable("dns error".into()).kind(),
            "unreachable"
        );
    }
}
