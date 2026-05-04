# mdshelf

A single-binary Rust server that turns folders of Markdown files into browsable sites — with hot reload, frontmatter, MiniJinja layouts, and a bundled mobile-first theme. Mount multiple content trees under their own URL prefixes (e.g. `/docs`, `/notes`, `/blog`).

## Features

**Content**
- Multiple independent sites, each mounted at a distinct URL prefix
- YAML frontmatter: `title`, `description`, `layout`, `sidebar_order`, `draft`
- Rich Markdown extensions: tables, task lists, footnotes, math (`$…$` and ` ```math `), strikethrough, superscript, autolinks, description lists, multiline block quotes
- Intra-site `.md` links rewritten to clean URLs automatically
- Mixed content directories — point at any folder; only `.md` files are rendered, everything else (images, fonts, assets) are skipped.
- Draft pages (`draft: true`) return 404 and are invisible to visitors

**Navigation**
- Auto-generated table of contents from headings
- Breadcrumb trail derived from the directory structure
- Prev / next page navigation
- Auto-generated index pages for folders that have no `index.md`
- Home page listing all mounted sites with page counts

**Theme**
- Bundled `mdshelf-theme` — mobile-first, responsive, light/dark aware
- Syntax highlighting (class-based, served from `/__mdshelf/syntax.css`)
- Layered theme overrides: per-site > global > built-in
- Per-site accent colors — auto-assigned from a palette or configured explicitly
- MiniJinja templates for layouts and partials — override any file without forking the whole theme

**Server**
- **Hot reload** — WebSocket-based live reload; the browser refreshes automatically the moment you save a file
- Response compression (gzip / brotli / zstd via tower-http)
- Native system service integration: launchd (macOS), systemd (Linux), Windows SCM
- Config discovery: `./mdshelf.toml` then `~/.config/mdshelf/mdshelf.toml`

## Installation

### Homebrew (macOS / Linux)

```bash
brew install prbski/tap/mdshelf
```

### Shell script (macOS / Linux)

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/prbski/mdshelf/releases/latest/download/mdshelf-installer.sh | sh
```

### PowerShell (Windows)

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/prbski/mdshelf/releases/latest/download/mdshelf-installer.ps1 | iex"
```

### npm / npx

```bash
npm install -g mdshelf
# or run without installing:
npx mdshelf
```

### Cargo

```bash
cargo install mdshelf
```

## Upgrading

Stop the service, install the new binary with the same method you used originally, then restart:

```bash
mdshelf stop
# re-run your original installer, e.g.:
brew upgrade prbski/tap/mdshelf
mdshelf start
```

> If you installed as a system-level service (without `--user`) prefix the `stop` and `start` commands with `sudo`.

## Getting started

Scaffold a config and sample content into `~/.config/mdshelf`:

```bash
mdshelf init
```

Pass a directory to create a project-local setup instead:

```bash
mdshelf init . --with-theme
```

Then start the server:

```bash
mdshelf serve
```

Open `http://127.0.0.1:4444/` for the site index.

## Commands

| Command | Description |
|---|---|
| `mdshelf init [dir]` | Scaffold config and sample content |
| `mdshelf serve` | Start the web server |
| `mdshelf check` | Validate config and scan content |
| `mdshelf install` | Register as a native system service |
| `mdshelf start` | Start the installed service |
| `mdshelf stop` | Stop the installed service |
| `mdshelf restart` | Restart the installed service |
| `mdshelf status` | Show service status |
| `mdshelf uninstall` | Remove the service registration |

All commands accept `--config <path>` to point at a specific config file. Default search order: `./mdshelf.toml`, then `~/.config/mdshelf/mdshelf.toml`.

## Configuration

```toml
host = "127.0.0.1"
port = 4444

[server]
live_reload = true

[theme]
name = "mdshelf-theme"
# directory = "./theme"   # override bundled theme

[[sites]]
path  = "./docs"
mount = "/docs"
title = "Docs"

[[sites]]
path  = "./notes"
mount = "/notes"
title = "Notes"

[[sites]]
path  = "./blog"
mount = "/blog"
title = "Blog"
```

Each `[[sites]]` block mounts a directory at a URL prefix. Only `.md` files are rendered — other files (images, fonts, etc.) are served as static assets. `mount` must not be `/`. Add as many sites as you like.

See [examples/mdshelf.toml](examples/mdshelf.toml) for a complete example.

## Frontmatter

YAML between `---` delimiters is supported:

```yaml
---
title: Page title
description: Optional summary
layout: doc
sidebar_order: 10
draft: false
---
```

## Theme customisation

Override or extend templates by pointing `[theme].directory` at a local directory:

- `layouts/` — `base.html`, `doc.html`, `home.html`, `error.html`
- `partials/` — `header.html`, `sidebar.html`, `toc.html`, `breadcrumbs.html`, `footer.html`
- `assets/` — static files served from `/__assets/...`

Syntax highlighting CSS is served from `/__mdshelf/syntax.css` (class-based, light/dark aware).

## System service

Install and manage a native service (launchd on macOS, systemd on Linux, SCM on Windows):

```bash
mdshelf install
mdshelf start
mdshelf status
mdshelf stop
mdshelf uninstall
```

`--config` is optional and follows the same default search order as other commands. The resolved absolute path is baked into the service definition at install time, so the service always finds its config regardless of working directory.

Use `--user` for a per-user service where the platform supports it.

## Serving on port 80

Set `host` and `port` in your config:

```toml
host = "0.0.0.0"
port = 80
```

Port 80 is a privileged port on macOS and Linux, so the service must run as root. Install it without `--user`:

```bash
sudo mdshelf install
sudo mdshelf start
```

The service starts automatically on boot and binds port 80 without any further interaction.

> Use `host = "127.0.0.1"` instead of `0.0.0.0` if you only need local access (e.g. when fronted by Tailscale or a reverse proxy).

## HTTPS via Tailscale

The cleanest way to expose mdshelf over HTTPS — with a valid certificate and no reverse proxy setup — is [Tailscale Serve](https://tailscale.com/kb/1242/tailscale-serve).

Run mdshelf bound to `127.0.0.1` and tell Tailscale to proxy it. The `--bg` flag keeps it running after the terminal closes:

```bash
tailscale serve --bg 4444
```

Tailscale provisions a certificate automatically and makes the site available **only within your tailnet** at `https://<machine>.<tailnet>.ts.net`.

```bash
tailscale serve status   # confirm the URL
```

Pair it with `mdshelf install && mdshelf start` (see [System service](#system-service)) so both services survive reboots.

To stop serving:

```bash
tailscale serve off
```

### Public access via Tailscale Funnel

To make the site reachable from the **public internet** (not just your tailnet), use Funnel instead:

```bash
tailscale funnel --bg 4444
```

The site becomes accessible at the same `*.ts.net` URL from anywhere, without any DNS or firewall configuration. To stop:

```bash
tailscale funnel off
```

## Logging

Set `MDSHELF_LOG` (or `RUST_LOG`) for finer-grained output:

```bash
MDSHELF_LOG=debug mdshelf serve
```
