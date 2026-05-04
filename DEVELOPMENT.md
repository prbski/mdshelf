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

## Releases

Releases are built and published via [cargo-dist](https://opensource.axo.dev/cargo-dist/). Distribution targets, installers (shell, PowerShell, npm, Homebrew, MSI), and CI config are defined in [`dist-workspace.toml`](dist-workspace.toml).

Publish a new release by tagging:

```bash
git tag v0.x.y && git push --tags
```

The GitHub Actions workflow triggered by the tag builds all targets and publishes installers, GitHub Releases assets, and the Homebrew formula to the [tap](https://github.com/prbski/homebrew-tap).
