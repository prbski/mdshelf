use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, anyhow, bail};
use service_manager::{
    ServiceInstallCtx, ServiceLabel, ServiceLevel, ServiceManager, ServiceManagerKind,
    ServiceStartCtx, ServiceStatus, ServiceStatusCtx, ServiceStopCtx, ServiceUninstallCtx,
    TypedServiceManager,
};

use crate::auth::{AuthSettings, require_stored_credentials};
use crate::cli::{ServeArgs, ServiceArgs, ServiceNameArgs};
use crate::config::Config;
use crate::server::{self, tls};

pub fn install(args: ServiceArgs) -> Result<()> {
    let ServiceArgs {
        mut serve,
        name,
        user,
    } = args;
    serve.config = Some(Config::resolve_path(serve.config.as_deref())?);
    let plan = plan(&serve)?;

    let kind = ServiceManagerKind::native().context("detect native service manager")?;
    let mut manager = TypedServiceManager::target(kind).into_box();
    if user {
        manager
            .set_level(ServiceLevel::User)
            .context("switch service manager to user scope")?;
    } else {
        manager
            .set_level(ServiceLevel::System)
            .context("switch service manager to system scope")?;
    }

    let install_context = install_context(
        kind,
        service_label(&name)?,
        resolve_executable_path()?,
        serve_command(&serve)?,
        plan.home,
    )?;
    manager
        .install(install_context)
        .map_err(|err| anyhow!("install service: {err}"))?;
    println!("installed service {name}");
    if let Some(redirect_uri) = plan.redirect_uri {
        println!("register this redirect URI with your Google OAuth client: {redirect_uri}");
    }
    Ok(())
}

/// What the service needs beyond its command line.
struct Plan {
    /// The installer's `HOME`, when the service must resolve paths under it.
    home: Option<String>,
    redirect_uri: Option<String>,
}

/// Run the checks `serve` runs before it binds, so a service that could never start is
/// never registered.
///
/// Google credentials, the at-rest key and the ACME cache default to paths under
/// `$HOME`, which a system service does not have. The service gets the installing
/// user's `HOME`, so it resolves exactly the paths validated here.
fn plan(serve: &ServeArgs) -> Result<Plan> {
    let config = server::load_config(serve)?;
    let tls_mode = tls::resolve(&config, serve)?;
    let public_url = tls::public_url(&config, serve, &tls_mode, config.port)?;

    let redirect_uri = match serve.auth.as_deref() {
        Some(provider) => {
            server::check_auth_provider(provider)?;
            config.validate_link_prefix()?;
            require_stored_credentials()?;
            let auth = config.auth.clone().unwrap_or_default();
            Some(AuthSettings::resolve(&config, &auth, public_url)?.redirect_uri())
        }
        None => None,
    };

    let needs_home = serve.auth.is_some() || (serve.domain.is_some() && serve.acme_cache.is_none());
    let home = if needs_home {
        Some(
            std::env::var("HOME")
                .ok()
                .filter(|home| !home.is_empty())
                .context("HOME is not set; the service needs it to find credentials and keys")?,
        )
    } else {
        None
    };
    Ok(Plan { home, redirect_uri })
}

/// The service definition for `kind`, with the command encoded the way that backend
/// reads it.
///
/// launchd and the Windows service controller keep arguments apart. systemd, OpenRC and
/// rc.d receive one string joined with spaces: systemd unquotes it by its own rules, so
/// every argument is quoted for systemd; the OpenRC and rc.d scripts word-split it with
/// no way to quote, so a value they would split or expand is refused. Only launchd,
/// systemd and WinSW pass environment variables on, so they alone can carry `HOME`.
fn install_context(
    kind: ServiceManagerKind,
    label: ServiceLabel,
    program: PathBuf,
    args: Vec<OsString>,
    home: Option<String>,
) -> Result<ServiceInstallCtx> {
    let (program, args) = match kind {
        ServiceManagerKind::Systemd => (
            PathBuf::from(systemd_quote(utf8(program.as_os_str())?)),
            args.iter()
                .map(|arg| Ok(OsString::from(systemd_quote(utf8(arg)?))))
                .collect::<Result<_>>()?,
        ),
        ServiceManagerKind::OpenRc | ServiceManagerKind::Rcd => {
            for value in
                std::iter::once(program.as_os_str()).chain(args.iter().map(OsString::as_os_str))
            {
                let value = utf8(value)?;
                if !value.chars().all(is_word_safe) {
                    bail!(
                        "{kind:?} passes the service command through an unquoted shell string, \
                         so `{value}` would be split or expanded.\n  \
                         Use paths and values without spaces or shell characters."
                    );
                }
            }
            (program, args)
        }
        ServiceManagerKind::Launchd | ServiceManagerKind::Sc | ServiceManagerKind::WinSw => {
            (program, args)
        }
    };

    let environment = match home {
        None => None,
        Some(home) => match kind {
            ServiceManagerKind::Launchd | ServiceManagerKind::WinSw => {
                Some(vec![("HOME".to_string(), home)])
            }
            // Written inside `Environment="HOME=…"`: quoted, so only the quote, the escape
            // character and systemd's `%` specifiers need escaping.
            ServiceManagerKind::Systemd => {
                Some(vec![("HOME".to_string(), systemd_escape(&home, false))])
            }
            _ => bail!(
                "{kind:?} services are not given environment variables, so the service could \
                 not find the credentials, at-rest key and ACME cache that `--auth` and \
                 `--domain` need under HOME.\n  \
                 Run `mdshelf serve` with those flags under a service definition you write."
            ),
        },
    };

    Ok(ServiceInstallCtx {
        label,
        program,
        args,
        contents: None,
        username: None,
        working_directory: None,
        environment,
        autostart: true,
        restart_policy: service_manager::RestartPolicy::default(),
    })
}

/// The service definition `install` would hand the `kind` backend for `serve` and
/// `home`, without registering anything. Lets a test feed it to a real backend writer.
#[cfg(any(test, feature = "test-support"))]
pub fn definition_for(
    kind: ServiceManagerKind,
    program: PathBuf,
    serve: &ServeArgs,
    home: Option<String>,
) -> Result<ServiceInstallCtx> {
    install_context(
        kind,
        service_label("mdshelf")?,
        program,
        serve_command(serve)?,
        home,
    )
}

fn utf8(value: &OsStr) -> Result<&str> {
    value
        .to_str()
        .with_context(|| format!("{} is not valid UTF-8", value.to_string_lossy()))
}

/// Characters a shell word may hold without quoting.
fn is_word_safe(character: char) -> bool {
    character.is_ascii_alphanumeric() || "/._:@=+,-".contains(character)
}

/// One `ExecStart=` word: quoted unless it is plainly safe, so spaces stay inside it,
/// with `%` (specifiers) and `$` (variable expansion) doubled to stay literal.
fn systemd_quote(value: &str) -> String {
    if !value.is_empty() && value.chars().all(is_word_safe) {
        return value.to_string();
    }
    format!("\"{}\"", systemd_escape(value, true))
}

/// Escape `value` for the inside of a double-quoted systemd string. `$` is literal in
/// `Environment=` but expands in `ExecStart=`.
fn systemd_escape(value: &str, command_line: bool) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '%' => escaped.push_str("%%"),
            '$' if command_line => escaped.push_str("$$"),
            '\n' => escaped.push_str("\\n"),
            _ => escaped.push(character),
        }
    }
    escaped
}

/// `serve` and its flags, rebuilt from the parsed arguments with every path made
/// absolute: the service starts in a working directory of the service manager's choosing.
///
/// The destructuring is exhaustive on purpose, so a new `serve` flag fails to compile
/// here until the service learns to pass it on.
fn serve_command(serve: &ServeArgs) -> Result<Vec<OsString>> {
    let ServeArgs {
        config,
        host,
        port,
        no_live_reload,
        auth,
        public_url,
        domain,
        tls_cert,
        tls_key,
        behind_proxy,
        acme_contact,
        acme_cache,
        acme_staging,
    } = serve;

    let mut command = vec![OsString::from("serve")];
    let mut push = |flag: &str, value: OsString| {
        command.push(OsString::from(flag));
        command.push(value);
    };
    if let Some(path) = config {
        push("--config", absolute(path)?.into());
    }
    if let Some(host) = host {
        push("--host", host.into());
    }
    if let Some(port) = port {
        push("--port", port.to_string().into());
    }
    if let Some(provider) = auth {
        push("--auth", provider.into());
    }
    if let Some(url) = public_url {
        push("--public-url", url.into());
    }
    if let Some(domain) = domain {
        push("--domain", domain.into());
    }
    if let Some(path) = tls_cert {
        push("--tls-cert", absolute(path)?.into());
    }
    if let Some(path) = tls_key {
        push("--tls-key", absolute(path)?.into());
    }
    if let Some(contact) = acme_contact {
        push("--acme-contact", contact.into());
    }
    if let Some(path) = acme_cache {
        push("--acme-cache", absolute(path)?.into());
    }
    for (set, flag) in [
        (*no_live_reload, "--no-live-reload"),
        (*behind_proxy, "--behind-proxy"),
        (*acme_staging, "--acme-staging"),
    ] {
        if set {
            command.push(OsString::from(flag));
        }
    }
    Ok(command)
}

fn absolute(path: &Path) -> Result<PathBuf> {
    std::path::absolute(path).with_context(|| format!("resolving {}", path.display()))
}

pub fn uninstall(args: ServiceNameArgs) -> Result<()> {
    let mut manager = <dyn ServiceManager>::native().context("detect native service manager")?;
    if args.user {
        manager.set_level(ServiceLevel::User)?;
    } else {
        manager.set_level(ServiceLevel::System)?;
    }
    let label = service_label(&args.name)?;
    manager
        .uninstall(ServiceUninstallCtx {
            label: label.clone(),
        })
        .map_err(|err| anyhow!("uninstall service: {err}"))?;
    println!("uninstalled service {}", args.name);
    Ok(())
}

pub fn start(args: ServiceNameArgs) -> Result<()> {
    let mut manager = <dyn ServiceManager>::native().context("detect native service manager")?;
    if args.user {
        manager.set_level(ServiceLevel::User)?;
    } else {
        manager.set_level(ServiceLevel::System)?;
    }
    let label = service_label(&args.name)?;
    manager
        .start(ServiceStartCtx {
            label: label.clone(),
        })
        .map_err(|err| anyhow!("start service: {err}"))?;
    println!("started service {}", args.name);
    Ok(())
}

pub fn stop(args: ServiceNameArgs) -> Result<()> {
    let mut manager = <dyn ServiceManager>::native().context("detect native service manager")?;
    if args.user {
        manager.set_level(ServiceLevel::User)?;
    } else {
        manager.set_level(ServiceLevel::System)?;
    }
    let label = service_label(&args.name)?;
    manager
        .stop(ServiceStopCtx {
            label: label.clone(),
        })
        .map_err(|err| anyhow!("stop service: {err}"))?;
    println!("stopped service {}", args.name);
    Ok(())
}

pub fn restart(args: ServiceNameArgs) -> Result<()> {
    stop(args.clone())?;
    start(args)?;
    Ok(())
}

pub fn status(args: ServiceNameArgs) -> Result<()> {
    let mut manager = <dyn ServiceManager>::native().context("detect native service manager")?;
    if args.user {
        manager.set_level(ServiceLevel::User)?;
    } else {
        manager.set_level(ServiceLevel::System)?;
    }
    let label = service_label(&args.name)?;
    let status = manager
        .status(ServiceStatusCtx {
            label: label.clone(),
        })
        .map_err(|err| anyhow!("service status: {err}"))?;
    match status {
        ServiceStatus::NotInstalled => println!("{}: not installed", args.name),
        ServiceStatus::Running => println!("{}: running", args.name),
        ServiceStatus::Stopped(reason) => {
            if let Some(message) = reason {
                println!("{}: stopped ({})", args.name, message);
            } else {
                println!("{}: stopped", args.name);
            }
        }
    }
    Ok(())
}

/// Returns the path to the running executable without resolving symlinks.
///
/// `std::env::current_exe()` calls `realpath` on macOS, which resolves Homebrew
/// symlinks to versioned Cellar paths. Using argv[0] preserves the stable
/// PATH entry (e.g. `/opt/homebrew/bin/mdshelf`) so the service automatically
/// picks up upgrades on restart without needing to be reinstalled.
fn resolve_executable_path() -> Result<PathBuf> {
    let arg0 = std::env::args_os().next().context("argv[0] is missing")?;
    let path = PathBuf::from(&arg0);

    if path.is_absolute() {
        return Ok(path);
    }

    if path.components().count() > 1 {
        let cwd = std::env::current_dir().context("get current working directory")?;
        return Ok(cwd.join(path));
    }

    if let Some(path_var) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path_var) {
            let candidate = directory.join(&path);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }

    std::env::current_exe().context("resolve path to mdshelf executable")
}

fn service_label(name: &str) -> Result<ServiceLabel> {
    let application: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect();
    let qualified = format!("com.mdshelf.{application}");
    ServiceLabel::from_str(&qualified).map_err(|err| anyhow!("invalid service label: {err}"))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command};

    fn install_args(flags: &[&str]) -> ServeArgs {
        let argv = ["mdshelf", "install"].iter().chain(flags);
        match Cli::try_parse_from(argv).expect("install parses").command {
            Command::Install(args) => args.serve,
            other => panic!("parsed as {other:?}"),
        }
    }

    fn serve_args(command: &[OsString]) -> ServeArgs {
        let argv = std::iter::once(OsString::from("mdshelf")).chain(command.iter().cloned());
        match Cli::try_parse_from(argv)
            .expect("service command parses")
            .command
        {
            Command::Serve(args) => args,
            other => panic!("parsed as {other:?}"),
        }
    }

    /// The service must start with exactly the options it was installed with: every
    /// flag survives, and relative paths are pinned to the directory `install` ran in.
    #[test]
    fn the_service_runs_serve_with_every_install_option() {
        let cases: [&[&str]; 2] = [
            &[
                "--config",
                "site.toml",
                "--host",
                "0.0.0.0",
                "--port",
                "8443",
                "--no-live-reload",
                "--auth",
                "google",
                "--public-url",
                "https://docs.acme.com",
                "--behind-proxy",
                "--tls-cert",
                "certs/full.pem",
                "--tls-key",
                "certs/key.pem",
            ],
            &[
                "--domain",
                "docs.acme.com",
                "--acme-contact",
                "ops@acme.com",
                "--acme-cache",
                "acme",
                "--acme-staging",
            ],
        ];
        for flags in cases {
            let installed = install_args(flags);
            let mut expected = installed.clone();
            for path in [
                &mut expected.config,
                &mut expected.tls_cert,
                &mut expected.tls_key,
                &mut expected.acme_cache,
            ]
            .into_iter()
            .flatten()
            {
                *path = std::path::absolute(&*path).unwrap();
            }

            let started = serve_args(&serve_command(&installed).expect("command builds"));
            assert_eq!(format!("{started:?}"), format!("{expected:?}"));
        }
    }

    #[test]
    fn a_plain_install_runs_serve_with_only_its_config() {
        let command = serve_command(&install_args(&["--config", "/srv/site.toml"])).unwrap();
        assert_eq!(
            command,
            ["serve", "--config", "/srv/site.toml"].map(OsString::from)
        );
    }

    fn definition(
        kind: ServiceManagerKind,
        config: &str,
        home: Option<&str>,
    ) -> Result<ServiceInstallCtx> {
        let serve = install_args(&["--config", config]);
        definition_for(
            kind,
            PathBuf::from("/usr/local/bin/mdshelf"),
            &serve,
            home.map(String::from),
        )
    }

    /// OpenRC and rc.d word-split the command with no quoting, so a value they would
    /// break is refused at install rather than registered as a service that cannot start.
    #[test]
    fn shell_script_backends_refuse_values_they_would_split() {
        for kind in [ServiceManagerKind::OpenRc, ServiceManagerKind::Rcd] {
            assert!(definition(kind, "/srv/site.toml", None).is_ok());
            let error = definition(kind, "/srv/My Docs/site.toml", None).unwrap_err();
            assert!(
                error.to_string().contains("/srv/My Docs/site.toml"),
                "{error}"
            );
        }
    }

    /// `--auth` needs HOME in the service; a backend that drops environment variables
    /// would register a service that cannot find its credentials.
    #[test]
    fn only_backends_that_pass_environment_accept_home() {
        for kind in [ServiceManagerKind::Launchd, ServiceManagerKind::Systemd] {
            let ctx = definition(kind, "/srv/site.toml", Some("/home/ana")).unwrap();
            assert_eq!(
                ctx.environment,
                Some(vec![("HOME".into(), "/home/ana".into())])
            );
        }
        for kind in [
            ServiceManagerKind::OpenRc,
            ServiceManagerKind::Rcd,
            ServiceManagerKind::Sc,
        ] {
            assert!(definition(kind, "/srv/site.toml", Some("/home/ana")).is_err());
        }
    }
}
