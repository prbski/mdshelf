---
active: true
iteration: 21
session_id: 86be8722-c141-4ccc-8d75-e025d05feede
max_iterations: 30
completion_promise: "COMPLETE"
started_at: "2026-08-22T11:40:40Z"
---

Implement Google Workspace OAuth per spec at docs/specs/google-workspace-oauth.md

Work in /Users/patryk/develop/mdshelf/mdshelf (Rust, edition 2024, axum 0.8).

PHASES:
1. Identity & session foundation (US-1..US-8): [auth] config + startup validation, mock OIDC test harness, /auth/login with state+PKCE+nonce, /auth/callback with full ID-token verification, SQLite sidecar schema, AEAD-encrypted refresh tokens with a 0600 key file, session cookie lifecycle and logout, idle-resume revalidation that fails closed - verify with 'make test && make clippy && make fmt-check'
2. ACL model (US-9..US-14): allow/deny frontmatter parsing with strict list validation, index.md governs its folder recursively including itself, resolver with inherit + most-specific-wins + explicit deny + fail-closed default, derived rules_index kept live by the notify watcher, mdshelf check validates ACLs and exits non-zero, mdshelf acl explain prints the resolution trace - verify with 'make test && make mdshelf-check'
3. Enforcement across every surface (US-15..US-20): authorization middleware on every request, branded interstitial for anonymous visitors on ALL paths, unified 404 deny page byte-identical for restricted and nonexistent paths, per-viewer tree cached by ACL signature, per-viewer search index and sitemap, ACL-gated attachments and raw markdown, per-socket live-reload filtering, mdshelf export --as <email> - verify with 'make test'
4. Operations & polish (US-21..US-26): access log with 90d retention, mdshelf auth setup wizard, TLS via ACME or --tls-cert or --behind-proxy, mdshelf acl doctor, mdshelf acl grant as the ONLY command that writes to the vault, documentation - verify with 'make test && make clippy && make fmt-check && make release'

CRITICAL INVARIANTS - never violate:
- Fail closed. A path matching no rule at any level is DENIED. A malformed allow/deny block DENIES for everyone.
- allow/deny frontmatter keys must NEVER reach rendered HTML or template metadata.
- Restricted and nonexistent paths must return byte-identical responses to a signed-in user.
- Without --auth google, behaviour must be byte-identical to today.
- Only 'mdshelf acl grant' may write to the user's vault. Nothing else, ever.
- No secret (client secret, refresh token, encryption key) in any log line at any level.

VERIFICATION (run after each phase):
- make test
- make clippy
- make fmt-check
- make mdshelf-check

ESCAPE HATCH: After 20 iterations without progress:
- Document what's blocking in docs/specs/google-workspace-oauth.md under 'Implementation Notes'
- List approaches attempted
- Stop and ask for human guidance

Output <promise>COMPLETE</promise> when all phases pass verification.
