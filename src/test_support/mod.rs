//! Test-only harness: a local OIDC issuer and helpers for driving a real server.
//!
//! The point of the mock issuer is that it is a *real* one — it publishes a discovery
//! document and a JWKS, and signs RS256 tokens with an actual RSA key. mdshelf's
//! verification path is therefore exercised exactly as it would be against Google,
//! including the failure modes: a forged signature, a wrong audience or issuer, an
//! expired token, a replayed nonce, and an unverified address.

pub mod mock_idp;
pub mod server;

pub use mock_idp::{MockIdp, TokenBehaviour, TokenSpec};
pub use server::{TEST_CLIENT_ID, TestServer, TestSite, client};
