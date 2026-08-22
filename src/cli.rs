use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use tracing_subscriber::{EnvFilter, fmt};

use crate::config::Config;
use crate::content::Universe;
use crate::render::Renderer;
use crate::theme::ThemeStack;

pub const DEFAULT_SERVICE_NAME: &str = "mdshelf";

#[derive(Parser, Debug)]
#[command(
    name = "mdshelf",
    about = "Serve folders of markdown files as Astro-style websites.",
    version,
    propagate_version = true
)]
pub struct Cli {
    /// Increase log verbosity. Repeat for more detail (e.g. -vv).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Suppress non-error output.
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run the web server in the foreground (default for CLI usage).
    Serve(ServeArgs),
    /// Scaffold a config file and (optionally) copy the default theme for customization.
    Init(InitArgs),
    /// Validate a config file and dry-run content discovery.
    Check(CheckArgs),
    /// Export the site as a static bundle of HTML and CSS files.
    Export(ExportArgs),
    /// Inspect the access rules declared in your vault's frontmatter.
    #[command(subcommand)]
    Acl(AclCommand),
    /// Query the access log: who read what, and when.
    Audit(AuditArgs),
    /// Configure Google sign-in.
    #[command(subcommand)]
    Auth(AuthCommand),
    /// Register `mdshelf` as a system service (launchd / systemd / Windows SCM).
    Install(ServiceArgs),
    /// Remove the registered system service.
    Uninstall(ServiceNameArgs),
    /// Start the registered system service.
    Start(ServiceNameArgs),
    /// Stop the registered system service.
    Stop(ServiceNameArgs),
    /// Restart the registered system service.
    Restart(ServiceNameArgs),
    /// Show the status of the registered system service.
    Status(ServiceNameArgs),
}

#[derive(Args, Debug, Clone)]
pub struct ServeArgs {
    /// Path to the TOML config file. Defaults to `./mdshelf.toml` then `~/.config/mdshelf/mdshelf.toml`.
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    /// Override the bind host from the config.
    #[arg(long)]
    pub host: Option<String>,

    /// Override the bind port from the config.
    #[arg(short, long)]
    pub port: Option<u16>,

    /// Disable the browser live-reload WebSocket. The filesystem watcher that
    /// rebuilds content on disk changes always runs.
    #[arg(long)]
    pub no_live_reload: bool,

    /// Require sign-in with the named identity provider. Only `google` is supported.
    /// Omitting this leaves the server entirely unauthenticated.
    #[arg(long, value_name = "PROVIDER")]
    pub auth: Option<String>,

    /// Externally visible origin used to build the OAuth redirect URI, e.g.
    /// `https://docs.acme.com`. Defaults to the bind address.
    #[arg(long, value_name = "URL")]
    pub public_url: Option<String>,

    /// Obtain and auto-renew a Let's Encrypt certificate for this domain.
    /// Requires ports 80 and 443 to be reachable from the internet.
    #[arg(long, value_name = "HOST", conflicts_with_all = ["tls_cert", "behind_proxy"])]
    pub domain: Option<String>,

    /// Serve TLS with an existing certificate chain (PEM).
    #[arg(long, value_name = "FILE", requires = "tls_key")]
    pub tls_cert: Option<PathBuf>,

    /// Private key for `--tls-cert` (PEM).
    #[arg(long, value_name = "FILE", requires = "tls_cert")]
    pub tls_key: Option<PathBuf>,

    /// TLS is terminated upstream (ALB, nginx, Caddy, Cloudflare). Requires
    /// `--public-url` so redirect URIs match what browsers actually see.
    #[arg(long, requires = "public_url")]
    pub behind_proxy: bool,

    /// Contact address for the ACME account. Let's Encrypt uses it for expiry notices.
    #[arg(long, value_name = "EMAIL", requires = "domain")]
    pub acme_contact: Option<String>,

    /// Directory for cached ACME certificates. Defaults to `~/.config/mdshelf/acme`.
    #[arg(long, value_name = "DIR", requires = "domain")]
    pub acme_cache: Option<PathBuf>,

    /// Use the Let's Encrypt staging environment (untrusted certs, high rate limits).
    #[arg(long, requires = "domain")]
    pub acme_staging: bool,
}

#[derive(Args, Debug, Clone)]
pub struct InitArgs {
    /// Directory for `mdshelf.toml`, `content/`, and optional `theme/`. Defaults to `~/.config/mdshelf`.
    #[arg(default_value = "~/.config/mdshelf")]
    pub directory: PathBuf,

    /// Also copy the bundled default theme into `<directory>/theme` for customization.
    #[arg(long)]
    pub with_theme: bool,

    /// Overwrite existing files instead of erroring out.
    #[arg(long)]
    pub force: bool,
}

#[derive(Args, Debug, Clone)]
pub struct CheckArgs {
    /// Path to the TOML config file.
    #[arg(short, long)]
    pub config: Option<PathBuf>,
}

#[derive(Args, Debug, Clone)]
pub struct ExportArgs {
    /// Path to the TOML config file.
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    /// Output directory for the static bundle. Defaults to `./dist`.
    #[arg(short, long, default_value = "dist")]
    pub output: PathBuf,

    /// Overwrite an existing output directory instead of erroring out.
    #[arg(long)]
    pub force: bool,

    /// Export only the site with this mount path or title (repeatable).
    /// When exactly one site is selected, the bundle is written at the output
    /// root without the mount prefix. Omit to export every configured site.
    #[arg(long)]
    pub site: Vec<String>,

    /// Export exactly what this address is allowed to see. Required when the vault
    /// declares any access rules, because a static bundle has no authentication.
    #[arg(long = "as", value_name = "EMAIL")]
    pub as_viewer: Option<String>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum AclCommand {
    /// Show, rule by rule, why an address can or cannot read a page.
    Explain(AclExplainArgs),
    /// Report access-rule problems: malformed blocks, unreachable subtrees, unused grants.
    Doctor(AclDoctorArgs),
    /// Grant an address access to a page or folder by editing its frontmatter.
    Grant(AclGrantArgs),
}

#[derive(Args, Debug, Clone)]
pub struct AclDoctorArgs {
    /// Path to the TOML config file.
    #[arg(short, long)]
    pub config: Option<PathBuf>,
}

#[derive(Args, Debug, Clone)]
pub struct AclGrantArgs {
    /// Path to the TOML config file.
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    /// The address to grant access to.
    pub email: String,

    /// Page or folder to grant access to, as a site-relative path.
    pub path: String,

    /// Do not prompt before creating a folder's `index.md`.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Subcommand, Debug, Clone)]
pub enum AuthCommand {
    /// Walk through creating a Google OAuth client, then verify it with a test sign-in.
    Setup(AuthSetupArgs),
}

#[derive(Args, Debug, Clone)]
pub struct AuthSetupArgs {
    /// Path to the TOML config file, used to work out the redirect URI.
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    /// The URL browsers will use to reach this server, e.g. `https://docs.acme.com`.
    #[arg(long, value_name = "URL")]
    pub public_url: Option<String>,

    /// Instead of the wizard, write a self-signed development certificate here.
    #[arg(long, value_name = "DIR", num_args = 0..=1, default_missing_value = ".")]
    pub self_signed: Option<PathBuf>,
}

#[derive(Args, Debug, Clone)]
pub struct AuditArgs {
    /// Path to the TOML config file.
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    /// Show who read this path.
    #[arg(long, conflicts_with = "email")]
    pub path: Option<String>,

    /// Show what this address read.
    #[arg(long)]
    pub email: Option<String>,

    /// Delete every log entry and session for an address (GDPR erasure).
    #[arg(long, requires = "email")]
    pub forget: bool,
}

#[derive(Args, Debug, Clone)]
pub struct AclExplainArgs {
    /// Path to the TOML config file.
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    /// Page to resolve, as a site-relative path (e.g. `hr/comp.md`) or a URL
    /// (e.g. `/docs/hr/comp`).
    pub path: String,

    /// The address to resolve it for.
    pub email: String,
}

#[derive(Args, Debug, Clone)]
pub struct ServiceArgs {
    /// Path to the TOML config file the installed service will load.
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    /// Service name to register. Defaults to `mdshelf`.
    #[arg(long, default_value = DEFAULT_SERVICE_NAME)]
    pub name: String,

    /// Install at user (per-user) scope instead of system scope.
    #[arg(long)]
    pub user: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ServiceNameArgs {
    /// Service name. Defaults to `mdshelf`.
    #[arg(long, default_value = DEFAULT_SERVICE_NAME)]
    pub name: String,

    /// Operate at user scope instead of system scope.
    #[arg(long)]
    pub user: bool,
}

impl Cli {
    pub fn verbosity(&self) -> Verbosity {
        if self.quiet {
            Verbosity::Quiet
        } else {
            match self.verbose {
                0 => Verbosity::Default,
                1 => Verbosity::Verbose,
                _ => Verbosity::Trace,
            }
        }
    }

    pub async fn run(self) -> Result<()> {
        match self.command {
            Command::Serve(args) => crate::server::run(args).await,
            Command::Init(args) => init(args),
            Command::Check(args) => check(args),
            Command::Export(args) => crate::export::run(args),
            Command::Acl(AclCommand::Explain(args)) => acl_explain(args),
            Command::Acl(AclCommand::Doctor(args)) => acl_doctor(args),
            Command::Acl(AclCommand::Grant(args)) => acl_grant(args),
            Command::Audit(args) => audit(args),
            Command::Auth(AuthCommand::Setup(args)) => auth_setup(args).await,
            Command::Install(args) => crate::service::install(args),
            Command::Uninstall(args) => crate::service::uninstall(args),
            Command::Start(args) => crate::service::start(args),
            Command::Stop(args) => crate::service::stop(args),
            Command::Restart(args) => crate::service::restart(args),
            Command::Status(args) => crate::service::status(args),
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub enum Verbosity {
    Quiet,
    Default,
    Verbose,
    Trace,
}

impl Verbosity {
    pub fn as_filter(self) -> &'static str {
        match self {
            Verbosity::Quiet => "mdshelf=error",
            Verbosity::Default => "mdshelf=info,tower_http=warn",
            Verbosity::Verbose => "mdshelf=debug,tower_http=info",
            Verbosity::Trace => "mdshelf=trace,tower_http=debug",
        }
    }
}

pub fn install_tracing(verbosity: Verbosity) {
    let filter = EnvFilter::try_from_env("MDSHELF_LOG")
        .unwrap_or_else(|_| EnvFilter::new(verbosity.as_filter()));
    let _ = fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();
}

fn init(args: InitArgs) -> Result<()> {
    let directory = expand_init_directory(&args.directory)?;
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("creating directory {}", directory.display()))?;

    let config_path = directory.join("mdshelf.toml");
    write_file(&config_path, EXAMPLE_CONFIG.as_bytes(), args.force)?;
    println!("wrote {}", config_path.display());

    if args.with_theme {
        let theme_dir = directory.join("theme");
        crate::theme::extract_default_theme(&theme_dir, args.force)
            .with_context(|| format!("extracting default theme to {}", theme_dir.display()))?;
        println!("extracted default theme into {}", theme_dir.display());
    }

    let demo = directory.join("content/welcome.md");
    write_file(&demo, DEMO_PAGE.as_bytes(), args.force)?;
    println!("wrote {}", demo.display());

    Ok(())
}

fn check(args: CheckArgs) -> Result<()> {
    let config = Config::load(args.config.as_deref())?;
    let theme = ThemeStack::from_config(&config)?;
    let renderer = Renderer::new(&theme)?;
    let universe = Universe::build(&config)?;

    let mut total_pages = 0usize;
    println!("config: {} site(s)", config.sites.len());
    for site in universe.sites() {
        let count = site.pages().count();
        total_pages += count;
        println!(
            "  - {} mounted at {} -> {} ({} page(s))",
            site.title,
            site.mount,
            site.root.display(),
            count
        );
    }
    println!("templates loaded: {}", renderer.template_names().len());
    println!("total pages: {}", total_pages);

    // D31: a malformed rule block is an access-control fault, so it fails the check
    // rather than waiting to 404 a page for the whole team at request time.
    let mut acl_errors = 0usize;
    for site in universe.sites() {
        for (file, error) in site.acl().poisoned() {
            acl_errors += 1;
            match error.line {
                Some(line) => eprintln!("  ✗ {file}:{line}  {}", error.message),
                None => eprintln!("  ✗ {file}  {}", error.message),
            }
        }
    }

    if acl_errors > 0 {
        anyhow::bail!(
            "{acl_errors} access-rule error(s). Files with an invalid `allow` or `deny` \
             block are unreadable by everyone until the block is fixed."
        );
    }

    let rule_count: usize = universe
        .sites()
        .iter()
        .map(|site| site.acl().rows().len())
        .sum();
    if rule_count > 0 {
        println!("access rules: {rule_count} (all valid)");
    }
    Ok(())
}

fn acl_explain(args: AclExplainArgs) -> Result<()> {
    let config = Config::load(args.config.as_deref())?;
    let universe = Universe::build(&config)?;

    let email = crate::auth::normalize_email(&args.email);
    if !crate::auth::is_valid_email(&email) {
        anyhow::bail!("`{}` is not a valid email address", args.email);
    }

    let (site, rel_path) = locate_target(&universe, &args.path)?;
    let resolution = site.acl().resolve(&rel_path, &email);

    println!("site:  {} (mounted at {})", site.title, site.mount);
    println!("path:  {}", rel_path.display());
    println!("email: {email}");
    println!();

    if resolution.steps.is_empty() {
        println!("  (no rule at any level mentions this path)");
    }
    for step in &resolution.steps {
        let verdict = match (step.poisoned, step.decision) {
            (true, _) => "INVALID".to_string(),
            (false, Some(decision)) => decision.as_str().to_uppercase(),
            (false, None) => "—".to_string(),
        };
        let marker = if step.decisive { "→" } else { " " };
        println!(
            "  {marker} {:<6} {:<40} {}",
            step.level.as_str(),
            step.source,
            verdict
        );
    }

    println!();
    println!("verdict: {}", resolution.reason());
    Ok(())
}

async fn auth_setup(args: AuthSetupArgs) -> Result<()> {
    // The redirect URI depends on the origin browsers use, so fall back to the config's
    // bind address only when the operator has not told us something better.
    let public_url = match args.public_url.as_deref() {
        Some(explicit) => {
            let trimmed = explicit.trim_end_matches('/');
            if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
                anyhow::bail!("--public-url must start with http:// or https://");
            }
            trimmed.to_string()
        }
        None => match Config::load(args.config.as_deref()) {
            Ok(config) => format!("http://{}:{}", config.host, config.port),
            Err(_) => "http://127.0.0.1:4444".to_string(),
        },
    };

    crate::auth::setup::run(&public_url, args.self_signed.as_ref()).await
}

fn acl_doctor(args: AclDoctorArgs) -> Result<()> {
    let config = Config::load(args.config.as_deref())?;
    let universe = Universe::build(&config)?;

    // The access log tells us which grants have ever been used. It only exists when
    // auth has actually run, so its absence is not itself a problem.
    let seen: Vec<String> = open_store(&config)
        .ok()
        .and_then(|store| store.seen_emails().ok())
        .unwrap_or_default();

    let mut errors = 0usize;
    let mut warnings = 0usize;

    for site in universe.sites() {
        let acl = site.acl();
        println!("{} ({})", site.title, site.mount);

        if acl.is_empty() {
            println!(
                "  ⚠ no access rules at all — with `--auth google` this site is \
                 invisible to everyone (fail-closed default)"
            );
            warnings += 1;
            continue;
        }

        for (file, error) in acl.poisoned() {
            errors += 1;
            match error.line {
                Some(line) => println!("  ✗ {file}:{line}  {}", error.message),
                None => println!("  ✗ {file}  {}", error.message),
            }
        }

        // A folder whose every page resolves to Deny for every address named anywhere
        // in the site is unreachable: almost always a mistake rather than an intent.
        let known = acl.known_emails();
        let mut unreachable: BTreeSet<String> = BTreeSet::new();
        for page in site.pages() {
            let readable = known.iter().any(|email| acl.allows(&page.rel_path, email));
            if !readable {
                let folder = crate::acl::index::parent_folder(&page.rel_path);
                unreachable.insert(if folder.is_empty() {
                    "(site root)".to_string()
                } else {
                    folder
                });
            }
        }
        for folder in &unreachable {
            println!("  ⚠ {folder}  no address named in this vault can read this");
            warnings += 1;
        }

        for email in &known {
            if !seen.is_empty() && !seen.contains(email) {
                println!("  ⚠ {email}  granted access but never seen in the access log");
                warnings += 1;
            }
        }

        // D8's sharp edge: a folder with no index file is governed from further up, and
        // adding one later silently changes access for the whole subtree.
        let folders_with_rules: BTreeSet<&str> = acl.rule_folders().collect();
        let mut inheriting: BTreeSet<String> = BTreeSet::new();
        for page in site.pages() {
            let folder = crate::acl::index::parent_folder(&page.rel_path);
            if !folder.is_empty() && !folders_with_rules.contains(folder.as_str()) {
                inheriting.insert(folder);
            }
        }
        for folder in &inheriting {
            println!(
                "  · {folder}/  no index.md — inherits from an ancestor; creating one \
                 would change access for everything beneath it"
            );
        }
    }

    println!();
    if errors > 0 {
        anyhow::bail!("{errors} error(s), {warnings} warning(s)");
    }
    println!("no errors ({warnings} warning(s))");
    Ok(())
}

/// The only command in mdshelf that writes to the user's vault (D32).
///
/// Everything else — serving, exporting, checking — is strictly read-only. Writes here
/// are explicit, confirmed, and refuse to proceed if the file moved underneath them.
fn acl_grant(args: AclGrantArgs) -> Result<()> {
    let config = Config::load(args.config.as_deref())?;
    let universe = Universe::build(&config)?;

    let email = crate::auth::normalize_email(&args.email);
    if !crate::auth::is_valid_email(&email) {
        anyhow::bail!("`{}` is not a valid email address", args.email);
    }

    let (site, target) = resolve_grant_target(&universe, &args.path, args.yes)?;
    let absolute = site.root.join(&target);

    let before = std::fs::read_to_string(&absolute)
        .with_context(|| format!("reading {}", absolute.display()))?;
    let modified_before = std::fs::metadata(&absolute).and_then(|m| m.modified()).ok();

    let Some(updated) = crate::acl::edit::add_to_allow_list(&before, &email)? else {
        println!("{email} is already listed in {}", target.display());
        return Ok(());
    };

    // Refuse if the file changed between the read and the write: silently discarding
    // somebody's concurrent edit to an access-control file is not an acceptable
    // failure mode.
    let modified_now = std::fs::metadata(&absolute).and_then(|m| m.modified()).ok();
    if modified_before != modified_now {
        anyhow::bail!(
            "{} changed on disk while it was being edited; nothing was written",
            absolute.display()
        );
    }

    std::fs::write(&absolute, updated)
        .with_context(|| format!("writing {}", absolute.display()))?;
    println!("added {email} to `allow` in {}", target.display());
    Ok(())
}

/// Work out which file a grant should be written into, creating a folder index if the
/// user agrees (D32).
fn resolve_grant_target(
    universe: &Universe,
    raw: &str,
    assume_yes: bool,
) -> Result<(std::sync::Arc<crate::content::Site>, PathBuf)> {
    let relative = raw.trim_start_matches('/').trim_end_matches('/');

    for site in universe.sites() {
        let candidate = site.root.join(relative);

        if candidate.is_file() {
            return Ok((site.clone(), PathBuf::from(relative)));
        }

        if candidate.is_dir() {
            // A folder is governed by its index file (D7/D8).
            for stem in ["index.md", "README.md", "readme.md"] {
                if candidate.join(stem).is_file() {
                    return Ok((site.clone(), PathBuf::from(relative).join(stem)));
                }
            }
            let index_path = PathBuf::from(relative).join("index.md");
            if !assume_yes
                && !confirm(&format!(
                    "{} has no index.md, which is where a folder's rules live.\nCreate it? [y/N] ",
                    candidate.display()
                ))?
            {
                anyhow::bail!("cancelled; nothing was written");
            }
            let absolute = site.root.join(&index_path);
            let title = candidate
                .file_name()
                .map(|name| crate::content::page::humanize(&name.to_string_lossy()))
                .unwrap_or_else(|| "Index".to_string());
            std::fs::write(&absolute, format!("---\ntitle: {title}\n---\n"))
                .with_context(|| format!("creating {}", absolute.display()))?;
            println!("created {}", absolute.display());
            return Ok((site.clone(), index_path));
        }

        // Allow the URL form (`hr/comp`) as well as the filename.
        if let Some(page) = site
            .page(relative)
            .or_else(|| site.page(&format!("{relative}/index")))
        {
            return Ok((site.clone(), page.rel_path.clone()));
        }
    }

    anyhow::bail!("no page or folder matches `{raw}` in any configured site")
}

fn confirm(prompt: &str) -> Result<bool> {
    use std::io::Write;
    print!("{prompt}");
    std::io::stdout().flush().ok();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn audit(args: AuditArgs) -> Result<()> {
    let config = Config::load(args.config.as_deref())?;
    let store = open_store(&config)?;

    if args.forget {
        let email = crate::auth::normalize_email(args.email.as_deref().unwrap_or_default());
        let (entries, sessions) = store.forget_email(&email)?;
        println!("removed {entries} log entr(ies) and {sessions} session(s) for {email}");
        return Ok(());
    }

    let entries = match (args.path.as_deref(), args.email.as_deref()) {
        (Some(path), _) => store.access_by_path(path)?,
        (None, Some(email)) => store.access_by_email(&crate::auth::normalize_email(email))?,
        (None, None) => anyhow::bail!("pass --path <path> or --email <address>"),
    };

    if entries.is_empty() {
        println!("no access log entries match");
        return Ok(());
    }

    // The outcome is the whole point of the column. Without it, "who has read this
    // document" silently includes everyone who was *refused* it — the opposite
    // conclusion, in the one situation where the answer matters.
    let mut read = 0usize;
    let mut refused = 0usize;
    for entry in &entries {
        let outcome = if entry.outcome == "allow" {
            read += 1;
            "read"
        } else {
            refused += 1;
            "REFUSED"
        };
        println!(
            "  {:<9} {:<28} {:<36} {}",
            outcome,
            entry.email,
            entry.path,
            format_timestamp(entry.ts)
        );
    }
    println!();
    println!("{read} read, {refused} refused");
    Ok(())
}

/// Open the sidecar for a read-only CLI query.
fn open_store(config: &Config) -> Result<crate::auth::store::Store> {
    let auth = config.auth.clone().unwrap_or_default();
    let path = auth
        .database
        .clone()
        .unwrap_or_else(|| config.source_dir.join("mdshelf.db"));
    if !path.exists() {
        anyhow::bail!(
            "no access log at {} yet — it is created the first time the server runs \
             with `--auth google`.",
            path.display()
        );
    }
    crate::auth::store::Store::open(&path)
}

/// Format a millisecond timestamp as UTC, without pulling in a date library.
fn format_timestamp(ms: i64) -> String {
    let seconds = ms / 1000;
    let days = seconds.div_euclid(86_400);
    let time_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}Z",
        time_of_day / 3600,
        (time_of_day % 3600) / 60,
        time_of_day % 60
    )
}

/// Howard Hinnant's days-from-civil algorithm, inverted.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Find the site and site-relative path for a user-supplied path or URL.
fn locate_target(
    universe: &Universe,
    raw: &str,
) -> Result<(std::sync::Arc<crate::content::Site>, PathBuf)> {
    // A URL with a mount prefix, e.g. /docs/hr/comp.
    for site in universe.sites() {
        if let Some(tail) = raw.strip_prefix(site.mount.as_str())
            && (tail.is_empty() || tail.starts_with('/'))
        {
            let trimmed = tail.trim_start_matches('/');
            if let Some(page) = find_page_by_url_path(site, trimmed) {
                return Ok((site.clone(), page));
            }
        }
    }

    // Otherwise treat it as a path relative to some site root.
    let relative = raw.trim_start_matches('/');
    for site in universe.sites() {
        // Resolve to the casing the filesystem uses, exactly as the server does.
        //
        // Rules are keyed on the on-disk path. Answering for the string the user typed
        // meant `acl explain HR/comp.md` reported ALLOW where the server denies — the
        // wrong answer, in the direction of false reassurance, from the one command
        // whose whole purpose is telling you whether a page is locked down.
        if let Some(resolved) =
            crate::content::source::true_relative_path(&site.root, Path::new(relative))
            && site.root.join(&resolved).exists()
        {
            return Ok((site.clone(), resolved));
        }
        if let Some(page) = find_page_by_url_path(site, relative) {
            return Ok((site.clone(), page));
        }
    }

    // Resolving a path that does not exist is still meaningful: it reports what the
    // inherited rules would say if the file were created there.
    let site = universe
        .sites()
        .first()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no sites are configured"))?;
    Ok((site, PathBuf::from(relative)))
}

fn find_page_by_url_path(site: &crate::content::Site, url_path: &str) -> Option<PathBuf> {
    let key = url_path.trim_matches('/');
    let stripped = key.strip_suffix(".md").unwrap_or(key);
    site.page(stripped)
        .or_else(|| site.page(&format!("{stripped}/index")))
        .map(|page| page.rel_path.clone())
}

fn expand_init_directory(directory: &Path) -> Result<PathBuf> {
    let as_str = directory
        .to_str()
        .with_context(|| format!("init directory must be UTF-8: {}", directory.display()))?;
    Ok(PathBuf::from(shellexpand::tilde(as_str).into_owned()))
}

fn write_file(path: &Path, contents: &[u8], force: bool) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating parent of {}", path.display()))?;
    }
    if path.exists() && !force {
        anyhow::bail!(
            "{} already exists; pass --force to overwrite",
            path.display()
        );
    }
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

const EXAMPLE_CONFIG: &str = include_str!("../examples/mdshelf.toml");
const DEMO_PAGE: &str = include_str!("../examples/welcome.md");
