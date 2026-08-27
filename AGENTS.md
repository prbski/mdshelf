# mdshelf — agent guide

Rust 2024 web server that serves folders of markdown as websites. Ships as both a
library (`src/lib.rs`) and a binary (`mdshelf`), because the authorization suite drives a
real in-process server rather than testing unit by unit.

`DEVELOPMENT.md` is the human setup guide; this file is the agent operating contract.

## Verification

Never claim a change works without running these. There is no CI test gate —
`.github/workflows/release.yml` only builds tagged releases, so local verification is the
only gate.

```bash
make check      # cargo check
make test       # cargo test --all-features
make clippy     # cargo clippy --all-targets --all-features
make fmt-check  # cargo fmt --check
```

`make fmt` before finishing. There is no `rustfmt.toml` or `clippy.toml`; defaults apply,
so do not hand-format around them.

After touching `src/acl/`, `src/auth/`, `src/links/`, `src/server/`, or `src/content/`, run
`cargo test --all-features --test leak_suite --test share_serving` first — it is the suite that catches
authorization regressions, and it is faster feedback than the full run. Every suite in
`tests/` uses `src/test_support/`, so `--all-features` is mandatory; without it they do
not compile.

Manual smoke test:

```bash
make serve                 # http://127.0.0.1:4321/, CONFIG=examples/mdshelf.toml
make mdshelf-check         # validate config + scan content, no server
```

Auth tests need no network and no Google credentials: `src/test_support/mock_idp.rs` is a
local OIDC issuer signing with fixtures in `tests/fixtures/`. Keep it that way — never
introduce a test that reaches the internet.

## Map

| Path | Owns |
| --- | --- |
| `src/cli.rs` | clap surface: `serve`, `init`, `check`, `export`, `acl grant`, `share {list,revoke}`, `service {install,uninstall,start,stop,status}` |
| `src/config.rs` | `mdshelf.toml` parsing and validation |
| `src/content/` | filesystem scan, page/frontmatter parsing, tree, site index |
| `src/render/` | comrak markdown, minijinja templates, syntect highlighting |
| `src/acl/` | per-path access rules: resolver, index, `acl grant` editing |
| `src/auth/` | Google OIDC, session store, cookie crypto, sign-in pages |
| `src/links/` | share links: tokens, `[links]` settings, `share` commands, deny page, Share control |
| `src/server/` | axum routes, TLS/ACME, live reload over WebSocket, link serving and the `/__share*` endpoints |
| `src/export/` | static site export |
| `src/theme/`, `src/service/` | theming; OS service installation |
| `src/test_support/` | mock IDP + server harness, gated behind the `test-support` feature |
| `tests/` | `leak_suite`, `acl_model`, `auth_flow`, `operations`, `no_secret_logging`, `share_links`, `share_serving`, `share_interface`, `page_source` |

`examples/private-vault/` exercises site-, folder-, and file-level rules, including the
`deny` that excludes someone a broader rule already granted. Use it when changing ACL
behavior.

## Invariants — do not regress

Full list in `DEVELOPMENT.md`; these are the ones a change can silently break:

- **Fail closed.** A path no rule names is denied. A malformed rule block denies everyone.
- A share link reaches **one page plus its referenced assets**, and only while its issuer
  can still read that page. Dead, unknown and malformed links are byte-identical.
- No share token in any log line, error message, or database column. `mdshelf.db` now
  holds link state that cannot be recreated.
- `allow`/`deny` values must never reach rendered HTML or template metadata. That
  includes the embedded page source: `Page.body` is captured *after* the frontmatter
  split, so frontmatter must never be able to reach it.
- Restricted and nonexistent paths must return **byte-identical** responses — no timing,
  status, header, or body tell that distinguishes them.
- Without `--auth google`, behavior must be byte-identical to pre-auth mdshelf.
- `mdshelf acl grant` is the only command permitted to write into a user's vault.
- No secret may appear in any log line at any level (`tests/no_secret_logging.rs`).

## Conventions

- Errors: `anyhow::Result` throughout the CLI, config, and content paths; the one typed
  error is the `thiserror` HTTP error in `src/server/error.rs`. Follow that split.
- New test-only helpers go in `src/test_support/` behind `#[cfg(any(test, feature = "test-support"))]`,
  and any dependency they need is `optional = true` wired into the `test-support` feature.
- Adding a dependency is a real decision: the release profile is `lto = "thin"`,
  `codegen-units = 1`, `strip = "symbols"`, and binaries ship to five installer channels.
- Do not edit `dist-workspace.toml`, `wix/`, or push tags — releases go out via cargo-dist
  on `v*` tags and publish the Homebrew tap.
- Commit subjects are imperative and describe user-visible effect
  ("Add static export command with per-site support"), not file lists.
