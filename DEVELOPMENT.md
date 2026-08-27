# Development

## Prerequisites

- [Rust](https://rustup.rs) (stable toolchain)
- `cargo` available on `PATH`

## Running locally

```bash
cargo run -- serve --config examples/mdshelf.toml
```

Open `http://127.0.0.1:4321/` for the site index.

Validate config and scan content without starting the server:

```bash
cargo run -- check --config examples/mdshelf.toml
```

## Makefile targets

```
make build          cargo build
make release        cargo build --release
make check          cargo check
make test           cargo test
make clippy         cargo clippy
make fmt            cargo fmt
make fmt-check      cargo fmt --check
make clean          cargo clean
make run / serve    run dev server (default config: examples/mdshelf.toml)
make mdshelf-check  validate config and scan content
make install        cargo install --path .
```

Override the config file for any target:

```bash
make serve CONFIG=path/to/mdshelf.toml
```

## Working on authentication

The auth suite runs against a local OpenID Connect issuer, so no network access or
Google credentials are needed:

```bash
cargo test --all-features
```

The issuer lives in `src/test_support/mock_idp.rs`. It publishes a discovery document
and a JWKS, and signs RS256 tokens with the fixture key in `tests/fixtures/`, so
mdshelf's verification path runs exactly as it would against Google — including the
failure modes (forged signature, wrong audience or issuer, expiry, replayed nonce,
unverified address). `TokenSpec` selects which property to corrupt; `TokenBehaviour`
makes the token endpoint reject a grant or return a server error.

`tests/leak_suite.rs` is the test to run first after touching anything in `src/acl`,
`src/server/routes.rs`, or `src/content`. It asserts that a viewer who may read part of
a vault sees no trace of the rest through any surface.

To drive a real server against the mock issuer by hand, point mdshelf at it:

```bash
export MDSHELF_OIDC_DISCOVERY_URL=http://127.0.0.1:PORT/.well-known/openid-configuration
export MDSHELF_GOOGLE_CLIENT_ID=test-client-id
export MDSHELF_GOOGLE_CLIENT_SECRET=test-client-secret
cargo run -- serve --auth google --config examples/mdshelf.toml
```

`examples/private-vault/` is a small vault demonstrating site-, folder-, and
file-level rules, including the `deny` that a folder needs in order to exclude someone
a broader rule already granted.

### Invariants to preserve

- Fail closed. A path no rule names is denied; a malformed rule block denies everyone.
- `allow`/`deny` must never reach rendered HTML or template metadata.
- Restricted and nonexistent paths must return byte-identical responses.
- Without `--auth google`, behaviour must be byte-identical to before auth existed.
- `mdshelf acl grant` is the only command that may write to a user's vault.
- No secret may appear in any log line at any level. Share tokens count: a link URL *is*
  its token, so the request-trace span redacts it before it can be formatted.
- A share link reaches exactly one page plus the assets that page references — never a
  second page, never the raw source, never an unreferenced file.
- A share link serves only while its issuer can still read that page, revalidated on
  every request. Expired, revoked, unknown, malformed and nonexistent links must be
  byte-identical.
- `mdshelf.db` is no longer disposable: it holds share links that cannot be recreated.

## Releases

Releases are built and published via [cargo-dist](https://opensource.axo.dev/cargo-dist/). Distribution targets, installers (shell, PowerShell, npm, Homebrew, MSI), and CI config are defined in [`dist-workspace.toml`](dist-workspace.toml).

Publish a new release by tagging:

```bash
git tag v0.x.y && git push --tags
```

The GitHub Actions workflow triggered by the tag builds all targets and publishes installers, GitHub Releases assets, and the Homebrew formula to the [tap](https://github.com/prbski/homebrew-tap).
