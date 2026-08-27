# mdshelf

Turn any folder of Markdown files into a fast, beautiful, browsable site. Point mdshelf at your OpenClaw, Hermes, GBrain, wikis, notes, docs, etc. — mount each one under its own URL (e.g. `/agent`, `/hermes`, `gbrain`, `/docs`, `/notes`, `/blog`) — and read them anywhere with hot reload, clean typography, and a mobile-first theme out of the box.

Pairs beautifully with [Tailscale](https://tailscale.com): one command serves your shelf to every device on your tailnet — phone, iPad, laptop. Sharing with Tailscale is fully private by default: no port forwarding, no public DNS, nothing exposed to the open internet.

One command installs it as a native system service (launchd, systemd, or Windows SCM) that runs quietly in the background and survives reboots.

Ships as a single static Rust binary with zero runtime dependencies.

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
- Copy or download any page's Markdown source straight from its header

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
| `mdshelf export` | Export a static bundle of HTML and CSS files |
| `mdshelf auth setup` | Walk through creating a Google OAuth client, then verify it |
| `mdshelf acl explain <path> <email>` | Show why an address can or cannot read a page |
| `mdshelf acl doctor` | Report access-rule problems |
| `mdshelf acl grant <email> <path>` | Add an address to a page or folder's `allow` list |
| `mdshelf audit --path/--email` | Query the access log |
| `mdshelf share <path>` | Create a link that shares one page with anyone holding the URL |
| `mdshelf share list` | List share links (`--all` for dead ones, `--json` for an inventory) |
| `mdshelf share revoke <id>` | Revoke one link, or `--all` to revoke every live link |
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

Each `[[sites]]` block mounts a directory at a URL prefix. Only `.md` files are rendered — other files are ignored. `mount` must not be `/`. Add as many sites as you like.

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

Two further keys, `allow` and `deny`, control who may read a page — see
[Private sites with Google sign-in](#private-sites-with-google-sign-in). They are
stripped before rendering, so the addresses in them never reach the browser.

## Private sites with Google sign-in

`mdshelf serve --auth google` puts every page behind Google sign-in and serves each
reader only what they have been invited to. Without the flag nothing changes: the
server behaves exactly as it always has.

### Who may read what

Access rules live in the vault, as frontmatter, in the file they protect:

```yaml
---
title: Compensation 2026
allow:
  - ana@corp.com
  - hr-lead@corp.com
deny:
  - intern@corp.com
---
```

Because the rule is part of the file, renaming, moving, or deleting it carries or
removes the rule with it. There is nothing to keep in sync.

Rules exist at three levels:

| Where | Governs |
|---|---|
| Any `.md` file | That file alone |
| A folder's `index.md` | **That folder, everything beneath it, and the index page itself** |
| The vault root's `index.md` | The whole site |

Both keys take a **list**. A bare string is an error, not a one-element list — so the
common typo `allow: a@x.com, b@y.com` is rejected rather than quietly granting access
to one malformed address.

Only `index.md` governs a folder. `readme.md` and `index.markdown` are ordinary pages to
mdshelf — served at `/readme` and `/index.markdown` — so a rule in one of them applies to
that page alone, not to everything beside it.

### How a decision is reached

1. Collect every rule that applies: the file's own, then each ancestor folder's
   `index.md` from nearest to furthest, then the site root.
2. The most specific level that **names the address** decides. A `deny` there beats an
   inherited `allow`, and an `allow` there beats an inherited `deny`.
3. If no level names the address, the answer is **deny**.

> **A site-level `allow` reaches into every folder.** A folder's `allow` *adds* people;
> it does not remove anyone. To keep somebody out of a subtree that a broader rule
> already grants, name them in that folder's `deny`. `mdshelf acl doctor` will not
> guess this for you — use `mdshelf acl explain <path> <email>` whenever the outcome
> surprises you.

### Fail closed

A path that no rule names is invisible. **A vault with no rules at all shows nobody
anything** when `--auth google` is on — start by granting someone in the root
`index.md`. Likewise, a malformed `allow` or `deny` block makes that file unreadable by
everyone until it is fixed; `mdshelf check` fails on such a block so it never reaches a
running server, and `mdshelf acl doctor` reports it on a server already running.

Readers who are signed in but not invited get the same response for a restricted page
and for one that does not exist, so the site cannot be probed to learn what it contains.

### Setting up Google credentials

mdshelf uses **your** Google OAuth client; no credentials ship with the binary.

```bash
mdshelf auth setup
```

The wizard prints the console steps, gives you the exact redirect URI to register,
stores the credentials in `~/.config/mdshelf/credentials.env` (mode 0600), and proves
with a real sign-in before it exits. `MDSHELF_GOOGLE_CLIENT_ID` and
`MDSHELF_GOOGLE_CLIENT_SECRET` override the file if you use a secret manager.

### HTTPS

Google accepts a plain-HTTP redirect URI only for `localhost`, so any real deployment
needs a certificate. mdshelf refuses to serve authenticated sessions over plain HTTP on
a non-loopback address rather than warning and doing it anyway.

```bash
# Let's Encrypt, obtained and renewed automatically (needs ports 80 and 443)
mdshelf serve --auth google --domain docs.acme.com

# A certificate you already have
mdshelf serve --auth google --tls-cert fullchain.pem --tls-key privkey.pem

# TLS terminated by an ALB, nginx, Caddy, or Cloudflare in front
mdshelf serve --auth google --behind-proxy --public-url https://docs.acme.com
```

For LAN or offline testing, `mdshelf auth setup --self-signed <dir>` writes a
development certificate. Browsers will warn about it, and Google will not accept a
self-signed host as a redirect URI, so it is only useful before auth is switched on.

### Sessions, revocation, and Google availability

Removing an address from a vault file takes effect on the **next request** — the
watcher reloads the rules and every request resolves them afresh.

Sessions are re-validated against Google when they have been idle for more than 30
minutes, so a suspended Google account loses access without a reader being interrupted
mid-page. Sessions expire absolutely after `session_max_age` (default 30 days).

> **Google is a hard dependency for re-validation.** If a re-validation attempt fails —
> whether Google explicitly rejects the grant *or* is simply unreachable — the session
> ends and the reader signs in again. A Google incident, or a self-hosted box that
> briefly loses its uplink, will therefore sign out readers whose sessions have gone
> idle. This is deliberate: it is what makes "revoked means revoked" true without
> qualification. Each such event is logged at WARN with the cause, so an outage is
> visible in the logs rather than presenting as mysterious sign-outs.

### Static export

A static bundle has no authentication, so exporting a vault that declares rules
requires saying whose view it is:

```bash
mdshelf export --as ana@corp.com --output ./for-ana
```

The bundle then contains exactly the pages, navigation, listings, and attachments that
address can see, and reports how many pages were skipped. `mdshelf export` on a vault
with no rules is unchanged. Note that the bundle itself is unprotected once you send it.

### Shareable links

A **Share** control appears on every page a signed-in viewer can read. Pick how long the
link should live and you get an opaque URL:

```
https://docs.acme.com/s/kJ8mQ2xR7pL4nT9v
```

Anyone holding that URL reads **that one page** — no account, no sign-in — in a clean
reading view, until it expires, until you revoke it, or until your own access to the
page goes away. The URL reveals nothing to anyone who merely sees it: not the site, not
the folder, not the page title, not that the page exists.

Share links require `--auth google`. Without it every page is already public, so a link
would imply a protection that is not there.

The same thing from a terminal:

```bash
# Create. Prints exactly one URL, and only once.
mdshelf share /docs/hr/comp --for 1d --as ana@corp.com

# Or until a specific instant. Read as UTC unless it carries an offset.
mdshelf share /docs/hr/comp --until 2026-09-01

# What am I exposing?
mdshelf share list
mdshelf share list --all --json      # inventory, including dead links

# The incident lever.
mdshelf share revoke ab12cd
mdshelf share revoke --all
```

`--for` and `--until` are alternatives, not a pair. Without either, `[links]
default_lifetime` applies. `--base-url` supplies the origin browsers use to reach the
server when the config binds an address nobody can visit. Signed-in viewers see their
own links on `/__shares`.

**The URL is shown once and cannot be recovered.** mdshelf stores only `sha256(token)`,
so nothing — not the database, not `share list`, not a support request — can produce a
lost URL again. Create another instead.

#### What a share link is not

- **Not a folder or site link.** One link reaches one page plus the images and files
  that page references. Share several pages by making several links, or use
  `mdshelf export --as`.
- **Not password-protected.** The URL is the whole credential.
- **Not a second copy of your access rules.** A link is a *window onto its issuer's own
  live access*, revalidated on every request.
- **Not recoverable, and not extendable.** To change an expiry, revoke and re-mint.
- **Not analytics.** Reads land in the access log; querying it is `mdshelf audit`.

#### Link configuration

```toml
[links]
enabled           = true    # incident kill switch; leaves the rows intact
prefix            = "/s"    # the route links live under
default_lifetime  = "1d"    # used when neither --for nor --until is given
max_lifetime      = "30d"   # the cap; a longer request is refused, not clamped
revoked_retention = "90d"   # how long dead rows are kept before the sweep deletes them

[auth]
bad_link_retention = "7d"   # how long requests with unknown tokens are kept
```

A site mounted at the same place as `prefix` fails startup and `mdshelf check`: move the
share route rather than the site.

#### Security note

> **A share link is a bearer credential.** Whoever holds the URL has the access —
> forwarding the URL forwards the access, and mdshelf cannot tell the difference. Four
> consequences worth stating plainly:
>
> - **The audit names a link, not a person.** A read appears as `link:ab12cd`, because
>   that is the honest answer to "who read this?" for a URL that anyone may have passed
>   on. `mdshelf share list` prints the same id, so the two join by eye.
> - **The banner exposes the sharer's address.** The reading view says "Shared by
>   ana@corp.com", and that reaches everyone the URL reaches, forwarded recipients
>   included. It is your own address, never a third party's.
> - **A link tracks its issuer's live access.** Remove someone from an allow list and
>   every link they made stops working on the next request — so offboarding needs no
>   separate cleanup. The same mechanism can break a working link with no signal to
>   either side when somebody changes teams; `share list` shows the link as dead so the
>   sharer can see why.
> - **Link-preview bots fetch URLs.** A bearer URL posted in a chat channel may be
>   fetched and rendered into that channel. Responses carry `Cache-Control: no-store`
>   and `X-Robots-Tag: noindex, nofollow`, which limits what persists, but the fetch
>   itself is inherent to bearer links.

### Auth-related configuration

```toml
[auth]
session_max_age = "30d"        # absolute session ceiling
owner_email     = "owner@corp.com"  # request-access link on the deny page
audit_retention = "90d"        # access-log retention, pruned hourly
bad_link_retention = "7d"      # requests with unknown share tokens
# database = "./mdshelf.db"    # sessions, access log, and share links
# key_file = "~/.config/mdshelf/secret.key"  # encrypts refresh tokens, mode 0600
```

**`mdshelf.db` is not disposable.** Sessions, the access log and the derived rule index
can all be rebuilt or lived without, but share links cannot: only their hashes are
stored, so deleting the database destroys every live link with no way to recreate one.
Keep `mdshelf share list --json` if you want an inventory of what a deletion would cost.

### What mdshelf writes

`mdshelf acl grant` is the **only** command that writes to your vault, and it asks
before creating a folder's `index.md`. Serving, checking, exporting, and explaining are
strictly read-only.

## Theme customisation

Override or extend templates by pointing `[theme].directory` at a local directory:

- `layouts/` — `base.html`, `doc.html`, `home.html`, `error.html`, `link.html`
- `partials/` — `header.html`, `sidebar.html`, `toc.html`, `breadcrumbs.html`, `footer.html`
- `assets/` — static files served from `/__assets/...`

### Theme capabilities

Two parts of the theme are optional in a way worth knowing about:

- **`{{ share_control }}`** renders the Share popover. The bundled theme places it in
  `partials/header.html`. A theme that never mentions the variable renders normally and
  simply has no sharing — mdshelf never injects it into your markup, so removing that
  line is a supported way to turn the button off for a site. The CLI still works.
- **`layouts/link.html`** is the reading view a share-link recipient sees. A theme
  without it falls back to a self-contained page that still picks up the theme's
  stylesheets, so recipients never see a template error.

Syntax highlighting CSS is served from `/__mdshelf/syntax.css` (class-based, light/dark aware).

## System service

Install and manage a native service (launchd on macOS, systemd on Linux, SCM on Windows):

```bash
sudo mdshelf install
sudo mdshelf start
sudo mdshelf status
sudo mdshelf stop
sudo mdshelf uninstall
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

Pair it with `sudo mdshelf install && sudo mdshelf start` (see [System service](#system-service)) so both services survive reboots.

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

## Copy and download a page as Markdown

Every page mdshelf serves is a `.md` file, and the page header carries a **Page actions**
button that hands you that file:

- **Copy as Markdown** — puts the source on the clipboard.
- **Download Markdown** — saves it under its original vault filename, e.g. `setup.md`,
  so it round-trips straight back into Obsidian and sibling `.md` links keep resolving.

Both actions use the source already embedded in the page you are reading, so they are
instant, work offline, and work inside an exported bundle opened from `file://`.

**Frontmatter is not included.** The copied and downloaded Markdown is the body only — no
YAML block, and therefore no `allow` or `deny` list. If the body does not already open
with an `# H1`, the page title is prepended, so a pasted document always has one. Line
endings are normalized to LF with exactly one trailing newline.

> **Clipboard access is blocked over plain HTTP.** Browsers expose `navigator.clipboard`
> only in a secure context — HTTPS, or `localhost` / `127.0.0.1`. Over a plain `http://`
> LAN or tailnet address it is simply absent, so mdshelf falls back to a legacy copy, and
> if that is refused too it falls back to manual selection: a **Select and copy** panel
> opens with the text already selected, so Ctrl/Cmd-C always works. Serving over HTTPS
> removes the fallback entirely.

A generated folder-index page has no `.md` file behind it, so both actions appear
disabled with the reason. Downloads normally happen entirely in the browser; iOS and
iPadOS Safari mishandle that, so there they go through `GET /__mdshelf/md/<page>`, which
enforces exactly the same access rules as viewing the page — a page you may not read
returns the same response as one that does not exist.

### Static export

```bash
mdshelf export                          # all sites → ./dist
mdshelf export --site docs              # one site, flat at output root
mdshelf export --site docs --site notes # selected sites, mount prefixes kept
mdshelf export -o ./build --force       # overwrite existing output
```

Use `--site` with a mount path (`docs`, `/docs`) or site title. Exporting exactly one site writes a standalone bundle at the output root (no mount prefix). Exporting multiple sites keeps each under its mount path and skips the multi-site home page.

## Logging

Set `MDSHELF_LOG` (or `RUST_LOG`) for finer-grained output:

```bash
MDSHELF_LOG=debug mdshelf serve
```
