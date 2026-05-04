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
    Ok(())
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
